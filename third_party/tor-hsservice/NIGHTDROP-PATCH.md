# Patched copy of tor-hsservice 0.47.0

Upstream: <https://crates.io/crates/tor-hsservice> (part of arti), MIT OR Apache-2.0
(`LICENSE-MIT`, `LICENSE-APACHE`, from the arti repository). Used through `[patch.crates-io]` in
the workspace `Cargo.toml`.

**Why:** the onion-service descriptor publisher reuploads more often the longer the Tor client
runs. `publish/reactor.rs` keeps reupload timers in a `BinaryHeap` and never removes an earlier
timer for the same time period, so every extra upload round (an IPT change, a network change, a
failed round) leaves a timer behind that reschedules itself. On a phone this grew to 200-270 HSDir
uploads an hour after about a day, most of Night Drop's background traffic. Arti's own comment on
the field described the problem as a TODO; still unfixed in 0.47.0, the version vendored here
(re-applied 2026-10-05 for 0.1.28; the patch applied with line offsets only).
Measurements: `docs/background-traffic.md`.

**What changed:**

- `publish/reactor.rs`: before scheduling a period's reupload, drop any earlier timer for that
  period (`reupload_timers.retain(...)`), so the heap holds at most one per period. The field's
  doc comment says so, replacing the TODO.
- `publish.rs`: test `reupload_rate_does_not_grow_with_extra_uploads` - six upload rounds, then
  24 h of mock time; expects 12-25 reupload rounds. Unpatched it fails with 51.

**Remove this directory and the `[patch.crates-io]` entry** once arti ships the fix and Night Drop
moves to that arti.
