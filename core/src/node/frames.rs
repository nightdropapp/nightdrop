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
                let accepted = crypto::accept_inbound(&mut self.identity, &identity_key, &olm)?;
                let contact_id = identity_key;
                // Prefer the address the sender advertised (works over Tor/relay where the
                // transport can't tell us who called); fall back to the transport's view.
                let peer_address = if !address.is_empty() {
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
                    self.chats.insert(
                        contact_id.clone(),
                        Chat {
                            contact: Contact {
                                id: contact_id.clone(),
                                their_name: DEFAULT_NAME.to_string(),
                                my_name: DEFAULT_NAME.to_string(),
                                remote_storage: false,
                                disappearing_secs: 0,
                                backed_up: false,
                                peer_backed_up: false,
                                verified: false,
                                peer_verified: false,
                                peer_captures_silent: None,
                                peer_relays: Vec::new(),
                                remote_storage_healthy: true,
                                last_seen_secs: 0, // these three are filled in `contacts()` from the chat
                                local_name: String::new(),
                                identity_tag: String::new(),
                            },
                            peer_address: peer_address.clone(),
                            session: accepted.session,
                            history: Vec::new(),
                            // Inbound request: needs approval unless we auto-authorize.
                            authorized: !self.require_authorization,
                            // Associate the most recent invite code so approval can echo it.
                            code: self.last_invite_code.clone(),
                            closed: false,
                            relay_receipts: HashMap::new(),
                            // Pairing is itself contact: start the clock rather than
                            // reporting a brand-new chat as silent.
                            last_seen: Some(crate::api::now_secs()),
                            client_key: None, // minted and announced immediately after pairing
                            local_name: String::new(),
                            remote_storage_healthy: true,
                        },
                    );
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
                // Onion client auth (#22): now that we know the joiner's onion, hand them our client
                // key for *their* onion (the reverse leg of the exchange) so we can reach them, and
                // authorize their side implicitly happens when their `ClientKey` arrives. No-op off Tor.
                if !peer_address.is_empty() {
                    self.announce_client_key(&contact_id, &peer_address);
                    // Screenshot capability (#1) — same reason as the joiner side: this chat did not
                    // exist when the launch-time broadcast ran.
                    self.announce_captures_to(&contact_id);
                }
                if accepted.first_plaintext.is_empty() {
                    return Ok(None);
                }
                let text = String::from_utf8_lossy(&accepted.first_plaintext).into_owned();
                if let Some(chat) = self.chats.get_mut(&contact_id) {
                    chat.history
                        .push(ChatMessage::text(false, text.clone(), String::new()));
                }
                Ok(Some((contact_id, text)))
            }
            Frame::Message { from, id, message } => {
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
                chat.last_seen = Some(crate::api::now_secs());
                let text = String::from_utf8_lossy(&plaintext).into_owned();
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
                chat.history
                    .push(ChatMessage::text(false, text.clone(), id.clone()));
                // Accepted: owe them a receipt naming it.
                self.pending_receipts.push((from.clone(), id));
                // First real message from the peer → the transient pairing/approval notices have
                // served their purpose; drop them so they don't linger above the conversation.
                self.clear_system_notices(&from, &["await_approval", "approved"]);
                Ok(Some((from, text)))
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
                chat.contact.remote_storage = enabled;
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
                chat.contact.disappearing_secs = secs;
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
                if self.chats.contains_key(&from) {
                    self.clear_system_notices(&from, &["await_approval"]);
                    if let Some(chat) = self.chats.get_mut(&from) {
                        chat.history.push(ChatMessage::system_tagged(
                            "✅ Your chat request was approved. You can start chatting."
                                .to_string(),
                            "approved",
                        ));
                    }
                    return Ok(Some((from, String::new())));
                }
                Ok(None)
            }
            Frame::CodeInUse { from, code } => {
                // The code we joined with was already spent: warn and close our side.
                devlog!("[nightdrop] code '{code}' reported already used by {from}");
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
                if let Some(chat) = self.chats.get_mut(&from) {
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
                // Transparency (#1): the peer captured this conversation to their gallery, where
                // nothing we do — disappearing timers, remote-storage caps, unsend — can reach it.
                // Authenticate first: an unauthenticated version would let anyone who knows the
                // identity key manufacture distrust between two people.
                if !self.verify_control(&from, &message, MARK_SCREENSHOT) {
                    return Ok(None);
                }
                if let Some(chat) = self.chats.get_mut(&from) {
                    // Logged every time, not once: unlike `BackedUp`'s state flag, repetition here
                    // is information the user should have.
                    chat.history.push(ChatMessage::system(
                        "📸 The other person took a screenshot of this chat.".to_string(),
                    ));
                    return Ok(Some((from, String::new())));
                }
                Ok(None)
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
                if chat.contact.peer_verified == peer_verified {
                    return Ok(None); // no change → no history spam
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
                    chat.last_seen = Some(crate::api::now_secs());
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
                chat.last_seen = Some(crate::api::now_secs());
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
                let name = String::from_utf8_lossy(&plaintext).into_owned();
                chat.contact.their_name = if name.trim().is_empty() {
                    DEFAULT_NAME.to_string()
                } else {
                    name.trim().to_string()
                };
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
                    if new_address.trim().is_empty() || new_address == chat.peer_address {
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
            Frame::Relays { from, message } => {
                // The peer changed their advertised extra relay set (#17): decrypt and store it as
                // this contact's peer_relays, so future offline mail fans out to those relays too.
                let olm = message.to_olm()?;
                let Some(chat) = self.chats.get_mut(&from) else {
                    return Ok(None);
                };
                if !chat.authorized {
                    return Ok(None);
                }
                let plaintext = crypto::decrypt(&mut chat.session, &olm)?;
                let list = String::from_utf8_lossy(&plaintext).into_owned();
                let relays: Vec<String> = list
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if relays == chat.contact.peer_relays {
                    return Ok(None);
                }
                chat.contact.peer_relays = relays;
                devlog!("[nightdrop] peer {from} announced a new relay set");
                Ok(Some((from, String::new())))
            }
            Frame::ClientKey { from, client_key } => {
                // Onion client auth (#22): the peer handed us their client key for our onion.
                // Authorize it only for a contact we actually have a chat with (approved or a
                // pending inbound request) — never a pure stranger — so nobody can plant a key by
                // guessing our identity id. `authorize_client` validates the key and is a no-op off
                // Tor / when no auth dir is configured. Silent (no UI change).
                if self.chats.contains_key(&from) {
                    let _ = self.transport.authorize_client(&from, &client_key);
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
                let (transfer_id, kind, mime, size, thumb) = unpack_media_incoming(&envelope)?;
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
            // Short-code SPAKE2 runs over the rendezvous mailbox before any transport session
            // exists (see `run_join_handshake`/`service_pending_invites`), so a `Pake` frame on
            // the peer transport is unexpected — reserved for a future in-band re-key.
            Frame::Pake { .. } => Ok(None),
        }
    }
}
