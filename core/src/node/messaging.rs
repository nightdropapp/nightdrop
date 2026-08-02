//! [`Node`] messaging: send/edit/unsend, media, the transport/relay pump, and the
//! per-chat setters + time sweep. Split out of `node.rs` (IMPROVEMENT_PLAN.md §2.1).
use super::*;

/// Refused-send reason while short-code pairing is still waiting on the invitee. Matched by the
/// UI to show a "wait for them to accept" toast rather than a generic failure.
pub(crate) const AWAITING_APPROVAL: &str =
    "waiting for the other person to accept the chat before messages can be sent";

/// True from the moment a short-code join completes until the invitee approves (or their first
/// message arrives). Sending in that window would queue a message the peer's core will drop as
/// unauthorized, leaving it sitting in the sender's history looking delivered — so refuse it.
fn awaiting_approval(chat: &super::Chat) -> bool {
    chat.history
        .iter()
        .any(|m| m.system && m.kind == "await_approval")
}

impl Node {
    /// Send a message to an established contact (advances the ratchet). Delivered directly
    /// over the transport if the peer is reachable; otherwise queued in the relay's
    /// store-and-forward mailbox (§6), if one is attached.
    pub fn send(&mut self, contact_id: &str, text: &str) -> Result<()> {
        let from = self.identity_key();
        let chat = self
            .chats
            .get_mut(contact_id)
            .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
        if !chat.authorized {
            anyhow::bail!("authorize this contact before messaging");
        }
        if awaiting_approval(chat) {
            anyhow::bail!(AWAITING_APPROVAL);
        }
        if chat.closed {
            anyhow::bail!("this chat was deleted; create a new one to keep talking");
        }
        // Advance the ratchet and seal the frame under the lock — this is the ordered, security-
        // critical step and must stay synchronous (per-message ratchet ordering). Store the message
        // right away as "queued"; only the opaque-byte *delivery* below is what may defer.
        let msg_id = random_msg_id();
        let message = crypto::encrypt(&mut chat.session, text.as_bytes());
        let frame = Frame::Message {
            from,
            id: msg_id.clone(),
            message: WireOlm::from_olm(&message),
        };
        let bytes = wire::encode(&frame);
        let mut msg = ChatMessage::text(true, text.to_string(), msg_id.clone());
        msg.delivery = "queued".to_string();
        chat.history.push(msg);

        if self.transport.is_synchronous() {
            // In-memory/demo: delivery is instant, so do it inline and callers see a fully-resolved
            // status on return (tests depend on this).
            self.attempt_delivery(contact_id, &msg_id, &bytes);
        } else {
            // Real network: hand delivery to the background poller so composing a message never
            // blocks on a Tor round-trip. The message is already stored as "queued"; the poller
            // flips it to "sent"/"delivered" (or leaves it "queued" on the relay) when it runs.
            self.pending_sends.push(PendingRelaySend {
                contact_id: contact_id.to_string(),
                msg_id,
                bytes,
            });
        }
        Ok(())
    }

    /// Deliver an already-stored, already-sealed message (opaque bytes): directly to the peer, with
    /// the relay set as offline fallback / server-storage copy (§6). Updates the stored message's
    /// delivery status and relay receipts in place. Shared by the inline path (synchronous
    /// transports) and the poller's deferred delivery ([`flush_pending_sends`]), so both take the
    /// exact same code path. No-op if the chat was deleted before delivery ran.
    pub(crate) fn attempt_delivery(&mut self, contact_id: &str, msg_id: &str, bytes: &[u8]) {
        let Some(chat) = self.chats.get_mut(contact_id) else {
            return;
        };
        let delivered = self.transport.send(&chat.peer_address, bytes).is_ok();
        // Store a sealed copy on the relay(s) (24h) when the peer is offline (fallback) OR when
        // opt-in server storage is enabled (§6). Fans out to the recipient's whole relay set (#17).
        let mut needs_relay_retry = false;
        if !delivered || chat.contact.remote_storage {
            let mut targets = chat.contact.peer_relays.clone();
            for r in &self.discovered_relays {
                if !targets.contains(r) {
                    targets.push(r.clone());
                }
            }
            match queue_on_relays(
                self.transport.as_ref(),
                &self.relay,
                &targets,
                contact_id,
                bytes,
            ) {
                Ok(copies) => {
                    // Keep the receipts while the message is queued: an edit/unsend can then
                    // recall every undelivered copy and replace it outright.
                    if !delivered {
                        chat.relay_receipts.insert(msg_id.to_string(), copies);
                    }
                    // Server storage reached a relay (a copy exists) → banner stays "healthy".
                    if chat.contact.remote_storage {
                        chat.remote_storage_healthy = true;
                    }
                }
                // Reached neither the peer NOR any relay (e.g. arti's Tor circuits were still cold):
                // do NOT drop it. Retry the relay from the poller (`flush_pending_relay`).
                Err(_) if !delivered => needs_relay_retry = true,
                // Delivered directly, but server storage is on and no relay took the copy: flag it
                // so the UI can say "not stored server-side" instead of implying a copy exists.
                Err(_) => chat.remote_storage_healthy = false,
            }
        }
        if delivered {
            flip_queued_delivered(&mut chat.history); // peer is reachable now
        }
        // Set THIS message's authoritative status (the flip above may have moved it to "delivered").
        if let Some(m) = chat
            .history
            .iter_mut()
            .find(|m| m.from_me && m.msg_id == msg_id)
        {
            m.delivery = if delivered { "sent" } else { "queued" }.to_string();
        }
        if needs_relay_retry {
            self.pending_relay.push(PendingRelaySend {
                contact_id: contact_id.to_string(),
                msg_id: msg_id.to_string(),
                bytes: bytes.to_vec(),
            });
        }
    }

    /// Deliver messages composed while a **non-synchronous** transport was in use — `send` stored
    /// them "queued" and deferred the network so the UI wasn't blocked. Run on the poll cadence;
    /// each attempt also warms arti. Returns the contact ids whose messages changed status.
    pub(crate) fn flush_pending_sends(&mut self) -> Vec<String> {
        let mut affected = Vec::new();
        for p in std::mem::take(&mut self.pending_sends) {
            self.attempt_delivery(&p.contact_id, &p.msg_id, &p.bytes);
            if !affected.contains(&p.contact_id) {
                affected.push(p.contact_id);
            }
        }
        affected
    }

    /// Retry queuing messages that reached neither the peer nor any relay when first sent (arti's
    /// circuits were cold). Called on the relay-poll cadence; each attempt also warms arti. On
    /// success the relay receipts are recorded (so edit/unsend can still recall the copy) and the
    /// message stops being pending. Returns the contact ids whose messages just got queued.
    pub(crate) fn flush_pending_relay(&mut self) -> Vec<String> {
        if self.pending_relay.is_empty() {
            return Vec::new();
        }
        let mut affected = Vec::new();
        for p in std::mem::take(&mut self.pending_relay) {
            // Drop the retry if the chat was deleted/closed in the meantime.
            let Some(mut peer_relays) = self
                .chats
                .get(&p.contact_id)
                .filter(|c| !c.closed)
                .map(|c| c.contact.peer_relays.clone())
            else {
                continue;
            };
            for r in &self.discovered_relays {
                if !peer_relays.contains(r) {
                    peer_relays.push(r.clone());
                }
            }
            match queue_on_relays(
                self.transport.as_ref(),
                &self.relay,
                &peer_relays,
                &p.contact_id,
                &p.bytes,
            ) {
                Ok(copies) => {
                    if let Some(chat) = self.chats.get_mut(&p.contact_id) {
                        chat.relay_receipts.insert(p.msg_id.clone(), copies);
                    }
                    if !affected.contains(&p.contact_id) {
                        affected.push(p.contact_id.clone());
                    }
                }
                // Still can't reach a relay — keep it for the next poll.
                Err(_) => self.pending_relay.push(p),
            }
        }
        affected
    }

    /// Retry authenticated control signals that reached neither the peer nor any relay when they
    /// were raised (arti cold / relay briefly unreachable): a chat-delete
    /// [`Closed`](crate::wire::Frame::Closed) (§11.6), or a
    /// [`Screenshot`](crate::wire::Frame::Screenshot) notice (#1) taken while the device was off
    /// the network.
    /// Called on the relay-poll cadence; re-posts the already-sealed frame (relay-first, direct
    /// fallback) until a copy lands, then drops it. Chat-independent and in-memory, like
    /// [`flush_pending_relay`](Self::flush_pending_relay).
    pub(crate) fn flush_pending_control(&mut self) {
        if self.pending_control.is_empty() {
            return;
        }
        for p in std::mem::take(&mut self.pending_control) {
            let queued = queue_on_relays(
                self.transport.as_ref(),
                &self.relay,
                &p.relays,
                &p.recipient_ik,
                &p.bytes,
            )
            .is_ok();
            let direct = !queued && self.transport.send(&p.peer_address, &p.bytes).is_ok();
            if queued || direct {
                self.dirty = true; // delivered → persist the now-shorter queue
            } else {
                self.pending_control.push(p); // still unreachable — keep for the next poll
            }
        }
    }

    /// Post one piece of **cover traffic** (#4): a dummy blob to our *own* mailbox, so the relay
    /// sees activity it cannot tell apart from real mail. Drained and dropped on our next poll like
    /// anything else addressed to us.
    ///
    /// Self-addressed on purpose. The relay's only per-identity signal is "mailbox X was posted to
    /// at time T" — it never learns the sender (Tor) — so muddying *our own* mailbox is exactly the
    /// observable, needs no cooperation from contacts, and works for an identity with no contacts
    /// at all, whose otherwise-silent mailbox is itself informative.
    ///
    /// Best-effort and silent: a failed cover post is not worth a retry, a log line, or a user's
    /// attention. See `docs/design/cover-traffic.md`.
    pub(crate) fn send_cover_traffic(&mut self) {
        let Some(relay) = self.relay.clone() else {
            return; // nothing to cover: no relay means no mailbox to watch
        };
        // Random payload, then the same fixed-size bucketing every frame gets — which is what makes
        // this indistinguishable from a real message rather than merely encrypted.
        let mut padding = vec![0u8; 32 + (rand::random::<usize>() % 96)];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut padding);
        let bytes = wire::encode(&Frame::Cover { padding });
        let me = self.identity_key();
        let Ok(blob) = relay_wrap(&me, &bytes) else {
            return;
        };
        // Our own relay set, exactly as a peer would post to us.
        let targets: Vec<String> = self.my_relays.clone();
        let _ = queue_on_relays(self.transport.as_ref(), &Some(relay), &targets, &me, &blob);
    }

    /// Fetch the operator-signed relay directory (§3.1), verify it against the baked-in key, and —
    /// if strictly newer than the version we hold — adopt its relay set as our shared defaults.
    /// Tries every relay we currently know (primary + my_relays + discovered) so a **rotated**
    /// primary (its onion lost) doesn't block discovery: whichever live relay serves the newer,
    /// validly-signed list wins. Blocking (one small round-trip per relay until one answers), run
    /// on the relay-poll cadence. Returns true (and marks state dirty to persist) if the set changed.
    pub(crate) fn refresh_directory(&mut self) -> bool {
        let mut clients: Vec<RelayClient> = Vec::new();
        if let Some(primary) = &self.relay {
            clients.push(primary.clone());
        }
        for addr in self.my_relays.iter().chain(self.discovered_relays.iter()) {
            clients.push(build_relay(self.transport.as_ref(), addr));
        }
        for client in &clients {
            let Ok(Some(wire)) = client.get_directory() else {
                continue;
            };
            let Some(signed) = crate::directory::SignedDirectory::from_wire(&wire) else {
                continue;
            };
            // Verified against DIRECTORY_PUBKEY — an unsigned/forged list can't move our relay set.
            let Some(dir) = signed.verify(&crate::directory::DIRECTORY_PUBKEY) else {
                continue;
            };
            if dir.version <= self.directory_version {
                continue; // not newer (monotonic anti-rollback)
            }
            let cleaned: Vec<String> = dir
                .relays
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let changed = cleaned != self.discovered_relays;
            self.discovered_relays = cleaned;
            self.directory_version = dir.version;
            // Forget health for relays we no longer advertise/discover.
            let keep = self.discovered_relays.clone();
            self.relay_reachable
                .retain(|addr, _| keep.contains(addr) || self.my_relays.contains(addr));
            if changed {
                self.dirty = true;
            }
            return changed;
        }
        false
    }

    /// Joiner side of short-code pairing: show a notice that nothing will be delivered until the
    /// other person accepts the chat. Cleared by [`clear_system_notices`] on approval, or when the
    /// first message arrives (see the `Approved`/`Message` handlers).
    pub(crate) fn note_awaiting_approval(&mut self, contact_id: &str) {
        if let Some(chat) = self.chats.get_mut(contact_id) {
            chat.history.push(ChatMessage::system_tagged(
                "⏳ Waiting for the other person to accept the chat. Your messages won't be \
                 delivered until they do."
                    .to_string(),
                "await_approval",
            ));
        }
    }

    /// Remove the transient join/approval notices of the given `kinds` from a chat's history
    /// (the "awaiting approval" hint and/or the "approved" confirmation).
    pub(crate) fn clear_system_notices(&mut self, contact_id: &str, kinds: &[&str]) {
        if let Some(chat) = self.chats.get_mut(contact_id) {
            chat.history
                .retain(|m| !(m.system && kinds.contains(&m.kind.as_str())));
        }
    }

    /// Edit the text of one of our earlier messages (product rule: within
    /// [`EDIT_WINDOW`] of sending, or at any time while it is still **queued** on the
    /// relay — the peer hasn't seen it yet). The local copy is updated and marked
    /// `edited`; the peer converges via one of two paths:
    ///
    /// * **Still queued + receipt held**: recall the undelivered blob from the relay and
    ///   post the new text in its place (same `msg_id`) — the peer only ever sees the
    ///   final version, so their side shows no "edited" tag.
    /// * **Already delivered (or recall failed)**: send an E2E [`Edit`](Frame::Edit)
    ///   frame naming the `msg_id`; the peer replaces the text and shows "edited".
    pub fn edit_message(&mut self, contact_id: &str, msg_id: &str, new_text: &str) -> Result<()> {
        let from = self.identity_key();
        let chat = self
            .chats
            .get_mut(contact_id)
            .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
        if chat.closed {
            anyhow::bail!("this chat was deleted");
        }
        let now = crate::api::now_secs();
        let msg = chat
            .history
            .iter_mut()
            .find(|m| m.from_me && !m.system && m.kind == "text" && m.msg_id == msg_id)
            .ok_or_else(|| anyhow::anyhow!("message not found or not editable"))?;
        let queued = msg.delivery == "queued";
        let in_window = msg.at != 0 && now.saturating_sub(msg.at) <= EDIT_WINDOW.as_secs();
        if !queued && !in_window {
            anyhow::bail!("messages can only be edited within 15 minutes of sending");
        }
        msg.text = new_text.to_string();
        msg.edited = true;

        // Path 1: replace the still-queued relay blob(s) so the old text is never delivered.
        // With fan-out (#17) we recall every copy. Only an all-copy recall can be replaced
        // invisibly: if even one relay says "not found" it may already have delivered the old
        // message, so fall through and send a normal E2E edit instead.
        if queued {
            let copies = chat.relay_receipts.remove(msg_id).unwrap_or_default();
            // Recall *every* fanned-out copy before re-posting the new text (each client is rebuilt
            // from its stored address, so this survives a restart). Must not short-circuit — a
            // `.any()` would strand the old text on the sibling relays (#17).
            let recalled_all =
                recall_receipts(self.transport.as_ref(), &self.relay, contact_id, &copies);
            if recalled_all {
                let message = crypto::encrypt(&mut chat.session, new_text.as_bytes());
                let frame = Frame::Message {
                    from,
                    id: msg_id.to_string(),
                    message: WireOlm::from_olm(&message),
                };
                let new_copies = queue_on_relays(
                    self.transport.as_ref(),
                    &self.relay,
                    &chat.contact.peer_relays,
                    contact_id,
                    &wire::encode(&frame),
                )?;
                chat.relay_receipts.insert(msg_id.to_string(), new_copies);
                return Ok(());
            }
        }

        // Path 2: the peer (may) have the original — send an explicit edit.
        let envelope = pack_edit(msg_id, new_text);
        let message = crypto::encrypt(&mut chat.session, &envelope);
        let frame = Frame::Edit {
            from,
            message: WireOlm::from_olm(&message),
        };
        let peer_address = chat.peer_address.clone();
        self.deliver(&peer_address, contact_id, &frame)
    }

    /// Unsend ("delete for both") one of our earlier text messages — same eligibility as
    /// [`edit_message`](Self::edit_message) (queued, or within [`EDIT_WINDOW`]). The local
    /// copy becomes a "deleted" tombstone (`kind = "deleted"`, empty text); the peer converges:
    ///
    /// * **Still queued + receipt held**: recall the undelivered blob — the peer never receives
    ///   the message at all, so nothing needs to be sent.
    /// * **Already delivered (or recall failed)**: send a [`Unsend`](Frame::Unsend) frame naming
    ///   the `msg_id`; the peer replaces it with the same tombstone.
    pub fn unsend_message(&mut self, contact_id: &str, msg_id: &str) -> Result<()> {
        let from = self.identity_key();
        let chat = self
            .chats
            .get_mut(contact_id)
            .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
        if chat.closed {
            anyhow::bail!("this chat was deleted");
        }
        let now = crate::api::now_secs();
        let msg_index = chat
            .history
            .iter_mut()
            .position(|m| m.from_me && !m.system && m.kind == "text" && m.msg_id == msg_id)
            .ok_or_else(|| anyhow::anyhow!("message not found or not deletable"))?;
        let msg = &chat.history[msg_index];
        let queued = msg.delivery == "queued";
        let in_window = msg.at != 0 && now.saturating_sub(msg.at) <= EDIT_WINDOW.as_secs();
        if !queued && !in_window {
            anyhow::bail!("messages can only be unsent within 15 minutes of sending");
        }
        // Path 1: recall every still-queued copy so the peer never receives the message (#17).
        if queued {
            let copies = chat.relay_receipts.remove(msg_id).unwrap_or_default();
            // Recall *every* fanned-out copy (#17), rebuilding each client from its stored address so
            // this still works after a restart. Must not short-circuit — a `.any()` would stop at the
            // first success and leave sibling relays holding the message.
            let recalled_all =
                recall_receipts(self.transport.as_ref(), &self.relay, contact_id, &copies);
            if recalled_all {
                // The recipient never received this held message. Remove it locally as well:
                // a tombstone would leave evidence of a message that never existed for them.
                chat.history.remove(msg_index);
                return Ok(());
            }
        }

        // The peer may already have the original (or a fanned-out copy remains), so preserve
        // the conversation position locally and tell them to tombstone their copy too.
        make_tombstone(&mut chat.history[msg_index]);

        // Path 2: the peer (may) have the original — tell them to delete it.
        let envelope = pack_unsend(msg_id);
        let message = crypto::encrypt(&mut chat.session, &envelope);
        let frame = Frame::Unsend {
            from,
            message: WireOlm::from_olm(&message),
        };
        let peer_address = chat.peer_address.clone();
        self.deliver(&peer_address, contact_id, &frame)
    }

    /// Send a media attachment (image/video) to a contact, E2E-encrypted on the session and
    /// sealed at rest. `kind` is "image"/"video"; `mime` like "image/png". The bytes go out
    /// inside a [`Media`](Frame::Media) frame (relay fallback when offline) and a local
    /// sealed copy is kept for display.
    pub fn send_media(
        &mut self,
        contact_id: &str,
        data: &[u8],
        mime: &str,
        kind: &str,
        thumb: &[u8],
    ) -> Result<()> {
        if data.len() as u64 > MAX_MEDIA_BYTES {
            anyhow::bail!(
                "attachment too large (max {} MB)",
                MAX_MEDIA_BYTES / (1024 * 1024)
            );
        }
        let from = self.identity_key();
        let transfer_id = crate::storage::random_password();

        // 1) Fire the small "incoming" pre-signal FIRST (videos), before the heavy work on
        // the payload (sealing/encrypting 10s of MB) — so the receiver sees it right away.
        if kind == "video" {
            let (peer_address, incoming) = {
                let chat = self
                    .chats
                    .get_mut(contact_id)
                    .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
                if !chat.authorized {
                    anyhow::bail!("authorize this contact before messaging");
                }
                if awaiting_approval(chat) {
                    anyhow::bail!(AWAITING_APPROVAL);
                }
                if chat.closed {
                    anyhow::bail!("this chat was deleted; create a new one to keep talking");
                }
                let env = pack_media_incoming(&transfer_id, kind, mime, data.len() as u64, thumb);
                let m = crypto::encrypt(&mut chat.session, &env);
                let bytes = wire::encode(&Frame::MediaIncoming {
                    from: from.clone(),
                    message: WireOlm::from_olm(&m),
                });
                (chat.peer_address.clone(), bytes)
            };
            if self.transport.send(&peer_address, &incoming).is_err() {
                if let Some(relay) = &self.relay {
                    if let Ok(sealed) = relay_wrap(contact_id, &incoming) {
                        let _ = relay.post(&mailbox_handle(contact_id), &sealed, RELAY_TTL);
                    }
                }
            }
        }

        // 2) Now the heavy payload: seal a local copy, encrypt, and send.
        let media_id = self.store_media(data)?;
        let thumb_id = if thumb.is_empty() {
            String::new()
        } else {
            self.store_media(thumb)?
        };
        let (peer_address, remote_storage, media_bytes) = {
            let chat = self
                .chats
                .get_mut(contact_id)
                .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
            let env = pack_media(&transfer_id, kind, mime, data);
            let m = crypto::encrypt(&mut chat.session, &env);
            let bytes = wire::encode(&Frame::Media {
                from,
                message: WireOlm::from_olm(&m),
            });
            (
                chat.peer_address.clone(),
                chat.contact.remote_storage,
                bytes,
            )
        };

        let delivered = self.transport.send(&peer_address, &media_bytes).is_ok();
        devlog!(
            "[nightdrop] send_media: {} bytes ({kind}) to {peer_address} -> wire {} bytes, delivered={delivered}",
            data.len(),
            media_bytes.len(),
        );
        if !delivered || remote_storage {
            let peer_relays = self
                .chats
                .get(contact_id)
                .map(|c| c.contact.peer_relays.clone())
                .unwrap_or_default();
            let posted = queue_on_relays(
                self.transport.as_ref(),
                &self.relay,
                &peer_relays,
                contact_id,
                &media_bytes,
            );
            if !delivered {
                posted?; // peer offline: at least one relay must accept
            } else if remote_storage {
                // Delivered directly; reflect whether the server-storage copy actually landed.
                let healthy = posted.is_ok();
                if let Some(chat) = self.chats.get_mut(contact_id) {
                    chat.remote_storage_healthy = healthy;
                }
            }
        }
        if let Some(chat) = self.chats.get_mut(contact_id) {
            if delivered {
                flip_queued_delivered(&mut chat.history);
            }
            let mut msg = ChatMessage::media(
                true,
                kind.to_string(),
                mime.to_string(),
                media_id,
                data.len() as u64,
                transfer_id,
                thumb_id,
            );
            msg.delivery = if delivered { "sent" } else { "queued" }.to_string();
            chat.history.push(msg);
        }
        Ok(())
    }

    /// Seal `data` under the media store key into a fresh file; returns its id. Errors if no
    /// media store is configured (persistence disabled).
    pub(super) fn store_media(&self, data: &[u8]) -> Result<String> {
        let (dir, key) = self
            .media_store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("media not available without persistence"))?;
        let id = crate::storage::random_password(); // reuse as an unguessable random id
        let sealed = crate::storage::seal(key, data)?;
        std::fs::write(format!("{dir}/{id}.bin"), sealed)?;
        Ok(id)
    }

    /// Decrypt and return the bytes of a stored attachment (for inline image display).
    pub fn media_bytes(&self, media_id: &str) -> Result<Vec<u8>> {
        let (dir, key) = self
            .media_store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("media not available"))?;
        let sealed = std::fs::read(format!("{dir}/{media_id}.bin"))?;
        crate::storage::open(key, &sealed)
    }

    /// Decrypt a stored attachment to a plaintext file and return its path (for opening a video
    /// in the system player without copying the bytes through the FFI boundary).
    ///
    /// The plaintext lands in an **app-private, owner-only** scratch dir (§1.4) — a sibling of the
    /// sealed store, not the world-readable system temp — under an unguessable id. It is swept on
    /// the next launch ([`set_media_store`](Self::set_media_store)) and on logout
    /// ([`clear_open_cache`](Self::clear_open_cache)); the residual exposure is the interval a
    /// decrypted file exists on disk while (and briefly after) the external player holds it, plus
    /// the case of a crash before the next-launch sweep. Documented in `website/limits.html`.
    pub fn media_to_file(&self, media_id: &str, suggested_ext: &str) -> Result<String> {
        let data = self.media_bytes(media_id)?;
        let dir = self
            .open_cache_dir()
            .ok_or_else(|| anyhow::anyhow!("media not available without persistence"))?;
        create_private_dir(&dir)?;
        let ext = if suggested_ext.is_empty() {
            "bin"
        } else {
            suggested_ext
        };
        let path = dir.join(format!("{media_id}.{ext}"));
        write_private_file(&path, &data)?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// The app-private scratch dir for **decrypted** attachments (§1.4): a sibling of the sealed
    /// media store, so it inherits the app-private location while staying separate from the sealed
    /// `.bin` files (a startup sweep can clear it without touching them). `None` if no store is set.
    fn open_cache_dir(&self) -> Option<std::path::PathBuf> {
        self.media_store
            .as_ref()
            .map(|(dir, _)| std::path::Path::new(dir).with_file_name("nightdrop-open"))
    }

    /// Delete the decrypted-attachment scratch dir and everything in it (§1.4). Called at startup
    /// (clears plaintext stranded by a crash in a previous run) and on logout / identity wipe.
    pub fn clear_open_cache(&self) {
        if let Some(dir) = self.open_cache_dir() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Process all pending inbound frames from the transport. Returns `(contact_id, text)`
    /// for each received user message (the empty handshake is consumed silently).
    pub fn pump(&mut self) -> Result<Vec<(String, String)>> {
        let mut received = Vec::new();
        while let Some((from_address, bytes)) = self.transport.try_recv() {
            let frame = wire::decode(&bytes)?;
            if let Some(msg) = self.process_frame(Some(from_address), frame)? {
                received.push(msg);
            }
        }
        Ok(received)
    }

    /// Drain any messages waiting in our relay mailbox (delivered while we were offline).
    /// Returns the same `(contact_id, text)` shape as [`pump`](Self::pump).
    ///
    /// Convenience composition of the split phases — snapshot, lock-free drain, apply — for
    /// synchronous callers (tests, the demo path) that already hold the core lock and do no Tor
    /// I/O. The background poller instead runs the drain **off** the lock (§1.5.2); see
    /// [`relay_drain_plan`](Self::relay_drain_plan) / [`drain_relay_mailboxes`] /
    /// [`apply_relay_harvest`](Self::apply_relay_harvest).
    #[allow(dead_code)] // used by the node tests; the app path goes through the split phases
    pub fn poll_relay(&mut self) -> Result<Vec<(String, String)>> {
        let Some(plan) = self.relay_drain_plan() else {
            return Ok(Vec::new());
        };
        let harvest = drain_relay_mailboxes(&plan);
        self.apply_relay_harvest(harvest)
    }

    /// Snapshot the relay clients + our mailbox handle so the blocking `take` round-trips can run
    /// **without** the core lock ([`drain_relay_mailboxes`], §1.5.2). Includes the primary relay
    /// plus our advertised extras (#17). `None` if no relay is configured (nothing to drain).
    pub(crate) fn relay_drain_plan(&self) -> Option<RelayDrainPlan> {
        let mut clients: Vec<(Option<String>, RelayClient)> = Vec::new();
        if let Some(primary) = &self.relay {
            clients.push((None, primary.clone()));
        }
        for addr in self.my_relays.iter().chain(self.discovered_relays.iter()) {
            clients.push((
                Some(addr.clone()),
                build_relay(self.transport.as_ref(), addr),
            ));
        }
        if clients.is_empty() {
            return None;
        }
        Some(RelayDrainPlan {
            handle: mailbox_handle(&self.identity_key()),
            clients,
        })
    }

    /// Apply blobs drained lock-free by [`drain_relay_mailboxes`]: record relay reachability,
    /// de-duplicate fan-out copies by content hash (both within this cycle and across cycles via
    /// `seen_relay_blobs`), unseal + process each frame, and send silent delivery acks. Same
    /// effects as the combined path — only the network **reads** were hoisted off the lock (§1.5.2).
    pub(crate) fn apply_relay_harvest(
        &mut self,
        harvest: RelayHarvest,
    ) -> Result<Vec<(String, String)>> {
        use sha2::{Digest, Sha256};
        let me = self.identity_key();
        // Fold each addressed relay's reachability into relay-health (the "your relay is offline"
        // warning); the primary is untracked (baked-in default).
        for (addr, reachable) in harvest.reachability {
            self.relay_reachable.insert(addr, reachable);
        }
        if self.seen_relay_blobs.len() > 8192 {
            self.seen_relay_blobs.clear(); // bound memory; a rare re-dup is acceptable
        }
        let mut received = Vec::new();
        let mut to_ack: Vec<String> = Vec::new(); // senders whose user messages we drained
        for blob in harvest.blobs {
            let digest: [u8; 32] = Sha256::digest(&blob).into();
            if !self.seen_relay_blobs.insert(digest) {
                continue; // a fan-out duplicate we already handled
            }
            // Unseal the relay envelope (see `relay_wrap`). A blob not sealed to our key is
            // garbage someone posted to our mailbox — skip it, never abort the whole drain.
            let Ok(bytes) = relay_unwrap(&me, &blob) else {
                continue;
            };
            let Ok(frame) = wire::decode(&bytes) else {
                continue;
            };
            if let Some(from) = user_frame_sender(&frame) {
                if !to_ack.contains(&from) {
                    to_ack.push(from);
                }
            }
            // Relay-delivered frames carry their own sender id (no transport address).
            if let Some(msg) = self.process_frame(None, frame)? {
                received.push(msg);
            }
        }
        // Send a silent, authenticated delivery ack to each peer whose message(s) we just picked
        // up off the relay, so their "Held for delivery" flips to "Delivered" (§11.3).
        for from in to_ack {
            if let Some((addr, frame)) = self.authed_control(&from, MARK_ACK, |me, message| {
                Frame::Ack { from: me, message }
            }) {
                let _ = self.deliver(&addr, &from, &frame);
            }
        }
        Ok(received)
    }
}

impl Node {
    /// Set our per-chat display name (§4). Blank falls back to [`DEFAULT_NAME`]; on a live
    /// chat the new name is also sent E2E-encrypted so the peer relabels our messages.
    /// Give this contact a nickname of your own. **Never sent** — unlike `set_my_name`, nothing
    /// goes on the wire: only you know that this identity key is the person you met, and the peer
    /// cannot supply that knowledge. Empty clears it, falling back to their chosen name plus their
    /// identity tag. See `docs/design/contact-naming.md`.
    pub fn set_local_name(&mut self, contact_id: &str, name: &str) -> Result<()> {
        let chat = self
            .chats
            .get_mut(contact_id)
            .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
        chat.local_name = name.trim().to_string();
        self.dirty = true;
        Ok(())
    }

    pub fn set_my_name(&mut self, contact_id: &str, name: &str) -> Result<()> {
        let from = self.identity_key();
        let resolved = if name.trim().is_empty() {
            DEFAULT_NAME.to_string()
        } else {
            name.trim().to_string()
        };
        // Set our name, and (if the chat is live) encrypt it for the peer so their side
        // relabels our messages. Build the frame inside the borrow, deliver after it ends.
        let outgoing = {
            let chat = self
                .chats
                .get_mut(contact_id)
                .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
            chat.contact.my_name = resolved.clone();
            if chat.authorized && !chat.closed {
                let message = crypto::encrypt(&mut chat.session, resolved.as_bytes());
                let frame = Frame::Name {
                    from,
                    message: WireOlm::from_olm(&message),
                };
                Some((chat.peer_address.clone(), frame))
            } else {
                None
            }
        };
        if let Some((peer_address, frame)) = outgoing {
            let _ = self.deliver(&peer_address, contact_id, &frame); // best-effort (relay fallback)
        }
        Ok(())
    }

    /// Toggle opt-in 24h server storage for a chat (§6). While enabled, sends also post an
    /// encrypted copy to the relay and the UI shows the persistent warning banner. The new
    /// state is sent E2E to the peer so **both** parties see the warning (invariant) —
    /// best-effort with relay fallback, like a rename.
    pub fn set_remote_storage(&mut self, contact_id: &str, enabled: bool) -> Result<()> {
        let from = self.identity_key();
        let outgoing = {
            let chat = self
                .chats
                .get_mut(contact_id)
                .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
            chat.contact.remote_storage = enabled;
            // Start optimistic on (re)enable; the next send corrects it if no relay is reachable.
            chat.remote_storage_healthy = true;
            if chat.authorized && !chat.closed {
                let payload: &[u8] = if enabled { b"on" } else { b"off" };
                let message = crypto::encrypt(&mut chat.session, payload);
                let frame = Frame::Storage {
                    from,
                    message: WireOlm::from_olm(&message),
                };
                Some((chat.peer_address.clone(), frame))
            } else {
                None
            }
        };
        if let Some((peer_address, frame)) = outgoing {
            let _ = self.deliver(&peer_address, contact_id, &frame);
        }
        Ok(())
    }

    /// Tell every established contact our current `.onion` address (§5c, #11). Each gets an
    /// E2E `Address` frame (best-effort, relay fallback), so a rebuilt Tor keystore — which
    /// gives us a new onion — doesn't orphan our contacts. Records the announced address so we
    /// don't re-announce the same one. Returns how many contacts we notified.
    #[allow(dead_code)] // driven by the Tor-backed api path (`new_tor`) + tests
    pub fn announce_address(&mut self) -> usize {
        let from = self.identity_key();
        let address = self.address();
        // Gather targets first (borrow ends), then send — deliver needs &self and encrypt &mut.
        let targets: Vec<String> = self
            .chats
            .iter()
            .filter(|(_, c)| c.authorized && !c.closed)
            .map(|(id, _)| id.clone())
            .collect();
        let mut sent = 0;
        for contact_id in &targets {
            let Some(chat) = self.chats.get_mut(contact_id) else {
                continue;
            };
            let message = crypto::encrypt(&mut chat.session, address.as_bytes());
            let frame = Frame::Address {
                from: from.clone(),
                message: WireOlm::from_olm(&message),
            };
            let peer_address = chat.peer_address.clone();
            let _ = self.deliver(&peer_address, contact_id, &frame);
            sent += 1;
        }
        self.restored_address = address;
        sent
    }

    /// If the live transport address differs from the one we last persisted (a changed onion,
    /// e.g. after a lost/rebuilt keystore), announce the new address to contacts (#11). A
    /// brand-new node (no prior address) or an unchanged address does nothing. Returns whether
    /// an announcement was sent.
    #[allow(dead_code)] // driven by the Tor-backed api path (`new_tor`) + tests
    pub fn announce_address_if_changed(&mut self) -> bool {
        let current = self.address();
        if current.is_empty()
            || self.restored_address.is_empty()
            || current == self.restored_address
        {
            // Keep our baseline current even on the no-op path (first run learns its address).
            self.restored_address = current;
            return false;
        }
        self.announce_address();
        true
    }

    /// Put every saved per-peer client key back into the (in-memory) keystore, and mint one for any
    /// chat that has none (#22, `docs/design/onion-key-at-rest.md`).
    ///
    /// Called once at startup, before anything tries to reach a peer. Without it a restricted
    /// peer's descriptor is unfetchable and every chat quietly degrades to relay-only.
    ///
    /// A chat with no saved key is one paired before the keys were persisted. Minting a fresh one
    /// and announcing it is enough — `Frame::ClientKey` is accepted on an existing chat, and the
    /// announcement falls back to the relay — so nothing has to be re-paired by hand. Returns how
    /// many were re-announced, for the diagnostics.
    // Only the Tor construction paths call this; a non-Tor build has no keystore to restore into.
    #[cfg_attr(not(feature = "tor"), allow(dead_code))]
    pub(crate) fn restore_client_keys(&mut self) -> usize {
        let saved: Vec<(String, Option<[u8; 32]>, String)> = self
            .chats
            .iter()
            .map(|(id, c)| (id.clone(), c.client_key, c.peer_address.clone()))
            .collect();
        let mut reannounced = 0usize;
        for (id, key, addr) in saved {
            if addr.is_empty() {
                continue;
            }
            match key {
                Some(secret) => {
                    let _ = self.transport.insert_client_key(&addr, &secret);
                }
                None => {
                    // Pre-existing chat: mint, keep, and tell them. They authorize the new key and
                    // direct connectivity resumes without either side re-pairing.
                    self.announce_client_key(&id, &addr);
                    reannounced += 1;
                }
            }
        }
        if reannounced > 0 {
            crate::diag!(
                "keys: re-announced {reannounced} client key(s) to peers — they will be reachable \
                 directly again once each authorizes it (relay works meanwhile)"
            );
        }
        reannounced
    }

    /// Tell every established contact our current advertised extra relay set (#17), so their
    /// offline mail to us fans out to those relays too. E2E `Relays` frame, best-effort (relay
    /// fallback). Called after the user edits their relays.
    pub fn announce_relays(&mut self) {
        let from = self.identity_key();
        let list = self.my_relays.join(",");
        let targets: Vec<String> = self
            .chats
            .iter()
            .filter(|(_, c)| c.authorized && !c.closed)
            .map(|(id, _)| id.clone())
            .collect();
        for contact_id in &targets {
            let Some(chat) = self.chats.get_mut(contact_id) else {
                continue;
            };
            let message = crypto::encrypt(&mut chat.session, list.as_bytes());
            let frame = Frame::Relays {
                from: from.clone(),
                message: WireOlm::from_olm(&message),
            };
            let peer_address = chat.peer_address.clone();
            let _ = self.deliver(&peer_address, contact_id, &frame);
        }
    }

    /// Set this chat's disappearing-messages timer (`secs`, 0 = off). Messages older than the
    /// timer are deleted on both devices by [`sweep_time`](Self::sweep_time). Like server
    /// storage it is a **shared** setting: the new value is sent E2E to the peer so both sides
    /// expire on the same horizon, and a system notice records the change locally.
    pub fn set_disappearing(&mut self, contact_id: &str, secs: u64) -> Result<()> {
        let from = self.identity_key();
        let outgoing = {
            let chat = self
                .chats
                .get_mut(contact_id)
                .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
            chat.contact.disappearing_secs = secs;
            chat.history.push(ChatMessage::system(format!(
                "⏱️ You set disappearing messages to {}.",
                disappearing_label(secs)
            )));
            if chat.authorized && !chat.closed {
                let message = crypto::encrypt(&mut chat.session, secs.to_string().as_bytes());
                let frame = Frame::Disappearing {
                    from,
                    message: WireOlm::from_olm(&message),
                };
                Some((chat.peer_address.clone(), frame))
            } else {
                None
            }
        };
        if let Some((peer_address, frame)) = outgoing {
            let _ = self.deliver(&peer_address, contact_id, &frame);
        }
        Ok(())
    }

    /// Time-based housekeeping, run on the poller's relay cadence (§11.3/§11.4):
    ///
    /// * a queued message older than the relay's 24h TTL was reaped server-side without
    ///   ever being fetched — flip its badge to **expired** (and drop its stale receipt);
    /// * in opt-in server-storage (**ephemeral**) chats, destroy device copies older than
    ///   24h — messages and their sealed media files. Normal local-first history is
    ///   untouched, and messages without timestamps (pre-upgrade) are left alone.
    pub fn sweep_time(&mut self) {
        let now = crate::api::now_secs();
        let horizon = RELAY_TTL.as_secs();
        let mut changed = false;
        let mut dead_media: Vec<String> = Vec::new();
        for chat in self.chats.values_mut() {
            for m in chat.history.iter_mut() {
                if m.from_me
                    && m.delivery == "queued"
                    && m.at != 0
                    && now.saturating_sub(m.at) > horizon
                {
                    m.delivery = "expired".to_string();
                    chat.relay_receipts.remove(&m.msg_id);
                    changed = true;
                }
            }
            // Device-side deletion horizon: the shorter of the 24h server-storage time-bomb
            // (§11.4, if on) and the user's per-chat disappearing timer (#10, if set).
            let mut limit: Option<u64> = None;
            if chat.contact.remote_storage {
                limit = Some(horizon);
            }
            if chat.contact.disappearing_secs > 0 {
                let d = chat.contact.disappearing_secs;
                limit = Some(limit.map_or(d, |h| h.min(d)));
            }
            if let Some(limit) = limit {
                let before = chat.history.len();
                chat.history.retain(|m| {
                    let expired = m.at != 0 && now.saturating_sub(m.at) > limit;
                    if expired {
                        for id in [m.media_id.as_str(), m.thumb_id.as_str()] {
                            if !id.is_empty() {
                                dead_media.push(id.to_string());
                            }
                        }
                    }
                    !expired
                });
                changed |= chat.history.len() != before;
            }
        }
        if let Some((dir, _)) = &self.media_store {
            for id in dead_media {
                let _ = std::fs::remove_file(format!("{dir}/{id}.bin"));
            }
        }
        self.dirty |= changed;
    }
}
