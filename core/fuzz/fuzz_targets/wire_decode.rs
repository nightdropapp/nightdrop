#![no_main]
//! Fuzz the wire frame decoder — the primary parser of attacker-controlled bytes. Every frame a
//! peer or relay hands us flows through `wire::decode` (length prefix + versioned JSON envelope +
//! padding). It must never panic, over-read, or hang on arbitrary input; a bug here is a remote DoS
//! at best.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // A malformed frame must return Err, not panic. We ignore the Ok/Err result.
    let _ = nightdrop::wire::decode(data);
});
