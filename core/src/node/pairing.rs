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
        if let Some(chat) = self.chats.get_mut(contact_id) {
            chat.contact.verified = verified;
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
        self.deliver(peer_address, &bundle.identity_key, &frame)?;

        let contact_id = bundle.identity_key.clone();
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
                peer_address: peer_address.to_string(),
                session,
                history: Vec::new(),
                authorized: true, // we initiated this chat
                // The joiner already knows the chat is live (no approval to wait on); the
                // code is tracked on the inviter side for the approval echo.
                code: None,
                closed: false,
                relay_receipts: HashMap::new(),
                remote_storage_healthy: true,
            },
        );
        // Onion client auth (#22): hand the inviter our client key for their onion so they can
        // authorize us to reach their (possibly restricted) descriptor. No-op off Tor.
        self.announce_client_key(&contact_id, peer_address);
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
        self.pending_invites.retain(|p| p.expiry > now);
        // Snapshot the minimal data so we don't borrow `self` across the relay round-trips.
        let invites: Vec<(String, String, String, Duration)> = self
            .pending_invites
            .iter()
            .map(|p| (p.slot.clone(), p.secret.clone(), p.payload.clone(), p.ttl))
            .collect();
        for (slot, secret, payload, ttl) in invites {
            for relay in &relays {
                let Ok(openers) = relay.take(&rendezvous_handle(&slot, RDV_JOINER)) else {
                    continue;
                };
                for opener in openers {
                    if let Ok(response) = build_invite_response(&secret, &payload, &opener) {
                        // Post the answer to every relay so the joiner finds it wherever they poll.
                        for out in &relays {
                            let _ =
                                out.post(&rendezvous_handle(&slot, RDV_INVITER), &response, ttl);
                        }
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
