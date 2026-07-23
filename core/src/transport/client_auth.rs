//! Authorized-client key directory for Tor onion-service **client authorization** (restricted
//! discovery, `docs/design/onion-client-auth.md`, TODO #22). This is the pure-Rust, arti-free half
//! of the feature: it manages the directory of authorized-client key files that arti's
//! `DirectoryKeyProvider` reads to decide **who may fetch our onion descriptor at all** — so only
//! paired contacts can even open a connection to us.
//!
//! ## File format (must match arti's `DirectoryKeyProvider`)
//! One file per authorized contact, named `<nickname>.auth`, whose contents are that contact's
//! descriptor-encryption public key as produced by arti's `HsClientDescEncKey::to_string()` on
//! *their* device — `descriptor:x25519:<base32>`. arti re-parses it authoritatively; here we only
//! shape-check before writing (the pure-Rust core deliberately carries no arti types, so this module
//! stays in the default build and is covered by the normal test suite).
//!
//! ## Why the key is opaque to us
//! Each contact generates the keypair on their side (arti `TorClient::generate_service_discovery_key`
//! for our onion) and sends us only the **public** string during pairing; we write it verbatim. We
//! never mint or hold their secret. Revoking a contact = deleting their file (arti, watching the
//! directory, stops encrypting the descriptor to them).
//!
//! ## Status
//! The directory management here is complete and tested. Pointing arti's watched `DirectoryKeyProvider`
//! at this directory, and the client-side `generate_service_discovery_key` exchange, live behind an
//! **experimental** arti-client API and need two live Tor devices to validate — deferred, see the
//! design doc. This module is the seam both sides build on.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::Result;

/// Extension arti's `DirectoryKeyProvider` requires on each key file.
const KEY_EXTENSION: &str = "auth";
/// Prefix of a v3 descriptor-encryption key string (`descriptor:x25519:<base32>`).
const KEY_PREFIX: &str = "descriptor:x25519:";

/// A stable, slug-safe client nickname for a contact. A contact id is a base64 identity key, which
/// is **not** a valid `HsClientNickname` (a Tor "slug": no `/ + . :`, case-/charset-limited), so we
/// hash it and hex-encode a prefix, led by a letter. Deterministic, collision-resistant enough for
/// a per-device contact set, and reversible only to "which file is this contact's".
pub fn client_nickname(contact_id: &str) -> String {
    let digest = Sha256::digest(contact_id.as_bytes());
    let mut s = String::with_capacity(1 + 16);
    s.push('c'); // lead with a letter so it's a valid slug start
    for b in &digest[..8] {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Shape-check a peer-supplied descriptor-encryption key string before writing it. We accept the
/// documented `descriptor:x25519:<base32>` form leniently (arti does the authoritative parse); this
/// only rejects obvious garbage so a malformed pairing payload can't plant a junk `.auth` file.
pub fn validate_key(key: &str) -> Result<()> {
    let body = key
        .trim()
        .strip_prefix(KEY_PREFIX)
        .ok_or_else(|| anyhow::anyhow!("client key must start with '{KEY_PREFIX}'"))?;
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'=') {
        anyhow::bail!("client key body is not base32");
    }
    Ok(())
}

/// The `.auth` file path for a contact within `dir`.
fn key_path(dir: &Path, contact_id: &str) -> PathBuf {
    dir.join(format!("{}.{KEY_EXTENSION}", client_nickname(contact_id)))
}

/// Authorize a paired contact to reach our onion: validate and write their descriptor-encryption
/// key to `<dir>/<nickname>.auth`. arti's watched `DirectoryKeyProvider` picks it up and starts
/// encrypting our descriptor to them. Idempotent (overwrites on re-pair / key rotation).
pub fn authorize(dir: &Path, contact_id: &str, key: &str) -> Result<()> {
    validate_key(key)?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(key_path(dir, contact_id), key.trim())?;
    Ok(())
}

/// Revoke a contact (chat deleted / logout): remove their `.auth` file so arti stops publishing our
/// descriptor to them. A missing file is success (idempotent).
pub fn revoke(dir: &Path, contact_id: &str) -> Result<()> {
    match std::fs::remove_file(key_path(dir, contact_id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Whether a contact currently holds an authorization file.
pub fn is_authorized(dir: &Path, contact_id: &str) -> bool {
    key_path(dir, contact_id).is_file()
}

/// How many clients are currently authorized (files ending in `.auth`). Used to guard against the
/// footgun of enabling restricted discovery with an empty set — which would make the onion
/// unreachable by everyone.
pub fn authorized_count(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x == KEY_EXTENSION)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "descriptor:x25519:N2NU7BSRL6YODZCYPN4CREB54TYLKGIE2FYORB2Q4FYQR7 CETXQ";
    // A realistic-looking (uppercase base32) key without the stray space:
    const KEY_OK: &str = "descriptor:x25519:N2NU7BSRL6YODZCYPN4CREB54TYLKGIE2FYORB2Q4FYQR7CETXQ";

    /// A fresh, unique temp directory for a test (no `tempfile` dev-dep in this crate); removed and
    /// recreated so a previous crashed run can't leak state in.
    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nightdrop-clientauth-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn nicknames_are_slug_safe_stable_and_distinct() {
        let a = client_nickname("AAAAcontactAAAA");
        let b = client_nickname("BBBBcontactBBBB");
        assert_eq!(a, client_nickname("AAAAcontactAAAA"), "deterministic");
        assert_ne!(a, b, "distinct contacts differ");
        // Slug-safe: starts with a letter, only lowercase hex after.
        assert!(a.starts_with('c'));
        assert!(a
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert!(!a.contains(['/', '+', '.', ':']));
    }

    #[test]
    fn validate_key_accepts_well_formed_and_rejects_junk() {
        assert!(validate_key(KEY_OK).is_ok());
        assert!(validate_key(&format!("  {KEY_OK}\n")).is_ok(), "trims");
        assert!(validate_key("descriptor:x25519:").is_err(), "empty body");
        assert!(validate_key("N2NU7BSRL6YOD").is_err(), "no prefix");
        assert!(validate_key("descriptor:x25519:has spaces!").is_err());
        assert!(
            validate_key(KEY).is_err(),
            "the stray-space variant is rejected"
        );
    }

    #[test]
    fn authorize_revoke_round_trip() {
        let d = scratch("roundtrip");
        let d = d.as_path();
        assert_eq!(authorized_count(d), 0);
        assert!(!is_authorized(d, "alice"));

        authorize(d, "alice", &format!("  {KEY_OK}  ")).unwrap();
        assert!(is_authorized(d, "alice"));
        assert_eq!(authorized_count(d), 1);
        // Stored verbatim (trimmed), so arti reads exactly the key the peer sent.
        let stored =
            std::fs::read_to_string(d.join(format!("{}.auth", client_nickname("alice")))).unwrap();
        assert_eq!(stored, KEY_OK);

        // Idempotent overwrite (re-pair / rotation) doesn't add a second file.
        authorize(d, "alice", KEY_OK).unwrap();
        assert_eq!(authorized_count(d), 1);

        // A second contact is a second file.
        authorize(d, "bob", KEY_OK).unwrap();
        assert_eq!(authorized_count(d), 2);

        revoke(d, "alice").unwrap();
        assert!(!is_authorized(d, "alice"));
        assert_eq!(authorized_count(d), 1);
        // Revoking a missing contact is fine.
        revoke(d, "alice").unwrap();
    }

    #[test]
    fn authorize_rejects_a_malformed_key_without_writing() {
        let d = scratch("malformed");
        assert!(authorize(&d, "mallory", "not-a-key").is_err());
        assert_eq!(authorized_count(&d), 0);
    }
}
