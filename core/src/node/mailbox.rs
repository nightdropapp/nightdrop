//! Per-pair, epoch-rotating **v2 mailbox handles** (`docs/design/mailbox-handles.md`).
//!
//! A v2 handle is derived from a secret only the two people in a chat hold, the recipient's key,
//! and the UTC day — different for every sender, and new every day — so a relay cannot link one
//! person's deposits across senders or across days. 0.1.x also had a **v1** handle, a static hash
//! of the recipient's identity key that linked all of them; 0.2 neither posts nor polls it (§10).
//!
//! **Losing a message is worse than the leak**, so a handle is used only where both sides can
//! compute it:
//!
//! * A pair made by 0.2 gets its secret **at pairing**: the joiner puts a random seed in its
//!   `Hello`, which goes only directly and is E2E-encrypted ([`MailboxPair::seeded`]).
//! * A pair from 0.1.x that never finished the old agreement *agrees* one over the chat: each side
//!   contributes 32 random bytes in a [`Frame::MailboxKey`], and the secret is derived from both.
//!   A side **posts** to the pair's handle only once the peer has *proven* it holds the same
//!   secret (a confirmation hash). Until then the pair has no relay route, and its frames go
//!   directly or wait ([`Node::post_handle`]).
//! * A side **polls** the handles of every pair whose secret it holds — for yesterday, today and
//!   tomorrow, so clock skew needs no negotiation — plus its own cover-traffic handle.
//! * A changed contribution (a peer restored an older backup) resets confirmation at once, so
//!   nothing is posted to a handle the other side can no longer compute.

use super::*;

/// Marker prefix inside a [`Frame::MailboxKey`] plaintext, so a stray or truncated decrypt cannot
/// be mistaken for a contribution.
const MARK_MAILBOX_V2: &[u8] = b"nightdrop/ctl/mailbox/v2";

/// One UTC day. Handles rotate at this cadence; readers accept the neighbouring day either side.
pub(super) const EPOCH_SECS: u64 = 24 * 60 * 60;

/// This chat's side of the v2 agreement. `None` on a [`Chat`] until we first announce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MailboxPair {
    /// Our random contribution. Generated once, then fixed for the life of the chat.
    pub(super) own: [u8; 32],
    /// The peer's contribution, once received.
    pub(super) peer: Option<[u8; 32]>,
    /// The peer proved it computed the same secret. **Only then do we post v2** to it.
    pub(super) peer_confirmed: bool,
    /// We have sent a frame carrying *our* confirmation. Governs replies, so they terminate.
    pub(super) confirm_sent: bool,
    /// When our contribution first reached the peer or a relay (unix secs). Against the peer's
    /// later activity, this is what tells an older build from an offline one
    /// ([`Node::peer_on_old_version`]).
    pub(super) announced_at: Option<u64>,
}

impl MailboxPair {
    pub(super) fn fresh() -> Self {
        use rand::RngCore;
        let mut own = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut own);
        Self {
            own,
            peer: None,
            peer_confirmed: false,
            confirm_sent: false,
            announced_at: None,
        }
    }

    /// A pair agreed at pairing, from the seed the joiner put in its `Hello` (§10). Both sides
    /// hold the same seed as both contributions, so the secret is the same on both and confirmed
    /// from the start: the inviter read it from the joiner's Hello, and the joiner made it.
    pub(super) fn seeded(seed: [u8; 32]) -> Self {
        Self {
            own: seed,
            peer: Some(seed),
            peer_confirmed: true,
            confirm_sent: true,
            announced_at: None,
        }
    }
}

/// Base64 for the state file (sealed with everything else, like session pickles).
fn b64_32(b: &[u8; 32]) -> String {
    base64_handle(b)
}

fn unb64_32(s: &str) -> Option<[u8; 32]> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    URL_SAFE_NO_PAD.decode(s).ok()?.try_into().ok()
}

/// Rebuild a chat's agreement state from its persisted fields. A missing or unreadable own
/// contribution yields `None` — the chat re-agrees, which is safe (see the module docs).
pub(super) fn from_persisted(chat: &crate::storage::PersistedChat) -> Option<MailboxPair> {
    let own = unb64_32(chat.mailbox_own.as_deref()?)?;
    let peer = chat.mailbox_peer.as_deref().and_then(unb64_32);
    Some(MailboxPair {
        own,
        peer,
        // Never confirmed without the contribution it confirms.
        peer_confirmed: peer.is_some() && chat.mailbox_peer_confirmed,
        confirm_sent: peer.is_some() && chat.mailbox_confirm_sent,
        announced_at: chat.mailbox_announced_at,
    })
}

/// The persisted fields for a chat's agreement state:
/// `(own, peer, peer_confirmed, confirm_sent, announced_at)`.
pub(super) fn to_persisted(
    pair: &Option<MailboxPair>,
) -> (Option<String>, Option<String>, bool, bool, Option<u64>) {
    match pair {
        None => (None, None, false, false, None),
        Some(p) => (
            Some(b64_32(&p.own)),
            p.peer.as_ref().map(b64_32),
            p.peer_confirmed,
            p.confirm_sent,
            p.announced_at,
        ),
    }
}

/// The pair secret: both contributions, order-independent, bound to both identity keys.
pub(super) fn pair_secret(a_ik: &str, b_ik: &str, own: &[u8; 32], peer: &[u8; 32]) -> [u8; 32] {
    let (lo, hi) = if own <= peer {
        (own, peer)
    } else {
        (peer, own)
    };
    let (k1, k2) = if a_ik <= b_ik {
        (a_ik, b_ik)
    } else {
        (b_ik, a_ik)
    };
    let mut ikm = Vec::with_capacity(64);
    ikm.extend_from_slice(lo);
    ikm.extend_from_slice(hi);
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(b"nightdrop/mailbox/v2/pair"), &ikm);
    let mut info = Vec::new();
    info.extend_from_slice(k1.as_bytes());
    info.push(0);
    info.extend_from_slice(k2.as_bytes());
    let mut out = [0u8; 32];
    hk.expand(&info, &mut out)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    out
}

/// What a peer sends to prove it holds `secret`. One-way, so it reveals nothing about it.
pub(super) fn confirm_hash(secret: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"nightdrop/mailbox/v2/confirm");
    h.update(secret);
    h.finalize().into()
}

/// The v2 handle for mail **to** `recipient_ik` within the pair, on UTC day `epoch`. Directional:
/// the two people in a chat get different handles, or each would drain the other's mail.
pub(super) fn v2_handle(secret: &[u8; 32], recipient_ik: &str, epoch: u64) -> String {
    let hk = hkdf::Hkdf::<sha2::Sha256>::from_prk(secret).expect("32-byte PRK is valid");
    let mut info = Vec::new();
    info.extend_from_slice(b"nightdrop/mailbox/v2");
    info.extend_from_slice(recipient_ik.as_bytes());
    info.extend_from_slice(&epoch.to_be_bytes());
    let mut out = [0u8; 15];
    hk.expand(&info, &mut out)
        .expect("15 bytes is a valid HKDF-SHA256 output length");
    format!("mbx:{}", base64_handle(&out))
}

/// The current UTC day.
pub(super) fn current_epoch() -> u64 {
    crate::api::now_secs() / EPOCH_SECS
}

/// Fixed number of polling fragments per epoch (§5c, §8). At the 50-contact cap that is ~7
/// handles per fragment; below it, fragments are smaller, never more numerous. Fixed rather than
/// derived from the contact count, so adding a contact moves nobody else.
pub(super) const POLL_FRAGMENTS: u64 = 7;

/// The polled set of each epoch is padded with dummy handles to a multiple of this (and to at least
/// this), so one round does not reveal how many contacts we have. §5c: it keeps the count fuzzy, it
/// does not make it private — a handle that is polled all day and never receives looks like a dummy.
pub(super) const POLL_PAD: usize = 8;

/// The most contacts the app allows (§8). A product limit, not a protocol one: it bounds how many
/// handles a relay watches one reader collect, and what a phone polls over Tor each round.
pub const MAX_CONTACTS: usize = 50;

/// How far a sender's clock may be off before its v2 mail can go unpolled (design doc §9). Readers
/// poll tomorrow's handles only in this last stretch of the UTC day, and yesterday's until a drain
/// that started this long after midnight has emptied them. Wider tolerates worse clocks; narrower
/// polls fewer handles for less of the day. Timezones do not enter into it: epochs are UTC days of
/// Unix time, so only a clock that is actually wrong can fall outside.
pub(super) const SKEW_MARGIN_SECS: u64 = 3 * 60 * 60;

/// One group of handles polled together, on circuits no other group shares.
pub(crate) struct PollFragment {
    /// The isolation group ([`Transport::relay_dialer_isolated`]).
    pub(crate) group: u64,
    /// The UTC day these v2 handles belong to.
    pub(crate) epoch: Option<u64>,
    pub(crate) handles: Vec<String>,
}

/// A stable 64-bit id from labelled parts — isolation groups and fragment buckets.
pub(super) fn group_id(parts: &[&[u8]]) -> u64 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part);
    }
    u64::from_be_bytes(h.finalize()[..8].try_into().expect("8 bytes"))
}

/// The isolation group for **posting** to one recipient: each recipient on its own circuits, so a
/// relay cannot see that deposits for two people came from one sender.
pub(super) fn post_group(recipient_ik: &str) -> u64 {
    group_id(&[b"nightdrop/isolation/post", recipient_ik.as_bytes()])
}

/// `relay`, or its sibling on the circuits of `group` when the transport can isolate (Tor). A
/// client whose address is unknown, or a transport that cannot isolate, is returned as is.
pub(super) fn isolated(transport: &dyn Transport, relay: &RelayClient, group: u64) -> RelayClient {
    match relay
        .addr()
        .and_then(|addr| Some((addr, transport.relay_dialer_isolated(addr, group)?)))
    {
        Some((addr, dialer)) => RelayClient::with_dialer_for(addr, dialer),
        None => relay.clone(),
    }
}

/// 15 handle bytes for a dummy, so it has exactly the shape of a real handle.
fn dummy_bytes(id: u64, seed: &[u8; 32]) -> [u8; 15] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"nightdrop/poll/dummy-handle");
    h.update(seed);
    h.update(id.to_be_bytes());
    h.finalize()[..15].try_into().expect("15 bytes")
}

/// Encode a contribution (and, once known, our confirmation) for [`Frame::MailboxKey`].
fn encode_payload(own: &[u8; 32], confirm: Option<&[u8; 32]>) -> Vec<u8> {
    let mut p = Vec::with_capacity(MARK_MAILBOX_V2.len() + 64);
    p.extend_from_slice(MARK_MAILBOX_V2);
    p.extend_from_slice(own);
    if let Some(c) = confirm {
        p.extend_from_slice(c);
    }
    p
}

/// Parse a decrypted [`Frame::MailboxKey`] payload. `None` for anything malformed.
fn decode_payload(p: &[u8]) -> Option<([u8; 32], Option<[u8; 32]>)> {
    let rest = p.strip_prefix(MARK_MAILBOX_V2)?;
    match rest.len() {
        32 => Some((rest.try_into().ok()?, None)),
        64 => Some((
            rest[..32].try_into().ok()?,
            Some(rest[32..].try_into().ok()?),
        )),
        _ => None,
    }
}

/// How long after our contribution went out a peer's activity still proves nothing (§5.4): time
/// for their reply to cross a relay (a 5-minute poll round, twice) and arrive behind frames they
/// sent before reading ours. A false notice inside this window would tell someone their contact
/// needs an update when the reply was simply still on its way.
pub(super) const OLD_VERSION_GRACE_SECS: u64 = 10 * 60;

/// Refusal at the contact cap (§8). Said plainly, because the reason explains the product.
pub(super) const AT_CONTACT_CAP: &str =
    "You have 50 contacts, the most Night Drop allows. Delete a \
    chat to add a new one. The limit keeps a relay from rebuilding your contact list from the \
    mailboxes your device checks.";

impl Node {
    /// Open, approved chats — what the contact cap counts. A request still awaiting approval is not
    /// a contact yet, and a closed chat polls nothing.
    pub(super) fn open_contacts(&self) -> usize {
        self.chats
            .values()
            .filter(|c| c.authorized && !c.closed)
            .count()
    }

    /// Refuse a **new** contact at the cap. Re-pairing someone already in the list is not new.
    pub(super) fn check_contact_cap(&self, contact_id: &str) -> Result<()> {
        let existing = self
            .chats
            .get(contact_id)
            .is_some_and(|c| c.authorized && !c.closed);
        if !existing && self.open_contacts() >= MAX_CONTACTS {
            anyhow::bail!(AT_CONTACT_CAP);
        }
        Ok(())
    }

    /// Whether `chat`'s peer is on a build from before v2 mailboxes (§5.4). The evidence: they have
    /// been active — any authenticated frame, a silent ack included — well after our contribution
    /// reached them, and never sent theirs. A current build replies to a contribution on receipt
    /// and announces its own on every launch, so the silence means it dropped the frame undecoded.
    /// A peer that is merely offline has shown no activity since, and does not qualify.
    pub(super) fn peer_on_old_version(chat: &Chat) -> bool {
        let Some(pair) = &chat.mailbox else {
            return false;
        };
        let (Some(announced), Some(seen)) = (pair.announced_at, chat.last_seen) else {
            return false;
        };
        chat.authorized
            && !chat.closed
            && pair.peer.is_none()
            && seen > announced.saturating_add(OLD_VERSION_GRACE_SECS)
    }

    /// The handle to post mail for `contact_id` under **right now**: the pair's v2 handle once the
    /// pair is confirmed; `None` otherwise, which means **no relay route** to them
    /// ([`Node::route_to`]): their frames go directly or wait, never to a handle they cannot poll.
    pub(super) fn post_handle(&self, contact_id: &str) -> Option<String> {
        let pair = self.chats.get(contact_id)?.mailbox.as_ref()?;
        match (pair.peer_confirmed, pair.peer.as_ref()) {
            (true, Some(peer)) => {
                let secret = pair_secret(&self.identity_key(), contact_id, &pair.own, peer);
                Some(v2_handle(&secret, contact_id, current_epoch()))
            }
            _ => None,
        }
    }

    /// Whether the pair with `contact_id` has a confirmed mailbox secret ([`post_handle`]).
    ///
    /// [`post_handle`]: Self::post_handle
    pub(super) fn mailbox_agreed(&self, contact_id: &str) -> bool {
        self.post_handle(contact_id).is_some()
    }

    /// The secret behind our **own** cover-traffic handle: derived from the persisted polling seed,
    /// so it is known only to this device, fixed for the identity, and needs no contact.
    fn cover_secret(&self) -> [u8; 32] {
        let hk =
            hkdf::Hkdf::<sha2::Sha256>::new(Some(b"nightdrop/mailbox/v2/cover"), &self.poll_seed);
        let mut out = [0u8; 32];
        hk.expand(b"self", &mut out)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        out
    }

    /// Where a cover post goes on UTC day `epoch` (`cover-traffic.md`, `optional-relay.md` §6):
    /// the **inbound** handle of one agreed contact, picked at random each time — the mailbox that
    /// contact's real mail to us lands in, on the relays it posts to (ours) — or, with no agreed
    /// contact yet, our own cover handle. Either way a handle we poll, so the post is drained and
    /// dropped like any cover frame, and the mailbox it pads is one real mail uses.
    pub(super) fn cover_handle(&self, epoch: u64) -> String {
        use rand::seq::IteratorRandom;
        let me = self.identity_key();
        let pick = self
            .chats
            .iter()
            .filter(|(_, c)| c.authorized && !c.closed)
            .filter_map(|(id, c)| {
                let pair = c.mailbox.as_ref()?;
                let peer = pair.peer.as_ref()?;
                Some(pair_secret(&me, id, &pair.own, peer))
            })
            .choose(&mut rand::thread_rng());
        let secret = pick.unwrap_or_else(|| self.cover_secret());
        v2_handle(&secret, &me, epoch)
    }

    /// Every handle we poll at `now`, flattened (dummies included). Tests only — production drains
    /// by fragment ([`poll_fragments_at`](Self::poll_fragments_at)).
    #[cfg(test)]
    pub(super) fn drain_handles_at(&self, now: u64) -> Vec<String> {
        self.poll_fragments_at(now)
            .into_iter()
            .flat_map(|f| f.handles)
            .collect()
    }

    /// The UTC days whose v2 handles we poll at `now` (design doc §9): today always; tomorrow in the
    /// last [`SKEW_MARGIN_SECS`] of the day, for senders whose clocks run fast; yesterday until
    /// [`prev_epoch_drained`](Node::prev_epoch_drained) says a drain after the margin emptied it —
    /// senders whose clocks run slow, and everything posted before midnight while we were away. A
    /// device offline across midnight therefore keeps polling yesterday however late it returns.
    pub(super) fn polled_epochs(&self, now: u64) -> Vec<u64> {
        let today = now / EPOCH_SECS;
        let into_day = now % EPOCH_SECS;
        let mut epochs = Vec::with_capacity(3);
        if today > 0 && self.prev_epoch_drained != Some(today - 1) {
            epochs.push(today - 1);
        }
        epochs.push(today);
        if into_day >= EPOCH_SECS - SKEW_MARGIN_SECS {
            epochs.push(today + 1);
        }
        epochs
    }

    /// The epoch a successful drain at `now` would retire: yesterday, once it is polled and the
    /// margin since midnight has passed (before that, slow-clocked senders may still post to it).
    pub(super) fn settling_epoch(&self, now: u64) -> Option<u64> {
        let yesterday = (now / EPOCH_SECS).checked_sub(1)?;
        (now % EPOCH_SECS >= SKEW_MARGIN_SECS && self.polled_epochs(now).contains(&yesterday))
            .then_some(yesterday)
    }

    /// Our mailboxes in polling fragments (§5c), fixed for each handle's epoch, for the days
    /// [`polled_epochs`](Self::polled_epochs) names at `now`.
    ///
    /// * **Each epoch in its own fragments.** Yesterday's, today's and tomorrow's handles for one
    ///   pair are polled in *their own day's* partition — together they would link the pair across
    ///   days, which is what rotation exists to prevent.
    /// * **A contact's bucket** comes from a keyed hash of our persisted seed, the epoch and the
    ///   contact, so the partition is the same every round of that epoch and survives a restart.
    ///   Re-drawing it per round is the trap §5c describes: a relay intersects the fragments it sees
    ///   and reassembles the whole set.
    /// * **Padded with dummies** per epoch, stable for that epoch like real handles.
    pub(crate) fn poll_fragments_at(&self, now: u64) -> Vec<PollFragment> {
        let me = self.identity_key();
        let cover = self.cover_secret();
        let mut fragments = Vec::new();
        for epoch in self.polled_epochs(now) {
            let e = epoch.to_be_bytes();
            let mut buckets: Vec<Vec<String>> = vec![Vec::new(); POLL_FRAGMENTS as usize];
            let bucket_of = |label: &[u8]| {
                (group_id(&[b"nightdrop/poll/bucket", &self.poll_seed, &e, label]) % POLL_FRAGMENTS)
                    as usize
            };
            let mut real = 0usize;
            for (contact_id, chat) in &self.chats {
                if chat.closed {
                    continue;
                }
                let Some(pair) = &chat.mailbox else { continue };
                let Some(peer) = &pair.peer else { continue };
                let secret = pair_secret(&me, contact_id, &pair.own, peer);
                buckets[bucket_of(contact_id.as_bytes())].push(v2_handle(&secret, &me, epoch));
                real += 1;
            }
            // Our own cover handle (`cover_handle`), polled like a contact's: cover posts to it
            // while we have no agreed contact, and are drained and dropped like any other.
            buckets[bucket_of(b"cover")].push(v2_handle(&cover, &me, epoch));
            real += 1;
            let padded = real.max(1).div_ceil(POLL_PAD) * POLL_PAD;
            for i in 0..(padded - real) as u64 {
                let label = [b"dummy".as_slice(), &i.to_be_bytes()].concat();
                let id = group_id(&[b"nightdrop/poll/dummy", &self.poll_seed, &e, &label]);
                let dummy = format!("mbx:{}", base64_handle(&dummy_bytes(id, &self.poll_seed)));
                buckets[bucket_of(&label)].push(dummy);
            }
            for (b, handles) in buckets.into_iter().enumerate() {
                if handles.is_empty() {
                    continue;
                }
                fragments.push(PollFragment {
                    group: group_id(&[
                        b"nightdrop/isolation/poll",
                        &self.poll_seed,
                        &e,
                        &(b as u64).to_be_bytes(),
                    ]),
                    epoch: Some(epoch),
                    handles,
                });
            }
        }
        fragments
    }

    /// Send our contribution to every open, authorized chat that is not yet confirmed. Once per
    /// run per chat, retried on the next tick if it could not be delivered — the heal for a lost
    /// frame, and the start of the agreement for chats that predate v2.
    pub fn announce_mailbox(&mut self) {
        if !self.announce_ready() {
            return;
        }
        let ids: Vec<String> = self
            .chats
            .iter()
            .filter(|(id, c)| {
                c.authorized
                    && !c.closed
                    && !c.mailbox.as_ref().is_some_and(|p| p.peer_confirmed)
                    && !self.mailbox_announced.contains(id.as_str())
                    // Mailboxes live on relays: with no relay on one side there is nothing to agree.
                    && self.relays_meet(id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            // Flagged while in flight; a failed delivery clears it again (`apply_outbound`).
            self.mailbox_announced.insert(id.clone());
            if !self.send_mailbox_key(&id) {
                self.mailbox_announced.remove(&id);
            }
        }
    }

    /// Send our contribution — with our confirmation once we can compute the secret — on the chat.
    /// Returns whether a frame went out (queued, on an asynchronous transport); the delivery itself
    /// is recorded when it completes.
    pub(super) fn send_mailbox_key(&mut self, contact_id: &str) -> bool {
        #[cfg(test)]
        if self.legacy_no_mailbox {
            return false;
        }
        // Mailboxes live on relays: with no relays between us (either side direct only, or
        // their set not known yet) there is nothing to agree, and an agreement they never answer
        // would later read as "they're on an older version" (`peer_on_old_version`). Checked here
        // rather than by each caller: the pairing paths start the agreement before any tick.
        if !self.relays_meet(contact_id) {
            return false;
        }
        let me = self.identity_key();
        let frame_confirms: bool;
        let (addr, frame) = {
            let Some(chat) = self.chats.get_mut(contact_id) else {
                return false;
            };
            if !chat.authorized || chat.closed {
                return false;
            }
            if chat.mailbox.is_none() {
                chat.mailbox = Some(MailboxPair::fresh());
                self.dirty = true;
            }
            let pair = chat.mailbox.as_mut().expect("set just above");
            let confirm = pair
                .peer
                .map(|peer| confirm_hash(&pair_secret(&me, contact_id, &pair.own, &peer)));
            let payload = encode_payload(&pair.own, confirm.as_ref());
            // Sealed before anything is marked sent: an unusable session (non-contributory ratchet
            // key) sends nothing and must not leave `confirm_sent` set.
            let Ok(message) = crypto::encrypt(&mut chat.session, &payload) else {
                return false;
            };
            if confirm.is_some() {
                pair.confirm_sent = true;
            }
            frame_confirms = confirm.is_some();
            (
                chat.peer_address.clone(),
                Frame::MailboxKey {
                    from: me.clone(),
                    message: WireOlm::from_olm(&message),
                },
            )
        };
        self.dirty = true;
        if frame_confirms {
            crate::diag!("mailbox: sending our contribution + confirmation");
        }
        // Whether it lands is recorded when it has gone out (`apply_outbound`): the relay copy's
        // `announced_at`, and the `mailbox_announced` flag cleared again on failure.
        let done = super::OutboundDone::Announced(super::Announce::Mailbox, contact_id.to_string());
        let _ = self.deliver_with(&addr, contact_id, &frame, false, done);
        true
    }

    /// A [`Frame::MailboxKey`] from `from`. Stores their contribution, checks their confirmation,
    /// and replies with ours when the agreement still needs it. Silent: no history entry.
    pub(super) fn on_mailbox_key(&mut self, from: &str, message: &WireOlm) {
        #[cfg(test)]
        if self.legacy_no_mailbox {
            return;
        }
        let me = self.identity_key();
        let reply = {
            let Some(chat) = self.chats.get_mut(from) else {
                return;
            };
            if !chat.authorized {
                return;
            }
            // Undecryptable = ignored, never an error. It happens legitimately — a reply sealed on a
            // session the peer has just replaced (a re-pair) — and a lost key frame costs nothing,
            // because the next run's announce resends it.
            let Ok(olm) = message.to_olm() else {
                return;
            };
            let Ok(pt) = crypto::decrypt(&mut chat.session, &olm) else {
                return;
            };
            let Some((theirs, their_confirm)) = decode_payload(&pt) else {
                return;
            };
            chat.mark_alive();
            let pair = chat.mailbox.get_or_insert_with(MailboxPair::fresh);
            if pair.peer != Some(theirs) {
                // New or changed: a changed contribution means they lost the old secret (a restore),
                // so stop posting v2 to them at once and re-agree.
                crate::diag!(
                    "mailbox: {} peer contribution",
                    if pair.peer.is_some() {
                        "CHANGED"
                    } else {
                        "new"
                    }
                );
                pair.peer = Some(theirs);
                pair.peer_confirmed = false;
                pair.confirm_sent = false;
            }
            let secret = pair_secret(&me, from, &pair.own, &theirs);
            if their_confirm == Some(confirm_hash(&secret)) {
                if !pair.peer_confirmed {
                    crate::diag!("mailbox: peer confirmed the secret — posting v2 from now on");
                }
                pair.peer_confirmed = true;
            } else if their_confirm.is_some() {
                crate::diag!(
                    "mailbox: peer confirmation did not match — no relay route until it does"
                );
            }
            // Reply when they have not confirmed (they need ours), or when we have not yet sent our
            // confirmation (they need that). A confirm-bearing frame after both sides confirmed
            // triggers nothing, so this cannot ping-pong.
            their_confirm.is_none() || !pair.confirm_sent
        };
        self.dirty = true;
        if reply {
            self.send_mailbox_key(from);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_derive_the_same_secret_in_either_order() {
        let (a, b) = ([1u8; 32], [2u8; 32]);
        assert_eq!(
            pair_secret("alice", "bob", &a, &b),
            pair_secret("bob", "alice", &b, &a)
        );
        assert_ne!(
            pair_secret("alice", "bob", &a, &b),
            pair_secret("alice", "carol", &a, &b),
            "bound to the pair's identity keys"
        );
    }

    #[test]
    fn handles_differ_by_direction_day_and_pair() {
        let s = pair_secret("alice", "bob", &[1; 32], &[2; 32]);
        let to_bob = v2_handle(&s, "bob", 20_000);
        assert_ne!(
            to_bob,
            v2_handle(&s, "alice", 20_000),
            "each direction has its own handle"
        );
        assert_ne!(to_bob, v2_handle(&s, "bob", 20_001), "rotates daily");
        let other = pair_secret("carol", "bob", &[3; 32], &[4; 32]);
        assert_ne!(
            to_bob,
            v2_handle(&other, "bob", 20_000),
            "two senders to one recipient differ"
        );
        assert_eq!(
            to_bob.len(),
            "mbx:".len() + 20,
            "the same shape as every other handle, dummies included"
        );
        assert!(to_bob.starts_with("mbx:"));
    }

    #[test]
    fn payloads_round_trip_and_reject_garbage() {
        let own = [7u8; 32];
        let c = [9u8; 32];
        assert_eq!(
            decode_payload(&encode_payload(&own, None)),
            Some((own, None))
        );
        assert_eq!(
            decode_payload(&encode_payload(&own, Some(&c))),
            Some((own, Some(c)))
        );
        assert_eq!(decode_payload(b"nightdrop/ctl/mailbox/v2short"), None);
        assert_eq!(decode_payload(&[0u8; 56]), None);
    }
}
