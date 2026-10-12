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

Reported upstream as arti issue #2805, "tor-hsservice: duplicate reupload timers make descriptor
republishing grow with uptime" (<https://gitlab.torproject.org/tpo/core/arti/-/issues/2805>,
submitted 2026-10-03 via anonticket, approved and public by 2026-10-09).

**What changed:**

- `publish/reactor.rs`: before scheduling a period's reupload, drop any earlier timer for that
  period (`reupload_timers.retain(...)`), so the heap holds at most one per period. The field's
  doc comment says so, replacing the TODO.
- `publish.rs`: test `reupload_rate_does_not_grow_with_extra_uploads` - six upload rounds, then
  24 h of mock time; expects 12-25 reupload rounds. Unpatched it fails with 51.

## Second fix: a change made during an upload was never published (2026-10-11)

**Why:** an upload's success marked every HSDir it reached **clean**, even when the descriptor had
been marked dirty again while that upload was in flight (arti's own `TODO (#1098)` above the line
describes the race). The newer descriptor was then never uploaded: the round that would have
carried it found nothing dirty. In Night Drop this lost **restricted discovery clients**. Two
`.auth` files written milliseconds apart (an invite's QR pass and its short-code pass, both issued
when the invite screen opens) gave two change events; the first started an upload, the second
dirtied it mid-flight, and the upload then rate-limited further publishing for a minute and marked
everything clean. The published descriptor never included the second key until a restart, so a
joiner holding that pass got "Decryption failure on onion service descriptor: incorrect decryption
key" on every fetch and its `Hello` could not land (S25 joining a Linux test device, 2026-10-11;
reproduced on demand by writing two valid keys 5 ms apart: 4 -> 5 clients published, not 6).
Probably also the cause of the two open device bugs in `TODO.txt` where a client key added while
the app ran was not honoured until a restart.

**What changed:**

- `publish/reactor.rs`: `TimePeriodContext::dirty_generation`, bumped by `mark_all_dirty` and kept
  across a rebuild of the context; an upload records it when it starts
  (`TimePeriodUploadResult::dirty_generation`), and its results mark an HSDir clean only if it has
  not moved since.
- `publish.rs`: test `a_change_during_an_upload_is_published_after_it`, with a thread-local switch
  that holds the mock HSDirs' responses so a change can be made mid-upload. Unpatched it uploads 16
  times, not 24: the third round never happens.

## Third fix: one failed upload to a new HSDir read as a whole ring missing (2026-10-11)

**Why:** an upload round goes only to the HSDirs whose descriptor is dirty, but its results
**replaced** the time period's whole result list. After a consensus change adds one HSDir to a
ring, the round uploads to that HSDir alone; if it fails, the list holds one failure and nothing
else, so `upload_result_state` reports `State::DegradedUnreachable` ("missing from one of the two
HsDir rings") although the ring's other HSDirs hold the current descriptor. The failed HSDir stays
dirty and is retried on the next trigger, so the reading can last long enough for the relay
watchdog to restart the relay (`relay/src/main.rs`, 900 s), and a restart rotates the introduction
points. Seen on the dev relay as the ring warning returning after hours of clean self-dials, with
the dirty-generation fix already in place. Not yet confirmed with publisher debug logs; the dev
relay now runs with `tor_hsservice=debug` to catch the next one. arti main (checked 2026-10-11)
still replaces the list in `set_upload_results`.

A second way to get a false failure, not changed here: a retry resends the same descriptor with
the same revision counter (`upload_descriptor_with_retries` reuses it), so when an attempt times
out after the HSDir already stored it, every retry is refused with HTTP 400 "Invalid HS
descriptor. Rejected." and the HSDir is recorded as failed while it holds the current descriptor
(seen on a test device, 2026-10-12: one timeout, then 12 refusals). With the merge it costs that
one HSDir's result, not the ring's.

**What changed:**

- `publish/reactor.rs`: `TimePeriodContext::merge_upload_results` replaces only the results of the
  HSDirs in the batch and keeps the others. A full re-upload (after `mark_all_dirty`) still covers
  every HSDir, so it replaces them all, as before.
- `publish/reactor.rs`: test `a_failed_upload_to_one_new_hsdir_keeps_the_ring_reachable`. With the
  old replace it fails (1 result kept instead of 3).

Testing this crate needs its dev-dependencies, which the Night Drop workspace does not resolve:
copy the directory elsewhere, append `[workspace]` to its `Cargo.toml`, and run
`cargo test --all-features --lib` there (52 pass, 2 ignored, on 2026-10-11).

**Remove this directory and the `[patch.crates-io]` entry** once arti ships all three fixes and Night
Drop moves to that arti.
