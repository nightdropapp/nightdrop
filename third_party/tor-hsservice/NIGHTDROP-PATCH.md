# Night Drop patches to tor-hsservice 0.48.0

The active fork is `tor-hsservice-0.48.0/`, based on the published Arti crate. The old 0.47.0
fork remains here for history; the workspace now uses the 0.48.0 fork.

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

**Upstream:** reported via anonticket 2026-10-12, public and assigned to an arti maintainer by
2026-10-14; the report links the patch and test (commit `80807d91`, branch
`arti-descriptor-dirty-race` on GitHub — keep that branch, the report points at it). On 2026-10-19
an arti maintainer replied that this is what `tor-dirpublish` is meant to handle, and that they may
migrate the onion-service publisher to it (checking its retry schedule first). So the upstream fix
may arrive as a different publisher rather than this patch: keep the patch until an arti release
fixes the race, and before dropping it read that release's publisher and run this test's scenario
against it (§5's rule).

## Third fix: one failed upload to a new HSDir read as a whole ring missing (2026-10-11)

**Why:** an upload round goes only to the HSDirs whose descriptor is dirty, but its results
**replaced** the time period's whole result list. After a consensus change adds one HSDir to a
ring, the round uploads to that HSDir alone; if it fails, the list holds one failure and nothing
else, so `upload_result_state` reports `State::DegradedUnreachable` ("missing from one of the two
HsDir rings") although the ring's other HSDirs hold the current descriptor. The failed HSDir stays
dirty and is retried on the next trigger, so the reading can last long enough for the relay
watchdog to restart the relay (`relay/src/main.rs`, 900 s), and a restart rotates the introduction
points. Seen on the dev relay as the ring warning returning after hours of clean self-dials, with
the dirty-generation fix already in place. Confirmed with publisher debug logs on the dev relay
(2026-10-12 UTC, period 20738): one HSDir kept timing out while the other seven took the
descriptor ("uploaded successfully to 7/8"), then a round went to that HSDir alone, "0/1", and the
watchdog reported the ring missing in the same second. The lone round can be a new HSDir after a
consensus change or, as here, the next try of one that keeps failing. arti main (checked 2026-10-11)
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

## Fourth fix: a consensus with no secondary ring read as unreachable (2026-11-02)

**Why:** a time period's ring needs that period's shared random value (SRV). When the directory
authorities fail to agree on one, the consensus carries only the current SRV, so `NetDir` has the
current ring (with the disaster SRV as its fallback) and no secondary ring. The publisher then has
one time-period context, and `upload_result_state` answered `DegradedUnreachable` for any fewer
than two, even with the descriptor on every HSDir of the current ring. That is the ring clients
fetch from (`hs_dirs_download` uses only `hsdir_rings.current`), so the service was reachable.

Seen live: the consensus lacked `shared-rand-previous-value` from about 2026-11-01 01:00 to 24:00
UTC and again from 2026-11-02 13:00 UTC (the consensus at 14:00 had only the current value). The
relay published to one period ("uploaded successfully to 8/8 HSDirs") and its own self-dial
reached it in 3-6 s every cycle, yet its watchdog (`relay/src/main.rs`, which treats
`DegradedUnreachable` as dark) restarted it every ~20 minutes: about 70 restarts, each rotating the
introduction points. On phones started during the gap, `published()` (`core/src/transport/tor.rs`)
stayed false, so the "Publishing your address to Tor" banner never cleared (S25, 2026-11-02).

**What changed:**

- `publish/reactor.rs`: in `upload_result_state`, when there are fewer than two contexts but the
  `NetDir` itself has only one ring, judge the current ring alone: nothing uploaded and nothing
  failed is `Bootstrapping`, any success is `DegradedReachable` (degraded, since the secondary
  period has no ring), all failed is `DegradedUnreachable`. A missing context while the `NetDir`
  has two rings is still `DegradedUnreachable`, as upstream.
- `publish/reactor.rs`: test `upload_result_status_without_a_secondary_ring`. `publish.rs`: the two
  publish tests (`reupload_after_publishing`, `publish_after_ipt_change_no_errors`) run on a test
  network with no SRV and asserted `DegradedUnreachable` after a clean publish; they now expect
  `DegradedReachable`. All three fail with the old logic.

## Keep rejected descriptors out of persistent state

Arti's publisher writes a descriptor rejected with HTTP 400 to the service state directory for
debugging. Night Drop removes that write: descriptors contain onion-service metadata and should
not be left as plaintext persistent files. Upload failure reporting remains in place.

The 0.48 fork carries this behavior together with the four publisher fixes above. See the source
changes under `tor-hsservice-0.48.0/src/`.

**Upstream:** not reported yet.

The 0.48 fork includes regression tests for the publisher changes. Testing it needs its
dev-dependencies, which the Night Drop workspace does not resolve: copy `tor-hsservice-0.48.0/`
elsewhere, append `[workspace]` to its `Cargo.toml`, and run `cargo test --all-features --lib`
there (54 pass, 2 ignored, on 2026-11-14). The four fixes were checked against the 0.47 fork's
diff the same day: all present, reworded.

**Remove the 0.48 fork and `[patch.crates-io]` entry** once a released arti version includes all
four publisher fixes and Night Drop's no-plaintext-descriptor behavior.
