# Measuring background traffic and battery on Android

The conclusions are in `ARCHITECTURE.md` §11.8 ("What background delivery costs"). This file keeps
the method and the baseline from each run, so a later change can be measured the same way and
compared. The investigation (2026-09-30 to 2026-10-06) found arti's accumulating reupload timers
(fixed by `third_party/tor-hsservice`, in 0.1.27), kept the wake lock, and left the 2 s
circuit-build floor open.

## How to run one

- **Device:** Galaxy S25 (`SM_S931W`, battery 3,900 mAh) for Runs 2-7; Run 8 is the Wi-Fi-only
  Galaxy S23 (`SM_S911W`, 3,900 mAh). Numbers from different phones are not comparable. Before
  the reset, check that no other Night Drop install (the real app beside a test id) is running.
- **Build:** a release-signed diagnostics build, on a test app id unless the run is of the real
  install (see MAINTENANCE.md §7). Diagnostics write `nightdrop-diag.log` on the device.
- **Before the night:** charge, unplug, `adb shell dumpsys batterystats --reset`, note the time and
  how long the Tor client has been up. Uptime matters: the timer bug grew with it, so compare runs
  at similar uptime. Turn adb over Wi-Fi off, leave the phone untouched, screen off.
- **In the morning, collect:**
  - `dumpsys battery` (level, charge counter);
  - `dumpsys batterystats` (the app's attributed mAh, split into mobile radio, wake lock and CPU;
    screen-off and deep-doze percentages; whole-phone drain; other apps' share);
  - `dumpsys netstats` for the app's uid (the uid changes on reinstall: `dumpsys package <id> | grep
    userId`); the buckets are 2 hours and overlap the window edges, so traffic is approximate;
  - the diagnostic log: count "descriptor reupload timer elapsed", HSDir upload successes and
    failures, circuits launched, intro-point changes, circuits abandoned at the build timeout, and
    background mailbox checks, per hour since the Tor client started. Check the log's rate limiter
    dropped no lines in the window.
  - network changes (the log, or `dumpsys connectivity`): they drive republishing, and runs with
    4x more of them are not like for like.
- **Read it with care:** Android splits radio cost between the apps sending at the time, so Night
  Drop's attributed share moves with what else ran (Play Services, Instagram, Prime Video all did).
  Compare the app's own mAh/h, its radio and wake-lock lines, and its traffic, not whole-phone
  drain. One night is n=1.
- **What healthy looks like:** reupload timer firings of 2-4 an hour at most (two time periods,
  each every 1-2 h), flat over uptime; uploads that follow intro-point changes; background checks
  every 5 minutes with the wake lock held.

## Baselines

Runs 2-6 on the S25 on mobile data; screen off > 93% unless noted.

| Run | Night | Build | Night Drop | Radio / wake lock | Traffic up / down | Timer firings |
|---|---|---|---|---|---|---|
| 2 | 2026-09-30/10-01 | 0.1.25, no wake lock | 190 mAh (8.4 h) | 173 / – | ~3.6 / ~2.7 MB/h | 81 |
| 3 | 2026-10-01/02 | 0.1.26 | 58.5 mAh/h | 273 / 126 mAh | ~5.8 / ~2.4 MB/h | 30.2/h |
| 4 | 2026-10-02/03 | 0.1.26 + reupload fix | 34.4 mAh/h | 15.7 / 17.9 mAh/h | ~1.6 / ~1.4 MB/h | 0.3/h |
| 5 | 2026-10-03, daytime | Run 4 without wake lock | 19.8 mAh/h | 19.3 / – mAh/h | – | – |
| 6 | 2026-10-05/06 | 0.1.28 (arti 0.47) | 29.8 mAh/h | 12.5 / 17.1 mAh/h | ~0.8 / ~1.0 MB/h | – |
| 7 | 2026-10-10/11 | 0.2 pre-release, relay, S25 on **Wi-Fi** | 18.2 mAh/h | – / 17.5 mAh/h | ~1.7 / ~1.6 MB/h | 0.4/h |
| 8 | 2026-10-10/11 | 0.2 pre-release, direct only, **S23** on Wi-Fi | 20.6 mAh/h | 0.25 (Wi-Fi) / 19.4 mAh/h | ~1.4 / ~1.5 MB/h | 0.5/h |
| 9 | 2026-10-12, daytime | 0.2 (c8db5d4), S23 on Wi-Fi, no contacts, **cover on** | 20.0 mAh/h | 0.17 (Wi-Fi) / 19.5 mAh/h | ~0.9 / ~1.0 MB/h | – |
| 10 | 2026-10-12/13 | Run 9 with **cover off** | 20.7 mAh/h | 0.26 (Wi-Fi) / 19.5 mAh/h | ~2.7 / ~1.8 MB/h | – |

- Run 1 (2026-09-30) was a busy day, not a clean run: a process up ~24 h did 78-156 uploads an
  hour; after a restart, 0-32. That contrast is what pointed at uptime.
- Run 3 is the clearest picture of the bug: timer firings rose hour by hour from 1-3 after a start
  to 22-36 at 17-23 h of uptime, HSDir uploads to 200-270 an hour, and arti's 1-minute publish
  rate limit was hit 86 times.
- Run 5 checked mailboxes every 14-26 minutes instead of 5: the cost that decided to keep the wake
  lock.
- Runs 7 and 8 ran side by side on the same Wi-Fi, untouched from 23:37 to 06:58 (the counts below
  are for that window; the mAh/h for the whole night, ~10 and ~12 h). Run 7 holds nothing; Run 8
  holds one message for a contact that is off all night, and its retries are the point of it.
  - **The radio column is not comparable with Runs 2-6.** The S25's Android does not attribute
    Wi-Fi power to apps, so Run 7's 18.2 mAh/h lacks the radio share that LTE runs carry; the
    S23 (Android 14) does, and charged Night Drop 0.25 mAh/h for Wi-Fi. The wake lock line
    (17.5 and 19.4 mAh/h) is the comparable one and matches Run 6.
  - **Retrying a held message costs nothing measurable.** The S23 dialled the offline contact 3
    times an hour (every 20 minutes: the 15-minute background backoff on a 5-minute tick), each
    failing within the second; its CPU line was 0.9 mAh/h against the S25's 0.65, with the
    S23's whole night at 20.6 mAh/h against 18.2 on different hardware.
  - **Both onion services stayed Degraded all night** (never Running): one introduction point
    kept failing to establish, the intro points changed 8-10 times an hour (Runs 4/6: 5-6), and
    31-43 circuits an hour were abandoned at the build timeout. HSDir uploads failed 85% on the S25
    (the Run 4/6 rate) and 59% on the S23. Both phones, so the network or Tor, not one device.
  - Whole phone, screen off: S25 52 mAh/h, S23 75 mAh/h; on both the foreground service's wake
    lock was held the whole night.
- Runs 9 and 10 are the second cover-traffic pair (`design/cover-traffic.md` §6), 8 h each,
  screen off throughout, back to back on one phone. The wake lock line was identical (156 mAh in
  each), and the cover-off run cost *more* in total: its own onion service churned (14 state
  changes against 4, about 5x the circuit builds and 6x the descriptor-publish activity), which
  moved its CPU line from 0.39 to 0.98 mAh/h and its upload from ~0.9 to ~2.7 MB/h. Night-to-night
  Tor conditions outweigh 19 cover posts.
- Runs 4 and 6 still showed 84% of HSDir upload attempts failing and ~7% of circuits abandoned at
  the 2 s floor (median build 583 ms, p99 1,590 ms, none succeeding above 1,986 ms in Run 3).

## Ruled out

| Suspect | Verdict |
|---|---|
| Screen and app use | Happens with the phone untouched |
| Mobile data vs Wi-Fi | Wi-Fi was as heavy |
| Retrying an offline contact | No dials to any other onion all night (Runs 2-6); Run 8's three an hour for a held message cost no measurable battery |
| No wake lock freezing the app mid-operation | Run 2's failures were spread evenly, not clustered after its 82 freezes |
| Our own mailbox polling or cover traffic | 40 mailbox checks and 7 dummies a night, as designed |
