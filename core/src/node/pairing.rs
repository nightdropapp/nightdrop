//! [`Node`] pairing & verification: safety numbers, pre-key bundle/connect, and the
//! short-code SPAKE2 invite hosting/join. Split out of `node.rs`.
use super::*;
use sha2::Digest as _;

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
    /// send the opening `Hello`, which carries our relay set. Returns the new contact id. The
    /// inviter's set is unknown until they send it (QR); see
    /// [`connect_with_bundle_and_set`](Self::connect_with_bundle_and_set).
    pub fn connect_with_bundle(
        &mut self,
        peer_address: &str,
        bundle: &PreKeyBundle,
    ) -> Result<String> {
        self.connect_with_bundle_and_set(peer_address, bundle, None)
    }

    /// [`connect_with_bundle`](Self::connect_with_bundle) when the invite also told us the
    /// inviter's relay set (a short-code response does, `optional-relay.md` §3 rule 3).
    pub fn connect_with_bundle_and_set(
        &mut self,
        peer_address: &str,
        bundle: &PreKeyBundle,
        inviter_relays: Option<RelaySet>,
    ) -> Result<String> {
        self.connect_with_invite(
            peer_address,
            bundle,
            InviteExtras {
                relays: inviter_relays,
                pass: None,
            },
        )
    }

    /// Join from an invite: open the session and send the `Hello`, using whatever else the invite
    /// carried (`optional-relay.md` §3 and §5).
    ///
    /// Onion access, both ways, without a relay:
    /// * the inviter's **pass** (`ck=`) is installed for their onion, so our `Hello` gets through
    ///   even when their onion admits only contacts;
    /// * our own client key for their onion goes *inside* the `Hello`, and is installed in place of
    ///   the pass once they accept (their first `RelaySet`);
    /// * a **reply pass** for our onion, authorized now for them, goes in the `Hello` too, so they
    ///   can reach us once they accept, whatever our onion admits.
    pub(crate) fn connect_with_invite(
        &mut self,
        peer_address: &str,
        bundle: &PreKeyBundle,
        invite: InviteExtras,
    ) -> Result<String> {
        let InviteExtras {
            relays: inviter_relays,
            pass,
        } = invite;
        // Before anything reaches the network: at the cap, nothing is sent.
        self.check_contact_cap(&bundle.identity_key)?;
        let mut session = crypto::open_outbound(&self.identity, bundle)?;
        let mine = self.my_relay_set();
        if let Some(pass) = &pass {
            if let Err(e) = self.transport.insert_client_key(peer_address, pass) {
                crate::diag!("pair: could not install the invite's pass: {e:#}");
            }
        }
        let own_key = self.transport.mint_pass().and_then(Result::ok);
        let own_key_minted = own_key.as_ref().map(|_| ());
        // No pass: our own key is what we present, from now on.
        if let (None, Some((_, secret))) = (&pass, &own_key) {
            let _ = self.transport.insert_client_key(peer_address, secret);
        }
        let reply_pass = self
            .transport
            .mint_pass()
            .and_then(Result::ok)
            .filter(|(public, _)| {
                self.transport
                    .authorize_client(&bundle.identity_key, public)
                    .is_ok()
            });
        // The pair's mailbox secret, from the first frame (`mailbox.rs`, `MailboxPair::seeded`).
        let mailbox_seed: [u8; 32] = rand::random();
        #[cfg(not(test))]
        let seeded = true;
        #[cfg(test)]
        let seeded = !self.legacy_no_mailbox;
        let body = serde_json::to_vec(&super::HelloBody {
            relays: mine.clone(),
            client_key: own_key.as_ref().map(|(public, _)| public.clone()),
            pass: reply_pass
                .as_ref()
                .map(|(_, secret)| super::encode_secret(secret)),
            mailbox: seeded.then(|| super::encode_secret(&mailbox_seed)),
        })?;
        let hello = crypto::encrypt(&mut session, &body)?;
        let frame = Frame::Hello {
            identity_key: self.identity_key(),
            address: self.address(), // where the peer should reply (we may be unreachable-by-metadata)
            message: WireOlm::from_olm(&hello),
        };
        // Held for delivery, not tried once. The first-contact Hello is the frame pairing depends on,
        // and the joiner's Tor is often minutes old (a fresh install joins right after setup): a
        // single attempt that reached nothing used to fail the join for good — the code already
        // spent, the inviter never shown a request, and this side left waiting for an approval
        // that could not come (phone, 2026-10-09). In `pending_control` it is saved with the state
        // and retried until it lands.
        //
        // **Directly only, for everyone** (`optional-relay.md` §5): the pass gets it through the
        // inviter's onion, so no relay is part of first contact, and one path is tested rather
        // than two.
        crate::diag!("pair: sending first-contact Hello to the peer");
        let handle = self.post_handle(&bundle.identity_key).unwrap_or_default();
        self.queue_pending_control(
            super::PendingControl {
                recipient_ik: bundle.identity_key.clone(),
                peer_address: peer_address.to_string(),
                relays: Vec::new(),
                nightdrop: false,
                bytes: wire::encode(&frame),
                handle,
                in_flight: false,
            },
            super::Route::DirectThenRelay,
        );
        crate::diag!("pair: Hello queued — retried until the peer or a relay takes it");

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
            if inviter_relays.is_some() {
                chat.relay_set = inviter_relays;
            }
            chat.relays_told = Some(mine);
            chat.mailbox = seeded.then(|| super::mailbox::MailboxPair::seeded(mailbox_seed));
            set_join_keys(chat, pass, own_key.map(|(_, secret)| secret));
            chat.history.push(ChatMessage::system(
                "🔑 Re-paired with a new secure session. Compare the safety number again if you \
                 want to confirm who this is."
                    .to_string(),
            ));
        } else {
            // We initiated this chat, so it is authorized at once. The joiner already knows the
            // chat is live; the code is tracked on the inviter side for the approval echo.
            let mut chat = Chat::new_paired(&contact_id, peer_address, session, true, None);
            chat.relay_set = inviter_relays;
            // The Hello carries it, and is held until it lands.
            chat.relays_told = Some(mine);
            chat.mailbox = seeded.then(|| super::mailbox::MailboxPair::seeded(mailbox_seed));
            set_join_keys(&mut chat, pass, own_key.map(|(_, secret)| secret));
            self.chats.insert(contact_id.clone(), chat);
        }
        // Onion client auth (#22): our client key for their onion rode in the Hello (above), so
        // the inviter authorizes it when the user accepts, not before. A transport that cannot
        // mint one this way sends it the 0.1 way, in a `ClientKey` frame (no-op off Tor).
        if own_key_minted.is_none() {
            self.announce_client_key(&contact_id, peer_address);
        }
        // Screenshot capability (#1): a contact paired after the launch-time broadcast would
        // otherwise never learn it, and be left reading our silence as "they'd be told".
        self.announce_captures_to(&contact_id);
        self.announce_block_to(&contact_id);
        self.announce_burns_to(&contact_id);
        self.announce_version_to(&contact_id);
        // The Hello carried the pair's mailbox seed, so there is nothing to agree: `seeded` above.
        Ok(contact_id)
    }
}

impl Node {
    // -------- Short-code pairing via the rendezvous mailbox (§5b/§5c) --------
    //
    // Interactive SPAKE2. The old scheme sealed the invite under an Argon2 key
    // derived from the short-code words with a *fixed* salt, so the relay — holding that
    // ciphertext — could try candidate codes offline until one decrypted (low-entropy codes
    // made that practical). SPAKE2 removes the offline attack: neither the joiner's opener nor
    // the inviter's response lets an observer test a code guess without a fresh online run, and
    // the payload is sealed under a key that only completing the handshake with the *right* code
    // reproduces. The AEAD tag on that seal doubles as key confirmation — a wrong code makes
    // `open` fail, so an imposter is turned away and a MITM (who lacks the code) cannot forge a
    // response either. The tradeoff is that pairing is now interactive: the inviter must be
    // reachable to answer, which the background poller handles via [`service_invites`].

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
            anyhow::bail!("short codes need a relay; with direct only, add contacts by QR code");
        }
        let bundle = self.publish_bundle();
        // Our relay set rides in the sealed response, so the joiner knows where our mail goes
        // before its first message (`optional-relay.md` §3 rule 3), and so does a pass for our
        // onion, as in a QR (§5).
        let mut payload = format!(
            "nightdrop://pair?addr={}&ik={}&otk={}&rs={}",
            self.address(),
            bundle.identity_key,
            bundle.one_time_key,
            crate::api::encode_relay_set(&self.my_relay_set())
        );
        if let Some(ck) = self.issue_pass(&bundle.one_time_key) {
            payload.push_str(&format!("&ck={ck}"));
        }
        // Replace any stale invite for the same slot (e.g. the code was regenerated).
        self.pending_invites.retain(|p| p.slot != slot);
        self.pending_invites.push(PendingInvite {
            slot: slot.to_string(),
            secret: secret.to_string(),
            payload,
            ttl,
            expiry: Instant::now() + ttl,
            openers: Vec::new(),
        });
        Ok(())
    }

    /// True while at least one short-code invite is outstanding — lets the poller tick faster so
    /// pairing feels responsive (an inviter watching the code on screen).
    pub fn has_pending_invites(&self) -> bool {
        !self.pending_invites.is_empty()
    }

    /// Inviter side of short-code pairing, in one call: [`invite_service_plan`](Self::invite_service_plan),
    /// [`service_invites`] and [`apply_invite_service`](Self::apply_invite_service). The poller runs
    /// the middle step without the core lock; this composition is for tests and simple callers.
    #[cfg(test)]
    pub fn service_pending_invites(&mut self) {
        if let Some(plan) = self.invite_service_plan() {
            let outcome = service_invites(&plan);
            self.apply_invite_service(outcome);
        }
    }

    /// What servicing the hosted short-code invites needs, snapshotted under the lock — or `None`
    /// when there is nothing to do (no invite, or no relay). Drops expired invites on the way.
    ///
    /// The relay round trips then run in [`service_invites`] **without** the core lock, on their
    /// own cadence: the poller runs this every [`RELAY_POLL_PAIRING`](crate::api) while an invite
    /// is open. It used to run only after a whole mailbox drain round had finished, and with v2
    /// polling fragments a round on a phone took over a minute — longer than a joiner waits — so
    /// an Android inviter answered openers after the joiner had given up (S25, 2026-10-07).
    pub(crate) fn invite_service_plan(&mut self) -> Option<InviteServicePlan> {
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
        if self.pending_invites.is_empty() {
            return None;
        }
        let relays = self.rendezvous_relays();
        if relays.is_empty() {
            return None;
        }
        Some(InviteServicePlan {
            relays,
            invites: self.pending_invites.clone(),
        })
    }

    /// Fold the result of [`service_invites`] back in: each invite's distinct openers so far (the
    /// guess count, `MAX_OPENERS_PER_INVITE`), and the invites retired for exceeding it. An invite
    /// that vanished meanwhile (expired, or replaced) is left alone.
    pub(crate) fn apply_invite_service(&mut self, outcome: InviteServiceOutcome) {
        for (slot, seen) in outcome.openers {
            if let Some(p) = self.pending_invites.iter_mut().find(|p| p.slot == slot) {
                p.openers = seen;
            }
        }
        if !outcome.retired.is_empty() {
            self.pending_invites
                .retain(|p| !outcome.retired.contains(&p.slot));
        }
        self.dirty = true;
    }

    /// The relays used for short-code pairing rendezvous (§3.1): the primary shared default plus
    /// our configured extras (#17 `my_relays`) and the directory's shared relays, so pairing broadcasts across the whole set and
    /// survives loss of any single relay both sides don't uniquely depend on. Clients are cheap
    /// clones; built once per call for driving the joiner handshake outside the lock.
    pub fn rendezvous_relays(&self) -> Vec<RelayClient> {
        self.relay_set()
    }

    /// The primary, our own extra relays and the directory's, **once each**. The signed directory
    /// lists the primary too, under the same address: without the check every rendezvous post and
    /// take went to the same relay twice — a joiner's opener "posted to 2/2 relays" was two copies
    /// on one relay (2026-10-07), the same doubling the drain plan already guards against.
    pub(crate) fn relay_set(&self) -> Vec<RelayClient> {
        let (primary, onions) = self.post_route(&self.my_relay_set());
        let mut relays: Vec<RelayClient> = primary.iter().cloned().collect();
        let mut seen: Vec<&str> = primary
            .as_ref()
            .and_then(|p| p.addr())
            .into_iter()
            .collect();
        for addr in &onions {
            if seen.contains(&addr.as_str()) {
                continue;
            }
            seen.push(addr);
            relays.push(build_relay(self.transport.as_ref(), addr));
        }
        relays
    }

    /// A clone of the attached primary relay client, for the paths that still use a single relay.
    pub fn relay_client(&self) -> Option<RelayClient> {
        self.relay.clone()
    }

    /// Joiner side: open a session toward the inviter from a payload recovered by
    /// [`run_join_handshake`], and return the new contact.
    pub fn connect_from_invite_payload(&mut self, payload: &str) -> Result<Contact> {
        let (addr, bundle) = crate::api::parse_invite(payload)?;
        let invite = crate::api::parse_invite_extras(payload);
        let contact_id = self.connect_with_invite(&addr, &bundle, invite)?;
        self.contacts()
            .into_iter()
            .find(|c| c.id == contact_id)
            .ok_or_else(|| anyhow::anyhow!("chat not created"))
    }
}

/// Snapshot for [`service_invites`] (see [`Node::invite_service_plan`]).
pub(crate) struct InviteServicePlan {
    relays: Vec<RelayClient>,
    invites: Vec<PendingInvite>,
}

/// What [`service_invites`] found, for [`Node::apply_invite_service`].
pub(crate) struct InviteServiceOutcome {
    /// Per invite slot: the distinct openers seen so far.
    openers: Vec<(String, Vec<[u8; 32]>)>,
    /// Slots retired for exceeding `MAX_OPENERS_PER_INVITE`.
    retired: Vec<String>,
}

/// Inviter side of short-code pairing, **without the core lock**: answer any joiner's SPAKE2
/// opener sitting in the rendezvous. Best-effort — a transient relay error is swallowed so it never
/// aborts the poller.
///
/// Broadcasts across the whole rendezvous set ([`Node::rendezvous_relays`]): we look for a joiner's
/// opener on **every** relay and post our answer to **all** of them, so a joiner reaches us over
/// any relay they share with us — losing the single primary no longer stops pairing, as long as
/// both sides share one live relay.
///
/// A joiner re-posts the same opener while it waits, to every relay, so one take can hold many
/// copies of it (twelve, on a phone that had not looked for a minute). Each distinct opener is
/// answered **once per call**: the answer waits on the relay for the joiner, and answering every
/// copy only spent a relay round trip apiece — 24 s of them in that case — before the next one.
pub(crate) fn service_invites(plan: &InviteServicePlan) -> InviteServiceOutcome {
    let mut outcome = InviteServiceOutcome {
        openers: Vec::new(),
        retired: Vec::new(),
    };
    let relays = &plan.relays;
    for PendingInvite {
        slot,
        secret,
        payload,
        ttl,
        openers: seen,
        ..
    } in &plan.invites
    {
        let mut seen = seen.clone();
        let mut answered: Vec<[u8; 32]> = Vec::new();
        // Both handles depend only on the slot, so compute them once per invite rather than
        // rebuilding them for every relay and every opener inside the loops below.
        let joiner_handle = rendezvous_handle(slot, RDV_JOINER);
        let inviter_handle = rendezvous_handle(slot, RDV_INVITER);
        let mut retire = false;
        'relays: for relay in relays {
            let Ok(openers) = relay.take(&joiner_handle) else {
                continue;
            };
            if openers.is_empty() {
                continue; // nobody is trying this code right now — the common case
            }
            crate::diag!("invite: took {} opener(s) from a relay", openers.len());
            for opener in openers {
                let digest: [u8; 32] = sha2::Sha256::digest(&opener).into();
                if answered.contains(&digest) {
                    continue; // a re-posted copy of one already answered in this call
                }
                // Each *distinct* opener is one guess at the code (MAX_OPENERS_PER_INVITE).
                if !seen.contains(&digest) {
                    if seen.len() >= super::MAX_OPENERS_PER_INVITE {
                        crate::diag!(
                            "invite: more than {} different openers for one code — retiring \
                             it; the joiner needs a fresh code",
                            super::MAX_OPENERS_PER_INVITE
                        );
                        retire = true;
                        break 'relays;
                    }
                    seen.push(digest);
                }
                answered.push(digest);
                match build_invite_response(secret, payload, &opener) {
                    Ok(response) => {
                        // Post the answer to every relay so the joiner finds it wherever they poll.
                        let mut posted = 0usize;
                        for out in relays {
                            if out.post(&inviter_handle, &response, *ttl).is_ok() {
                                posted += 1;
                            }
                        }
                        crate::diag!(
                            "invite: answered an opener, posted to {posted}/{} relays{}",
                            relays.len(),
                            if posted == 0 {
                                " — ANSWER LOST, the joiner's next re-post gets another"
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
        if retire {
            outcome.retired.push(slot.clone());
        }
        outcome.openers.push((slot.clone(), seen));
    }
    outcome
}

/// What an invite carries besides the address and bundle (`optional-relay.md` §3 and §5).
#[derive(Default)]
pub(crate) struct InviteExtras {
    /// The inviter's relay set (a short-code response carries it; a QR does not).
    pub(crate) relays: Option<RelaySet>,
    /// The inviter's pairing pass for their onion (`ck=`).
    pub(crate) pass: Option<[u8; 32]>,
}

/// A joiner's onion keys for the inviter's onion. With a pass, it is what we present until they
/// accept, and our own key waits; without one (an invite from a transport with no restricted
/// discovery) our own key is all there is, used at once. Either way `client_key` holds what is
/// installed, so a restart restores it.
fn set_join_keys(chat: &mut super::Chat, pass: Option<[u8; 32]>, own: Option<[u8; 32]>) {
    match pass {
        Some(pass) => {
            chat.client_key = Some(pass);
            chat.own_key_pending = own;
        }
        None => {
            chat.client_key = own.or(chat.client_key);
            chat.own_key_pending = None;
        }
    }
}
