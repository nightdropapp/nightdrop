//! [`Node`] messaging: send/edit/unsend, media, the transport/relay pump, and the
//! per-chat setters + time sweep. Split out of `node.rs`.
use super::*;

/// How long a directly-sent message may sit unconfirmed before [`Node::sweep_unconfirmed`] stops
/// trusting the dial and puts a copy on the relay.
///
/// Long enough that an ordinary receipt beats it — the peer has to notice the frame (their poller
/// ticks at 80ms in the foreground, 2s backgrounded) and dial us back over Tor, which is seconds.
/// Short enough that a message lost to a torn-down core is on its way again while the conversation
/// is still live. Every expiry costs one relay round-trip and, for a peer that already has the
/// message, a duplicate it discards — so erring long is cheap and erring short is merely wasteful.
pub(crate) const RECEIPT_TIMEOUT: Duration = Duration::from_secs(30);

/// Consecutive failed direct sends — with none ever succeeding this run — before the direct path is
/// treated as wedged (see [`Node::direct_path_wedged`]).
///
/// Three, not one: a single failure is an offline peer, which is ordinary and must not provoke a
/// teardown. Three in a row with nothing ever getting through is a device that cannot reach the
/// network, and the remedy it unlocks is capped at once per launch anyway.
pub(crate) const DIRECT_WEDGED_THRESHOLD: u32 = 3;

/// Refused-send reason while short-code pairing is still waiting on the invitee. Matched by the
/// UI to show a "wait for them to accept" toast rather than a generic failure.
pub(crate) const AWAITING_APPROVAL: &str =
    "waiting for the other person to accept the chat before messages can be sent";

/// Our own message that a deferred send's outcome refers to: a text message by its `msg_id`, an
/// attachment (which has none) by its `transfer_id`. Never matches on an empty id, which every
/// attachment and system notice carries.
fn sent_message_mut<'a>(history: &'a mut [ChatMessage], id: &str) -> Option<&'a mut ChatMessage> {
    if id.is_empty() {
        return None;
    }
    history.iter_mut().find(|m| {
        m.from_me && !m.system && (m.msg_id == id || (m.msg_id.is_empty() && m.transfer_id == id))
    })
}

/// True from the moment a short-code join completes until the invitee approves (or their first
/// message arrives). Sending in that window would queue a message the peer's core will drop as
/// unauthorized, leaving it sitting in the sender's history looking delivered — so refuse it.
pub(super) fn awaiting_approval(chat: &super::Chat) -> bool {
    chat.history
        .iter()
        .any(|m| m.system && m.kind == "await_approval")
}

impl Node {
    /// Send a message to an established contact (advances the ratchet). Delivered directly
    /// over the transport if the peer is reachable; otherwise queued in the relay's
    /// store-and-forward mailbox (§6), if one is attached.
    pub fn send(&mut self, contact_id: &str, text: &str) -> Result<()> {
        self.send_inner(contact_id, text, 0)
    }

    /// Send a **burn message** (`docs/design/burn-messages.md`): blurred on arrival, deleted
    /// `burn_secs` after the recipient first reveals it, and deleted unviewed after 24h.
    ///
    /// **Refuses** unless the peer has announced [`Frame::Burns`] support. A burn that silently
    /// lands as a permanent message is the single failure this feature must not have, and the
    /// send is the only moment anyone can act on it — there are no read receipts, so the sender
    /// would otherwise never learn. Callers surface the error; they must not fall back to a
    /// normal send.
    pub fn send_burn(&mut self, contact_id: &str, text: &str, burn_secs: u64) -> Result<()> {
        if burn_secs == 0 {
            anyhow::bail!("a burn message needs a timer");
        }
        let supported = self
            .chats
            .get(contact_id)
            .map(|c| c.contact.peer_supports_burn == Some(true))
            .unwrap_or(false);
        if !supported {
            anyhow::bail!(
                "this contact's app version cannot burn messages — it would keep this one"
            );
        }
        self.send_inner(contact_id, text, burn_secs)
    }

    /// Shared body of [`send`](Self::send) and [`send_burn`](Self::send_burn). `burn_secs == 0`
    /// is an ordinary message; anything else seals a [`Frame::Burn`] instead, with the duration
    /// inside the ciphertext.
    fn send_inner(&mut self, contact_id: &str, text: &str, burn_secs: u64) -> Result<()> {
        if text.len() > super::limits::MAX_TEXT_BYTES {
            anyhow::bail!("message too long (the limit is 64 KB)");
        }
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
        let plaintext = if burn_secs == 0 {
            text.as_bytes().to_vec()
        } else {
            pack_burn(burn_secs, text)
        };
        let message = crypto::encrypt(&mut chat.session, &plaintext)?;
        let wire_olm = WireOlm::from_olm(&message);
        let frame = if burn_secs == 0 {
            Frame::Message {
                from,
                id: msg_id.clone(),
                message: wire_olm,
            }
        } else {
            Frame::Burn {
                from,
                id: msg_id.clone(),
                message: wire_olm,
            }
        };
        let bytes = wire::encode(&frame);
        let mut msg = ChatMessage::text(true, text.to_string(), msg_id.clone());
        msg.delivery = "queued".to_string();
        msg.burn_secs = burn_secs;
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
    /// transports only — the deferred path now plans, dials off the lock, and applies
    /// ([`plan_pending_sends`](Self::plan_pending_sends) / [`execute_sends`] /
    /// [`apply_send_outcomes`](Self::apply_send_outcomes)), because dialling here holds the core
    /// lock. No-op if the chat was deleted before delivery ran.
    pub(crate) fn attempt_delivery(&mut self, contact_id: &str, msg_id: &str, bytes: &[u8]) {
        // Their app is too old to read it (0.1.x): hold it until they update, rather than hand it
        // over to be discarded.
        if self.peer_outdated(contact_id) {
            self.hold_message(contact_id, msg_id, bytes.to_vec());
            return;
        }
        let handle = self.post_handle(contact_id).unwrap_or_default();
        let (primary, targets) = self.route_to(contact_id);
        let Some(chat) = self.chats.get_mut(contact_id) else {
            return;
        };
        let delivered = self.transport.send(&chat.peer_address, bytes).is_ok();
        // Store a sealed copy on the relay(s) (24h) when the peer is offline (fallback) OR when
        // opt-in server storage is enabled (§6). Fans out to the recipient's whole relay set (#17).
        let mut needs_relay_retry = false;
        if !delivered || chat.contact.remote_storage {
            match queue_on_relays(
                self.transport.as_ref(),
                &primary,
                &targets,
                contact_id,
                &handle,
                bytes,
            ) {
                Ok(copies) => {
                    self.relay_ever_succeeded = true;
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
        // A successful dial says the peer's onion service answered — NOT that their app processed
        // the frame, and nothing at all about messages sent earlier. This used to promote every
        // queued message on that basis. Only a receipt naming a message may confirm it.
        if let Some(m) = chat
            .history
            .iter_mut()
            .find(|m| m.from_me && m.msg_id == msg_id)
        {
            m.delivery = if delivered { "sent" } else { "queued" }.to_string();
        }
        // Whether the direct path is working at all, for the guard heal's client-side check.
        self.note_direct_result(delivered);
        // Handed over but unconfirmed: start the clock. If no receipt names it before
        // [`RECEIPT_TIMEOUT`], the poller re-queues it on the relay — see `sweep_unconfirmed`.
        if delivered {
            self.awaiting_receipt.push(AwaitingReceipt {
                contact_id: contact_id.to_string(),
                msg_id: msg_id.to_string(),
                since: crate::api::now_secs(),
            });
        }
        if delivered {
            self.held_backoff.remove(contact_id);
        } else if !self.has_relay_route(contact_id) {
            self.held_retry_failed(contact_id, crate::api::now_secs());
        }
        if needs_relay_retry {
            self.hold_message(contact_id, msg_id, bytes.to_vec());
        }
    }

    /// Put a relay copy behind any directly-sent message that no receipt has confirmed within
    /// [`RECEIPT_TIMEOUT`]. Returns the contact ids whose messages changed status.
    ///
    /// A successful dial means the peer's onion service answered — not that their app processed the
    /// frame. On 2026-08-02 a core was torn down mid-flight and the message simply vanished: the
    /// sender's side said sent, the receiver never had it, and nothing asked again. Now nothing is
    /// trusted until a [`Frame::Delivered`] names it, and when none arrives the message goes to the
    /// relay, where it survives the peer being offline, restarted, or rebuilt.
    ///
    /// Runs **once** per message: the retry moves it to "queued", and from there the ordinary relay
    /// lifecycle owns it (drained → receipted → "delivered", or reaped at 24h → "expired"). A peer
    /// that already has the message discards the copy as a duplicate and receipts it, which is what
    /// settles the sender. A peer too old to send receipts never confirms, so each of its messages
    /// takes one relay copy — a duplicate on their screen, which is the safe direction to be wrong
    /// in and the reason receipts exist.
    ///
    /// Media is not covered: its id lives inside the encrypted envelope, so it carries no receipt
    /// to wait for (`ARCHITECTURE.md`, delivery receipts).
    pub(crate) fn sweep_unconfirmed(&mut self) -> Vec<String> {
        if self.awaiting_receipt.is_empty() {
            return Vec::new();
        }
        let now = crate::api::now_secs();
        let from = self.identity_key();
        let mut still_waiting = Vec::new();
        let mut affected: Vec<String> = Vec::new();
        for a in std::mem::take(&mut self.awaiting_receipt) {
            if now.saturating_sub(a.since) < RECEIPT_TIMEOUT.as_secs() {
                still_waiting.push(a);
                continue;
            }
            let handle = self.post_handle(&a.contact_id).unwrap_or_default();
            let Some(chat) = self.chats.get_mut(&a.contact_id) else {
                continue; // chat deleted while we waited
            };
            if chat.closed {
                continue;
            }
            // Still unconfirmed *and* still present: an edit, an unsend or a receipt that landed
            // between the sweep and now all mean there is nothing to re-queue.
            //
            // Never a burn message. The copy below is an ordinary `Frame::Message`, and the
            // recipient's burn tombstone clears the id that would mark it a duplicate — so a burn
            // that was revealed and deleted while its receipt was slow (Doze has taken ~45 s,
            // longer than RECEIPT_TIMEOUT) would come back as a permanent message. Re-sending it
            // as a burn would resurrect it instead. A lost burn stays lost: that is the direction
            // this feature has to fail in (`docs/design/burn-messages.md`).
            let Some(text) = chat
                .history
                .iter()
                .find(|m| {
                    m.from_me && m.msg_id == a.msg_id && m.delivery == "sent" && m.burn_secs == 0
                })
                .map(|m| m.text.clone())
            else {
                continue;
            };
            // Re-sealed rather than re-posted verbatim: the original bytes are long gone, and the
            // ratchet advancing again is harmless — the id is what the peer dedups on.
            // An unusable session (non-contributory ratchet key) gets no retry.
            let Ok(message) = crypto::encrypt(&mut chat.session, text.as_bytes()) else {
                continue;
            };
            let frame = Frame::Message {
                from: from.clone(),
                id: a.msg_id.clone(),
                message: WireOlm::from_olm(&message),
            };
            let bytes = wire::encode(&frame);
            let peer_address = chat.peer_address.clone();
            // A contact with no relay route gets the copy directly instead, on a fresh
            // connection, when its backoff allows (`optional-relay.md` §4).
            let relayless = !self.has_relay_route(&a.contact_id);
            if relayless && !self.may_retry(&a.contact_id, crate::api::now_secs()) {
                still_waiting.push(a);
                continue;
            }
            let (route, peer_address) = if relayless {
                (Route::DirectThenRelay, peer_address)
            } else {
                (Route::RelayOnly, String::new())
            };
            let mut send = self.detach(&peer_address, &a.contact_id, bytes, relayless);
            send.handle = handle;
            let contact_id = a.contact_id.clone();
            // Off the lock on Tor: `apply_outbound` flips the message to "queued" and records the
            // copies, or puts the entry back to wait for the next sweep.
            if self.send_or_queue(send, route, OutboundDone::Unconfirmed(a)) == Some(true)
                && !affected.contains(&contact_id)
            {
                affected.push(contact_id);
            }
        }
        // Extend, not assign: a failed post on a synchronous transport has already been put back.
        self.awaiting_receipt.extend(still_waiting);
        affected
    }

    /// Record the outcome of a direct (onion-to-onion) send, feeding
    /// [`direct_path_wedged`](Self::direct_path_wedged).
    pub(crate) fn note_direct_result(&mut self, delivered: bool) {
        if delivered {
            self.direct_failures = 0;
            self.direct_ever_succeeded = true;
        } else {
            self.direct_failures = self.direct_failures.saturating_add(1);
        }
    }

    /// Whether the direct path looks **wedged**: several sends in a row have failed to reach a peer
    /// and not one has ever succeeded this run.
    ///
    /// This is the client-side counterpart to "our onion won't publish". A device can publish its
    /// own descriptor perfectly while being unable to *reach* anybody — 417 circuit builds, 61
    /// timeouts and `Unable to build circuit to introduction point`, all through a handful of guard
    /// channels opened at startup and never replaced (observed on a phone, 2026-08-03). Nothing
    /// noticed, because the only health signal was our own publication, so the app fell back to the
    /// relay for every message, indefinitely, and called that healthy.
    ///
    /// Two guards keep this from firing on an ordinary offline contact. `direct_ever_succeeded`:
    /// once *any* direct send has worked this run the path is demonstrably fine. And
    /// `relay_ever_succeeded`, which is the stronger one — the relay is dialled over the **same
    /// Tor path**, so a relay that answers proves our circuits work and an unreachable peer is
    /// their problem, not ours. Without it, one offline contact plus three messages on a fresh
    /// launch would provoke a pointless teardown. Only when nothing at all answers — no peer, no
    /// relay — after [`DIRECT_WEDGED_THRESHOLD`] tries is this device the suspect.
    pub(crate) fn direct_path_wedged(&self) -> bool {
        !self.direct_ever_succeeded
            && !self.relay_ever_succeeded
            && self.direct_failures >= DIRECT_WEDGED_THRESHOLD
    }

    /// Whether this exact frame has already been taken in, by either intake path — and record it if
    /// not. Applies **only to frames carrying user content**, which is deliberate.
    ///
    /// Those are the frames whose payload is sealed on the ratchet, so a second copy is not merely
    /// redundant but undecryptable: its message key was spent by the first. Control frames are the
    /// opposite — several carry no per-instance ciphertext at all, so a legitimate repeat is
    /// byte-identical to the original. `Frame::Approved` is exactly that: re-pairing an already
    /// approved chat echoes the same approval, and swallowing it as a "duplicate" leaves the other
    /// side waiting to be accepted forever.
    fn is_duplicate_user_frame(&mut self, frame: &Frame, bytes: &[u8]) -> bool {
        use sha2::{Digest, Sha256};
        if user_frame_sender(frame).is_none() {
            return false;
        }
        if self.seen_frames.len() > 8192 {
            self.seen_frames.clear(); // bound memory; a rare re-dup is acceptable
        }
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        !self.seen_frames.insert(digest)
    }

    /// Test hook: pretend the unconfirmed sends have been waiting `secs` longer, so a sweep can be
    /// exercised without a test that sleeps for [`RECEIPT_TIMEOUT`].
    #[cfg(test)]
    pub(crate) fn backdate_unconfirmed(&mut self, secs: u64) {
        for a in &mut self.awaiting_receipt {
            a.since = a.since.saturating_sub(secs);
        }
    }

    /// Snapshot the deferred sends so [`execute_sends`] can perform them **off** the core lock, the
    /// way [`relay_drain_plan`](Self::relay_drain_plan) does for the relay drain (§1.5.2). `None`
    /// when there is nothing waiting.
    ///
    /// Cheap: clones of already-sealed bytes and a handle on the transport. The expensive part —
    /// the dial, and any relay posts behind it — is what moves out from under the lock.
    pub(crate) fn plan_pending_sends(&mut self) -> Option<SendPlan> {
        if self.pending_sends.is_empty() {
            return None;
        }
        let mut items = Vec::new();
        for p in std::mem::take(&mut self.pending_sends) {
            // Drop anything whose chat went away while it waited.
            let Some(chat) = self.chats.get(&p.contact_id) else {
                continue;
            };
            // Held until they update, as in `attempt_delivery`.
            if chat.outdated() {
                self.hold_message(&p.contact_id, &p.msg_id, p.bytes);
                continue;
            }
            let (primary, relay_targets) = self.route_to(&p.contact_id);
            items.push(PlannedSend {
                handle: self.post_handle(&p.contact_id).unwrap_or_default(),
                contact_id: p.contact_id,
                msg_id: p.msg_id,
                bytes: p.bytes,
                peer_address: chat.peer_address.clone(),
                primary,
                relay_targets,
                remote_storage: chat.contact.remote_storage,
            });
        }
        if items.is_empty() {
            return None;
        }
        Some(SendPlan {
            transport: Arc::clone(&self.transport),
            items,
        })
    }

    /// Fold the results of an off-lock send batch back into the node: delivery status, relay
    /// receipts, server-storage health, the unconfirmed-receipt clock, and a retry for anything
    /// that reached neither the peer nor a relay. Returns the contacts whose chats changed.
    ///
    /// Deliberately mirrors what [`attempt_delivery`](Self::attempt_delivery) does inline for
    /// synchronous transports, so both paths end in the same state.
    pub(crate) fn apply_send_outcomes(&mut self, outcomes: SendOutcomes) -> Vec<String> {
        let mut affected = Vec::new();
        for o in outcomes.items {
            self.note_direct_result(o.delivered);
            if let Some(chat) = self.chats.get_mut(&o.contact_id) {
                if let Some(copies) = o.copies {
                    self.relay_ever_succeeded = true;
                    // Receipts are only useful while the message is still sitting on a relay: an
                    // edit/unsend recalls those copies. A directly-delivered one has nothing to
                    // recall.
                    if !o.delivered {
                        chat.relay_receipts.insert(o.msg_id.clone(), copies);
                    }
                    if chat.contact.remote_storage {
                        chat.remote_storage_healthy = true;
                    }
                } else if o.relay_failed && o.delivered && chat.contact.remote_storage {
                    // Delivered, but server storage is on and no relay took the copy — say so
                    // rather than implying a copy exists.
                    chat.remote_storage_healthy = false;
                }
                if let Some(m) = sent_message_mut(&mut chat.history, &o.msg_id) {
                    m.delivery = if o.delivered { "sent" } else { "queued" }.to_string();
                }
            }
            if o.delivered {
                self.awaiting_receipt.push(AwaitingReceipt {
                    contact_id: o.contact_id.clone(),
                    msg_id: o.msg_id.clone(),
                    since: crate::api::now_secs(),
                });
            } else if o.relay_failed {
                // Reached neither the peer nor any relay: hold it (`optional-relay.md` §4).
                if !self.has_relay_route(&o.contact_id) {
                    self.held_retry_failed(&o.contact_id, crate::api::now_secs());
                }
                self.hold_message(&o.contact_id, &o.msg_id, o.bytes);
            }
            if o.delivered {
                self.held_backoff.remove(&o.contact_id);
            }
            if !affected.contains(&o.contact_id) {
                affected.push(o.contact_id);
            }
        }
        affected
    }

    /// First load of a 0.1.x state (an upgrade to 0.2): hold again every text message of ours that
    /// no receipt confirmed. Between two releases that cannot talk, anything sent after one side
    /// updated never arrived, and its retry was in memory and its relay copy unreadable to the
    /// other version. Re-sealed under the same id, so a copy that did arrive after all is dropped
    /// as a duplicate (and receipted) by the peer. Burn messages are not resent, for the reason
    /// in [`sweep_unconfirmed`](Self::sweep_unconfirmed). Returns how many were held.
    pub(crate) fn resend_after_upgrade(&mut self) -> usize {
        let from = self.identity_key();
        let mut resend: Vec<(String, String, Vec<u8>)> = Vec::new();
        for (contact_id, chat) in self.chats.iter_mut() {
            if !chat.authorized || chat.closed {
                continue;
            }
            let texts: Vec<(String, String)> = chat
                .history
                .iter()
                .filter(|m| {
                    m.from_me
                        && !m.system
                        && m.kind == "text"
                        && m.burn_secs == 0
                        && !m.msg_id.is_empty()
                        && (m.delivery == "sent" || m.delivery == "queued")
                })
                .map(|m| (m.msg_id.clone(), m.text.clone()))
                .collect();
            for (id, text) in texts {
                let Ok(message) = crypto::encrypt(&mut chat.session, text.as_bytes()) else {
                    break; // an unusable session gets no retry
                };
                let frame = Frame::Message {
                    from: from.clone(),
                    id: id.clone(),
                    message: WireOlm::from_olm(&message),
                };
                resend.push((contact_id.clone(), id, wire::encode(&frame)));
            }
        }
        let n = resend.len();
        for (contact_id, id, bytes) in resend {
            self.hold_message(&contact_id, &id, bytes);
        }
        if n > 0 {
            crate::diag!("upgrade: holding {n} unconfirmed message(s) to send again in 0.2");
        }
        n
    }

    /// Retry authenticated control signals that reached neither the peer nor any relay when they
    /// were raised (arti cold / relay briefly unreachable): a chat-delete
    /// [`Closed`](crate::wire::Frame::Closed) (§11.6), or a
    /// [`Screenshot`](crate::wire::Frame::Screenshot) notice (#1) taken while the device was off
    /// the network.
    /// Called on the relay-poll cadence; re-posts the already-sealed frame (relay-first, direct
    /// fallback) off the lock, and drops it once a copy lands (`apply_outbound`).
    /// Chat-independent, and persisted with the state until then.
    pub(crate) fn flush_pending_control(&mut self) {
        self.flush_pending_control_for(None);
    }

    /// [`flush_pending_control`](Self::flush_pending_control), for one contact when `only` is set
    /// (one just heard from). A contact with no relay route is retried on its backoff, as the held
    /// queue is (`optional-relay.md` §4).
    pub(crate) fn flush_pending_control_for(&mut self, only: Option<&str>) {
        let now = crate::api::now_secs();
        let allowed: Vec<bool> = self
            .pending_control
            .iter()
            .map(|p| {
                !p.in_flight
                    && only.is_none_or(|c| c == p.recipient_ik)
                    && (self.relay_route_for_pending(p) || self.may_retry(&p.recipient_ik, now))
            })
            .collect();
        let mut due = Vec::new();
        for (p, ok) in self.pending_control.iter_mut().zip(allowed) {
            if ok {
                p.in_flight = true; // one copy at a time: the next poll skips it until it comes back
                due.push(p.bytes.clone());
            }
        }
        for bytes in due {
            let Some(send) = self
                .pending_control
                .iter()
                .find(|p| p.bytes == bytes)
                .map(|p| self.detach_pending(p))
            else {
                continue;
            };
            self.send_or_queue(
                send,
                Route::RelayThenDirect,
                OutboundDone::PendingControl(bytes),
            );
        }
    }

    /// Post one piece of **cover traffic** (#4): a dummy blob to one of *our own* mailboxes, so the
    /// relay sees activity it cannot tell apart from real mail. Drained and dropped on our next poll
    /// like anything else addressed to us.
    ///
    /// Self-addressed on purpose. The relay's only per-mailbox signal is "mailbox X was posted to
    /// at time T" — it never learns the sender (Tor) — so muddying *our own* mailboxes is exactly
    /// the observable, and needs no cooperation from contacts. Which mailbox: the inbound handle of
    /// a contact picked at random, the one their real mail to us uses, or our own cover handle
    /// while we have no agreed contact (`Node::cover_handle`, `optional-relay.md` §6).
    ///
    /// Best-effort and silent: a failed cover post is not worth a retry, a log line, or a user's
    /// attention. See `docs/design/cover-traffic.md`.
    pub(crate) fn send_cover_traffic(&mut self) {
        let (primary, peer_relays) = self.post_route(&self.my_relay_set());
        if primary.is_none() && peer_relays.is_empty() {
            return; // nothing to cover: no relay means no mailbox to watch
        }
        // Random payload, then the same fixed-size bucketing every frame gets — which is what makes
        // this indistinguishable from a real message rather than merely encrypted.
        let mut padding = vec![0u8; 32 + (rand::random::<usize>() % 96)];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut padding);
        let bytes = wire::encode(&Frame::Cover { padding });
        let me = self.identity_key();
        let Ok(blob) = relay_wrap(&me, &bytes) else {
            return;
        };
        // Our own relay set, exactly as a peer would post to us. Logged every time, success or
        // failure (`OutboundDone::Cover`): a privacy feature whose whole output is
        // indistinguishable from real traffic leaves nothing else to observe, so without the line
        // "cover traffic costs nothing measurable" and "cover traffic never ran" read the same —
        // exactly the trap the relay watchdog fell into.
        let send = DetachedSend {
            transport: Arc::clone(&self.transport),
            primary,
            peer_relays,
            recipient_ik: me.clone(),
            handle: self.cover_handle(super::mailbox::current_epoch()),
            peer_address: String::new(), // relay only
            bytes: blob,
            fresh: false,
        };
        self.send_or_queue(send, Route::RelayOnly, OutboundDone::Cover);
    }

    /// The relays to ask for the list when our onion site cannot be reached: the primary, our own
    /// and the discovered ones. Snapshotted under the lock; asked off it (`fetch_directory`).
    pub(crate) fn directory_relays(&self) -> Vec<RelayClient> {
        self.relay_set()
    }

    /// Adopt a signed relay list if it verifies against the current list-signing key and is newer
    /// than ours. `roots` is [`DIRECTORY_ROOTS`](crate::directory::DIRECTORY_ROOTS) outside tests.
    pub(crate) fn adopt_directory(
        &mut self,
        wire: &str,
        roots: &crate::keyroll::Roots,
    ) -> DirectoryOutcome {
        let Some(signed) = crate::directory::SignedDirectory::from_wire(wire) else {
            return DirectoryOutcome::Invalid;
        };
        // Verified against the operator key: an unsigned or forged list can't move our relay set,
        // wherever it came from (a relay, or a compromised website). The key is the newest one a
        // recovery-signed rotation has named (`keyroll.rs`); an older one is refused.
        let (key, moved) = crate::keyroll::current_key(
            roots,
            self.directory_key.as_ref(),
            signed.rotation.as_ref(),
        );
        let Some(dir) = signed.verify(&key) else {
            return DirectoryOutcome::Invalid;
        };
        if let Some(trust) = moved {
            crate::diag!(
                "directory: the list-signing key was rotated (sequence {})",
                trust.seq
            );
            self.directory_key = Some(trust);
            self.dirty = true;
        }
        if dir.version <= self.directory_version {
            return DirectoryOutcome::Current; // not newer (monotonic anti-rollback)
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
        let mut keep = self.discovered_relays.clone();
        keep.extend(self.polled_relay_set(crate::api::now_secs()).relays);
        self.relay_reachable.retain(|addr, _| keep.contains(addr));
        self.dirty = true; // the version moved, even when the set did not
        DirectoryOutcome::Applied { changed }
    }

    /// Whether to fetch the relay list from our onion site now (`directory::SITE_PATH`): once Tor
    /// is up (a fetch before that only fails), and on the persisted daily / retry schedule.
    pub(crate) fn directory_fetch_due(&self, now: u64) -> bool {
        // A direct-only device never posts to a relay, so it has no use for the list
        // (`optional-relay.md` §2).
        !self.direct_only() && self.announce_ready() && now >= self.directory_next_check
    }

    /// A fetch is starting: returns what it needs to run without the lock. The next fetch is pushed
    /// out by the retry interval first, so one that never comes back (the app is killed mid-way)
    /// cannot turn into one per launch.
    pub(crate) fn begin_directory_fetch(&mut self, now: u64) -> DirectoryFetchPlan {
        self.directory_next_check = now + crate::directory::RETRY_INTERVAL_SECS;
        self.dirty = true;
        DirectoryFetchPlan {
            transport: Arc::clone(&self.transport),
            relays: self.directory_relays(),
        }
    }

    /// Apply a fetch's result. A site list that verifies, newer or not, counts as success: the next
    /// fetch is a day away. Otherwise the relays' answers are used (every build up to 0.1.27 asked
    /// them on every poll) and the site is tried again after the retry interval.
    pub(crate) fn finish_directory_fetch(
        &mut self,
        result: DirectoryFetchResult,
        now: u64,
        roots: &crate::keyroll::Roots,
    ) {
        let DirectoryFetchResult {
            site: fetched,
            relay_lists,
        } = result;
        let outcome = match &fetched {
            Some(Ok(wire)) => self.adopt_directory(wire, roots),
            _ => DirectoryOutcome::Invalid,
        };
        match outcome {
            DirectoryOutcome::Applied { .. } | DirectoryOutcome::Current => {
                crate::diag!(
                    "directory: site list OK (version {}, {} relay(s))",
                    self.directory_version,
                    self.discovered_relays.len()
                );
                self.directory_next_check = now + crate::directory::CHECK_INTERVAL_SECS;
            }
            DirectoryOutcome::Invalid => {
                match &fetched {
                    None => crate::diag!("directory: no anonymized path; asking the relays"),
                    Some(Err(e)) => {
                        crate::diag!("directory: site fetch failed ({e:#}); asking the relays")
                    }
                    Some(Ok(_)) => {
                        crate::diag!("directory: site list did not verify; asking the relays")
                    }
                }
                // What the relays answered (asked off the lock, `fetch_directory`): the first list
                // that is genuine and newer wins, as relay polling always did.
                for wire in &relay_lists {
                    if let DirectoryOutcome::Applied { .. } = self.adopt_directory(wire, roots) {
                        break;
                    }
                }
                self.directory_next_check = now + crate::directory::RETRY_INTERVAL_SECS;
            }
        }
        self.dirty = true;
    }

    /// Joiner side of short-code pairing: show a notice that nothing will be delivered until the
    /// other person accepts the chat. Cleared by [`clear_system_notices`](Self::clear_system_notices) on approval, or when the
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
    ///
    /// In one go, for tests on a synchronous transport; the API runs the halves itself, around
    /// the recall, so the network round is not under the lock.
    #[cfg(test)]
    pub fn edit_message(&mut self, contact_id: &str, msg_id: &str, new_text: &str) -> Result<()> {
        if new_text.len() > super::limits::MAX_TEXT_BYTES {
            anyhow::bail!("message too long (the limit is 64 KB)");
        }
        let recalled = self
            .begin_edit(contact_id, msg_id, new_text)?
            .map(|plan| plan.run());
        self.finish_edit(contact_id, msg_id, new_text, recalled)
    }

    /// First half of [`edit_message`](Self::edit_message), under the lock: check the message may
    /// be edited and change it locally. For a message still queued on relays it hands back the
    /// copies to recall — a network round, for the caller to run without the lock — before
    /// [`finish_edit`](Self::finish_edit) can know which path the edit takes.
    pub(crate) fn begin_edit(
        &mut self,
        contact_id: &str,
        msg_id: &str,
        new_text: &str,
    ) -> Result<Option<RecallPlan>> {
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
        // Held on this device for a contact who was offline (`optional-relay.md` §4): they have
        // not seen it, so like a queued one it stays editable, and `finish_edit` re-seals the
        // held copy instead of recalling anything.
        let waiting = msg.delivery == "waiting";
        let in_window = msg.at != 0 && now.saturating_sub(msg.at) <= EDIT_WINDOW.as_secs();
        if !queued && !waiting && !in_window {
            anyhow::bail!("messages can only be edited within 15 minutes of sending");
        }
        msg.text = new_text.to_string();
        msg.edited = true;
        if !queued {
            return Ok(None);
        }
        // With fan-out (#17) every copy is recalled (`recall_receipts` never short-circuits — a
        // `.any()` would strand the old text on the sibling relays), each client rebuilt from its
        // stored address, so this survives a restart.
        let copies = chat.relay_receipts.remove(msg_id).unwrap_or_default();
        Ok(Some(self.recall_plan(contact_id, copies)))
    }

    /// Second half of [`edit_message`](Self::edit_message), under the lock again. `recalled` is
    /// whether every queued copy was recalled (`None`: the message was not queued).
    ///
    /// Path 1: all copies recalled — the old text can never be delivered, so post the new text as
    /// the message itself. Only an all-copy recall can be replaced invisibly: if even one relay
    /// says "not found" it may already have delivered the old message, so path 2 sends a normal E2E
    /// edit instead. Either frame goes through the outbox for the caller to send
    /// ([`OutboundDone::Caller`]).
    pub(crate) fn finish_edit(
        &mut self,
        contact_id: &str,
        msg_id: &str,
        new_text: &str,
        recalled: Option<bool>,
    ) -> Result<()> {
        let from = self.identity_key();
        // Still held here and not on its way: the contact never had it, so the held copy itself
        // becomes the new text, in the same place in the queue. Nothing is sent.
        if let Some(i) = self.held_unsent(contact_id, msg_id) {
            let chat = self
                .chats
                .get_mut(contact_id)
                .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
            let burn_secs = chat
                .history
                .iter()
                .find(|m| m.from_me && m.msg_id == msg_id)
                .map_or(0, |m| m.burn_secs);
            let plaintext = if burn_secs == 0 {
                new_text.as_bytes().to_vec()
            } else {
                pack_burn(burn_secs, new_text)
            };
            let message = WireOlm::from_olm(&crypto::encrypt(&mut chat.session, &plaintext)?);
            let (from, id) = (from, msg_id.to_string());
            let frame = if burn_secs == 0 {
                Frame::Message { from, id, message }
            } else {
                Frame::Burn { from, id, message }
            };
            self.held[i].bytes = wire::encode(&frame);
            self.dirty = true;
            return Ok(());
        }
        let chat = self
            .chats
            .get_mut(contact_id)
            .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
        if recalled == Some(true) {
            let message = crypto::encrypt(&mut chat.session, new_text.as_bytes())?;
            let frame = Frame::Message {
                from,
                id: msg_id.to_string(),
                message: WireOlm::from_olm(&message),
            };
            let send = self.detach("", contact_id, wire::encode(&frame), false);
            let done = OutboundDone::Caller(Some((contact_id.to_string(), msg_id.to_string())));
            if self.send_or_queue(send, Route::RelayOnly, done) == Some(false) {
                anyhow::bail!("peer offline and no relay accepted the message");
            }
            return Ok(());
        }
        let envelope = pack_edit(msg_id, new_text);
        let message = crypto::encrypt(&mut chat.session, &envelope)?;
        let frame = Frame::Edit {
            from,
            message: WireOlm::from_olm(&message),
        };
        let peer_address = chat.peer_address.clone();
        self.deliver_with(
            &peer_address,
            contact_id,
            &frame,
            false,
            OutboundDone::Caller(None),
        )
    }

    /// Unsend ("delete for both") one of our earlier text messages — same eligibility as
    /// [`edit_message`](Self::edit_message) (queued, or within [`EDIT_WINDOW`]). The local
    /// copy becomes a "deleted" tombstone (`kind = "deleted"`, empty text); the peer converges:
    ///
    /// * **Still queued + receipt held**: recall the undelivered blob — the peer never receives
    ///   the message at all, so nothing needs to be sent.
    /// * **Already delivered (or recall failed)**: send a [`Unsend`](Frame::Unsend) frame naming
    ///   the `msg_id`; the peer replaces it with the same tombstone.
    ///
    /// In one go, for tests on a synchronous transport; the API runs the halves itself, around
    /// the recall, so the network round is not under the lock.
    #[cfg(test)]
    pub fn unsend_message(&mut self, contact_id: &str, msg_id: &str) -> Result<()> {
        let recalled = self
            .begin_unsend(contact_id, msg_id)?
            .map(|plan| plan.run());
        self.finish_unsend(contact_id, msg_id, recalled)
    }

    /// First half of [`unsend_message`](Self::unsend_message), under the lock: the same checks and
    /// hand-off as [`begin_edit`](Self::begin_edit). Nothing changes locally yet: which change it is
    /// (gone, or a tombstone) depends on the recall.
    pub(crate) fn begin_unsend(
        &mut self,
        contact_id: &str,
        msg_id: &str,
    ) -> Result<Option<RecallPlan>> {
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
            .iter()
            .find(|m| m.from_me && !m.system && m.kind == "text" && m.msg_id == msg_id)
            .ok_or_else(|| anyhow::anyhow!("message not found or not deletable"))?;
        let queued = msg.delivery == "queued";
        let waiting = msg.delivery == "waiting"; // held here, as in `begin_edit`
        let in_window = msg.at != 0 && now.saturating_sub(msg.at) <= EDIT_WINDOW.as_secs();
        if !queued && !waiting && !in_window {
            anyhow::bail!("messages can only be unsent within 15 minutes of sending");
        }
        if !queued {
            return Ok(None);
        }
        // Every still-queued copy (#17), each client rebuilt from its stored address so this still
        // works after a restart.
        let copies = chat.relay_receipts.remove(msg_id).unwrap_or_default();
        Ok(Some(self.recall_plan(contact_id, copies)))
    }

    /// Second half of [`unsend_message`](Self::unsend_message), under the lock again.
    pub(crate) fn finish_unsend(
        &mut self,
        contact_id: &str,
        msg_id: &str,
        recalled: Option<bool>,
    ) -> Result<()> {
        let from = self.identity_key();
        // Still held here and not on its way: the contact never had it. Drop it from the queue
        // and from the chat, as a fully recalled message is.
        if let Some(i) = self.held_unsent(contact_id, msg_id) {
            self.held.remove(i);
            self.dirty = true;
            if let Some(chat) = self.chats.get_mut(contact_id) {
                chat.history.retain(|m| !(m.from_me && m.msg_id == msg_id));
            }
            return Ok(());
        }
        let chat = self
            .chats
            .get_mut(contact_id)
            .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
        let Some(msg_index) = chat
            .history
            .iter()
            .position(|m| m.from_me && !m.system && m.msg_id == msg_id)
        else {
            return Ok(()); // gone meanwhile (a burn, a chat clear): nothing left to unsend
        };
        if recalled == Some(true) {
            // The recipient never received this held message. Remove it locally as well: a
            // tombstone would leave evidence of a message that never existed for them.
            chat.history.remove(msg_index);
            return Ok(());
        }
        // The peer may already have the original (or a fanned-out copy remains), so preserve the
        // conversation position locally and tell them to tombstone their copy too.
        make_tombstone(&mut chat.history[msg_index]);
        let envelope = pack_unsend(msg_id);
        let message = crypto::encrypt(&mut chat.session, &envelope)?;
        let frame = Frame::Unsend {
            from,
            message: WireOlm::from_olm(&message),
        };
        let peer_address = chat.peer_address.clone();
        self.deliver_with(
            &peer_address,
            contact_id,
            &frame,
            false,
            OutboundDone::Caller(None),
        )
    }

    /// What recalling `copies` of a message to `contact_id` needs, to run without the lock.
    fn recall_plan(&self, contact_id: &str, copies: Vec<QueuedReceipt>) -> RecallPlan {
        RecallPlan {
            transport: Arc::clone(&self.transport),
            primary: self.relay.clone(),
            contact_id: contact_id.to_string(),
            copies,
        }
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
        self.send_media_inner(contact_id, data, mime, kind, thumb, 0)
    }

    /// Send an attachment as a **burn message**. Same gate as [`send_burn`](Self::send_burn):
    /// refused unless the peer announced support, never silently downgraded.
    ///
    /// A burn attachment carries **no thumbnail and no `MediaIncoming` pre-signal**. Both exist to
    /// show something while a video uploads, and showing a preview of a message that has not been
    /// revealed would give away the content this feature exists to withhold. The recipient sees a
    /// hidden tile until the payload arrives and they choose to open it.
    pub fn send_burn_media(
        &mut self,
        contact_id: &str,
        data: &[u8],
        mime: &str,
        kind: &str,
        burn_secs: u64,
    ) -> Result<()> {
        if burn_secs == 0 {
            anyhow::bail!("a burn message needs a timer");
        }
        let supported = self
            .chats
            .get(contact_id)
            .map(|c| c.contact.peer_supports_burn == Some(true))
            .unwrap_or(false);
        if !supported {
            anyhow::bail!(
                "this contact's app version cannot burn messages — it would keep this one"
            );
        }
        self.send_media_inner(contact_id, data, mime, kind, &[], burn_secs)
    }

    fn send_media_inner(
        &mut self,
        contact_id: &str,
        data: &[u8],
        mime: &str,
        kind: &str,
        thumb: &[u8],
        burn_secs: u64,
    ) -> Result<()> {
        if data.len() as u64 > MAX_MEDIA_BYTES {
            anyhow::bail!(
                "attachment too large (max {} MB)",
                MAX_MEDIA_BYTES / (1024 * 1024)
            );
        }
        let from = self.identity_key();
        let transfer_id = crate::storage::random_password();
        // The same gates as a text send, for every attachment (they used to apply to videos only).
        {
            let chat = self
                .chats
                .get(contact_id)
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
        }
        // Off the lock on a real network, as `send` does: an attachment is tens of megabytes over
        // Tor, and sending it inline held the core lock — every other call and the poller — for
        // the whole upload. The poller sends these in order, the pre-signal first.
        let deferred = !self.transport.is_synchronous();

        // 1) Fire the small "incoming" pre-signal FIRST (videos), before the heavy work on
        // the payload (sealing/encrypting 10s of MB) — so the receiver sees it right away.
        if kind == "video" && burn_secs == 0 {
            let (peer_address, incoming) = {
                let chat = self
                    .chats
                    .get_mut(contact_id)
                    .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
                let env = pack_media_incoming(&transfer_id, kind, mime, data.len() as u64, thumb);
                let m = crypto::encrypt(&mut chat.session, &env)?;
                let bytes = wire::encode(&Frame::MediaIncoming {
                    from: from.clone(),
                    message: WireOlm::from_olm(&m),
                });
                (chat.peer_address.clone(), bytes)
            };
            if deferred {
                // An id no message carries, so its outcome updates nothing in history.
                self.pending_sends.push(PendingRelaySend {
                    contact_id: contact_id.to_string(),
                    msg_id: format!("t:{transfer_id}:incoming"),
                    bytes: incoming,
                });
            } else if self.transport.send(&peer_address, &incoming).is_err() {
                let (primary, targets) = self.route_to(contact_id);
                let _ = queue_on_relays(
                    self.transport.as_ref(),
                    &primary,
                    &targets,
                    contact_id,
                    &self.post_handle(contact_id).unwrap_or_default(),
                    &incoming,
                );
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
            let env = if burn_secs == 0 {
                pack_media(&transfer_id, kind, mime, data)
            } else {
                pack_burn_media(burn_secs, &transfer_id, kind, mime, data)
            };
            let m = crypto::encrypt(&mut chat.session, &env)?;
            let wire_olm = WireOlm::from_olm(&m);
            let bytes = wire::encode(&if burn_secs == 0 {
                Frame::Media {
                    from,
                    message: wire_olm,
                }
            } else {
                Frame::BurnMedia {
                    from,
                    message: wire_olm,
                }
            });
            (
                chat.peer_address.clone(),
                chat.contact.remote_storage,
                bytes,
            )
        };

        if deferred {
            if let Some(chat) = self.chats.get_mut(contact_id) {
                let mut msg = ChatMessage::media(
                    true,
                    kind.to_string(),
                    mime.to_string(),
                    media_id,
                    data.len() as u64,
                    transfer_id.clone(),
                    thumb_id,
                );
                msg.delivery = "queued".to_string();
                msg.burn_secs = burn_secs;
                chat.history.push(msg);
            }
            // Media has no `msg_id`; its outcome is matched by `transfer_id` (`sent_message_mut`).
            self.pending_sends.push(PendingRelaySend {
                contact_id: contact_id.to_string(),
                msg_id: transfer_id,
                bytes: media_bytes,
            });
            return Ok(());
        }
        let delivered = self.transport.send(&peer_address, &media_bytes).is_ok();
        self.note_direct_result(delivered);
        devlog!(
            "[nightdrop] send_media: {} bytes ({kind}) to {peer_address} -> wire {} bytes, delivered={delivered}",
            data.len(),
            media_bytes.len(),
        );
        if !delivered || remote_storage {
            let (primary, targets) = self.route_to(contact_id);
            let posted = queue_on_relays(
                self.transport.as_ref(),
                &primary,
                &targets,
                contact_id,
                &self.post_handle(contact_id).unwrap_or_default(),
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
            // No promotion on a successful dial (see `attempt_delivery`): media carries no receipt
            // yet, so it stays "sent" — unconfirmed and honestly so — rather than claiming arrival.
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
            msg.burn_secs = burn_secs;
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

    /// Decrypt and return the bytes of a stored attachment, whatever they are. For the core's own
    /// use (backups, the player copy); what the app draws goes through [`media_bytes`](Self::media_bytes).
    pub(crate) fn media_plain(&self, media_id: &str) -> Result<Vec<u8>> {
        if !is_media_id(media_id) {
            anyhow::bail!("not a media id");
        }
        let (dir, key) = self
            .media_store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("media not available"))?;
        let sealed = std::fs::read(format!("{dir}/{media_id}.bin"))?;
        crate::storage::open(key, &sealed)
    }

    /// Decrypt a stored image (a photo, or a video's preview) for the app to draw.
    ///
    /// The app decodes these in-process, so only a plain JPEG of sane size leaves here; the engine's
    /// other decoders never see a contact's bytes (media_kind.rs, self-audit 2026-10-17 P2).
    pub fn media_bytes(&self, media_id: &str) -> Result<Vec<u8>> {
        let bytes = self.media_plain(media_id)?;
        if !super::media_kind::displayable_image(&bytes) {
            crate::diag!("media: refused to hand over an image that is not a plain JPEG");
            anyhow::bail!("this image is not in a format Night Drop shows");
        }
        Ok(bytes)
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
    ///
    /// Only a recognised video container is written, and its extension comes from its own bytes
    /// ([`media_kind::video_ext`](super::media_kind::video_ext)). `_suggested_ext` is ignored: the
    /// app used to pass the contact's mime here, which let a contact choose how the OS opened the
    /// file (self-audit 2026-10-13, finding 1). Kept in the signature so the bridge is unchanged.
    pub fn media_to_file(&self, media_id: &str, _suggested_ext: &str) -> Result<String> {
        if !is_media_id(media_id) {
            anyhow::bail!("not a media id");
        }
        let data = self.media_plain(media_id)?;
        let ext = super::media_kind::video_ext(&data).ok_or_else(|| {
            crate::diag!("media: refused to open an attachment that is not a known video format");
            anyhow::anyhow!("this attachment is not a video format Night Drop can open safely")
        })?;
        let dir = self
            .open_cache_dir()
            .ok_or_else(|| anyhow::anyhow!("media not available without persistence"))?;
        create_private_dir(&dir)?;
        let path = dir.join(format!("{media_id}.{ext}"));
        write_private_file(&path, &data)?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// Delete an attachment for good: its sealed file, and any decrypted copy `media_to_file`
    /// left in the open-cache for a player. Burn expiry, unsend and the disappearing timer used to
    /// remove only the sealed file, so a burned video that had been opened stayed on disk in the
    /// clear until the next launch swept the cache — days, on a desktop left running (self-audit
    /// 2026-10-13, round 6).
    pub(crate) fn forget_media(&self, id: &str) {
        if !is_media_id(id) {
            return;
        }
        if let Some((dir, _)) = &self.media_store {
            let _ = std::fs::remove_file(format!("{dir}/{id}.bin"));
        }
        if let Some(open) = self.open_cache_dir() {
            let prefix = format!("{id}.");
            if let Ok(entries) = std::fs::read_dir(&open) {
                for e in entries.flatten() {
                    if e.file_name().to_string_lossy().starts_with(&prefix) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }
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
        // Who we hear from directly this round: an authenticated frame stamps `last_seen`, and one
        // that arrived here, not from a relay, proves they are online right now.
        let seen_before: HashMap<String, u64> = self
            .chats
            .iter()
            .map(|(id, c)| (id.clone(), c.alive_count))
            .collect();
        while let Some((from_address, bytes)) = self.transport.try_recv() {
            // Skip what we cannot parse, exactly as the relay drain does. This used to be `?`,
            // which aborted the whole pump — and with it that tick's relay harvest and send
            // bookkeeping — over one frame. A peer running a NEWER build is the ordinary way to
            // meet an unknown frame: every new control frame added to `Frame` is undecodable to
            // everyone who has not updated yet, so a version skew must cost that frame and
            // nothing else. (`Frame::Captures` shipped after 0.1.19 and is announced to every open
            // chat at launch, so 0.1.19 peers meet one on the direct path routinely.)
            let Ok(frame) = wire::decode(&bytes) else {
                // From a contact still on 0.1 (an older wire version): say so in that chat, and
                // hold what we send them until they update.
                if let Some(from) = wire::legacy_sender(&bytes) {
                    self.note_legacy_frame(&from);
                    continue;
                }
                crate::diag!("pump: undecodable frame from a peer — skipped (newer build?)");
                continue;
            };
            // Same set the relay drain uses, so whichever copy of a message lands second is
            // recognised rather than handed to a ratchet that has already spent its key.
            if self.is_duplicate_user_frame(&frame, &bytes) {
                continue;
            }
            // Skip a frame that will not process (a ratchet that cannot open it, a malformed
            // envelope), as the relay drain does. Returning the error aborted the poll tick that
            // called this — *before* it applied the relay harvest it was holding, whose blobs had
            // already been taken off the relay destructively, so one bad direct frame lost every
            // relay message in that round.
            match self.process_frame(Some(from_address), frame) {
                Ok(Some(msg)) => received.push(msg),
                Ok(None) => {}
                Err(_) => crate::diag!(
                    "pump: a frame from a peer could not be processed — skipped, the rest go on"
                ),
            }
        }
        // Flush what we hold for anyone just heard from, at once (`optional-relay.md` §4).
        let heard: Vec<String> = self
            .chats
            .iter()
            .filter(|(id, c)| c.alive_count > seen_before.get(*id).copied().unwrap_or(0))
            .map(|(id, _)| id.clone())
            .collect();
        for id in heard {
            self.heard_from(&id);
        }
        // Per-message delivery receipts for anything that arrived over the DIRECT path. Without
        // these a directly-sent message reached "sent" and stopped there forever — no frame ever
        // promoted it — so the sender's UI showed a plain bubble that meant "we dialled their onion
        // successfully", while looking exactly like a message that had arrived (2026-08-02).
        //
        // The handlers themselves record what they accepted, *after* decrypting it (see
        // `Node::pending_receipts`) — the caller cannot: half these frames keep their id inside the
        // encrypted envelope, and reading it off the frame beforehand also risks receipting
        // something that is then dropped.
        let receipts = std::mem::take(&mut self.pending_receipts);
        self.send_receipts(&receipts);
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
    #[cfg(test)]
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
        self.relay_drain_plan_at(crate::api::now_secs())
    }

    /// [`relay_drain_plan`](Self::relay_drain_plan) as of `now` (unix secs).
    pub(crate) fn relay_drain_plan_at(&self, now: u64) -> Option<RelayDrainPlan> {
        use rand::seq::SliceRandom as _;
        // Our list and anything still draining out of it; direct only with nothing draining polls
        // nothing.
        let (primary, onions) = self.expand_relay_set(&self.polled_relay_set(now));
        let mut relays: Vec<(Option<String>, RelayClient)> = Vec::new();
        if let Some(primary) = &primary {
            relays.push((None, primary.clone()));
        }
        // Each relay once. The signed directory normally lists the primary itself, so without this
        // every round polled the one relay twice under two names — double the requests, and with
        // fragments double the onion connections (seen on a phone, 2026-09-26).
        let primary_addr = primary.as_ref().and_then(|p| p.addr().map(str::to_string));
        let mut seen: Vec<&str> = primary_addr.iter().map(String::as_str).collect();
        for addr in &onions {
            if seen.contains(&addr.as_str()) {
                continue;
            }
            seen.push(addr);
            relays.push((
                Some(addr.clone()),
                build_relay(self.transport.as_ref(), addr),
            ));
        }
        if relays.is_empty() {
            return None;
        }
        let fragments = self.poll_fragments_at(now);
        let mut stagger = false;
        let mut jobs = Vec::new();
        for (addr, base) in &relays {
            for fragment in &fragments {
                let client =
                    super::mailbox::isolated(self.transport.as_ref(), base, fragment.group);
                // Isolation happened only if a sibling client was built; `isolated` returns the base
                // unchanged otherwise. Tell by the transport, not by comparing clients.
                stagger |= base.addr().is_some_and(|a| {
                    self.transport
                        .relay_dialer_isolated(a, fragment.group)
                        .is_some()
                });
                jobs.push(DrainJob {
                    addr: addr.clone(),
                    client,
                    epoch: fragment.epoch,
                    handles: fragment.handles.clone(),
                });
            }
        }
        // A new order every round: a fragment that always followed another would announce their
        // relationship without ever sharing a circuit (§5c).
        jobs.shuffle(&mut rand::thread_rng());
        Some(RelayDrainPlan {
            jobs,
            settles: self.settling_epoch(now),
            stagger,
        })
    }

    /// Apply blobs drained lock-free by [`drain_relay_mailboxes`]: record relay reachability,
    /// de-duplicate fan-out copies by content hash (within this cycle and across cycles, and
    /// against copies that arrived directly, via `seen_frames`), unseal + process each frame, and send silent delivery acks. Same
    /// effects as the combined path — only the network **reads** were hoisted off the lock (§1.5.2).
    pub(crate) fn apply_relay_harvest(
        &mut self,
        harvest: RelayHarvest,
    ) -> Result<Vec<(String, String)>> {
        let me = self.identity_key();
        // Fold each addressed relay's reachability into relay-health (the "your relay is offline"
        // warning); the primary is untracked (baked-in default).
        for (addr, reachable) in harvest.reachability {
            self.relay_reachable.insert(addr, reachable);
        }
        if let Some(epoch) = harvest.settled {
            if self.prev_epoch_drained != Some(epoch) {
                crate::diag!("relay: yesterday's mailboxes drained past the skew margin — retired");
            }
            self.prev_epoch_drained = Some(epoch);
        }
        let mut received = Vec::new();
        let mut to_ack: Vec<String> = Vec::new(); // senders whose user messages we drained
        for blob in harvest.blobs {
            // Unseal the relay envelope (see `relay_wrap`). A blob not sealed to our key is
            // garbage someone posted to our mailbox — skip it, never abort the whole drain.
            let Ok(bytes) = relay_unwrap(&me, &blob) else {
                continue;
            };
            let Ok(frame) = wire::decode(&bytes) else {
                if let Some(from) = wire::legacy_sender(&bytes) {
                    self.note_legacy_frame(&from);
                }
                continue;
            };
            // Dedup on the frame within, not the envelope, so a copy we already took *directly*
            // is recognised here too — otherwise it reaches the ratchet as a replay of a message
            // key that is already spent, and fails.
            if self.is_duplicate_user_frame(&frame, &bytes) {
                continue; // a fan-out duplicate, or one this device already has
            }
            // Read before `process_frame` consumes the frame, but recorded only if it actually
            // produced a message. Taking a blob off the relay is not the same as accepting it: an
            // unapproved sender's message and one whose ratchet won't open are both dropped (see
            // `process_frame`), and acking those told the sender "delivered" about a message their
            // peer will never see — the precise lie `Frame::Delivered` was added to end, arriving
            // instead by the back door. Every user frame returns `Some` on its success path and
            // `None` on every drop path, so this gates exactly on "landed".
            let sender = user_frame_sender(&frame);
            // Relay-delivered frames carry their own sender id (no transport address).
            //
            // A frame that will not process must NOT abort the drain. These blobs have already
            // been `take`n — destructively — so everything after the failure would be lost for
            // good rather than retried, which turns one undecryptable message into silent data
            // loss for every *other* sender in the same batch.
            match self.process_frame(None, frame) {
                Ok(Some(msg)) => {
                    received.push(msg);
                    if let Some(from) = sender {
                        if !to_ack.contains(&from) {
                            to_ack.push(from);
                        }
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    crate::diag!(
                        "recv: a relay blob could not be processed — skipped, and the rest of the \
                         drain continues (it cannot be fetched again)"
                    );
                }
            }
        }
        // Send a silent, authenticated delivery ack to each peer whose message(s) we just picked
        // up off the relay, so their "Held for delivery" flips to "Delivered" (§11.3).
        for from in to_ack {
            if let Some((addr, frame)) = self.authed_control(&from, MARK_ACK, |me, message| {
                Frame::Ack { from: me, message }
            }) {
                self.send_after_surfacing(&from, &addr, &frame, false);
            }
        }
        // …and a precise receipt per message on top. The coarse `Ack` above stays on the wire for
        // peers on older builds, but confirms nothing here any more: only these do. Duplicates are
        // receipted too, so a sender's relay retry settles instead of repeating.
        let receipts = std::mem::take(&mut self.pending_receipts);
        self.send_receipts(&receipts);
        Ok(received)
    }
}

impl Node {
    /// Send one authenticated per-message delivery receipt for each `(peer, message id)`.
    ///
    /// Best-effort by design: a receipt that cannot be delivered must never fail the receive it
    /// belongs to — the message *has* arrived, and the only casualty is the sender's badge, which
    /// stays at "sent". That is the honest failure direction. Falls back to the relay like any
    /// control frame, so a peer who has gone offline still learns their message landed.
    ///
    /// The first receipt to each peer goes over a **fresh** connection. Nothing confirms a receipt,
    /// so one written into a connection kept open from earlier traffic, whose other end has gone,
    /// was lost without an error, and the relay fallback never ran: the sender's message sat on
    /// "Held for delivery" for good (2026-09-30). A fresh dial to a gone peer fails, and the receipt
    /// goes to the relay. Later receipts to the same peer in this batch take the ordinary send, which
    /// reuses the connection the first one opened if it got through, so a burst of messages costs
    /// one extra dial, not one per message.
    pub(crate) fn send_receipts(&mut self, receipts: &[(String, String)]) {
        let mut proven: Vec<&str> = Vec::new();
        for (from, msg_id) in receipts {
            if let Some((addr, frame)) =
                self.authed_control(from, msg_id.as_bytes(), |me, message| Frame::Delivered {
                    from: me,
                    message,
                })
            {
                let fresh = !proven.contains(&from.as_str());
                self.send_after_surfacing(from, &addr, &frame, fresh);
                if fresh {
                    proven.push(from);
                }
            }
        }
    }

    /// Deliver a receipt or ack for something this tick received. Inline on a synchronous
    /// transport (in-memory: instant, and tests expect the round trip done on return); otherwise
    /// sealed now — the ratchet must advance under the lock — and queued in
    /// [`receipt_sends`](Node::receipt_sends) for the poller to send once it has surfaced the
    /// messages. A receipt is a peer dial plus, on failure, a relay post; sent inline it made the
    /// notification wait for both.
    ///
    /// `fresh` sends it over a connection opened for it ([`Transport::send_fresh`]); see
    /// [`send_receipts`](Self::send_receipts) for why receipts need that.
    fn send_after_surfacing(
        &mut self,
        contact_id: &str,
        peer_address: &str,
        frame: &Frame,
        fresh: bool,
    ) {
        if self.transport.is_synchronous() {
            let _ = self.deliver_with(
                peer_address,
                contact_id,
                frame,
                fresh,
                OutboundDone::Nothing,
            );
            return;
        }
        let (primary, peer_relays) = self.route_to(contact_id);
        let send = DetachedSend {
            transport: Arc::clone(&self.transport),
            primary,
            peer_relays,
            recipient_ik: contact_id.to_string(),
            handle: self.post_handle(contact_id).unwrap_or_default(),
            peer_address: peer_address.to_string(),
            bytes: wire::encode(frame),
            fresh,
        };
        self.receipt_sends.push(send);
    }

    /// Hand over the receipts and acks queued by [`send_after_surfacing`](Self::send_after_surfacing),
    /// for the poller to [`execute`](DetachedSend::execute) **after** it has emitted the messages
    /// they confirm and released the core lock.
    pub(crate) fn take_receipt_sends(&mut self) -> Vec<DetachedSend> {
        std::mem::take(&mut self.receipt_sends)
    }

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

    /// Set our per-chat display name (§4). Blank falls back to [`DEFAULT_NAME`]; on a live
    /// chat the new name is also sent E2E-encrypted so the peer relabels our messages.
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
                let message = crypto::encrypt(&mut chat.session, resolved.as_bytes())?;
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
        // Turning it off always works; turning it on needs relays on both sides (§6).
        if enabled && !self.server_storage_available(contact_id) {
            anyhow::bail!(if self.direct_only() {
                "server storage needs a relay, and you are on direct only"
            } else {
                "server storage needs a relay on both sides, and this contact has none"
            });
        }
        let from = self.identity_key();
        let outgoing = {
            let chat = self
                .chats
                .get_mut(contact_id)
                .ok_or_else(|| anyhow::anyhow!("unknown contact"))?;
            chat.contact.remote_storage = enabled;
            chat.storage_timeline
                .push((crate::api::now_secs(), enabled));
            // Start optimistic on (re)enable; the next send corrects it if no relay is reachable.
            chat.remote_storage_healthy = true;
            if chat.authorized && !chat.closed {
                let payload: &[u8] = if enabled { b"on" } else { b"off" };
                let message = crypto::encrypt(&mut chat.session, payload)?;
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
    pub fn announce_address(&mut self) -> usize {
        let address = self.address();
        let mut sent = 0;
        for contact_id in self.address_targets() {
            if self.send_address_to(&contact_id).is_some() {
                sent += 1;
            }
        }
        self.restored_address = address;
        sent
    }

    /// Tell every established contact our address where it has not confirmed the current one.
    ///
    /// Compared per chat against what that contact confirmed ([`Chat::told_address`]), not against
    /// the address we last persisted: [`announce_address_if_changed`](Self::announce_address_if_changed)
    /// can only see a change against that baseline, so a change it missed was never repaired: a
    /// file-backup restore from 0.1.16–0.1.28 came back on a new onion without announcing it, and
    /// the save that followed made the new address the baseline
    /// (`docs/advisories/2026-10-07-backups-lost-the-onion-address.md`). Until 0.2 this sent to
    /// everyone on every launch, which told every contact when we came online; now a launch sends
    /// nothing unless something changed ([`ANNOUNCE_ONLINE_ON_START`]).
    pub fn reannounce_address(&mut self) {
        if !self.announce_ready() {
            return;
        }
        let address = self.address();
        let now = crate::api::now_secs();
        let pending: Vec<String> = self
            .address_targets()
            .into_iter()
            .filter(|id| {
                !self.address_announced.contains_key(id)
                    && (self.announce_online
                        || self.chats[id].told_address.as_deref() != Some(address.as_str()))
                    && self.may_retry(id, now)
            })
            .collect();
        for contact_id in pending {
            self.send_address_to(&contact_id);
        }
    }

    /// Chats an `Address` frame goes to: authorized and open.
    fn address_targets(&self) -> Vec<String> {
        self.chats
            .iter()
            .filter(|(_, c)| c.authorized && !c.closed)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Encrypt our current address for one chat and send it (relay fallback in `deliver`).
    /// `None` if the session cannot encrypt. Whether it landed is recorded when it has gone out:
    /// a failure clears the chat's `address_announced` flag, so the next tick retries.
    fn send_address_to(&mut self, contact_id: &str) -> Option<()> {
        let from = self.identity_key();
        let address = self.address();
        // Flagged while in flight, with what went out; a failed delivery clears it again.
        self.address_announced
            .insert(contact_id.to_string(), address.clone());
        let Some(chat) = self.chats.get_mut(contact_id) else {
            self.address_announced.remove(contact_id);
            return None;
        };
        let Ok(message) = crypto::encrypt(&mut chat.session, address.as_bytes()) else {
            self.address_announced.remove(contact_id);
            return None;
        };
        let frame = Frame::Address {
            from,
            message: WireOlm::from_olm(&message),
        };
        let peer_address = chat.peer_address.clone();
        let done = OutboundDone::Announced(Announce::Address, contact_id.to_string());
        let _ = self.deliver_with(&peer_address, contact_id, &frame, false, done);
        Some(())
    }

    /// If the live transport address differs from the one we last persisted (a changed onion,
    /// e.g. after a lost/rebuilt keystore), announce the new address to contacts (#11). A
    /// brand-new node (no prior address) or an unchanged address does nothing. Returns whether
    /// an announcement was sent.
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
        // Our PRIVATE-relay access keys (§3.2): without these the relay refuses us after a restart.
        for (relay, _, secret) in &self.relay_keys {
            let _ = self.transport.insert_client_key(relay, secret);
        }
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

    /// Tell every open, approved chat our relay set where it has not confirmed the current one
    /// (`optional-relay.md` §3 rule 4): after the user edits the list, after an upgrade from 0.1.x
    /// (which never told anyone a complete set), and again on later ticks for any that did not
    /// land. One in flight per chat.
    pub fn announce_relay_set(&mut self) {
        let mine = self.my_relay_set();
        let now = crate::api::now_secs();
        let ids: Vec<String> = self
            .chats
            .iter()
            .filter(|(id, c)| {
                c.authorized
                    && !c.closed
                    && c.relays_told.as_ref() != Some(&mine)
                    && !self.relay_set_in_flight.contains_key(id.as_str())
                    && self.may_retry(id, now)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.announce_relay_set_to(&id);
        }
    }

    /// Send one chat our relay set (`Frame::RelaySet`), unless it already confirmed this one or
    /// one is in flight. A request is not told: it drops control frames until approved, and
    /// approving tells it ([`authorize`](Self::authorize)).
    pub(crate) fn announce_relay_set_to(&mut self, contact_id: &str) {
        let mine = self.my_relay_set();
        let Some(chat) = self.chats.get(contact_id) else {
            return;
        };
        if !chat.authorized
            || chat.closed
            || chat.relays_told.as_ref() == Some(&mine)
            || self.relay_set_in_flight.contains_key(contact_id)
        {
            return;
        }
        let Ok(body) = serde_json::to_vec(&mine) else {
            return;
        };
        let Some((addr, frame)) = self.authed_control(contact_id, &body, |from, message| {
            Frame::RelaySet { from, message }
        }) else {
            return;
        };
        self.relay_set_in_flight
            .insert(contact_id.to_string(), mine);
        let done = OutboundDone::Announced(Announce::RelaySet, contact_id.to_string());
        let _ = self.deliver_with(&addr, contact_id, &frame, false, done);
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
            chat.disappearing_timeline
                .push((crate::api::now_secs(), secs));
            chat.history.push(ChatMessage::system(format!(
                "⏱️ You set disappearing messages to {}.",
                disappearing_label(secs)
            )));
            if chat.authorized && !chat.closed {
                let message = crypto::encrypt(&mut chat.session, secs.to_string().as_bytes())?;
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
            // For the caller to send before it returns (`api::set_disappearing`), not for the
            // poller: a message typed right after would otherwise go out first and land above the
            // notice on the peer's side (seen 2026-10-11).
            let _ = self.deliver_with(
                &peer_address,
                contact_id,
                &frame,
                false,
                OutboundDone::Caller(None),
            );
        }
        Ok(())
    }

    /// Start a burn message's countdown: the recipient just revealed it, at `viewed_at` (unix
    /// seconds; 0 = now). Idempotent — a second reveal does **not** restart the clock, or closing
    /// and reopening the chat would hold a message open for ever.
    ///
    /// `viewed_at` is the moment the UI **showed** it. The UI reveals on the tap and only then
    /// reaches the core, which can be seconds later when a poller tick holds the lock; stamping it
    /// here instead would run the deletion clock on a different start than the countdown the
    /// recipient is watching. Clamped to now, so it can only ever shorten a message's life.
    ///
    /// Returns true if this call started the clock (so the caller knows to persist). An opt-in
    /// receipt is sealed here but **not sent**: it waits in [`take_detached_sends`](Self::take_detached_sends)
    /// for the caller to deliver once it has released the lock.
    pub fn mark_burn_viewed(&mut self, contact_id: &str, msg_id: &str, viewed_at: u64) -> bool {
        let Some(chat) = self.chats.get_mut(contact_id) else {
            return false;
        };
        // Text carries `msg_id`; an attachment has none and is named by `transfer_id` instead
        // (see the `Media` handler). Accept either, so revealing works for both.
        let Some(msg) = chat.history.iter_mut().find(|m| {
            m.burn_secs > 0
                && !m.from_me
                && ((!m.msg_id.is_empty() && m.msg_id == msg_id)
                    || (!m.transfer_id.is_empty() && m.transfer_id == msg_id))
        }) else {
            return false;
        };
        if msg.viewed_at != 0 {
            return false;
        }
        let now = crate::api::now_secs_ceil();
        msg.viewed_at = if viewed_at == 0 {
            now
        } else {
            viewed_at.min(now)
        };
        // The id to name in the receipt: text carries `msg_id`, an attachment only `transfer_id`.
        let target = if msg.msg_id.is_empty() {
            msg.transfer_id.clone()
        } else {
            msg.msg_id.clone()
        };
        self.dirty = true;
        // Opt-in, and the recipient's call: this discloses when they read it. Nothing depends on
        // it arriving — the sender's 24h horizon stands either way — so a failure here is silent
        // by design rather than something to retry or surface.
        if self.burn_receipts && !target.is_empty() {
            self.send_burn_receipt(contact_id, &target);
        }
        true
    }

    /// Tell the sender we opened their burn message, so they can drop their copy now instead of
    /// waiting out the 24h horizon. Sealed now (the ratchet must advance under the lock), sent
    /// later by whoever drains [`take_detached_sends`](Self::take_detached_sends). Best-effort:
    /// see [`Node::mark_burn_viewed`].
    pub(super) fn send_burn_receipt(&mut self, contact_id: &str, target_id: &str) {
        let from = self.identity_key();
        let handle = self.post_handle(contact_id).unwrap_or_default();
        let (primary, peer_relays) = self.route_to(contact_id);
        let Some(chat) = self.chats.get_mut(contact_id) else {
            return;
        };
        let Ok(m) = crypto::encrypt(&mut chat.session, target_id.as_bytes()) else {
            return; // best-effort, like the rest of the receipt
        };
        let frame = Frame::Viewed {
            from,
            message: WireOlm::from_olm(&m),
        };
        let send = DetachedSend {
            transport: Arc::clone(&self.transport),
            primary,
            peer_relays,
            recipient_ik: contact_id.to_string(),
            handle,
            peer_address: chat.peer_address.clone(),
            bytes: wire::encode(&frame),
            // A view receipt is confirmed by nothing either: the same dead-connection loss as a
            // delivery receipt (`send_receipts`), with the burn timer's reveal riding on it.
            fresh: true,
        };
        self.detached_sends.push(send);
    }

    /// Hand over the frames a UI call sealed, for the caller to [`execute`](DetachedSend::execute)
    /// **after** releasing the core lock.
    pub(crate) fn take_detached_sends(&mut self) -> Vec<DetachedSend> {
        std::mem::take(&mut self.detached_sends)
    }

    /// Turn burn-view receipts on or off (recipient-controlled, off by default).
    pub fn set_burn_receipts(&mut self, on: bool) {
        self.burn_receipts = on;
    }

    /// Whether burn-view receipts are on.
    pub fn burn_receipts_enabled(&self) -> bool {
        self.burn_receipts
    }

    /// Delete burn messages whose time is up. Cheap (a scan of history), so it runs on **every**
    /// pump rather than the relay cadence: a 10-second burn must not wait on a minute-long tick.
    /// [`sweep_time`](Self::sweep_time) does not supersede this — it is the slower backstop that
    /// also catches the unviewed 24h horizon.
    ///
    /// Two horizons, both wall-clock:
    ///
    /// * **viewed** — `viewed_at + burn_secs`, the countdown the recipient watched;
    /// * **unviewed** — `at + RELAY_TTL` (24h), so a message nobody opened does not sit for ever.
    ///
    /// The **sender's own copy** burns on the 24h horizon, unless the recipient has opted in to
    /// burn-view receipts (`burn-messages.md` §8): then their `Frame::Viewed` removes it when they
    /// open it (the `Viewed` handler, not this sweep). Without that opt-in there is no read
    /// receipt, so the sender's copy has a fixed maximum life rather than a mirrored countdown.
    pub fn sweep_burns(&mut self) -> bool {
        let now = crate::api::now_secs();
        let unviewed_horizon = RELAY_TTL.as_secs();
        let mut changed = false;
        let mut dead_media: Vec<String> = Vec::new();
        for chat in self.chats.values_mut() {
            // An unopened message from the peer that reaches the horizon leaves a tombstone rather
            // than vanishing (`burn-messages.md` §6): silence reads as "nothing arrived", which is
            // exactly wrong. Its content goes the same as if it had burned — text, sealed files and
            // the ids that could reveal it — and with `burn_secs` cleared the retain below keeps it.
            for m in chat.history.iter_mut() {
                if m.burn_secs > 0
                    && m.at != 0
                    && !m.from_me
                    && m.viewed_at == 0
                    && now.saturating_sub(m.at) >= unviewed_horizon
                {
                    for id in [m.media_id.as_str(), m.thumb_id.as_str()] {
                        if !id.is_empty() {
                            dead_media.push(id.to_string());
                        }
                    }
                    make_burn_tombstone(m);
                    changed = true;
                }
            }
            let before = chat.history.len();
            chat.history.retain(|m| {
                if m.burn_secs == 0 || m.at == 0 {
                    return true;
                }
                let done = if m.viewed_at != 0 {
                    now.saturating_sub(m.viewed_at) >= m.burn_secs
                } else {
                    now.saturating_sub(m.at) >= unviewed_horizon
                };
                if done {
                    for id in [m.media_id.as_str(), m.thumb_id.as_str()] {
                        if !id.is_empty() {
                            dead_media.push(id.to_string());
                        }
                    }
                }
                !done
            });
            changed |= chat.history.len() != before;
        }
        for id in dead_media {
            self.forget_media(&id); // sealed file and any decrypted copy
        }
        self.dirty |= changed;
        changed
    }

    /// Time-based housekeeping, run on the poller's relay cadence (§11.3/§11.4):
    ///
    /// * a queued message older than the relay's 24h TTL was reaped server-side without
    ///   ever being fetched — flip its badge to **expired** (and drop its stale receipt);
    /// * in opt-in server-storage (**ephemeral**) chats, destroy device copies older than
    ///   24h — messages and their sealed media files. Normal local-first history is
    ///   untouched, and messages without timestamps (pre-upgrade) are left alone.
    pub fn sweep_time(&mut self) {
        self.sweep_burns();
        let now = crate::api::now_secs();
        self.expire_passes(now);
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
            // Device-side deletion, per message and forward-only: the 24h server-storage bomb
            // (§11.4) if storage was on when it was sent or received, and the disappearing timer
            // (#10) in effect then. Neither reaches messages from before it was switched on, and
            // both still expire what was sent while it was on. System notices are exempt, so an
            // emptied chat still says why.
            let (storage, timeline) = (&chat.storage_timeline, &chat.disappearing_timeline);
            if !storage.is_empty() || !timeline.is_empty() {
                let before = chat.history.len();
                chat.history.retain(|m| {
                    let age = now.saturating_sub(m.at);
                    let (bombed, timer) = if m.system {
                        (false, 0)
                    } else {
                        (
                            super::storage_on_at(storage, m.at),
                            super::timer_at(timeline, m.at),
                        )
                    };
                    let expired =
                        m.at != 0 && ((bombed && age > horizon) || (timer > 0 && age > timer));
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
        for id in dead_media {
            self.forget_media(&id); // sealed file and any decrypted copy
        }
        self.dirty |= changed;
    }
}

/// Whether `id` can be a media id: what [`store_media`](Node::store_media) generates
/// (`random_password`: `A-Z`, `2-9`, dashes). Every path built from a media id goes through this,
/// because ids also arrive from backups and restored history, which an attacker can craft: an id
/// with `/`, `\` or `..` in it would read, write or delete files outside the media store
/// (self-audit 2026-10-13, finding 2). Deliberately looser than the generator (any `A-Z0-9-`) so
/// it never strands real media; what matters is that no path character gets through.
pub(crate) fn is_media_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
}
