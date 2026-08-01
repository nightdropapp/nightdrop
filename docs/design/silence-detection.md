# Design draft — "No sign of them" (peer-side silence detection)

**Status:** 🟡 core done, UI in progress.
**Relates to:** `docs/design/duress-wipe.md` §5, which is why this exists.

## 1. Why this exists

A duress wipe **cannot tell the peers**. The wipe runs at the lock screen with no store key — the
duress slot unwraps filler by design — so there is no ratchet state to authenticate a `Closed` frame
with, for anyone. That is structural, not a missing feature (`duress-wipe.md` §5).

The peer's real need is narrow: *stop sending into a void, and re-establish out of band.* That need
can be met from the **receiving** side, with no protocol change and nothing sent at wipe time.

## 2. What it reports — and what it must never claim

**Silence, not a cause.** A wiped identity, a seized phone, a lost phone, a dead battery and a
fortnight's holiday are indistinguishable from here, and the UI must not imply otherwise. Wording is
therefore observational — "no sign of them since…", never "they deleted their account".

That ambiguity is a **feature**, not a limitation to be engineered away. A signal that meant "they
wiped" would be a record on the peer's device that an anti-forensics feature was used — exactly what
`duress-wipe.md` §5 refuses to broadcast. Silence detection is safe *because* it is vague.

It is also strictly more useful than a wipe notice would have been: it fires for every way a person
can vanish, not just the one the app can see.

## 3. The signal

`Chat::last_seen` — unix seconds of the last **authenticated** contact from the peer:

* a `Message` that decrypted on their ratchet;
* any control frame that passed `verify_control` — including the **silent delivery `Ack`**, which is
  the important one. A peer who reads without replying is alive, and a signal that only counted
  typed messages would report them as gone.

Stamped in exactly two places (`crypto::decrypt` success in `Frame::Message`, and `verify_control`
success), so no authenticated frame is missed and **nothing unauthenticated ever counts**. A forged
frame refreshing `last_seen` would let an adversary make a seized phone look alive; a test covers
that case specifically.

Pairing sets the initial value, so a brand-new chat is never reported as silent. Persisted as
`last_seen_unix` with `#[serde(default)]`; a state file from before this feature reads as *unknown*,
never as *silent*.

## 4. Threshold

**14 days**, and only where the user is already looking (the chat), never as a push notification —
"your friend may have been raided" arriving on a lock screen is its own hazard.

14 is a judgement call, chosen to sit clear of ordinary life: a long weekend offline, a holiday, a
flat phone for a few days. Short enough to matter within the relay's 24h store-and-forward horizon
being long past, long enough that it isn't crying wolf. Unknown (`0`) shows nothing.

## 5. Deliberately not doing

* **No notification.** See above.
* **No "last seen" timestamp on the chat list.** That is a presence feature, and presence is exactly
  what this app declines to build — it would leak activity patterns to the peer continuously, to
  spare them one notice a fortnight.
* **No cause attribution.** §2.
