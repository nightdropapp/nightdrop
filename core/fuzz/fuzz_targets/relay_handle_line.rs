#![no_main]
//! Fuzz the relay's request-line handler — the relay's main untrusted-input surface. `handle_line`
//! parses one client request (JSON) and returns a response line; a malicious or buggy client can
//! send anything. It must never panic or hang (resource exhaustion of an open relay via malformed
//! input is explicitly in scope — see SECURITY.md).
use libfuzzer_sys::fuzz_target;
use nightdrop::relay_client::RelayCore;
use std::sync::OnceLock;

// Build the core once (its constructor spawns a reaper thread) and reuse it across iterations.
static CORE: OnceLock<RelayCore> = OnceLock::new();

fuzz_target!(|data: &[u8]| {
    // The relay protocol is line-based UTF-8 JSON; non-UTF-8 can't be a line, so skip it.
    if let Ok(line) = std::str::from_utf8(data) {
        let core = CORE.get_or_init(|| RelayCore::new(None));
        let _ = core.handle_line(line);
    }
});
