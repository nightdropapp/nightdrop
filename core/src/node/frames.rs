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
                let is_repair = prior.map(|c| c.closed).unwrap_or(false);
                let was_verified = prior.map(|c| c.contact.verified).unwrap_or(false);
                let revive = prior.map(|c| c.closed).unwrap_or(true);
                if revive {
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
                                peer_relays: Vec::new(),
                                remote_storage_healthy: true,
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
                    return Ok(None); // ignore messages from a not-yet-authorized contact
                }
                let plaintext = crypto::decrypt(&mut chat.session, &olm)?;
                let text = String::from_utf8_lossy(&plaintext).into_owned();
                flip_queued_delivered(&mut chat.history); // they're online → queued msgs delivered
                chat.history
                    .push(ChatMessage::text(false, text.clone(), id));
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
                devlog!("[ghost] received approval from {from} (code='{code}')");
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
                devlog!("[ghost] code '{code}' reported already used by {from}");
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
                devlog!("[ghost] peer {from} deleted the chat");
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
            Frame::Ack { from, message } => {
                // Silent delivery ack: the peer drained our relay-held messages → mark them
                // delivered. Authenticate first so a forged/replayed ack can't lie about delivery.
                // No history entry, no reply (never ack an ack).
                if !self.verify_control(&from, &message, MARK_ACK) {
                    return Ok(None);
                }
                if let Some(chat) = self.chats.get_mut(&from) {
                    flip_queued_delivered(&mut chat.history);
                }
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
                    "[ghost] peer {from} is now known as '{}'",
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
                devlog!("[ghost] peer {from} announced a new address");
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
                devlog!("[ghost] peer {from} announced a new relay set");
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
                devlog!("[ghost] incoming {size}-byte attachment from {from}");
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
                            transfer_id,
                            String::new(),
                        ));
                    }
                }
                devlog!("[ghost] received {size}-byte attachment from {from}");
                Ok(Some((from, String::new())))
            }
            // Short-code SPAKE2 runs over the rendezvous mailbox before any transport session
            // exists (see `run_join_handshake`/`service_pending_invites`), so a `Pake` frame on
            // the peer transport is unexpected — reserved for a future in-band re-key.
            Frame::Pake { .. } => Ok(None),
        }
    }
}
