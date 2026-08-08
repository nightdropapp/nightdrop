# Design draft — Duress wipe (#3)

**Status:** 🟢 **implemented** (core, FFI, wipe, and UI — 2026-08-01). Not yet exercised on a
device, and testing it on one destroys that identity by design; see §9.
**Relates to:** `docs/design/app-lock.md` §4 (which constrained this design before it was written),
`ARCHITECTURE.md` §7d, and `Node::logout` — the non-coerced version of the same destruction.

## 1. What it is

A second secret at the lock screen. The normal one opens the store; the duress one **destroys it**
and lands the user in what looks exactly like a fresh install. Asked for as: *"a pin code to unlock
the app and a second one [to] wipe the account/logout/delete identity."*

The threat is coercion at close range — a border stop, a search, someone standing over you. The
existing `Log out / delete identity` is useless there: it sits behind a confirmation dialog, in a
menu, in an app you have already been made to unlock.

## 2. Why the app lock had to come first

`app-lock.md` §2 chose a design we verify **ourselves** (Argon2id over a user secret) rather than a
hardware-throttled Keystore unlock. That was not a free choice — it was made *for* this feature.
A Keystore-validated PIN is checked by the OS, which would reject a duress PIN as simply wrong, and
we would never see it. Duress and hardware throttling are mutually exclusive; this is the cost.

The consequence is inherited whole: **a short PIN stays offline-brute-forceable.** Duress protects
you from the person holding your phone, not from a forensic image already taken. Say so plainly.

## 3. The format — two slots, always

`store-key.lock` goes to **v2**, keeping v1 readable:

```
{ v: 2, m_cost_kib, t_cost, p_cost,
  normal: { salt, wrapped },
  duress: { salt, wrapped } }
```

Three decisions inside that, each load-bearing:

**Separate salt per slot.** One shared salt would let an offline attacker test both slots with a
single derivation. Two salts double their work per candidate — and we are already paying two
derivations ourselves (below), so the cost is symmetric.

**The duress slot is always present, even when no duress secret is set.** An absent-or-present
field would be a tell: anyone imaging the device could see that duress is armed, which is exactly
the knowledge that makes coercion effective ("unlock it, and don't use the other one"). When duress
is unarmed the slot holds a **decoy** — 32 random bytes sealed under a random key that was never
derived from anything, so it is indistinguishable from a real slot and can never open.

**The duress slot never wraps the real store key.** It wraps random bytes. Opening it is a signal,
not a key recovery; there is no path where presenting the duress secret yields readable data.

## 4. Constant work per attempt

Every unlock attempt derives **both** slots before deciding anything. The obvious implementation —
try normal, and only on failure try duress — makes a duress unlock take twice as long as a normal
one (~141 ms vs ~282 ms on the S25, measured in `app-lock.md` §7). That difference is invisible to
a human but trivial to instrument, and the observer we care about is standing next to you. Constant
work also hides whether the duress slot is armed at all.

Cost: a normal unlock goes from ~141 ms to ~282 ms. Still far under the "under a second" bar.

## 5. What the wipe does — and what it deliberately does not

Deletes, in this order: the lock file, the state blob, decrypted media, the arti state (the onion
identity), and the keystore copy of the store key. The lock file **must** go with the rest — leaving
it behind would show a lock screen for a store that no longer exists, which is both broken and
conspicuous.

**Each removal stands alone (2026-08-02, after a device test).** This was one `try` around the whole
list with a silent `catch`, and its *first* statement was the keystore delete — the one call that
throws on Android. When it threw, every deletion below it was skipped and nothing was recorded. The
identity still looked destroyed, because onboarding overwrites the state file; in fact the store key,
the sealed onion identity, arti's state and the authorized-client files all survived, and the next
identity came up **on the wiped identity's onion address**. Under coercion — the case this feature
exists for — that is the failure that matters most, and it was invisible.

So: every target is attempted independently, a failure is reported rather than swallowed, and the
store key is **overwritten with a fresh random key before it is deleted**, so that if the delete
still fails what remains unlocks nothing. A wipe that half-succeeds in silence is the one failure
mode this code must not have.

**The peers are not told, and this is structural — not a preference.** (Settled 2026-08-01 by a
device test, after two wrong turns. The first draft argued for silence on timing grounds. That
reasoning was then revised to "tell them", on the basis that `logout()` routes to onboarding before
any await, so a notice could fire behind an already-empty UI. Both missed the thing that actually
decides it.)

**There is nothing to send with.** The wipe runs at the *lock screen*, where the store has never
been opened — and the duress secret deliberately unwraps random filler rather than the store key
(§3), precisely so that presenting it can never yield readable data. No store key means no decrypted
state: no ratchet sessions, no identity key, no peer addresses. A `Closed` frame is an
*authenticated* control frame — a fixed marker encrypted on the chat's ratchet — so without the
store key the app cannot construct one, for anyone. `_core` is null and there is no transport.

Giving the duress slot the real store key would fix it and must not be done: an adversary who
compels both codes could then recover the key from a *copy* of the lock file and read the imaged
store. The whole point of the filler is that the wipe code is not a second way in.

So the accepted behaviour is: **peers learn from silence.** The UI says so at arming — "your
contacts are not told anything… agree on another way to reach each other before you need it" —
because a user who believes their contacts will be warned is being misled about their own
operational security.

`duress_logout()` is still wired up and still notifies *every* live chat rather than only un-backed
ones (no restore is coming). It is reachable only in the one case where a core happens to already be
running while locked — background delivery on, which keeps the key resident (`app-lock.md` §5). That
is a bonus, never a guarantee, and nothing in the UI claims it.

**If peers must be warned**, the only workable shape is a *farewell packet*: at arming time,
pre-seal one `Closed` per contact plus its relay targets, encrypted under the **duress** key, so the
wipe can post them without ever touching the store key. It is real work and carries its own
liabilities — each pre-made frame consumes a ratchet message index, the packet goes stale as the
ratchet advances, and a ready-to-send "chat deleted" sitting on disk is itself a small hazard.
Not started; it needs its own decision.

One further difference from `logout()`:

* **A server backup cannot be deleted, and that is a real residual risk.** The blob is addressed by
  `backup_handle(password)` — derived from the recovery password, which is shown once and
  deliberately never persisted (`ARCHITECTURE.md` §7). Nothing on the device can locate it, so the
  wipe cannot remove it. What remains is bounded: it is opaque, expires within its TTL (24 h
  default, 36 h max), and is useless without the password. The exposure is the user who wrote that
  password down being made to produce it inside the window.

  Closing it would mean persisting the relay's `delete_token` so the wipe can issue a delete first.
  That is a *pointer to the fact that a backup exists*, which is worth protecting — though it would
  live in the encrypted store being destroyed anyway. Deliberately deferred, not overlooked; it
  needs its own decision rather than being smuggled in here.

All of it is best-effort and hard-bounded — the wipe proceeds regardless of what reached the
network, because a wipe that can be prevented by taking the phone offline is not a wipe.

## 6. What the user is never shown, and the one moment they are told

**The app does not display whether duress is armed.** Showing it would mean persisting it, which
re-creates exactly the tell §3 removes. Settings offers two actions — *set or replace the duress
secret*, and *remove it* — with no state readout, and removing when nothing is armed is a
successful no-op that looks identical to removing a real one.

**The warning appears at arming time and nowhere else** (decision of 2026-08-01). This is not a
matter of not nagging: a persistent mention anywhere — a banner, a periodic reminder, a menu row
reading "Wipe code: on" — would reveal to whoever picks up the phone that duress is armed. The
arming flow is the only place the warning can live without becoming the tell. Everything after is
silence.

**Armed state is shown inside the wipe-code screen, and nowhere outside it** (revised 2026-08-01,
after the first build hid it entirely). The original rule — never display it — was wrong on both
sides of the trade:

* *It cost more than it looked.* With no readout, "remove the wipe code" had to be offered
  unconditionally, and a user could believe they had a wipe code when they didn't — discovering it
  while being coerced. A false belief in protection is worse than the leak it was avoiding.
* *It protected less than it looked.* The two tells are not equivalent. The **on-disk** one leaks to
  someone who images the device **without ever unlocking it** — that is the threat the decoy slot
  exists for, and it is untouched. A **UI** readout only reaches someone who has already compelled a
  normal unlock, at which point they have the messages anyway.

So the flag is stored **sealed under the store key** (§3): unreadable to an imaged device, readable
to the already-unlocked app. The menu row stays stateless — it says "Wipe code" whether or not one
is armed, so a glance at an unlocked phone gives nothing away; only a deliberate tap into the screen
shows where you stand.

The text has to be blunt, because the limit is real: **the wipe cannot be rehearsed.** Triggering it
to see what happens destroys everything, on purpose.

What the app *can* do, and does, is **verify at arming time that the secret just written really
opens the duress slot** — a correctness self-check, not a rehearsal. It is invisible, cannot be
re-run later, and cannot be used by someone holding the phone to discover whether duress exists. So
the honest wording is "we've confirmed this secret works; what you can't practise is the wipe
itself", rather than "you can't test this at all".

## 7. Rules on the secret itself

* Must not equal the normal secret (checked by trying it against the normal slot, not by string
  comparison — that also catches "same secret, different encoding").
* Same minimum lengths as the normal secret (6 digits / 12 characters).
* Setting or removing it requires the **normal** secret, so someone who has coerced one unlock
  can't quietly disarm duress afterwards.

## 8. Not possible: the fingerprint variant

Asked for as *"one fingerprint to unlock and another to wipe (right thumb versus left thumb)."*
Android's `BiometricPrompt` reports success or failure and nothing else — never which finger
matched. There is no API, on any Android version, that exposes finger identity, and this is
deliberate on Google's part. It cannot be built as specified.

## 8b. Verified on hardware, and the theory that turned out to be wrong (2026-08-08)

A wipe on a device once left the **store key** behind: the key survived, and nothing in the app
said so. The standing explanation was a `flutter_secure_storage` 10 migration — legacy
`EncryptedSharedPreferences` entries are migrated "on first access", and a delete against a
just-migrated entry failing once would mean every user upgrading from an older build had one wipe
that could half-fail. **That explanation is wrong**, on three independent grounds:

* `app/pubspec.lock` has pinned `flutter_secure_storage` 10.3.1, byte-identical `sha256`, since
  0.1.12 — well before the install that failed. No plugin upgrade ever happened.
* the ESP migration path (`FlutterSecureStorage.java`) only runs when
  `hasDataInEncryptedSharedPreferences()` is true, i.e. data written by plugin **9.2.4 or earlier**.
  Night Drop has never shipped one.
* the app passes a bare `const FlutterSecureStorage()`, and `encryptedSharedPreferences` defaults to
  `false` on both the Dart and Java sides, so nothing was ever written there to migrate.

The `__androidx_security_crypto_encrypted_prefs_*_keyset__` entries that *do* appear in the prefs
file are a side effect of the plugin initialising ESP merely to run that check — not legacy data.

**What remains plausible, and is still unproven:** the plugin's delete is
`editor.remove(key).apply()`, an *asynchronous* commit that returns as soon as the in-memory map is
updated. "The call returned" and "the entry is gone from disk" are different claims, which fits an
entry surviving with nothing thrown; process death during a wipe is the obvious way to lose the
flush. The wipe therefore now **reads the key back** and records a survivor as a failed step, so a
recurrence reports itself instead of being silent. Wait for that diagnostic rather than staging a
repro — the repro the old theory called for would prove nothing.

**The wipe itself is verified end to end** (Galaxy S25, Android 16, 2026-08-08), on a fresh identity
with no app lock so the key really was in the keystore rather than derived from a lock secret:
before, `shared_prefs/FlutterSecureStorage.xml` held `nightdrop_store_key`; after "Log out / delete
identity" it was gone, along with `nightdrop-state.bin`, `onion-key.sealed`, `arti-state/` (hss
onion keys included) and `nightdrop-media/`. No `wipe: could not remove` line appeared — and the
same log carried `diagnostics enabled` and six other diag lines through the wipe window, so that
silence is evidence rather than a dead channel.

`files/arti-cache/` survived that run and was **added to the wipe afterwards**. It is not a data
leak — arti's directory cache holds the public Tor consensus and microdescriptors, no key, no
address, no contact list — but it carries a modification time, and a wipe that leaves "Tor last ran
at 14:26" beside an app presenting itself as freshly onboarded is a wipe with an asterisk. Removing
it costs a fresh consensus fetch on the next launch.

## 9. Status

* ✅ Format, threat model, and behaviour decided (this document).
* ✅ **Core layer** — `core/src/storage/lock.rs`: v2 two-slot format with per-slot salts, the
  always-present decoy, constant-work `unlock` returning `Opened`, `set_duress` / `clear_duress`
  (both requiring the normal secret), `destroy` for the wipe, and v1 read + in-place upgrade.
  9 tests, including that the on-disk shape is identical armed or unarmed, that the duress secret
  yields no key and cannot `clear` the lock, that a v1 lock still opens, and that a rejected arming
  leaves the previous lock working. Arming self-checks the secret it wrote (§6) and rolls back if
  the check fails.
* ✅ **FFI surface.** `unlock_store_key` returns `StoreUnlock { duress, key_b64 }`; `set_duress_secret`,
  `clear_duress_secret`, and `destroy_store_lock` alongside it. Bindings regenerated and committed.
* ✅ **The wipe.** `_duressWipe` in `rust_nightdrop_core.dart`: destroys the lock file **first** (so
  an interrupted wipe comes up as a fresh install, not a lock screen over a dead store), then runs
  the ordinary teardown with `duress: true` — which routes to onboarding before any await, tells
  *every* live chat via `Node::duress_logout`, and is capped at 5 s so the wipe cannot be stalled or
  prevented by a peer that won't answer.
* ✅ **Settings UI.** Its **own** home-menu row ("Wipe code"), stateless in the label; inside, the
  armed state and the matching actions — *replace* and *remove* when armed, *set* when not, so
  "remove" is never offered with nothing to remove. Setting an app lock now **offers** a wipe code
  at the end of that flow, using the secret just chosen rather than asking again; declining is a
  plain choice. The normal secret is **verified before anything else is asked** — the first build
  validated it only at the end, walking the user through the whole flow before failing. The warning
  is shown at arming and nowhere else (§6).
* ✅ **`SECURITY.md`** — the inherited PIN limit, the no-rehearsal limit, the indistinguishability
  properties, and the server-backup residual risk.
* ⬜ **Not yet tested on hardware.** Verified by `cargo test` (129), `flutter test` (33, one new
  covering that a duress unlock is indistinguishable from a successful one at the lock screen),
  analyze and clippy. A device run needs care: arming and then entering the wipe code on the S25
  **will** destroy that identity and its chats.
