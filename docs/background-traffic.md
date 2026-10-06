# Background traffic and battery on Android: investigation

Status: **cause found and fixed, verified on a phone (Run 4), released in 0.1.27; 0.1.28's
dependency round measured lower again (Run 6).** Night Drop
uploaded far more in the background than its own work explains. The main cause was an arti bug: the
onion service republished its descriptor more and more often the longer the process ran (below).
Once the fix ships, the conclusion moves to `ARCHITECTURE.md` and this file is cut down to the
method.

Device for every run: Galaxy S25 (`SM_S931W`, battery 3,900 mAh). Data comes from
`dumpsys battery`, `dumpsys batterystats`, `dumpsys netstats` for the app's uid (10754), and the
diagnostic log (`nightdrop-diag.log`, diagnostics builds only). Both overnight runs were analysed
with the same scripts; the log's rate limiter dropped no lines in either window.

## Cause: arti's reupload timers accumulate

In `tor-hsservice` (0.43.0, which we ship, and still in 0.47.0, released 2026-10-01),
`publish/reactor.rs` keeps `reupload_timers` as a `BinaryHeap`. Every upload round pushes one
more timer, 60-120 minutes out, for its time period; duplicates are never removed, and each timer
that fires triggers a full reupload, which pushes another timer. So every *extra* upload round -
an introduction-point change, a network change, a failed attempt (a "0/1 HSDirs" round pushes a
timer too) - tends to become a repeating one. Timers only merge when one fires while an upload
for its period is already pending (`mark_dirty` returns false and the timer is dropped), so the
count can also fall, but extra rounds add timers faster than coincidences remove them. Arti's own comment on the field says so: "if, for
some reason, we upload the descriptor multiple times for the same TP, we will end up with multiple
ReuploadTimer entries for that TP, each of which will (eventually) result in a reupload."

The diagnostic log shows the consequence. Timer firings per hour since the Tor client started
(restarts at 2026-09-29 21:37, 2026-09-30 16:32 and 2026-10-01 11:22 UTC):

- After the 2026-09-29 start: **0-4 per hour** for the first 14 hours (a quiet night), then
  rising through a busy day of network changes and tests.
- After the 2026-10-01 11:22 start, hour by hour: 3, 1, 2, 9, 6, 11, 10, 13, 14, 13, 16, 19, 18,
  13, 12, 14, 17, then **32, 22, 28, 28, 29, 36, 36** through Run 3's night (17-23 h of uptime).
- Not strictly monotonic: during Run 2's night (10-18 h after its start) it held at 5-12 per hour,
  then 23 in its last hour.

HSDir uploads follow them: 0-30 per hour in quiet hours after a start, **200-270 per hour** by the
morning of 2026-10-02.
Each firing republishes to every HSDir for that period (8), and arti's 1-minute publish rate
limit was hit 86 times in Run 3's night. An example from 06:04 UTC: arti logs "reuploading
descriptor in 1h 9m", then "descriptor reupload timer elapsed" at 06:04:29, 06:05:00 and
06:08:51 - older timers firing back to back.

This explains Run 1's "process up ~24 h uploads far more than a freshly restarted one". A restart
clears the heap, which is why the churn seemed to come and go.

## Contributing: the 2 s circuit-build floor

Arti's learned circuit-build timeout sat at the **2,000 ms floor** that 0.1.25 introduced, for
the whole of Run 3 (42 readings, from 1,000 recorded builds). On mobile data at night, **241 of
3,424 circuits (7%) were abandoned at 2.00-2.13 s**. No circuit succeeded above 1,986 ms
(median 583 ms, p90 1,002 ms, p99 1,590 ms), so some of the abandoned ones would likely have
completed given more time. Abandoned circuits fail uploads, failed rounds add reupload timers,
so the floor feeds the accumulation above. It is not the main cause: Run 3 had half its uploads
succeed and still uploaded at 200+ per hour.

## Run 4: the fix, overnight 2026-10-03 01:21 to 08:43

Build: branch `hsfix-diag` (0.1.26 / 4122, wake lock as released, diagnostics + timing logs) with
the patched `third_party/tor-hsservice` (dedups reupload timers), installed 2026-10-02 07:26 - so
18-25 h of Tor-client uptime during the night, comparable to Run 3's 17-23 h. Same conditions;
screen off 99.5%, 82.6% deep doze, but **23 network changes** (Run 3: 6).

| | Run 3 (no fix) | Run 4 (fix) |
|---|---|---|
| Reupload timer firings | 30.2/h | **0.3/h** |
| HSDir uploads ok / failed | 217 / 220 per h | 21 / 112 per h |
| Circuits launched | 491/h | 170/h |
| Intro-point changes | 4.7/h | 5.7/h |
| Night Drop traffic | ~5.8 MB/h up, ~2.4 down | ~1.6 MB/h up, ~1.4 down |
| Night Drop (batterystats) | 411 mAh, 58.5 mAh/h | 253 mAh, 34.4 mAh/h |
| ...mobile radio / wake lock / CPU | 273 / 126 / 12 mAh | 116 / 132 / 5 mAh |
| Whole phone | 74.9 mAh/h | 81.8 mAh/h |

- **The fix does what it should.** Timer firings no longer grow with uptime; uploads now follow
  intro-point changes (each republishes, which is the design) rather than accumulated timers.
  Night Drop's radio cost fell 58%, its upload traffic 72%, its total 41%.
- **The whole phone did not drain less** that night: Instagram (64 mAh) and Prime Video (36 mAh)
  were active in the background, nearly all radio, and were not in Run 3; network changes were
  four times as frequent. Whole-phone drain is not a measure of Night Drop alone.
- **The wake lock is now Night Drop's largest cost** (132 of 253 mAh), roughly constant across
  runs. That is the remaining case for the no-wake-lock experiment (see the TODO).
- **Still open:** 84% of HSDir upload attempts fail (mostly "could not build circuit to HsDir");
  6.9% of circuits were abandoned at the 2 s build-timeout floor, as in Run 3. Fewer failed rounds
  no longer multiply, but they still cost circuits. Intro-point churn (~5-6/h) is the other driver
  of republishing.

## Run 5: no wake lock, daytime 2026-10-03 12:45 to 15:23 - the wake lock stays

Build: branch `nowake-diag` (main with the reupload fix + timing logs + EXPERIMENT `2cb172d`: the
background service holds no CPU or Wi-Fi lock), installed 08:56; verified before the run that
`dumpsys power` showed no wake lock for the app. Mobile data, screen off 98.3%, but daytime:
59% deep doze (night runs: 83-85%), Bluetooth on, 10 network changes - so the whole-phone figure
(233 mAh, 88 mAh/h) is not comparable with the overnight runs.

- **Night Drop: 52 mAh in 2 h 38 min = 19.8 mAh/h**, against Run 4's 34.4 mAh/h. The difference
  is the wake lock's own cost (Run 4: 132 mAh, ~18 mAh/h); radio 19.3 vs 15.8 mAh/h, CPU 0.5 vs
  0.7 mAh/h. The CPU was awake 28% of the time instead of all of it.
- **But background mailbox checks slowed from every 5 min to every 14-26 min** (12:56, 13:11,
  13:25, 13:47, 14:11, 14:35, 14:54, 15:20): without the lock the timer only runs in Android's
  maintenance windows. Offline mail to an idle phone arrives that much later.

**Decision (Shawn, 2026-10-03): keep the wake lock.** Dropping it saves about 18 mAh an hour and
costs 3-5x slower message checks; that is not worth it. The EXPERIMENT commit is not merged.

## Run 6: the 0.1.28 branch, overnight 2026-10-05/06 - lower again

Build: branch `0.1.28` at `6a4c2f3` (arti 0.47 with both patches, vodozemac 0.11, SQLite 3.53,
Flutter 3.47.6, the relay list fetched daily from the onion site instead of asked of the relay on
every poll), release-signed, diagnostics on, wake lock as released. Installed 2026-10-05 22:05 UTC
over 0.1.27; same process all night. Window: `batterystats` since unplug, 9 h 46 min, mobile data,
screen off 93.0% (41 min on, mostly the morning), **40 network changes** (Run 4: 23).

| | Run 4 (0.1.26 + fix) | Run 6 (0.1.28 branch) |
|---|---|---|
| Night Drop (batterystats) | 253 mAh, 34.4 mAh/h | **291 mAh, 29.8 mAh/h** |
| ...mobile radio | 116 mAh, ~15.7 mAh/h | **122 mAh, 12.5 mAh/h** |
| ...wake lock | 132 mAh, ~17.9 mAh/h | 167 mAh, 17.1 mAh/h |
| Night Drop mobile traffic | ~1.6 MB/h up, ~1.4 down | **~0.8 MB/h up, ~1.0 down** (7.66 / 9.44 MB) |
| Whole phone, unplugged screen-off stretch | 81.8 mAh/h | ~64 mAh/h (charge counter, 5 h 40 min) |

- About 13% less battery and half the upload, with more network changes than Run 4. The radio is
  where it went; the wake lock costs the same, as expected.
- The relay list was fetched once (22:07 UTC) and not again overnight, and the update check ran
  once; up to 0.1.27 the relay was asked for the list on every poll.
- Not separated: arti 0.43 -> 0.47 and the relay-list change landed together, so this run cannot
  say which saved what. One night, n=1, like the others.

## The runs

| | Run 2 | Run 3 |
|---|---|---|
| Night (EDT) | 2026-09-30 22:34 to 10-01 07:00 | 2026-10-02 00:04 to 07:05 |
| Build | `receipt-test`, 0.1.25 / 4112, **no wake lock** | `tor-timing-diag`, 0.1.26 / 4122, **wake lock (as released)** |
| Tor client up at start of night | ~10 h | ~17 h |
| Screen off / deep doze | 99.5% / 85% | 99.6% / 85% |
| **Whole phone** | 662 mAh, 78.6 mAh/h | 526 mAh, 74.9 mAh/h |
| Battery level | 86 to 73% (1.8 %/h) | 89 to 77% (1.8 %/h) |
| Night Drop (batterystats) | 190 mAh (radio 173, CPU 17) | 411 mAh (radio 273, wake lock 126, CPU 12) |
| Google Play Services | 186 mAh (two profiles) | 14 mAh |
| Night Drop traffic | ~3.6 MB/h up, ~2.7 down | ~5.8 MB/h up, ~2.4 down |
| HSDir uploads ok / failed | 584 / 4,176 | 1,525 / 1,544 |
| Reupload timer firings | 81 | 212 |
| Circuits launched | 5,204 | 3,445 |
| Intro-point changes | 35 | 33 |

Notes on reading it:

- **The whole phone drained at the same rate both nights (1.8 %/h).** Night Drop's attributed
  share doubled because Android splits radio cost between the apps sending at the time: in Run 2
  Play Services shared it, in Run 3 Night Drop was almost alone. The total radio cost barely
  moved (467 vs 439 mAh). The wake lock is charged 126 mAh in Run 3, but total CPU plus wake lock
  is the same per hour both nights (29 vs 28 mAh/h), so its marginal cost does not show.
- Traffic figures come from 2-hour netstats buckets that overlap the window edges, so they are
  approximate.
- Run 2's uploads failed 7 times in 8; Run 3's about 1 in 2. Whether the wake lock explains the
  difference is not established: Run 2's failures were not clustered after the app's freezes.
- One run each. Other apps' activity differed (Play Services above), and the Tor client's uptime
  differed - which, given the cause, matters.

Corrections: an earlier version of this file gave Run 2 as 622 mAh (31% Night Drop) and said the
phone never reached deep doze. The batterystats dump says 662 mAh (29%) and 85% deep doze.

## Run 1: busy day and a restart, logs pulled 2026-09-30

Not a clean run (screen on 78% of the day, adb and Wi-Fi toggled for tests):

- Upload tracks **onion-service churn**: descriptor re-uploads to 8 HSDirs, plus the circuits
  those build.
- A process that had been up ~24 h did **78-156 uploads/h** and **9-24 MB up per 2 h**.
- After a restart it idled at **0-32 uploads/h** and **~1.5 MB per 2 h** all night.
- Screen-off cost that day: about **0.16 %/h**.

## Ruled out

| Suspect | Verdict |
|---|---|
| Screen and app use | Ruled out: it happens with the phone untouched |
| Mobile data vs Wi-Fi | Ruled out: Wi-Fi was as heavy the evening before Run 2 |
| Retrying an offline contact | Ruled out: no dials to any other onion all night |
| No wake lock freezing the app mid-operation | Not supported: Run 2's failures are spread evenly, not clustered after its 82 freezes |
| Our own mailbox polling or cover traffic | Ruled out: 40 mailbox checks and 7 dummies a night, as designed |

## Fix options (not chosen yet)

1. **Deduplicate the timers in arti**: keep one reupload time per time period (arti's own TODO
   suggests a `HashMap<TimePeriod, Instant>`). Ship it as a patched crate under `third_party/`,
   like `saturating-time`, and report or contribute it upstream. This removes the growth.
2. **Restart the onion service periodically.** Clears the heap, but rotates introduction points
   (see CLAUDE.md: a restart is never free) - a workaround, not a fix.
3. **Revisit the 2 s floor** for mobile networks, which cuts the failed rounds that seed extra
   timers. Secondary; worth doing with 1, not instead of it.

To verify a fix: an overnight run on a Tor client that has been up a day or more should show
timer firings at roughly 2-4 per hour (two time periods, each every 1-2 h) instead of growing.
