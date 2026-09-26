# Reopening the app soon after closing it could show "Couldn't open your saved session"

**Date:** 2026-09-26
**Severity:** Low. Alarming screen, no data at risk — the saved state was never modified.
**Affected:** Android, up to and including 0.1.22. Fixed in 0.1.23.
**Not affected:** Desktop (closing the window ends the process).

## What happened

On Android, closing the app (swiping it away, Back from the home screen) destroys the screen and
its Flutter engine, but the **process usually lives on**. Reopening starts a fresh engine in that
same process, and it builds a fresh Rust core over the same Tor state directory.

The old core was not gone yet. Dropping it only *asked* its background poller to stop, and the
poller finishes the tick it is in first — which can be a Tor dial to an unreachable contact,
taken **while holding the core lock**. Until that dial timed out, the old core kept arti's state
lock, and the new launch failed to open the saved state. The app reads that failure as an
unreadable state file, so it showed the recovery screen, and **Try again** failed the same way
until the old dial finally gave up. That is why it seemed to recover by itself.

Reproduced on a Galaxy S25 on 2026-09-26 by closing and reopening within a few seconds of launch:
0.1.22's code showed the recovery screen on the first attempt.

## Was anything lost

No. The failed launch never writes the state file. Each failure did save one extra copy of it
aside (`*.unreadable-<time>` in the app's private storage), because the error was mistaken for a
corrupt file. Those copies are encrypted like the original and harmless.

## The fix (0.1.23)

- A new launch first shuts down any core an earlier screen left running in the process, and
  waits for it (`retire_previous_tor_core` in `core/src/api.rs`).
- It can do that even while the old poller holds the core lock: the Tor transport now exposes an
  abort that needs no lock, and peer dials race it the way relay requests already did, so a
  stuck dial ends at once instead of at its timeout.
- A held state lock is classed as a transient failure, so it no longer saves a copy aside.

Verified on the same phone with the same close-and-reopen sequence: 6 of 6 relaunches opened
normally, against a failure on the first attempt before the fix.

## If you saw it

Nothing to do. Updating to 0.1.23 stops it; until then, waiting a minute and tapping **Try
again** — or force-stopping the app from system settings — gets you back in. Do **not** choose
"set up a new identity" from that screen for this.
