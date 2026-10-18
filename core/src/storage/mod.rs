//! At-rest persistence (`ARCHITECTURE.md` §6 local-first, §8 device-theft). The whole
//! app state — identity, per-contact sessions, contacts, and **message history** — is
//! serialized and sealed under a 32-byte key.
//!
//! The key in production comes from the **OS keystore** (Keychain / Keystore / DPAPI /
//! libsecret), or, with the app lock on, is unwrapped with a passphrase- or PIN-derived key
//! ([`lock`], `docs/design/app-lock.md`); here it is injected as bytes so the logic is testable. Identity and session
//! key material additionally use vodozemac's own encrypted pickling under the same key;
//! the outer AEAD then also covers the message plaintext.

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};

use crate::Result;

pub mod lock;

/// 32-byte symmetric key: from the OS keystore, or derived from the app-lock secret ([`lock`]).
pub type StoreKey = [u8; 32];

const NONCE_LEN: usize = 12;

/// One persisted conversation. `session_pickle` is vodozemac-encrypted; the surrounding
/// fields (including `history` plaintext) are protected by the outer [`seal`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedChat {
    pub contact_id: String,
    pub peer_address: String,
    pub their_name: String,
    pub my_name: String,
    pub remote_storage: bool,
    /// Per-chat disappearing-messages timer in seconds (0 = off). `#[serde(default)]` keeps
    /// older state files loadable (they simply have the timer off).
    #[serde(default)]
    pub disappearing_secs: u64,
    /// When the disappearing timer changed and to what, as `(unix secs, timer secs)`, oldest
    /// first. A message expires by the timer in effect at its own timestamp, so turning a timer
    /// on never reaches back into the history (forward-only). `None` in a state from before 0.2,
    /// where the chat's current timer applied to every message: restored as one entry from the
    /// beginning, so those messages keep expiring as they did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disappearing_timeline: Option<Vec<(u64, u64)>>,
    /// When opt-in server storage was switched on or off, `(unix secs, on)`, oldest first; the
    /// 24h bomb deletes only messages sent while it was on. `None` in a state from before 0.2,
    /// where the bomb covered the whole chat: restored as on from the load, so nothing already
    /// there is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_timeline: Option<Vec<(u64, bool)>>,
    pub session_pickle: String,
    pub history: Vec<PersistedMessage>,
    /// Whether the chat was torn down (peer deleted / code re-used). Persisted so a deleted
    /// chat stays deleted across restarts instead of silently resurrecting. `#[serde(default)]`
    /// keeps older state files loadable.
    #[serde(default)]
    pub closed: bool,
    /// Whether we have this chat in a backup (#7); drives the logout Closed-signal (§11.6).
    #[serde(default)]
    pub backed_up: bool,
    /// Whether the peer told us they keep a backup of this chat (#7); drives the transparency
    /// warning. `#[serde(default)]` keeps older state files loadable.
    #[serde(default)]
    pub peer_backed_up: bool,
    /// Whether the user verified this contact's safety number out-of-band (key-verification
    /// design). `#[serde(default)]` keeps older state files loadable.
    #[serde(default)]
    pub verified: bool,
    /// Whether the peer told us their device cannot report screenshots (#1). Persisted so the
    /// warning survives a restart — recomputing it as "unknown" every launch would drop the signal
    /// until the peer next announced, which they only do on a change. `#[serde(default)]` (→ None)
    /// keeps older state files loadable and correctly says "they have not told us".
    #[serde(default)]
    pub peer_captures_silent: Option<bool>,
    /// Whether the **peer** signaled that *they* verified this chat's safety number — informational
    /// only (never sets our own `verified`). `#[serde(default)]` keeps older state files loadable.
    #[serde(default)]
    pub peer_verified: bool,
    /// The peer's extra relays as 0.1.x knew them (#17), on top of an implicit Night Drop relay.
    /// Read only to seed [`relay_set`](Self::relay_set) for a chat from before 0.2.
    #[serde(default)]
    pub peer_relays: Vec<String>,
    /// The peer's complete relay set (`Frame::RelaySet`, `optional-relay.md` §3). Absent, with
    /// `relay_set_unknown` false, on a chat from before 0.2: it is seeded with Night Drop +
    /// `peer_relays`, exactly what we posted to (§3 rule 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_set: Option<RelaySet>,
    /// A 0.2 chat whose peer has not told us their set yet: hold, never assume (§3 rule 3).
    #[serde(default, skip_serializing_if = "is_false")]
    pub relay_set_unknown: bool,
    /// Our set as this peer last confirmed hearing it. Absent = never told, which a chat from
    /// before 0.2 is, so the first 0.2 launch tells every contact once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relays_told: Option<RelaySet>,
    /// What the peer last confirmed hearing of our standing announcements (burn support, app
    /// version, address), so a launch re-sends only what changed. Absent on a chat from before
    /// 0.2, which is told once.
    #[serde(default, skip_serializing_if = "is_false")]
    pub told_burns: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub told_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub told_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub told_captures: Option<bool>,
    /// Pairing passes in waiting (`optional-relay.md` §5), base64. Joiner: our own client secret for
    /// the inviter's onion, installed in place of their QR pass once they accept. Inviter: the
    /// joiner's client key for our onion and their reply pass, both unused until Accept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_key_pending: Option<String>,
    /// The peer last reached us in an older wire version (0.1.x), so it cannot read what we send;
    /// cleared by any frame we can read from them.
    #[serde(default, skip_serializing_if = "is_false")]
    pub peer_wire_old: bool,
    /// We told this contact's 0.1.x app, in its own format, that we updated and they need to.
    #[serde(default, skip_serializing_if = "is_false")]
    pub told_upgrade: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_key_pending: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_pending: Option<String>,
    /// Whether the peer's build understands burn messages (`Frame::Burns`). `None`/absent means
    /// unknown, which is treated as unsupported — burn is never offered on a guess.
    #[serde(default)]
    pub peer_supports_burn: Option<bool>,
    /// The peer's announced app version (`Frame::Version`). `None`/absent = not announced yet.
    #[serde(default)]
    pub peer_app_version: Option<String>,
    /// Recall receipts for our still-**queued** (undelivered) messages, so an edit/unsend after an
    /// app restart can still pull an undelivered blob off the relay instead of letting the peer
    /// receive it and only then tombstoning it (§11.3). The `delete_token`s are secrets, but the
    /// whole state file is already sealed under the store key, so they're no more exposed than the
    /// session pickles beside them. `#[serde(default)]` keeps older state files loadable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued_receipts: Vec<PersistedReceipt>,
    /// Unix seconds of the last **authenticated** contact from the peer (message or verified
    /// control frame), for the "no sign of them" notice. `#[serde(default)]` keeps older state
    /// files loadable — they simply have no reading yet, which reads as "unknown", never as
    /// "silent".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_unix: Option<u64>,
    /// A nickname the local user gave this contact (`contact-naming.md`). Local only — it is never
    /// sent, so it lives here rather than travelling with a rename. `#[serde(default)]` keeps older
    /// state files loadable; a new field on a shipped format must never be required.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub local_name: String,
    /// Base64 of our 32-byte client descriptor-encryption secret for this peer's restricted onion
    /// (#22). Kept here — sealed with everything else — rather than in arti's keystore, which
    /// stored it unencrypted in a directory named after the peer's address
    /// (`docs/design/onion-key-at-rest.md`). `#[serde(default)]` keeps older state files loadable;
    /// a chat without one simply mints a fresh key and re-announces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key: Option<String>,
    /// Whether the local user has **approved** this chat, or it is still an inbound request awaiting
    /// approval (`ARCHITECTURE.md` §5 — authorization before first message).
    ///
    /// Added 2026-08-02, after a device test. This field did not exist, and restore hardcoded
    /// `authorized: true` on the reasoning that "persisted chats were authorized before saving" —
    /// which is simply not so, since a pending request is a chat and is saved like any other. So a
    /// stranger's unapproved request was **promoted to an approved contact by the next restart**,
    /// with no approval ever given: exactly the invariant this app is supposed to hold.
    ///
    /// `default = "yes"`, not `#[serde(default)]`. A missing field means a state file written before
    /// this existed, and defaulting those to `false` would demote every real contact on an upgrade
    /// to a request the user has to re-approve. Files written from now on carry the truth.
    #[serde(default = "yes")]
    pub authorized: bool,
    /// Our 32-byte contribution to the chat's v2 mailbox secret, base64 (`mailbox-handles.md`).
    /// Absent on older state files and on chats that have not agreed yet — they post and poll v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox_own: Option<String>,
    /// The peer's contribution, base64, once received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox_peer: Option<String>,
    /// The peer proved it holds the same secret, so we post v2 to it. Persisted: forgetting it
    /// only costs privacy (back to v1), but a restart must not re-open the agreement needlessly.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mailbox_peer_confirmed: bool,
    /// We have sent our own confirmation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mailbox_confirm_sent: bool,
    /// When our contribution first reached the peer or a relay (unix secs) — with the peer's later
    /// activity, what tells an older build from an offline one (`mailbox.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox_announced_at: Option<u64>,
}

/// `serde` default for [`PersistedChat::authorized`] — see the field's note on why absence must
/// read as approved.
fn yes() -> bool {
    true
}

/// One relay recall receipt for a still-queued message (see [`PersistedChat::queued_receipts`]).
/// `target_msg_id` is the chat message this receipt lets us recall; `relay_addr` is `None` for the
/// primary relay or the advertised extra relay's address otherwise; `msg_id`/`delete_token` are the
/// relay's own blob id and the secret token that authorizes deleting it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedReceipt {
    pub target_msg_id: String,
    #[serde(default)]
    pub relay_addr: Option<String>,
    /// The mailbox handle the copy was posted under, for recall. Absent on receipts saved before v2
    /// handles, which were all posted to v1 — an empty string means exactly that.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub handle: String,
    pub msg_id: String,
    pub delete_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedMessage {
    pub from_me: bool,
    pub text: String,
    /// A local system notice (e.g. "the other person deleted this chat"), not a real
    /// exchanged message. Defaults to false for backups written before this field existed.
    #[serde(default)]
    pub system: bool,
    /// Per-message token for edit correlation; empty for pre-edit-era messages.
    #[serde(default)]
    pub msg_id: String,
    /// True once replaced by a sender edit (the UI shows an "edited" tag).
    #[serde(default)]
    pub edited: bool,
    /// Unix seconds when the message was sent/received; 0 for pre-timestamp messages
    /// (which are exempt from the edit window and the ephemeral time-bomb).
    #[serde(default)]
    pub at: u64,
    /// Outgoing delivery state ("", "sent", "queued", "delivered", "expired") — persisted
    /// so a queued badge survives a restart and can still flip to delivered/expired.
    #[serde(default)]
    pub delivery: String,
    /// Media fields (empty/0 for plain text). The bytes live in a sealed file named by
    /// `media_id`; only the small reference is persisted here. `#[serde(default)]` keeps
    /// older state/backups loadable.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub media_id: String,
    #[serde(default)]
    pub media_size: u64,
    #[serde(default)]
    pub transfer_id: String,
    #[serde(default)]
    pub thumb_id: String,
    /// Burn timer in seconds for a per-message burn (`docs/design/burn-messages.md`); 0 means
    /// this is an ordinary message. Counts from [`viewed_at`](Self::viewed_at), not from `at`.
    #[serde(default)]
    pub burn_secs: u64,
    /// Unix seconds when the recipient first revealed a burn message; 0 = not yet viewed.
    /// Persisted so the countdown survives a restart — a burn you can reset by force-quitting
    /// would be theatre.
    #[serde(default)]
    pub viewed_at: u64,
}

/// An attachment carried *inside* a portable backup: its id plus the base64 plaintext bytes.
/// Only populated for backups (so they're self-contained / restorable on another device); the
/// at-rest state file leaves this empty and keeps media in sibling sealed files instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedMedia {
    pub id: String,
    /// base64-encoded plaintext bytes (the whole [`PersistedState`] is sealed around it).
    pub data: String,
}

/// A file carried inside a backup, by path relative to a base dir: arti's on-disk onion keystore,
/// how backups up to 0.1.15 carried the identity (see [`PersistedState::onion_keys`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedFile {
    pub path: String,
    /// base64-encoded file bytes.
    pub data: String,
}

/// One entry in our relay list (`docs/design/optional-relay.md` §2). The Night Drop relay is
/// symbolic: it stands for the baked-in relay plus whatever the signed directory lists now, so it
/// follows a rotation instead of freezing the onions of the day it was chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayEntry {
    NightDrop,
    Onion(String),
}

/// Someone's **complete** relay set, the shape `Frame::RelaySet` carries (`optional-relay.md` §3):
/// whether they receive on the Night Drop relay, and the onions they added. Both empty = direct
/// only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelaySet {
    pub nightdrop: bool,
    pub relays: Vec<String>,
}

impl RelaySet {
    /// The set a relay list stands for.
    pub fn of(entries: &[RelayEntry]) -> Self {
        let mut set = RelaySet::default();
        for e in entries {
            match e {
                RelayEntry::NightDrop => set.nightdrop = true,
                RelayEntry::Onion(addr) => {
                    if !set.relays.contains(addr) {
                        set.relays.push(addr.clone());
                    }
                }
            }
        }
        set
    }

    /// No relay at all: direct only.
    pub fn is_empty(&self) -> bool {
        !self.nightdrop && self.relays.is_empty()
    }
}

/// A frame — a chat-delete `Closed` (§11.6), a screenshot notice, or a joiner's first-contact
/// `Hello` — that hasn't reached a
/// relay yet, persisted so it survives an app restart before the poller's retry lands (otherwise
/// one sent during a relay/arti outage, followed by a restart, would silently never reach the peer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedPendingControl {
    pub recipient_ik: String,
    pub peer_address: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<String>,
    /// Whether the recipient's set includes the Night Drop relay. Absent in states written before
    /// 0.2, when every copy also went to the Night Drop relay.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub nightdrop: bool,
    /// base64-encoded sealed control-frame bytes (the whole [`PersistedState`] is sealed around it).
    pub bytes: String,
    /// The mailbox handle to post under: the chat's v2 handle when it had one. Empty in states
    /// written before 0.2, which retried under the recipient's v1 handle.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub handle: String,
}

/// One frame of the held queue (`docs/design/optional-relay.md` §4): sealed, waiting for its
/// recipient to come online or for a relay to take it. For a direct-only pair it is the only copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedHeld {
    pub contact_id: String,
    /// The message it carries; empty for a control frame.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub msg_id: String,
    /// base64 of the sealed frame (the whole [`PersistedState`] is sealed around it).
    pub bytes: String,
}

/// A pairing pass we issued (`optional-relay.md` §5): authorized on our onion under `name` until a
/// `Hello` uses the invite's one-time key `otk`, or until `expires` (unix seconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedPass {
    pub name: String,
    pub otk: String,
    pub expires: u64,
}

/// A short-code invite this device is hosting, persisted so it survives a **core rebuild**.
///
/// It used to live only in memory, and a rebuild is not rare: the guard heal does one, so does
/// "Reset Tor connection", so does a restore. The inviter's screen kept showing a code that
/// nothing was listening for any more, so the joiner got "the inviter never answered" and the
/// blame landed on the wrong side. Observed on 2026-08-03 with two fresh identities, where it is
/// worst — a new identity's first descriptor publish is slow enough that the 150 s heal reliably
/// fires *during* pairing, so the people most likely to hit it are new users on their first try.
///
/// The secret words are moderate-entropy and live at most [`SHORT_CODE_TTL`]; they sit inside the
/// same sealed state file as the ratchet sessions, so this adds no new class of at-rest secret.
///
/// [`SHORT_CODE_TTL`]: crate::api
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedInvite {
    pub slot: String,
    pub secret: String,
    /// The `nightdrop://pair?…` payload handed to the joiner.
    pub payload: String,
    /// TTL for the sealed response posted back to the joiner, in seconds.
    pub ttl_secs: u64,
    /// Wall-clock expiry. Stored as unix seconds because the in-memory form is an `Instant`,
    /// which is monotonic and meaningless across a process restart.
    pub expires_unix: u64,
    /// Hex SHA-256 of each distinct opener answered so far, so a restart does not reset the
    /// guess count (`node::MAX_OPENERS_PER_INVITE`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub openers: Vec<String>,
}

/// The full device state written to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    pub account_pickle: String,
    pub address: String,
    pub chats: Vec<PersistedChat>,
    /// Attachment bytes bundled into a backup so it's self-contained. Empty in the at-rest
    /// state file (media lives in sealed sibling files there). `#[serde(default)]` +
    /// skip-empty keeps older blobs loadable and the state file small.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<PersistedMedia>,
    /// The onion identity (base64 of the 64-byte expanded ed25519 secret), bundled into a backup
    /// so a restore comes back on the same `.onion` — otherwise every contact's stored address
    /// for us goes stale. `None` in the at-rest state file, where the identity is sealed beside it
    /// (`onion-key.sealed`). Added in 0.1.29: from 0.1.16 (the in-memory keystore) to 0.1.28,
    /// backups carried no identity at all (`docs/advisories/2026-10-07-backups-lost-the-onion-address.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onion_identity: Option<String>,
    /// arti's on-disk keystore files, which is how backups carried the identity up to 0.1.15,
    /// while arti kept it on disk. Still read from old backups; still written if an install has
    /// not migrated off the on-disk keystore yet. Empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub onion_keys: Vec<PersistedFile>,
    /// The onions in our relay list. Before 0.2 this was the whole list of *extra* relays, on top
    /// of an implicit Night Drop relay; it is still written so the state reads the same either way.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub my_relays: Vec<String>,
    /// Our relay list (`optional-relay.md` §2), in the user's order. `None` in states written
    /// before 0.2, which start as Night Drop + `my_relays`, exactly what they were using.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relays: Option<Vec<RelayEntry>>,
    /// Relays removed from the list, still drained until the unix time beside each: mail already
    /// queued there for us lives a day.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays_draining: Vec<(RelayEntry, u64)>,
    /// The held queue, in send order (`optional-relay.md` §4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<PersistedHeld>,
    /// Pairing passes we issued in a QR or short code and have not revoked yet (§5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passes: Vec<PersistedPass>,
    /// Shared relays learned from the operator-signed relay directory (§3.1), and the version we
    /// accepted. `#[serde(default)]` for forward-compat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_relays: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub directory_version: u64,
    /// The newest signing keys accepted through a recovery-signed rotation (`keyroll.rs`), for the
    /// update manifest and the relay list. None = the key baked into this build. Kept so a leaked
    /// older key can never be accepted again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_key: Option<crate::keyroll::Trust>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory_key: Option<crate::keyroll::Trust>,
    /// When the relay list is next fetched from the onion site (unix seconds). 0 = due once Tor is
    /// up; older state files have none.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub directory_next_check: u64,
    /// Undelivered control frames (chat-delete `Closed` signals, §11.6, and screenshot notices),
    /// persisted so one isn't lost across a restart before the retry lands. `#[serde(default)]` for forward-compat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_control: Vec<PersistedPendingControl>,
    /// Short-code invites still being hosted (§5b), persisted so a core rebuild mid-pairing does
    /// not silently strand the joiner. `#[serde(default)]` for forward-compat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_invites: Vec<PersistedInvite>,
    /// Base64 of the 32-byte seed keying our mailbox polling fragments (`node::mailbox`), so the
    /// partition stays fixed for its epoch across restarts. `#[serde(default)]` for older files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_seed: Option<String>,
    /// Our access keys for PRIVATE relays (§3.2), so an authorization survives a restart: the
    /// operator authorized one specific public key, and with arti's keystore in memory a re-minted
    /// key would be a different one. `#[serde(default)]` for older files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relay_keys: Vec<PersistedRelayKey>,
}

/// One PRIVATE relay access key (see [`PersistedState::relay_keys`]). Sealed with everything else,
/// like a chat's `client_key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedRelayKey {
    /// The relay's `.onion`.
    pub relay: String,
    /// The `descriptor:x25519:…` string the operator authorized, handed back on a repeat request.
    pub public: String,
    /// Base64 of the 32-byte secret.
    pub secret: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// Encrypt `plaintext` with `key` (random nonce prepended). AEAD = ChaCha20-Poly1305.
pub fn seal(key: &StoreKey, plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| anyhow::anyhow!("seal failed"))?;
    // Sized up front: seal runs on the whole state blob after every change, so avoid a
    // realloc-and-copy of the ciphertext.
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a blob produced by [`seal`]. Fails on a wrong key or corruption.
///
/// The "decrypt failed" wording is a **contract with the app**: it is how the UI tells a genuine
/// wrong-password/damaged-file failure apart from an unrelated one (Tor, network, disk) and so
/// decides whether blaming the password is honest. See `app/lib/src/core/backup_errors.dart`.
pub fn open(key: &StoreKey, blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < NONCE_LEN {
        anyhow::bail!("blob too short");
    }
    let (nonce, ciphertext) = blob.split_at(NONCE_LEN);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| anyhow::anyhow!("decrypt failed: wrong key or corrupt store"))
}

/// Serialize + seal the state to a file (local-first storage).
///
/// **Atomic replace.** The device state file holds the *only* copy of the identity, so a
/// half-written file (crash / power-loss / OOM-kill mid-write) would strand the user in
/// onboarding with no way back. We therefore write to a sibling temp file, flush it durably to
/// disk (`fsync`), then `rename` it over the target — a POSIX-atomic swap, so a crash can only
/// ever leave the *previous* intact state or the *new* intact state, never a truncated one.
pub fn save_to_file(path: &str, key: &StoreKey, state: &PersistedState) -> Result<()> {
    let json = serde_json::to_vec(state)?;
    write_atomic(std::path::Path::new(path), &seal(key, &json)?)
}

/// Replace `path` with `bytes` so a crash leaves either the old file or the new one, never a torn
/// one: write a sibling temp file, `fsync` it, then `rename` it over the target. The `fsync` is
/// what makes the rename safe — without it the rename can reach the disk before the data, and a
/// power cut in between leaves an empty file under the real name. Used for every file whose loss
/// strands the user: the state file, the app-lock file and the sealed onion identity.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    // Sibling temp in the same directory, so the rename stays within one filesystem (a
    // cross-device rename is not atomic and would fall back to copy). `create` truncates any
    // stale temp left by a previously-crashed write.
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    {
        // Owner-only (0600 on unix): sealed, but nobody else's business either.
        let mut f = {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                o.mode(0o600);
            }
            o.open(&tmp)?
        };
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp); // don't leave the temp behind if the swap failed
        return Err(e.into());
    }
    Ok(())
}

/// Load + unseal the state from a file.
pub fn load_from_file(path: &str, key: &StoreKey) -> Result<PersistedState> {
    let blob = std::fs::read(path)?;
    Ok(serde_json::from_slice(&open(key, &blob)?)?)
}

/// Derive a [`StoreKey`] from a password + salt with Argon2id (for encrypted backups, §7).
pub fn derive_key(password: &str, salt: &[u8]) -> Result<StoreKey> {
    let mut key = [0u8; 32];
    argon2::Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("key derivation failed: {e}"))?;
    Ok(key)
}

/// Generate a random recovery password to show the user **once** (§7a). Grouped,
/// ambiguity-free characters; 100 bits of entropy (20 from 32). Never persisted by the app.
pub fn random_password() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no 0/O/1/I
    let mut rng = rand::thread_rng();
    let mut out = String::with_capacity(23); // 20 chars + 3 dashes
    for i in 0..20 {
        if i > 0 && i % 5 == 0 {
            out.push('-');
        }
        out.push(CHARS[rng.gen_range(0..CHARS.len())] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_round_trips() {
        let key = [9u8; 32];
        let blob = seal(&key, b"secret history").unwrap();
        assert_ne!(blob, b"secret history");
        assert_eq!(open(&key, &blob).unwrap(), b"secret history");
    }

    #[test]
    fn wrong_key_fails_to_open() {
        let blob = seal(&[1u8; 32], b"top secret").unwrap();
        assert!(open(&[2u8; 32], &blob).is_err());
    }

    #[test]
    fn save_to_file_is_atomic_round_trips_and_leaves_no_temp() {
        let key = [5u8; 32];
        let dir = std::env::temp_dir().join(format!("nightdrop-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.bin");
        let path = path.to_str().unwrap().to_string();
        let tmp = format!("{path}.tmp");

        let mk = |addr: &str| PersistedState {
            account_pickle: "pk".into(),
            address: addr.into(),
            chats: Vec::new(),
            media: Vec::new(),
            onion_keys: Vec::new(),
            onion_identity: None,
            my_relays: Vec::new(),
            relays: None,
            relays_draining: Vec::new(),
            held: Vec::new(),
            passes: Vec::new(),
            discovered_relays: Vec::new(),
            directory_version: 0,
            update_key: None,
            directory_key: None,
            directory_next_check: 0,
            pending_control: Vec::new(),
            pending_invites: Vec::new(),
            poll_seed: None,
            relay_keys: Vec::new(),
        };

        // A stale temp from a previously-crashed write must not break the next save.
        std::fs::write(&tmp, b"garbage from a crashed write").unwrap();

        save_to_file(&path, &key, &mk("first")).unwrap();
        assert_eq!(load_from_file(&path, &key).unwrap().address, "first");
        assert!(
            !std::path::Path::new(&tmp).exists(),
            "the temp file is renamed away, not left behind"
        );

        // Overwriting an existing file keeps it loadable (the atomic swap replaces cleanly).
        save_to_file(&path, &key, &mk("second")).unwrap();
        assert_eq!(load_from_file(&path, &key).unwrap().address, "second");
        assert!(!std::path::Path::new(&tmp).exists());

        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Make the app's data folder readable by this user only (0700), on Unix. Its files are sealed, but
/// a world-readable folder (Flutter creates it with the default umask: 0755) still showed other
/// accounts the state file's size, one file per contact under `client-auth/` (their public onion
/// keys: how many contacts, and linkable), and the media folder's counts and times (self-audit
/// 2026-10-17, round 10). A restrictive folder covers everything in it, including files created
/// before this ran; the Linux runner also sets umask 077 for what comes after. Best effort: a
/// failure leaves the folder as it was, which is no worse than before.
pub fn restrict_dir(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(all(test, unix))]
mod restrict_dir_tests {
    #[test]
    fn the_data_folder_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("nd-restrict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::restrict_dir(&dir);
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
