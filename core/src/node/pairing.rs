//! [`Node`] pairing & verification: safety numbers, pre-key bundle/connect, and the
//! short-code SPAKE2 invite hosting/join. Split out of `node.rs`.
use super::*;

impl Node {
    // -------- Safety numbers & verification (§5, key-verification design) --------

    /// The 32-byte **safety fingerprint** for a chat: `SHA-256` over the *sorted* pair of
    /// long-term identity keys (ours + the contact's), domain-separated. Sorting makes it
    /// symmetric, so **both devices compute the identical value** — a MITM on the pairing
    /// channel would yield different numbers on the two ends. Errors if the contact is unknown.
    fn safety_digest(&self, contact_id: &str) -> Result<[u8; 32]> {
        use sha2::{Digest, Sha256};
        if !self.chats.contains_key(contact_id) {
            anyhow::bail!("unknown contact");
        }
        let me = self.identity_key();
        // Canonical order → same input on both sides. base64 has no ':' so it's an unambiguous sep.
        let (a, b) = if me.as_str() <= contact_id {
            (me.as_str(), contact_id)
        } else {
            (contact_id, me.as_str())
        };
        let mut h = Sha256::new();
        h.update(b"nightdrop/safety/v1");
        h.update(a.as_bytes());
        h.update(b":");
        h.update(b.as_bytes());
        Ok(h.finalize().into())
    }

    /// Human-comparable **safety number**: 12 space-separated groups of 5 digits derived from
    /// [`safety_digest`](Self::safety_digest). Both parties read the same string aloud / compare
    /// it out-of-band to confirm no MITM sat on pairing.
    pub fn safety_number(&self, contact_id: &str) -> Result<String> {
        Ok(render_safety_number(&self.safety_digest(contact_id)?))
    }

    /// The raw safety fingerprint as base64url — the payload behind the "verify by QR" flow, so a
    /// pair can scan instead of read. Compared against [`verify_safety_qr`](Self::verify_safety_qr).
    pub fn safety_qr(&self, contact_id: &str) -> Result<String> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        Ok(URL_SAFE_NO_PAD.encode(self.safety_digest(contact_id)?))
    }

    /// Compare a `scanned` safety-QR payload against this chat's fingerprint; on a match, mark
    /// the contact **verified** and return `true`. A mismatch leaves the state untouched.
    pub fn verify_safety_qr(&mut self, contact_id: &str, scanned: &str) -> Result<bool> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let ours = self.safety_digest(contact_id)?;
        let theirs = URL_SAFE_NO_PAD.decode(scanned.trim()).unwrap_or_default();
        let matches = theirs.len() == 32 && theirs == ours;
        if matches {
            self.set_verified(contact_id, true);
        }
        Ok(matches)
    }

    /// Set the per-contact **verified** flag (the user compared the safety number out-of-band).
    /// Persisted; because a contact is keyed by its identity key, a re-paired (new-key) contact
    /// starts unverified again — which is itself the "this is a new identity" signal.
    pub fn set_verified(&mut self, contact_id: &str, verified: bool) {
        let existed = match self.chats.get_mut(contact_id) {
            Some(chat) => {
                chat.contact.verified = verified;
                true
            }
            None => false,
        };
        if !existed {
            return;
        }
        // Tell the peer, informationally (§5b′): they'll see "the other person verified this chat"
        // but their own `verified` flag is untouched. The state rides in which marker decrypts, so
        // it's authenticated; a re-pair resets it on both ends. Best-effort with relay fallback.
        let marker = if verified {
            MARK_VERIFIED
        } else {
            MARK_UNVERIFIED
        };
        if let Some((addr, frame)) = self.authed_control(contact_id, marker, |from, message| {
            Frame::Verified { from, message }
        }) {
            let _ = self.deliver(&addr, contact_id, &frame);
        }
    }

    /// Mint a fresh pre-key bundle to advertise (QR payload / rendezvous).
    pub fn publish_bundle(&mut self) -> PreKeyBundle {
        self.identity.publish_prekey_bundle()
    }

    /// Joiner side: open an outbound session to a peer from their bundle + address, and
    /// send the opening `Hello` (an empty handshake message). Returns the new contact id.
    pub fn connect_with_bundle(
        &mut self,
        peer_address: &str,
        bundle: &PreKeyBundle,
    ) -> Result<String> {
        let mut session = crypto::open_outbound(&self.identity, bundle)?;
        let hello = crypto::encrypt(&mut session, b"");
        let frame = Frame::Hello {
            identity_key: self.identity_key(),
            address: self.address(), // where the peer should reply (we may be unreachable-by-metadata)
            message: WireOlm::from_olm(&hello),
        };
        // Relay-fallback the first-contact Hello: if the inviter's onion isn't reachable yet
        // (e.g. its descriptor is still republishing after a restart), queue it on the relay so
        // pairing completes when they next drain — instead of failing with a socket error.
        crate::diag!("pair: sending first-contact Hello to the peer");
        self.deliver(peer_address, &bundle.identity_key, &frame)?;
        crate::diag!("pair: Hello away — the peer should now show a chat request");

        let contact_id = bundle.identity_key.clone();
        if let Some(chat) = self.chats.get_mut(&contact_id) {
            // Re-pairing an existing contact — the same person again, or the reverse direction of a
            // pairing already in place. Adopt the NEW session so both ends match (otherwise this
            // side would encrypt with a session the peer no longer holds, and messages would fail
            // to decrypt), but keep the conversation history. A new session invalidates any earlier
            // safety-number check, so drop `verified` and warn.
            chat.session = session;
            chat.peer_address = peer_address.to_string();
            chat.authorized = true;
            chat.closed = false;
            chat.contact.verified = false;
            chat.contact.peer_verified = false;
            chat.history.push(ChatMessage::system(
                "🔑 Re-paired with a new secure session. Compare the safety number again if you \
                 want to confirm who this is."
                    .to_string(),
            ));
        } else {
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
                    peer_address: peer_address.to_string(),
                    session,
                    history: Vec::new(),
                    authorized: true, // we initiated this chat
                    // The joiner already knows the chat is live (no approval to wait on); the
                    // code is tracked on the inviter side for the approval echo.
                    code: None,
                    closed: false,
                    relay_receipts: HashMap::new(),
                    // Pairing is itself contact: start the clock rather than reporting a
                    // brand-new chat as silent.
                    last_seen: Some(crate::api::now_secs()),
                    client_key: None, // minted and announced immediately after pairing
                    local_name: String::new(),
                    remote_storage_healthy: true,
                },
            );
        }
        // Onion client auth (#22): hand the inviter our client key for their onion so they can
        // authorize us to reach their (possibly restricted) descriptor. No-op off Tor.
        self.announce_client_key(&contact_id, peer_address);
        // Screenshot capability (#1): a contact paired after the launch-time broadcast would
        // otherwise never learn it, and be left reading our silence as "they'd be told".
        self.announce_captures_to(&contact_id);
        Ok(contact_id)
    }
}

impl Node {
    // -------- Short-code pairing via the rendezvous mailbox (§5b/§5c) --------
    //
    // Interactive SPAKE2 (`TODO.md` #3). The old scheme sealed the invite under an Argon2 key
    // derived from the short-code words with a *fixed* salt, so the relay — holding that
    // ciphertext — could try candidate codes offline until one decrypted (low-entropy codes
    // made that practical). SPAKE2 removes the offline attack: neither the joiner's opener nor
    // the inviter's response lets an observer test a code guess without a fresh online run, and
    // the payload is sealed under a key that only completing the handshake with the *right* code
    // reproduces. The AEAD tag on that seal doubles as key confirmation — a wrong code makes
    // `open` fail, so an imposter is turned away and a MITM (who lacks the code) cannot forge a
    // response either. The tradeoff is that pairing is now interactive: the inviter must be
    // reachable to answer, which the background poller handles via [`service_pending_invites`].

    /// Inviter side: stage a short-code invite (§5b). Remembers the SPAKE2 secret and the
    /// pre-key/onion payload so the poller can answer a joiner's opener. Returns immediately —
    /// nothing decryptable-by-the-code is posted, so the relay cannot dictionary-attack it.
    pub fn stage_short_code_invite(
        &mut self,
        slot: &str,
        secret: &str,
        ttl: Duration,
    ) -> Result<()> {
        if self.rendezvous_relays().is_empty() {
            anyhow::bail!("no relay configured");
        }
        let bundle = self.publish_bundle();
        let payload = format!(
            "nightdrop://pair?addr={}&ik={}&otk={}",
            self.address(),
            bundle.identity_key,
            bundle.one_time_key
        );
        // Replace any stale invite for the same slot (e.g. the code was regenerated).
        self.pending_invites.retain(|p| p.slot != slot);
        self.pending_invites.push(PendingInvite {
            slot: slot.to_string(),
            secret: secret.to_string(),
            payload,
            ttl,
            expiry: Instant::now() + ttl,
        });
        Ok(())
    }

    /// True while at least one short-code invite is outstanding — lets the poller tick faster so
    /// pairing feels responsive (an inviter watching the code on screen).
    pub fn has_pending_invites(&self) -> bool {
        !self.pending_invites.is_empty()
    }

    /// Inviter side, run each poll tick: answer any joiner's SPAKE2 opener sitting in the
    /// rendezvous, and drop expired invites. Best-effort — a transient relay error is swallowed
    /// so it never aborts the wider poll cycle. Cheap when no invite is outstanding.
    ///
    /// Broadcasts across the whole rendezvous set (primary + our configured extras, §3.1): we
    /// look for a joiner's opener on **every** relay and post our answer to **all** of them, so a
    /// joiner reaches us over any relay they share with us — losing the single primary no longer
    /// stops pairing, as long as both sides share one live relay.
    pub fn service_pending_invites(&mut self) {
        let relays = self.rendezvous_relays();
        if relays.is_empty() {
            return;
        }
        let now = Instant::now();
        let before = self.pending_invites.len();
        self.pending_invites.retain(|p| p.expiry > now);
        if self.pending_invites.len() != before {
            crate::diag!(
                "invite: {} of {before} staged invite(s) expired — a joiner using that code now \
                 gets no answer",
                before - self.pending_invites.len()
            );
        }
        // Snapshot the minimal data so we don't borrow `self` across the relay round-trips.
        let invites: Vec<(String, String, String, Duration)> = self
            .pending_invites
            .iter()
            .map(|p| (p.slot.clone(), p.secret.clone(), p.payload.clone(), p.ttl))
            .collect();
        for (slot, secret, payload, ttl) in invites {
            // Both handles depend only on the slot, so compute them once per invite rather than
            // rebuilding them for every relay and every opener inside the loops below.
            let joiner_handle = rendezvous_handle(&slot, RDV_JOINER);
            let inviter_handle = rendezvous_handle(&slot, RDV_INVITER);
            for relay in &relays {
                let Ok(openers) = relay.take(&joiner_handle) else {
                    continue;
                };
                if openers.is_empty() {
                    continue; // nobody is trying this code right now — the common case
                }
                crate::diag!("invite: took {} opener(s) from a relay", openers.len());
                for opener in openers {
                    match build_invite_response(&secret, &payload, &opener) {
                        Ok(response) => {
                            // Post the answer to every relay so the joiner finds it wherever they poll.
                            let mut posted = 0usize;
                            for out in &relays {
                                if out.post(&inviter_handle, &response, ttl).is_ok() {
                                    posted += 1;
                                }
                            }
                            crate::diag!(
                                "invite: answered an opener, posted to {posted}/{} relays{}",
                                relays.len(),
                                if posted == 0 {
                                    " — ANSWER LOST, the opener is already consumed"
                                } else {
                                    ""
                                }
                            );
                        }
                        // A malformed opener, or one for a different code (an attacker probing the
                        // slot). Either way this one is now consumed.
                        Err(_) => crate::diag!("invite: could not build a response for an opener"),
                    }
                }
            }
        }
    }

    /// The relays used for short-code pairing rendezvous (§3.1): the primary shared default plus
    /// our configured extras (#17 `my_relays`), so pairing broadcasts across the whole set and
    /// survives loss of any single relay both sides don't uniquely depend on. Clients are cheap
    /// clones; built once per call for driving the joiner handshake outside the lock.
    pub fn rendezvous_relays(&self) -> Vec<RelayClient> {
        let mut relays = Vec::new();
        if let Some(primary) = &self.relay {
            relays.push(primary.clone());
        }
        for addr in self.my_relays.iter().chain(self.discovered_relays.iter()) {
            relays.push(build_relay(self.transport.as_ref(), addr));
        }
        relays
    }

    /// A clone of the attached primary relay client, for the paths that still use a single relay.
    #[allow(dead_code)] // retained for symmetry / non-rendezvous callers; tests use it
    pub fn relay_client(&self) -> Option<RelayClient> {
        self.relay.clone()
    }

    /// Joiner side: open a session toward the inviter from a payload recovered by
    /// [`run_join_handshake`], and return the new contact.
    pub fn connect_from_invite_payload(&mut self, payload: &str) -> Result<Contact> {
        let (addr, bundle) = crate::api::parse_invite(payload)?;
        let contact_id = self.connect_with_bundle(&addr, &bundle)?;
        self.contacts()
            .into_iter()
            .find(|c| c.id == contact_id)
            .ok_or_else(|| anyhow::anyhow!("chat not created"))
    }
}
