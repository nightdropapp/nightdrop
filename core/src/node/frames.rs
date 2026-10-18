//! [`Node::process_frame`]: the inbound wire-frame dispatcher. Split out of `node.rs`.
use super::*;

impl Node {
    /// Handle one decoded frame, whether it arrived directly (`from_address` set) or via
    /// the relay (`from_address` = None — routing uses the frame's own sender id).
    pub(super) fn process_frame(
        &mut self,
        from_address: Option<Address>,
        frame: Frame,
    ) -> Result<Option<(String, String)>> {
        // Any inbound frame we act on is a UI-visible change; the driver refreshes on this even
        // when no user message is produced (e.g. a silent Ack or a rename).
        self.dirty = true;
        match frame {
            Frame::Hello {
                identity_key,
                address,
                message,
            } => {
                crate::diag!("pair: inbound Hello — a peer is requesting a chat");
                let olm = message.to_olm()?;
                // Which invite this answers: its one-time key, which also names the pass we issued
                // with it.
                let otk = match &olm {
                    vodozemac::olm::OlmMessage::PreKey(m) => Some(m.one_time_key().to_base64()),
                    vodozemac::olm::OlmMessage::Normal(_) => None,
                };
                let accepted = crypto::accept_inbound(&mut self.identity, &identity_key, &olm)?;
                // A pass is single-use (`optional-relay.md` §5): this Hello used it.
                if let Some(otk) = &otk {
                    self.spend_pass(otk);
                }
                // Where the joiner's offline mail goes, from the first frame (`optional-relay.md`
                // §3 rule 3). `None` only from a Hello we cannot read, which then holds.
                let joiner_relays = super::hello_relay_set(&accepted.first_plaintext);
                // And the joiner's half of onion access (§5), held unused until the user accepts.
                let body = super::hello_body(&accepted.first_plaintext);
                let joiner_key = body
                    .as_ref()
                    .and_then(|b| b.client_key.clone())
                    .filter(|k| crate::transport::client_auth::validate_key(k).is_ok());
                let reply_pass = body
                    .as_ref()
                    .and_then(|b| b.pass.as_deref())
                    .and_then(super::decode_secret);
                // The pair's mailbox secret (`MailboxPair::seeded`). Absent only from a build that
                // predates it, which then agrees one the old way once approved.
                let mailbox = body
                    .as_ref()
                    .and_then(|b| b.mailbox.as_deref())
                    .and_then(super::decode_secret)
                    .map(super::mailbox::MailboxPair::seeded);
                let contact_id = identity_key;
                // Prefer the address the sender advertised (works over Tor/relay where the
                // transport can't tell us who called); fall back to the transport's view.
                // The address the joiner states is kept and dialed; a malformed one is ignored in
                // favour of the transport's own view.
                let peer_address = if super::limits::sane_address(&address) {
                    address
                } else {
                    from_address.unwrap_or_default()
                };
                // Create the chat if it's new, OR replace a previously-deleted (closed) one:
                // a fresh Hello on a torn-down chat is a re-pairing — start it clean. Capture the
                // prior chat's state *before* the overwrite so we can warn on a re-pair (§1.2): a
                // chat that existed and was closed is a known contact re-establishing a brand-new
                // secure session, after which any earlier safety-number verification no longer holds.
                let prior = self.chats.get(&contact_id);
                let was_verified = prior.map(|c| c.contact.verified).unwrap_or(false);
                // A Hello lands in one of three states: a brand-new contact, a re-pair of a chat we
                // deleted (closed), or a re-pair of a chat that's still OPEN (the peer paired again,
                // or the reverse direction of an existing pairing). All but the first are re-pairs
                // and must **adopt the new session** — the sender just built a fresh one and will
                // only encrypt with that, so keeping the old one would silently break decryption
                // (the double-pair bug). New/closed start clean; an open re-key keeps its history.
                let is_repair = prior.is_some();
                let open_rekey = prior.map(|c| !c.closed).unwrap_or(false);
                if open_rekey {
                    crate::diag!(
                        "pair: Hello from a contact we already have an open chat with — re-keying \
                         it rather than raising a new request"
                    );
                    let mut already_authorized = false;
                    if let Some(chat) = self.chats.get_mut(&contact_id) {
                        chat.session = accepted.session;
                        chat.peer_address = peer_address.clone();
                        chat.closed = false;
                        chat.relay_set = joiner_relays.clone();
                        // A new session starts with nothing told on it.
                        chat.relays_told = None;
                        chat.peer_key_pending = joiner_key.clone();
                        chat.pass_pending = reply_pass;
                        // The new session's pair secret; without one, the next agreement makes it.
                        chat.mailbox = mailbox;
                        chat.contact.verified = false; // new session invalidates prior verification
                        chat.contact.peer_verified = false; // …as does the peer's prior signal
                        already_authorized = chat.authorized;
                    }
                    // Re-pairing someone we already approved shows *us* no request — correct, we
                    // know them — but the joiner is sitting on "waiting for them to accept", and
                    // the approval echo only goes out from `authorize()`, which never runs here.
                    // Without this the joiner waits forever on a chat that is already live on our
                    // side. Echo the approval ourselves.
                    if already_authorized {
                        devlog!(
                            "[nightdrop] re-pair of an already-approved chat with {contact_id}: \
                             echoing the approval so their side stops waiting"
                        );
                        let from = self.identity_key();
                        let code = self
                            .chats
                            .get(&contact_id)
                            .and_then(|c| c.code.clone())
                            .unwrap_or_default();
                        let _ = self.deliver(
                            &peer_address,
                            &contact_id,
                            &Frame::Approved { from, code },
                        );
                    }
                } else {
                    // Which branch this took is the whole authorization story, so say it out loud:
                    // a stranger must land as a *request*, and "created a chat already approved"
                    // in this log means the invariant was bypassed.
                    crate::diag!(
                        "pair: new chat from an unknown identity — {}",
                        if self.require_authorization {
                            "held as a request pending approval"
                        } else {
                            "AUTO-APPROVED (require_authorization is off)"
                        }
                    );
                    // Inbound request: needs approval unless we auto-authorize. The most recent
                    // invite code goes with it so approval can echo it.
                    let mut chat = Chat::new_paired(
                        &contact_id,
                        &peer_address,
                        accepted.session,
                        !self.require_authorization,
                        self.last_invite_code.clone(),
                    );
                    chat.relay_set = joiner_relays.clone();
                    chat.peer_key_pending = joiner_key.clone();
                    chat.pass_pending = reply_pass;
                    chat.mailbox = mailbox;
                    self.chats.insert(contact_id.clone(), chat);
                }
                // Re-pair warning (§1.2): a known contact just re-established the chat with a new
                // session, so the safety number must be re-checked (louder if they were verified).
                if is_repair {
                    if let Some(chat) = self.chats.get_mut(&contact_id) {
                        chat.history.push(ChatMessage::system(
                            if was_verified {
                                "🔑 This contact re-paired with a new secure session. Your earlier \
                                 verification no longer applies — compare their safety number again \
                                 before trusting this chat."
                            } else {
                                "🔑 This contact re-paired, starting a new secure session. Verify \
                                 their safety number if you want to confirm who they are."
                            }
                            .to_string(),
                        ));
                    }
                }
                // Onion client auth (#22, `optional-relay.md` §5). A 0.2 joiner sent its key for our
                // onion and a reply pass for its own: they wait for the user's Accept
                // (`apply_pending_keys`). Without a reply pass (a transport with no restricted
                // discovery), answer with our own key as before. No-op off Tor.
                let authorized = self.chats.get(&contact_id).is_some_and(|c| c.authorized);
                if authorized {
                    self.apply_pending_keys(&contact_id);
                }
                if !peer_address.is_empty() && reply_pass.is_none() {
                    self.announce_client_key(&contact_id, &peer_address);
                }
                // Nothing else goes to a request: what this build is and can do is not a stranger's
                // business until the user accepts them (`authorize` sends it then).
                if !peer_address.is_empty() && authorized {
                    // Screenshot capability (#1) — same reason as the joiner side: this chat did not
                    // exist when the launch-time broadcast ran.
                    self.announce_captures_to(&contact_id);
                    self.announce_block_to(&contact_id);
                    self.announce_burns_to(&contact_id);
                    self.announce_version_to(&contact_id);
                    // Our relay set: the joiner holds mail for us until it arrives.
                    self.announce_relay_set_to(&contact_id);
                    // A Hello without a mailbox seed: agree one now rather than at the next tick.
                    if !self.mailbox_agreed(&contact_id) {
                        self.send_mailbox_key(&contact_id);
                    }
                }
                // The plaintext is the `HelloBody` (our relay set above), never a message: nothing
                // is shown, and the first real message arrives as a `Message` once approved.
                Ok(None)
            }
            Frame::Message { from, id, message } => {
                if !super::limits::sane_id(&id) {
                    return Ok(None); // a contact's id is kept with the message; bound it
                }
                let olm = message.to_olm()?;
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                if !chat.authorized {
                    // Dropped for good: the sender's side already showed it as sent, and nothing
                    // asks for it again. Said out loud because a message that vanishes with no
                    // trace on either device is the hardest kind of report to act on.
                    crate::diag!(
                        "recv: message from a contact still awaiting approval — DROPPED (the \
                         sender believes it was delivered)"
                    );
                    return Ok(None);
                }
                // Same reasoning: a ratchet that can't open this frame loses it permanently, and
                // `pump`'s `?` means the rest of this batch waits for the next tick. Never logs the
                // frame, the session, or any key material — only that it happened, and for whom.
                let plaintext = crypto::decrypt(&mut chat.session, &olm).inspect_err(|_| {
                    crate::diag!(
                        "recv: a message from a paired contact FAILED TO DECRYPT — DROPPED (their \
                         side shows it as sent; sessions have diverged)"
                    );
                })?;
                // Decrypting on their ratchet is proof it was them (silence-detection design).
                chat.mark_alive();
                let text =
                    super::limits::clamp_text(String::from_utf8_lossy(&plaintext).into_owned());
                // Their message proves they are alive; it says nothing whatever about whether they
                // picked up OUR earlier queued ones, which is what promoting those claimed.
                //
                // A message id we already hold is a resend of one that reached us by another path
                // (the sender re-queues on the relay when no receipt comes back). Do not show it
                // twice — but do let the caller receipt it, or the sender keeps retrying a message
                // we have had all along.
                let duplicate = !id.is_empty()
                    && chat
                        .history
                        .iter()
                        .any(|m| !m.from_me && m.msg_id == id && !m.system);
                if duplicate {
                    self.pending_receipts.push((from.clone(), id));
                    return Ok(None);
                }
                if !within_allowance(chat, 0) {
                    // Receipted anyway, or the sender re-queues it for ever.
                    self.pending_receipts.push((from.clone(), id));
                    return Ok(None);
                }
                chat.history
                    .push(ChatMessage::text(false, text.clone(), id.clone()));
                // Accepted: owe them a receipt naming it.
                self.pending_receipts.push((from.clone(), id));
                // First real message from the peer → the transient pairing/approval notices have
                // served their purpose; drop them so they don't linger above the conversation.
                self.clear_system_notices(&from, &["await_approval", "approved"]);
                Ok(Some((from, text)))
            }
            Frame::Burn { from, id, message } => {
                if !super::limits::sane_id(&id) {
                    return Ok(None);
                }
                // A burn message (`docs/design/burn-messages.md`). Same acceptance path as
                // `Message` — authorized, decryptable, de-duplicated — but the timer travels
                // inside the ciphertext and the message is stored blurred (not yet viewed).
                let olm = message.to_olm()?;
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                if !chat.authorized {
                    crate::diag!(
                        "recv: burn message from a contact still awaiting approval — DROPPED"
                    );
                    return Ok(None);
                }
                let plaintext = crypto::decrypt(&mut chat.session, &olm).inspect_err(|_| {
                    crate::diag!("recv: a burn message FAILED TO DECRYPT — DROPPED");
                })?;
                chat.mark_alive();
                let (burn_secs, text) = unpack_burn(&plaintext)?;
                if burn_secs == 0 {
                    // A burn frame whose duration we cannot read must not be rendered as an
                    // ordinary permanent message: that is exactly the false guarantee this
                    // feature exists to avoid. Drop it and say so.
                    crate::diag!(
                        "recv: burn message with an unreadable timer — DROPPED (refusing to \
                         render it as a permanent message)"
                    );
                    return Ok(None);
                }
                let duplicate = !id.is_empty()
                    && chat
                        .history
                        .iter()
                        .any(|m| !m.from_me && m.msg_id == id && !m.system);
                if duplicate {
                    self.pending_receipts.push((from.clone(), id));
                    return Ok(None);
                }
                if !within_allowance(chat, 0) {
                    self.pending_receipts.push((from.clone(), id));
                    return Ok(None);
                }
                let mut msg = ChatMessage::text(false, text.clone(), id.clone());
                msg.burn_secs = burn_secs;
                chat.history.push(msg);
                self.pending_receipts.push((from.clone(), id));
                self.clear_system_notices(&from, &["await_approval", "approved"]);
                // The event carries no text: a burn message must not surface its contents in a
                // notification or any other preview (`burn-messages.md` §4).
                Ok(Some((from, String::new())))
            }
            Frame::MailboxKey { from, message } => {
                // v2 mailbox agreement (`mailbox.rs`). Silent: a standing property of the pair, and
                // a line in every chat on rollout would be noise.
                self.on_mailbox_key(&from, &message);
                Ok(None)
            }
            Frame::Burns { from, message } => {
                // "My build understands burn messages." Standing property, no history entry —
                // on rollout it would otherwise post a line into every chat at once.
                if !self.verify_control(&from, &message, MARK_BURNS_V1) {
                    return Ok(None);
                }
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                if chat.contact.peer_supports_burn == Some(true) {
                    return Ok(None);
                }
                chat.contact.peer_supports_burn = Some(true);
                Ok(Some((from, String::new())))
            }
            Frame::Version { from, message } => {
                // "This is the Night Drop version I run." Standing property, no history entry.
                // Decrypted once (a ratchet decrypt spends a message key) and accepted only with the
                // version prefix and a plausible version after it: the text can reach the UI, and a
                // forged, replayed or spliced frame must change nothing.
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                let Ok(olm) = message.to_olm() else {
                    return Ok(None);
                };
                let Ok(pt) = crypto::decrypt(&mut chat.session, &olm) else {
                    return Ok(None);
                };
                let Some(version) = pt
                    .strip_prefix(MARK_VERSION_PREFIX)
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .filter(|v| is_plausible_version(v))
                else {
                    return Ok(None);
                };
                if !version_outdated(version) {
                    chat.peer_wire_old = false; // updated: what we hold for them can go now
                    chat.forget_upgrade_notice();
                }
                if chat.contact.peer_app_version.as_deref() == Some(version) {
                    return Ok(None);
                }
                // They updated. A build can ignore a marker it does not know yet still confirm the
                // frame, so what we "told" an older build may never have registered (seen in the
                // 0.2 device pass: Block screenshots announced to a build without the marker).
                // Tell the new build again.
                // Each change makes us send our announcements again, so it counts against the daily
                // allowance: a contact flipping versions cannot use us as an amplifier (R12-2).
                let updated = chat.contact.peer_app_version.is_some() && within_allowance(chat, 0);
                if updated {
                    chat.told_captures = None;
                    chat.told_block = None;
                }
                chat.contact.peer_app_version = Some(version.to_string());
                crate::diag!("version: a contact runs {version}");
                if updated {
                    // And forget that this run already told them, or the retry skips them.
                    self.captures_announced.remove(&from);
                    self.block_announced.remove(&from);
                }
                Ok(Some((from, String::new())))
            }
            Frame::Edit { from, message } => {
                // The peer edited an earlier message of theirs: replace its text and mark it
                // "edited". Only their own recent messages qualify — the target must be an
                // inbound text we received within the edit window (measured on our clock,
                // which ≈ receipt time; a queued-then-drained pair arrives together, so both
                // sides of that race stay editable).
                let olm = message.to_olm()?;
                let (target_id, new_text) = {
                    let Some(chat) = self.chats.get_mut(&from) else {
                        return Ok(None);
                    };
                    if !chat.authorized {
                        return Ok(None);
                    }
                    let plaintext = crypto::decrypt(&mut chat.session, &olm)?;
                    unpack_edit(&plaintext)?
                };
                let now = crate::api::now_secs();
                if let Some(chat) = self.chats.get_mut(&from) {
                    if let Some(msg) = chat.history.iter_mut().find(|m| {
                        !m.from_me
                            && !m.system
                            && m.kind == "text"
                            && !m.msg_id.is_empty()
                            && m.msg_id == target_id
                            && m.at != 0
                            && now.saturating_sub(m.at) <= EDIT_WINDOW.as_secs()
                    }) {
                        msg.text = new_text;
                        msg.edited = true;
                        return Ok(Some((from, String::new())));
                    }
                }
                Ok(None)
            }
            Frame::Unsend { from, message } => {
                // The peer unsent an earlier message of theirs: replace it with a tombstone.
                // Same eligibility window as an edit (their own recent inbound message).
                let olm = message.to_olm()?;
                let target_id = {
                    let Some(chat) = self.chats.get_mut(&from) else {
                        return Ok(None);
                    };
                    if !chat.authorized {
                        return Ok(None);
                    }
                    let plaintext = crypto::decrypt(&mut chat.session, &olm)?;
                    unpack_unsend(&plaintext)?
                };
                let now = crate::api::now_secs();
                if let Some(chat) = self.chats.get_mut(&from) {
                    if let Some(msg) = chat.history.iter_mut().find(|m| {
                        !m.from_me
                            && !m.system
                            && m.kind == "text"
                            && !m.msg_id.is_empty()
                            && m.msg_id == target_id
                            && m.at != 0
                            && now.saturating_sub(m.at) <= EDIT_WINDOW.as_secs()
                    }) {
                        make_tombstone(msg);
                        return Ok(Some((from, String::new())));
                    }
                }
                Ok(None)
            }
            Frame::Storage { from, message } => {
                // The peer toggled opt-in 24h server storage: mirror the flag so BOTH sides
                // show the persistent warning while it is active (§6 invariant), and leave
                // an explicit notice in the chat.
                let olm = message.to_olm()?;
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                if !chat.authorized {
                    return Ok(None);
                }
                let plaintext = crypto::decrypt(&mut chat.session, &olm)?;
                let enabled = plaintext == b"on";
                // Each switch leaves a notice and a timeline entry, so it counts against the
                // contact's daily allowance like a message; a repeat of the current state is no news.
                if enabled == chat.contact.remote_storage || !within_allowance(chat, 0) {
                    return Ok(None);
                }
                chat.contact.remote_storage = enabled;
                chat.storage_timeline
                    .push((crate::api::now_secs(), enabled));
                chat.history.push(ChatMessage::system(if enabled {
                    "☁️ The other person enabled 24h server storage for this chat. Messages \
                     are held (encrypted) on the relay for up to 24 hours."
                        .to_string()
                } else {
                    "☁️ The other person disabled server storage for this chat. Messages stay \
                     on your devices only."
                        .to_string()
                }));
                Ok(Some((from, String::new())))
            }
            Frame::Disappearing { from, message } => {
                // The peer changed the shared disappearing-messages timer: mirror it so both
                // devices expire on the same horizon, and leave a notice.
                let olm = message.to_olm()?;
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                if !chat.authorized {
                    return Ok(None);
                }
                let plaintext = crypto::decrypt(&mut chat.session, &olm)?;
                let secs: u64 = String::from_utf8(plaintext)
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if secs == chat.contact.disappearing_secs || !within_allowance(chat, 0) {
                    return Ok(None);
                }
                chat.contact.disappearing_secs = secs;
                chat.disappearing_timeline
                    .push((crate::api::now_secs(), secs));
                chat.history.push(ChatMessage::system(format!(
                    "⏱️ The other person set disappearing messages to {}.",
                    disappearing_label(secs)
                )));
                Ok(Some((from, String::new())))
            }
            Frame::Approved { from, code } => {
                // The inviter approved our request: drop the "waiting to be accepted" notice and
                // surface an approval confirmation (itself cleared on the first received message).
                devlog!("[nightdrop] received approval from {from} (code='{code}')");
                if let Some(chat) = self.chats.get(&from) {
                    // Only the approval that ends the wait is news. The frame can arrive twice (a
                    // direct copy that looked lost, then the relay copy behind it: S23, 2026-10-11)
                    // and is unauthenticated besides, so a repeat must change nothing on screen.
                    let was_waiting = super::messaging::awaiting_approval(chat);
                    self.clear_system_notices(&from, &["await_approval"]);
                    if let Some(chat) = self.chats.get_mut(&from).filter(|_| was_waiting) {
                        chat.history.push(ChatMessage::system_tagged(
                            "✅ Your chat request was approved. You can start chatting."
                                .to_string(),
                            "approved",
                        ));
                    }
                    // Our set went in the Hello, so this is normally a no-op; it covers a list
                    // edited while we waited. (`Approved` is unauthenticated, but this only sends
                    // our own set on our session with them, which a forger cannot read.)
                    self.announce_relay_set_to(&from);
                    // What we announced when we joined (capability, capture block, burns,
                    // version) went out before the inviter had read our request, and an inviter
                    // that gets a frame from someone it does not know yet drops it — while our side
                    // counted it delivered and never sent it again (0.2 device pass: the S23's
                    // version and capability never reached the contact it joined). Approval is the
                    // first moment they certainly know us: tell them again.
                    if was_waiting {
                        if let Some(chat) = self.chats.get_mut(&from) {
                            chat.told_captures = None;
                            chat.told_block = None;
                            chat.told_burns = false;
                            chat.told_version = None;
                        }
                        self.captures_announced.remove(&from);
                        self.block_announced.remove(&from);
                        self.burns_announced.remove(&from);
                        self.version_announced.remove(&from);
                        self.announce_captures_to(&from);
                        self.announce_block_to(&from);
                        self.announce_burns_to(&from);
                        self.announce_version_to(&from);
                    }
                    return Ok(Some((from, String::new())));
                }
                Ok(None)
            }
            Frame::CodeInUse { from, code } => {
                // The code we joined with was already spent: warn and close our side.
                devlog!("[nightdrop] code '{code}' reported already used by {from}");
                // Unauthenticated (`from` is a claim), so it may only end a request that is still
                // waiting — the one case the inviter sends it, refusing before it ever writes to
                // the session. Once anything has arrived on our session the chat is established and
                // ignores it; else anyone who could deliver a frame could close any chat
                // (self-audit 2026-10-13, finding 5).
                let waiting = self
                    .chats
                    .get(&from)
                    .is_some_and(|c| !c.session.has_received_message());
                if !waiting {
                    return Ok(None);
                }
                if let Some(chat) = self.chats.get_mut(&from) {
                    chat.closed = true;
                    chat.history.push(ChatMessage::system(
                        "⚠️ That invite code has already been used. Ask for a new code to start \
                         a chat."
                            .to_string(),
                    ));
                    return Ok(Some((from, String::new())));
                }
                Ok(None)
            }
            Frame::Closed { from, message } => {
                // The peer deleted the chat. Authenticate first — a forged/replayed Closed must
                // not be able to tear down a live chat.
                if !self.verify_control(&from, &message, MARK_CLOSED) {
                    return Ok(None);
                }
                // Keep the conversation visible but mark it closed and leave a notice; the user
                // creates a new chat to continue.
                devlog!("[nightdrop] peer {from} deleted the chat");
                if let Some(chat) = self.chats.get_mut(&from).filter(|c| !c.closed) {
                    chat.closed = true;
                    chat.history.push(ChatMessage::system(
                        "👻 The other person deleted this chat. A new chat will need to be \
                         created to keep talking."
                            .to_string(),
                    ));
                    return Ok(Some((from, String::new())));
                }
                Ok(None)
            }
            Frame::Cover { .. } => {
                // Cover traffic (#4): dropped on sight. It exists only to give the relay mailbox
                // activity that looks like real mail; nothing downstream should ever see it — no
                // history entry, no ack, no notification, and no `dirty` flag beyond the one set
                // above (a poll that drained only cover has changed nothing worth persisting).
                Ok(None)
            }
            Frame::Screenshot { from, message } => {
                // Transparency (#1): the peer captured this conversation — a screenshot, or a screen
                // recording (Android 15+) — where nothing we do (disappearing timers, remote-storage
                // caps, unsend) can reach it. Authenticated: the kind rides in *which* marker
                // decrypts, decrypted once (a ratchet decrypt spends a message key), so a stranger
                // who knows the identity key cannot manufacture distrust between two people.
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                let Ok(olm) = message.to_olm() else {
                    return Ok(None);
                };
                let note = match crypto::decrypt(&mut chat.session, &olm) {
                    Ok(pt) if pt == MARK_SCREENSHOT => {
                        "📸 The other person took a screenshot of this chat."
                    }
                    Ok(pt) if pt == MARK_RECORDING => {
                        "🔴 The other person is recording their screen with this chat open."
                    }
                    Ok(pt) if pt == MARK_CAPTURE_OVERRIDE_ON => {
                        "🔓 The other person turned off screenshot blocking for this chat on their \
                         phone. You are still told about each screenshot they take."
                    }
                    Ok(pt) if pt == MARK_CAPTURE_OVERRIDE_OFF => {
                        "🔒 The other person blocks screenshots in this chat again, as you asked."
                    }
                    _ => return Ok(None), // forged, replayed, or a kind this build doesn't know
                };
                chat.mark_alive();
                // Logged every time, not once: unlike `BackedUp`'s state flag, repetition here
                // is information the user should have — within the daily allowance.
                if !within_allowance(chat, 0) {
                    return Ok(None);
                }
                chat.history.push(ChatMessage::system(note.to_string()));
                Ok(Some((from, String::new())))
            }
            Frame::BackedUp { from, message } => {
                // Transparency (#7): the peer made a Full backup that includes this chat, so
                // our messages now also live in their backup. Authenticate first so a stranger
                // can't fake the scare. Set the flag (drives a persistent warning) and note it
                // once so repeated signals don't spam the history.
                if !self.verify_control(&from, &message, MARK_BACKEDUP) {
                    return Ok(None);
                }
                if let Some(chat) = self.chats.get_mut(&from) {
                    if !chat.contact.peer_backed_up {
                        chat.contact.peer_backed_up = true;
                        chat.history.push(ChatMessage::system(
                            "🗄️ The other person is keeping a backup of this chat, so your \
                             messages may persist in their backup."
                                .to_string(),
                        ));
                        return Ok(Some((from, String::new())));
                    }
                }
                Ok(None)
            }
            Frame::Verified { from, message } => {
                // Informational (§5b′): the peer told us *they* marked our safety number verified
                // (or un-verified). The state rides in *which* marker decrypts on their session, so
                // it's E2E-authenticated — a stranger can't forge it and a tampered flag can't lie.
                // Decrypt ONCE (a ratchet decrypt spends a message key, so we can't try both
                // markers with verify_control); branch on the plaintext. Crucially this only sets
                // `peer_verified` for the UI — never our own `verified`, so a compromised peer can't
                // fabricate a verified badge on our screen: each side still confirms independently.
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                let Ok(olm) = message.to_olm() else {
                    return Ok(None);
                };
                let peer_verified = match crypto::decrypt(&mut chat.session, &olm) {
                    Ok(pt) if pt == MARK_VERIFIED => true,
                    Ok(pt) if pt == MARK_UNVERIFIED => false,
                    _ => return Ok(None), // forged, replayed, or spliced — ignore
                };
                if chat.contact.peer_verified == peer_verified || !within_allowance(chat, 0) {
                    return Ok(None); // no change → no history spam; flipping counts as a message
                }
                chat.contact.peer_verified = peer_verified;
                let note = if peer_verified {
                    "✅ The other person marked this chat's safety number verified. Compare it \
                     yourself too to be sure — this is only what they told you."
                } else {
                    "⚠️ The other person cleared their verification of this chat's safety number."
                };
                chat.history.push(ChatMessage::system(note.to_string()));
                Ok(Some((from, String::new())))
            }
            Frame::Captures { from, message } => {
                // Whether the PEER's device can report screenshots (#1). Same shape as `Verified`:
                // decrypt once (a ratchet decrypt spends a message key, so we cannot try both
                // markers) and branch on the plaintext. Nothing here is ever inferred from silence
                // — a peer on an older build sends nothing and stays `None`, which the UI must show
                // as "unknown", not as "captures are visible". Claiming the reassuring answer
                // without evidence is the bug this whole signal exists to remove.
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                let Ok(olm) = message.to_olm() else {
                    return Ok(None);
                };
                let visible = match crypto::decrypt(&mut chat.session, &olm) {
                    Ok(pt) if pt == MARK_CAPTURES_VISIBLE => true,
                    Ok(pt) if pt == MARK_CAPTURES_SILENT => false,
                    // Their "Block screenshots": our app blocks captures while their chat is open.
                    // A standing property like the rest of this frame: no history entry. Carries a
                    // sequence; an older one arriving after a newer one is ignored.
                    Ok(pt) if pt.starts_with(MARK_CAPTURE_BLOCK_PREFIX) => {
                        let Some((on, seq)) =
                            std::str::from_utf8(&pt[MARK_CAPTURE_BLOCK_PREFIX.len()..])
                                .ok()
                                .and_then(|rest| rest.split_once(':'))
                                .and_then(|(v, s)| Some((v == "1", s.parse::<u64>().ok()?)))
                        else {
                            return Ok(None);
                        };
                        chat.mark_alive();
                        if seq <= chat.peer_block_seq {
                            return Ok(None);
                        }
                        chat.peer_block_seq = seq;
                        // A change costs a save and a platform call here: counted against the
                        // daily allowance, so flipping it cannot be a flood (round 12, R12-3).
                        if chat.contact.peer_blocks_capture == Some(on)
                            || !within_allowance(chat, 0)
                        {
                            return Ok(None);
                        }
                        chat.contact.peer_blocks_capture = Some(on);
                        return Ok(Some((from, String::new())));
                    }
                    _ => return Ok(None), // forged, replayed, or spliced — ignore
                };
                if chat.contact.peer_captures_silent == Some(!visible) {
                    return Ok(None);
                }
                chat.contact.peer_captures_silent = Some(!visible);
                // No history entry either way. This is a standing property of their device, not
                // something that happened, and on rollout it would post a line into every existing
                // chat at once. The chat header carries it instead.
                Ok(Some((from, String::new())))
            }
            Frame::Ack { from, message } => {
                // Silent delivery ack: the peer drained our relay-held messages → mark them
                // delivered. Authenticate first so a forged/replayed ack can't lie about delivery.
                // No history entry, no reply (never ack an ack).
                if !self.verify_control(&from, &message, MARK_ACK) {
                    return Ok(None);
                }
                // Deliberately promotes nothing. `Ack` names no message — it means only "I drained
                // your mailbox" — so honouring it flipped messages it could not vouch for,
                // including ones dropped on arrival and ones queued after the drain. Kept on the
                // wire (peers on older builds still send it) as proof of life; `Frame::Delivered`
                // is what confirms a message.
                if let Some(chat) = self.chats.get_mut(&from) {
                    chat.mark_alive();
                }
                Ok(None)
            }
            Frame::Delivered { from, message } => {
                // Per-message delivery receipt: the peer has actually processed the message named
                // by the decrypted payload. Decrypting on their session is the authentication —
                // a forgery or replay cannot mark anything delivered.
                let olm = message.to_olm()?;
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                let Ok(plaintext) = crypto::decrypt(&mut chat.session, &olm) else {
                    return Ok(None);
                };
                let named = String::from_utf8_lossy(&plaintext).into_owned();
                // Proof of life, exactly like any other authenticated frame from them.
                chat.mark_alive();
                // An empty payload names nothing. It would otherwise match the first attachment in
                // the chat, since media carries no `msg_id` — a wildcard, and the one shape of this
                // frame that could confirm a message nobody vouched for.
                if named.is_empty() {
                    return Ok(None);
                }
                // ONLY the named message. Not "everything older": a message can be lost while a
                // later one arrives (a core torn down mid-flight, 2026-08-02), and promoting its
                // neighbours would restore the very lie this frame exists to end.
                //
                // `t:` marks an attachment, identified by the `transfer_id` both sides hold — the
                // id media has instead of a `msg_id`.
                let found = match named.strip_prefix("t:") {
                    Some(transfer_id) => chat.history.iter_mut().find(|m| {
                        m.from_me && !m.transfer_id.is_empty() && m.transfer_id == transfer_id
                    }),
                    None => chat
                        .history
                        .iter_mut()
                        .find(|m| m.from_me && !m.msg_id.is_empty() && m.msg_id == named),
                };
                if let Some(m) = found {
                    if m.delivery != "delivered" {
                        crate::diag!(
                            "receipt: message confirmed delivered {}s after it was composed (was {:?})",
                            crate::api::now_secs().saturating_sub(m.at),
                            m.delivery
                        );
                    }
                    m.delivery = "delivered".to_string();
                }
                let msg_id = named;
                // Confirmed, so stop the relay-fallback clock for it (`sweep_unconfirmed`).
                self.awaiting_receipt
                    .retain(|a| !(a.contact_id == from && a.msg_id == msg_id));
                Ok(None)
            }
            Frame::Name { from, message } => {
                // The peer renamed themselves: decrypt and update their display name, which
                // relabels every message from them (the UI derives the sender from the
                // contact, not per-message).
                let olm = message.to_olm()?;
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                if !chat.authorized {
                    return Ok(None);
                }
                let plaintext = crypto::decrypt(&mut chat.session, &olm)?;
                let name = String::from_utf8_lossy(&plaintext);
                chat.contact.their_name = super::clean_display_name(&name);
                devlog!(
                    "[nightdrop] peer {from} is now known as '{}'",
                    chat.contact.their_name
                );
                // Report a change so the poller refreshes (and persists) the UI.
                Ok(Some((from, String::new())))
            }
            Frame::Address { from, message } => {
                // The peer's onion changed (rebuilt keystore): decrypt the new address and
                // update where we route replies, so the contact survives the change (#11).
                let olm = message.to_olm()?;
                let new_address = {
                    let Some(chat) = self.chats.get_mut(&from) else {
                        return Ok(None);
                    };
                    if !chat.authorized {
                        return Ok(None);
                    }
                    let plaintext = crypto::decrypt(&mut chat.session, &olm)?;
                    let new_address = String::from_utf8_lossy(&plaintext).into_owned();
                    // Each change also mints and sends a new client key, so it counts against the
                    // daily allowance: two addresses alternated could otherwise loop for ever.
                    if !super::limits::sane_address(&new_address)
                        || new_address == chat.peer_address
                        || !within_allowance(chat, 0)
                    {
                        return Ok(None);
                    }
                    chat.peer_address = new_address.clone();
                    chat.history.push(ChatMessage::system(
                        "🔄 The other person's connection address changed; messages will keep \
                         reaching them."
                            .to_string(),
                    ));
                    new_address
                };
                devlog!("[nightdrop] peer {from} announced a new address");
                // Onion client auth (#22): our old client key was for their old onion — mint a fresh
                // one for the new onion and send it so they re-authorize us and we stay reachable.
                self.announce_client_key(&from, &new_address);
                Ok(Some((from, String::new())))
            }
            Frame::RelaySet { from, message } => {
                // The peer's complete relay set (`optional-relay.md` §3): their offline mail goes
                // to exactly this, and nowhere while it is empty.
                let olm = message.to_olm()?;
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                if !chat.authorized {
                    return Ok(None);
                }
                let plaintext = crypto::decrypt(&mut chat.session, &olm)?;
                let set = match serde_json::from_slice::<RelaySet>(&plaintext) {
                    Ok(set) => super::sanitize_peer_set(set),
                    Err(_) => return Ok(None),
                };
                // Only an approved chat is sent a `RelaySet`, and this one decrypted on their
                // ratchet: the inviter has accepted us. Our own key for their onion replaces their
                // invite's pass now (`optional-relay.md` §5).
                if let Some(own) = chat.own_key_pending.take() {
                    let _ = self.transport.insert_client_key(&chat.peer_address, &own);
                    chat.client_key = Some(own);
                    crate::diag!("pair: accepted — our own onion key replaces the invite's pass");
                }
                if chat.relay_set.as_ref() == Some(&set) {
                    return Ok(None);
                }
                chat.relay_set = Some(set);
                devlog!("[nightdrop] peer {from} announced their relay set");
                Ok(Some((from, String::new())))
            }
            Frame::ClientKey { .. } => {
                // The unsealed form: its `from` is only a claim, so acting on it let anyone who
                // could deliver a frame replace a contact's onion key (self-audit 2026-10-13,
                // finding 5). Still decoded, so a pre-fix 0.2 test build's frame is not an error.
                crate::diag!("onion-auth: ignored an unsealed client key");
                Ok(None)
            }
            Frame::ClientKeySealed { from, message } => {
                // Onion client auth (#22): the peer handed us their client key for our onion.
                // Authorized only for an **approved** contact (`optional-relay.md` §5): onion access
                // follows the user's decision, so a pending request's key waits for Accept, and a
                // Decline leaves it none. A stranger with no chat plants nothing. `authorize_client`
                // validates the key and is a no-op off Tor. Silent (no UI change).
                // Decrypted on the sender's session: a forgery, replay or splice changes nothing.
                let Some(client_key) = self.chats.get_mut(&from).and_then(|chat| {
                    let olm = message.to_olm().ok()?;
                    let pt = crypto::decrypt(&mut chat.session, &olm).ok()?;
                    let key = pt.strip_prefix(MARK_CLIENT_KEY_PREFIX)?;
                    String::from_utf8(key.to_vec()).ok()
                }) else {
                    return Ok(None);
                };
                if let Some(chat) = self.chats.get_mut(&from) {
                    if chat.authorized {
                        let _ = self.transport.authorize_client(&from, &client_key);
                    } else if crate::transport::client_auth::validate_key(&client_key).is_ok() {
                        chat.peer_key_pending = Some(client_key);
                    }
                }
                Ok(None)
            }
            Frame::MediaIncoming { from, message } => {
                // A heads-up that an attachment (video) is on its way: show a placeholder with
                // its thumbnail + size so the receiver isn't left waiting blindly.
                let olm = message.to_olm()?;
                let envelope = {
                    let Some(chat) = self.chats.get_mut(&from) else {
                        return Ok(None);
                    };
                    if !chat.authorized {
                        return Ok(None);
                    }
                    crypto::decrypt(&mut chat.session, &olm)?
                };
                let (transfer_id, kind, mime, size, mut thumb) = unpack_media_incoming(&envelope)?;
                // A preview is a small JPEG (320 px); anything bigger is not one, and was stored
                // unbounded (self-audit 2026-10-17). Dropped, not the placeholder.
                if thumb.len() > super::limits::MAX_THUMB_BYTES {
                    thumb.clear();
                }
                if !self
                    .chats
                    .get_mut(&from)
                    .is_some_and(|c| within_allowance(c, thumb.len() as u64))
                {
                    return Ok(None);
                }
                let thumb_id = if thumb.is_empty() {
                    String::new()
                } else {
                    self.store_media(&thumb)?
                };
                if let Some(chat) = self.chats.get_mut(&from) {
                    // Placeholder: media_id empty == still receiving.
                    chat.history.push(ChatMessage::media(
                        false,
                        kind,
                        mime,
                        String::new(),
                        size,
                        transfer_id,
                        thumb_id,
                    ));
                }
                devlog!("[nightdrop] incoming {size}-byte attachment from {from}");
                Ok(Some((from, String::new())))
            }
            Frame::Media { from, message } => {
                // Decrypt the attachment envelope, seal it at rest, and add (or complete) a
                // media message.
                let olm = message.to_olm()?;
                let envelope = {
                    let Some(chat) = self.chats.get_mut(&from) else {
                        return Ok(None);
                    };
                    if !chat.authorized {
                        return Ok(None);
                    }
                    crypto::decrypt(&mut chat.session, &olm)?
                };
                let (transfer_id, kind, mime, data) = unpack_media(&envelope)?;
                let size = data.len() as u64;
                // Have we already completed this attachment? With server storage on, every message
                // is sent directly *and* copied to the relay, so both arrive — and a completed
                // message no longer matches the placeholder lookup below, which used to add the
                // photo a second time.
                //
                // Decided BEFORE the payload is written: `store_media` seals a fresh file per call,
                // so storing first and discarding the duplicate afterwards would strand one
                // orphaned copy of every attachment on disk, referenced by nothing and removed by
                // nothing.
                let duplicate = self.chats.get(&from).is_some_and(|c| {
                    c.history.iter().any(|m| {
                        !m.from_me
                            && !m.transfer_id.is_empty()
                            && m.transfer_id == transfer_id
                            && !m.media_id.is_empty()
                    })
                });
                if !duplicate
                    && !self
                        .chats
                        .get_mut(&from)
                        .is_some_and(|c| within_allowance(c, size))
                {
                    // Over the allowance: nothing stored, and any "incoming" placeholder goes too.
                    if let Some(chat) = self.chats.get_mut(&from) {
                        chat.history.retain(|m| {
                            !(m.transfer_id == transfer_id && !m.from_me && m.media_id.is_empty())
                        });
                    }
                    self.pending_receipts
                        .push((from.clone(), format!("t:{transfer_id}")));
                    return Ok(None);
                }
                if !duplicate {
                    let media_id = self.store_media(&data)?;
                    if let Some(chat) = self.chats.get_mut(&from) {
                        // Complete a matching "incoming" placeholder, else add a fresh message.
                        let placeholder = chat.history.iter_mut().find(|m| {
                            !m.transfer_id.is_empty()
                                && m.transfer_id == transfer_id
                                && m.media_id.is_empty()
                        });
                        if let Some(m) = placeholder {
                            m.media_id = media_id;
                        } else {
                            chat.history.push(ChatMessage::media(
                                false,
                                kind,
                                mime,
                                media_id,
                                size,
                                transfer_id.clone(),
                                String::new(),
                            ));
                        }
                    }
                }
                // Attachments have no `msg_id` — the id both sides share is the `transfer_id`, and
                // it is only legible here, after the envelope is open. Tagged so the sender cannot
                // confuse it with a text `msg_id`; an older sender simply ignores what it can't
                // match, which is a safe no-op.
                self.pending_receipts
                    .push((from.clone(), format!("t:{transfer_id}")));
                devlog!("[nightdrop] received {size}-byte attachment from {from}");
                if duplicate {
                    return Ok(None);
                }
                Ok(Some((from, String::new())))
            }
            Frame::BurnMedia { from, message } => {
                // A burn attachment. Simpler than `Media`: there is never a placeholder to
                // complete, because a burn attachment sends no `MediaIncoming` pre-signal.
                let olm = message.to_olm()?;
                let envelope = {
                    let Some(chat) = self.chats.get_mut(&from) else {
                        return Ok(None);
                    };
                    if !chat.authorized {
                        return Ok(None);
                    }
                    crypto::decrypt(&mut chat.session, &olm)?
                };
                let (burn_secs, transfer_id, kind, mime, data) = unpack_burn_media(&envelope)?;
                if burn_secs == 0 {
                    // Same rule as `Burn`: an attachment whose timer will not parse must not be
                    // stored as an ordinary permanent one.
                    crate::diag!(
                        "recv: burn attachment with an unreadable timer — DROPPED (refusing to \
                         keep it permanently)"
                    );
                    return Ok(None);
                }
                let size = data.len() as u64;
                let duplicate = self.chats.get(&from).is_some_and(|c| {
                    c.history.iter().any(|m| {
                        !m.from_me
                            && !m.transfer_id.is_empty()
                            && m.transfer_id == transfer_id
                            && !m.media_id.is_empty()
                    })
                });
                if !duplicate
                    && !self
                        .chats
                        .get_mut(&from)
                        .is_some_and(|c| within_allowance(c, size))
                {
                    self.pending_receipts
                        .push((from.clone(), format!("t:{transfer_id}")));
                    return Ok(None);
                }
                if !duplicate {
                    let media_id = self.store_media(&data)?;
                    if let Some(chat) = self.chats.get_mut(&from) {
                        let mut msg = ChatMessage::media(
                            false,
                            kind,
                            mime,
                            media_id,
                            size,
                            transfer_id.clone(),
                            String::new(),
                        );
                        msg.burn_secs = burn_secs;
                        chat.history.push(msg);
                    }
                }
                self.pending_receipts
                    .push((from.clone(), format!("t:{transfer_id}")));
                if duplicate {
                    return Ok(None);
                }
                Ok(Some((from, String::new())))
            }
            Frame::Viewed { from, message } => {
                // They opened a burn message of ours. Drop our copy now rather than at the 24h
                // horizon. Only ever applies to our OWN sent burn messages — a `Viewed` naming
                // anything else is ignored, so it can never be used to delete arbitrary history.
                let olm = message.to_olm()?;
                let target = {
                    let Some(chat) = self.chats.get_mut(&from) else {
                        return Ok(None);
                    };
                    if !chat.authorized {
                        return Ok(None);
                    }
                    String::from_utf8(crypto::decrypt(&mut chat.session, &olm)?)?
                };
                let mut dead_media: Vec<String> = Vec::new();
                let mut removed = false;
                if let Some(chat) = self.chats.get_mut(&from) {
                    let before = chat.history.len();
                    chat.history.retain(|m| {
                        let ours = m.from_me && m.burn_secs > 0;
                        let named = (!m.msg_id.is_empty() && m.msg_id == target)
                            || (!m.transfer_id.is_empty() && m.transfer_id == target);
                        if ours && named {
                            for id in [m.media_id.as_str(), m.thumb_id.as_str()] {
                                if !id.is_empty() {
                                    dead_media.push(id.to_string());
                                }
                            }
                            return false;
                        }
                        true
                    });
                    removed = chat.history.len() != before;
                }
                for id in dead_media {
                    self.forget_media(&id); // sealed file and any decrypted copy
                }
                if !removed {
                    return Ok(None);
                }
                self.dirty = true;
                Ok(Some((from, String::new())))
            }
            // Short-code SPAKE2 runs over the rendezvous mailbox before any transport session
            // exists (see `run_join_handshake`/`service_invites`), so a `Pake` frame on
            // the peer transport is unexpected — reserved for a future in-band re-key.
            Frame::Pake { .. } => Ok(None),
        }
    }
}

/// Whether a contact's message may be kept, counting it against their daily allowance
/// ([`limits::Allowance`]). Over the allowance it is dropped, and the chat gets one notice per day.
fn within_allowance(chat: &mut Chat, media_bytes: u64) -> bool {
    use super::limits::{Admit, OVER_ALLOWANCE_NOTICE};
    match chat.allowance.admit(crate::api::now_secs(), media_bytes) {
        Admit::Yes => true,
        Admit::DropAndNotice => {
            crate::diag!("recv: a contact went past the daily allowance — dropping until tomorrow");
            chat.history
                .push(ChatMessage::system(OVER_ALLOWANCE_NOTICE.to_string()));
            false
        }
        Admit::Drop => false,
    }
}
