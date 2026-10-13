#![no_main]
//! Fuzz the update-manifest parser: the bytes our onion site serves at `/update.json`, read by every
//! install. It parses a JSON envelope, decodes base64, checks an Ed25519 signature and parses the
//! signed payload (`update::parse_manifest`). Arbitrary input must return Err, never panic or hang.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = nightdrop::update::parse_manifest(data);
});
