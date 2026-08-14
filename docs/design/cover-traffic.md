# Design draft — Cover traffic (#4)

**Status:** 🟢 implemented (2026-08-01), opt-in and off by default. Not yet exercised on a
device; the default 30-minute mean is an untested guess at the battery cost (§6).
**Relates to:** `ARCHITECTURE.md` §6 (relay, store-and-forward) and the fixed-size framing note in
§11; `website/limits.html`, which must not be allowed to overstate this.

## 1. What the relay can actually see

Every frame is already **padded to a fixed size bucket** before it reaches the transport, so length
analysis is dead. Tor hides who is posting. What remains is **timing and volume, per mailbox**:

* mailbox `mbx:…` received a post at 21:04, and two more by 21:06;
* mailbox `mbx:…` was drained at 07:12.

The handle is `SHA-256("nightdrop/mailbox/v1" ‖ identity_key)` — **stable for the life of an
identity**. So a relay operator (or anyone who seizes one) can accumulate a per-identity behavioural
profile without breaking any crypto: waking hours, timezone, weekday rhythm, who is busy when.

That is the signal this feature exists to muddy. Not content, not contacts — *activity*.

## 2. What it does not see, which bounds the value

**The relay never sees the hot path.** Onion-to-onion delivery doesn't touch it; the relay carries
mail only when the peer is offline, or when opt-in 24h server storage is on (§6). A pair of users
who are online together generate *no relay traffic at all*, and cover traffic buys them nothing
there.

**The relay never learns the sender.** Posts arrive over Tor. It sees "someone posted to mailbox X",
never "Alice posted to Bob". Cover traffic therefore protects the *recipient's* activity profile,
which is the only per-identity thing on offer.

Neither of these makes the feature pointless — a seized relay with months of mailbox timings is a
real threat — but they bound what may honestly be claimed.

## 3. Shape: self-addressed chaff

The simplest design that covers exactly the observable: **post dummy mail to your own mailbox** on a
randomised schedule, then drain it like any other mail.

* **No peer cooperation.** Your contacts need not run it, need not support it, and never see it.
* **Works with zero contacts**, which matters — a brand-new identity whose mailbox is silent until
  its first message is itself informative.
* **Indistinguishable from real mail** to the relay: same sealed envelope, same size bucket, same
  post/take pattern.

A cover post is a `Frame::Cover` sealed to ourselves with `relay_wrap`, appended at the end of the
`Frame` enum (variant order is wire-visible). On drain it is dropped before anything else looks at
it: no history entry, no ack, no notification.

**Randomised intervals, not a fixed period.** A fixed cadence is its own fingerprint and trivially
subtracted — an observer removes every post at 20-minute boundaries and reads what is left. Intervals
are drawn from an exponential distribution (memoryless, so the next post carries no information
about the last), around a configurable mean.

## 4. The honest limit, which the UI must carry

This is **chaff, not constant-rate transmission.** Real messages still produce posts *in addition*
to the cover, so aggregate volume still rises when you are genuinely active. A patient observer with
a long enough window can still see that.

Hiding that properly means constant-rate transmission: every slot carries either a real message or a
dummy, with real messages *delayed* into the next slot. That genuinely conceals volume, and it costs
latency on every message — a messenger where sends wait up to N minutes is a different product. Not
proposed here; noted so the next person doesn't mistake chaff for the strong thing.

So the setting must say it **degrades** correlation resistance rather than eliminating it, in those
words or better. A user who believes this makes them untrackable is worse off than one who knows it
raises the cost.

## 5. Costs, and why it is opt-in

* **Battery and bandwidth** on the device, continuously, for a threat that many users don't face.
* **Relay load**, borne by whoever runs one. Cover traffic that could be turned up without limit is
  an abuse vector against volunteers, so the mean interval is floored.
* It only runs while the app can run: foreground, or background with the delivery service on
  (`app-lock.md` §5). A phone with the app closed posts nothing, and that gap is itself visible —
  another reason not to oversell it.

## 6. Open questions

* **Should cover traffic also fire on the direct path?** It would blunt a peer-adjacent observer
  rather than the relay. Probably not worth it: that observer sees Tor traffic either way.
* **Mean interval default.** Long enough to be cheap, short enough to blur a normal conversation
  burst. 20–40 minutes was the starting guess. **First measurement, 2026-08-13/14 — one night per
  condition, not yet replicated:**

  | | cover **on** | cover **off** |
  | --- | --- | --- |
  | window | 7h 15m 58s | 7h 14m 43s |
  | Night Drop | 502 mAh | 349 mAh |
  | cover posts | 13 | 0 |

  Galaxy S25, same debug+diag build both nights, zero contacts so nothing but chaff was on the
  wire, `specialUse` foreground service up throughout in both.

  The raw delta is 153 mAh, and **taking that as the cost would be wrong**. Run A's app burned
  80.6 mAh of CPU while the screen was on against run B's 5.7 — ten more minutes of screen cannot
  cost 75 mAh, so that is someone using the app, not chaff. Subtracting it leaves **~78 mAh**, or
  about **6 mAh per post** — ~2% of a 3900 mAh battery over the night, ~6–7% per day, raising the
  app's background cost by roughly a quarter.

  6 mAh to post a padded blob is a lot until you notice what a post *is*: a fresh onion circuit to
  the relay — descriptor fetch, introduction, rendezvous — which dwarfs the bytes.

  So 30 minutes is defensible but not free, and the mean is the lever if it needs to be cheaper
  (60 minutes roughly halves it, at the cost of coarser cover). **n=1 per condition**: Tor circuit
  conditions vary between nights and two runs cannot bound that, so treat 6 mAh/post as ±50% and
  do not move the default on this alone. A second pair of runs is scheduled.

  Two things make the numbers trustworthy rather than assumed. The "0 posts" in the control is a
  measurement, not an absent recorder: the capture logged 14 liveness heartbeats across the night.
  And run B's `batterystats` line reads `bg:` rather than `fgs:` purely because the counters were
  reset while the app was already backgrounded, so no state transition was observed — the
  `ForegroundService:WakeLock` ran 6h 46m and the service was still up at the end.
