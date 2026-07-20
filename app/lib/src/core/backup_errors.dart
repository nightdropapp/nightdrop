/// Classifying restore failures, so the UI only blames the password when that is really what
/// went wrong (§7).
///
/// Restore used to report *every* failure as "check the password and file" — including Tor's
/// "State already locked", which sent people hunting for a password mistake that never happened.
library;

/// The marker the Rust core puts on the one failure that genuinely is a wrong password or a
/// damaged file: `storage::open`'s "decrypt failed: wrong key or corrupt store". It appears in
/// the anyhow cause chain that crosses the bridge. Anything else — Tor, network, disk — is a
/// different problem and must not be described to the user as a password error.
///
/// Keep in sync with `core/src/storage/mod.rs`.
const _decryptFailureMarker = 'decrypt failed';

/// Whether [error] means the backup itself could not be decrypted (wrong password or bad file),
/// as opposed to a failure somewhere else in the restore.
bool isBackupDecryptFailure(Object error) =>
    error.toString().contains(_decryptFailureMarker);
