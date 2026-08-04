//! At-rest persistence (`ARCHITECTURE.md` §6 local-first, §8 device-theft). The whole
//! app state — identity, per-contact sessions, contacts, and **message history** — is
//! serialized and sealed under a 32-byte key.
//!
//! The key in production comes from the **OS keystore** (Keychain / Keystore / DPAPI /
//! libsecret); here it is injected as bytes so the logic is testable. Identity and session
//! key material additionally use vodozemac's own encrypted pickling under the same key;
//! the outer AEAD then also covers the message plaintext.

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};

use crate::Result;

/// 32-byte symmetric key, sourced from the OS keystore in production.
pub mod lock;

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
    /// Whether the **peer** signaled that *they* verified this chat's safety number — informational
    /// only (never sets our own `verified`). `#[serde(default)]` keeps older state files loadable.
    #[serde(default)]
    pub peer_verified: bool,
    /// The peer's advertised extra relay addresses (#17). `#[serde(default)]` for forward-compat.
    #[serde(default)]
    pub peer_relays: Vec<String>,
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

/// A file carried inside a backup, by path relative to a base dir (used for the Tor onion
/// keystore so a restored device reproduces the *same* `.onion` address and stays reachable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedFile {
    pub path: String,
    /// base64-encoded file bytes.
    pub data: String,
}

/// A chat-delete `Closed` signal (§11.6) that hasn't reached a relay yet, persisted so a delete
/// survives an app restart before the poller's retry lands (otherwise a delete during a relay/arti
/// outage, followed by a restart, would silently never notify the peer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedPendingControl {
    pub recipient_ik: String,
    pub peer_address: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<String>,
    /// base64-encoded sealed control-frame bytes (the whole [`PersistedState`] is sealed around it).
    pub bytes: String,
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
    /// The Tor onion keystore, bundled into a backup so restore reproduces the same `.onion`
    /// address (otherwise the peer's stored address goes stale and can't be reached). Empty in
    /// the at-rest state file — there the keystore lives in arti's own dir.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub onion_keys: Vec<PersistedFile>,
    /// Our advertised **extra** relay addresses (#17). `#[serde(default)]` for forward-compat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub my_relays: Vec<String>,
    /// Shared relays learned from the operator-signed relay directory (§3.1), and the version we
    /// accepted. `#[serde(default)]` for forward-compat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_relays: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub directory_version: u64,
    /// Undelivered chat-delete `Closed` signals (§11.6), persisted so a delete isn't lost across a
    /// restart before the retry lands. `#[serde(default)]` for forward-compat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_control: Vec<PersistedPendingControl>,
    /// Short-code invites still being hosted (§5b), persisted so a core rebuild mid-pairing does
    /// not silently strand the joiner. `#[serde(default)]` for forward-compat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_invites: Vec<PersistedInvite>,
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
    use std::io::Write as _;
    let json = serde_json::to_vec(state)?;
    let sealed = seal(key, &json)?;
    // Sibling temp in the same directory, so the rename stays within one filesystem (a
    // cross-device rename is not atomic and would fall back to copy). `create` truncates any
    // stale temp left by a previously-crashed write.
    let tmp = format!("{path}.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&sealed)?;
        f.sync_all()?; // fsync: the bytes are durably on disk before we swap the file in
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
/// ambiguity-free characters; ~95 bits of entropy. Never persisted by the app.
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
            my_relays: Vec::new(),
            discovered_relays: Vec::new(),
            directory_version: 0,
            pending_control: Vec::new(),
            pending_invites: Vec::new(),
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
