#![no_main]
//! Fuzz the signed relay-directory parser. A relay serves this blob on `GetDirectory`; a malicious
//! relay can serve arbitrary bytes, and parsing happens BEFORE signature verification. Both the
//! wire parse and the verify path (base64 + Ed25519 decode) must reject junk without panicking.
use libfuzzer_sys::fuzz_target;
use nightdrop::directory::SignedDirectory;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Some(dir) = SignedDirectory::from_wire(s) {
            // Exercise the verify path too (a fixed non-zero key; it won't validate, but this walks
            // the base64/signature/payload decoding on attacker-shaped input).
            let _ = dir.verify(&[7u8; 32]);
        }
    }
});
