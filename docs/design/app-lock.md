# Design draft — App lock (passphrase- or PIN-derived store key)

**Status:** 🟢 **implemented and verified on hardware** (PIN *and* passphrase, user's choice —
decision of 2026-07-29; device round trip 2026-08-01). See §7 for what was exercised, §8 for what
is still open.
**Relates to:** `ARCHITECTURE.md` §7 (backups, which already use Argon2id), the invariant
"security-critical code lives in the Rust core," and the planned **duress wipe** (`#3`), whose
design is constrained by the choice made here (§4).

## 1. Problem

The 32-byte at-rest store key currently lives in the platform keystore
(`flutter_secure_storage`, key `nightdrop_store_key`). That protects it from other apps, but not
from anyone holding the **unlocked** device or an image of it: the app reads the key with no user
interaction, so the whole history opens. An app lock makes the key depend on something the user
knows, so possession of the device is not sufficient.

## 2. Threat model — and why we offer two strengths

The decision that shapes everything: **a short PIN cannot be made resistant to offline brute
force, at any key-derivation cost.** A 6-digit PIN is ~20 bits; an attacker with the lock file
tries the whole keyspace. Raising Argon2 cost multiplies a million guesses by a constant — it
does not change the outcome. `SECURITY.md` already states the matching invariant for backups
(never let a user choose the password; passwords there are randomly generated).

The only mechanism that genuinely protects a short secret is **hardware-throttled** verification
(Android Keystore with `setUserAuthenticationRequired`), where the TEE rate-limits and eventually
invalidates. But that path **cannot support a duress secret** (§4), because the OS validates the
input and would reject the duress code as simply wrong.

So both are offered, and the UI must be honest about which is which:

| Choice | Protects against | Does **not** protect against |
| --- | --- | --- |
| **PIN** (short, numeric) | Someone picking up the unlocked phone; the Recents thumbnail; a casual look | Someone who images the device and brute-forces offline |
| **Passphrase** (real entropy) | All of the above, **and** an imaged device | A keylogger, a coerced disclosure (that's what #3 is for) |

Decision (user, 2026-07-29): **ship both**, letting the user choose their level. Not a default
that quietly picks the weak one — the chooser states the tradeoff in one line each.

## 3. Core layer — done

`core/src/storage/lock.rs`. Wraps the existing store key under `Argon2id(secret, salt)` and
seals it with the existing `storage::seal`/`open`, so a wrong secret fails the AEAD tag instead
of yielding a plausible wrong key. Nothing about the sealed-state format changes; the lock is a
sidecar file `store-key.lock` next to the state blob, holding `{v, salt, m_cost_kib, t_cost,
p_cost, wrapped}` — no secret, and useless without the secret.

* **Cost: 64 MiB / t=3 / p=1**, ~25× the memory of `Argon2::default()` (which `SECURITY.md`
  restricts to *random* backup passwords). Persisted per lock so it can be raised later without
  stranding existing locks. This buys real time against a passphrase attacker; it is close to
  irrelevant against a PIN, per §2 — offered anyway because it costs one unlock's latency.
* Free functions, **not** methods on the core: the core is constructed *with* the key, so the
  unlock has to happen before there is a core. Exposed via `core/src/api.rs` →
  `store_is_locked`, `set_store_passphrase`, `unlock_store_key`, `clear_store_passphrase`;
  bridged into `app/lib/src/rust/api.dart`.
* `clear_*` returns the key so the caller can restore the keystore copy — otherwise removing
  the lock would make the store unopenable.
* Tests: round-trip, wrong secret, tampered blob, `clear` proves knowledge first (and a failed
  `clear` leaves the lock intact), empty secret refused. 5 tests, clippy clean.

**The invariant the Dart side must honour:** when a lock is set, the keystore copy of the key
must be **deleted**. Leaving it is the one mistake that makes this whole feature theatre.

## 4. Interaction with the duress wipe (#3)

#3 wants a second secret that wipes instead of unlocking. That requires *us* to verify the
secret, which rules out hardware-throttled unlock (§2) — so the duress feature and
brute-force-resistance-for-short-secrets are mutually exclusive. A passphrase gets both.

Planned shape, deliberately accommodated by the file format above: a second wrapped slot. On
input, try the normal slot; on failure try the duress slot; if *that* opens, wipe. Two Argon2
derivations per attempt, which is acceptable at one unlock per launch. Bump `v` to 2 when adding
it, and keep reading v1.

## 5. Lock lifetime

Follows the **existing** background-delivery toggle — no new setting:

| Background delivery | On lock |
| --- | --- |
| **Off** | Key zeroized. Nothing can read the store until the next unlock. |
| **On** | Key stays resident, so the foreground service can keep receiving (Signal's behaviour). |

`BackgroundDelivery.isEnabled()` (`app/lib/src/core/background_delivery.dart`) is the source of
truth. The tension is inherent: you cannot both receive messages while locked and hold no key.

## 6. Remaining work

1. ~~**Dart seam.**~~ ✅ Done. `isStoreLocked`, `storeUnlocked`, `unlockStore`, `enableStoreLock`,
   `disableStoreLock`, `lockStore` on the abstract `NightdropCore`, with defaults that leave every
   non-Tor implementation (mock, in-process demo) behaving exactly as before — never locked — so
   the mock needed no changes. Implemented in `rust_nightdrop_core.dart`.
2. ~~**Centralise the key read.**~~ ✅ Done, and there were **four** inline reads, not two:
   `start()`, `createIdentity()`, `importBackup()`, `importServerBackup()`. Now `_readStoreKey()`
   (may be null) and `_ensureStoreKey()` (creates on first use, and refuses to touch the keystore
   while a lock is set). Any *new* call site must use these — a fifth inline read is how a stale
   keystore copy would survive `enableStoreLock` and quietly void the feature.
3. ~~**Lock screen.**~~ ✅ `app/lib/src/features/lock/lock_screen.dart`, gated in `app.dart`
   **before** the `identity == null` check — a locked store looks exactly like a fresh install from
   outside, and onboarding would have offered to overwrite recoverable data. `start()` now stops
   early and sets `needsUnlock` rather than reading a null key and falling through. One text field
   takes either kind of secret (a numeric keypad would make a passphrase unenterable), generic
   failure text, and a 0.5→5s growing delay after each failure.
4. ~~**Settings flow.**~~ ✅ `app_lock_settings.dart`, reached from the home menu ("App lock").
   Presents the two strengths with §2's trade on each, asks twice (a typo would be unrecoverable),
   enforces 6 digits / 12 characters, and gates on an explicit no-recovery confirmation. Warns when
   background delivery is on that the key stays resident.
5. ~~**Re-lock on background.**~~ ✅ `didChangeAppLifecycleState` → `lockStore()`, which itself
   no-ops while background delivery is on.
6. ~~**Docs.**~~ ✅ `ARCHITECTURE.md` §7d and a `SECURITY.md` entry stating the PIN limit plainly.

## 7. Verified on hardware (2026-08-01)

Galaxy S25, Android 16, **release** build, full round trip:

1. Enabled a 6-digit PIN through the settings flow — the chooser states §2's trade on each option,
   asks twice, and gates on the no-recovery confirmation.
2. `am force-stop` + relaunch → **the lock screen, not onboarding**, which is the property that
   matters: a locked store looks like a fresh install from outside, and onboarding would have
   offered to overwrite recoverable data.
3. Unlocked. **141 ms** from the tap to the core being constructed (tap logged at `08:44:08.821`,
   `nd-diag: diagnostics enabled` at `08:44:08.962`) — that spans the Argon2id derivation at
   64 MiB / t=3 plus the FFI hop. Close to the desktop's ~0.13s, and far under the "under a second"
   bar, so `M_COST_KIB` needs no lowering for this class of hardware.
4. Chats intact and the **identity survived**: the safety number for the paired contact was
   byte-identical before and after (`86706 80781 51086 …`).
5. Disabled the lock, then force-stopped and relaunched again → opens straight to the chat list,
   proving `disableStoreLock` restored the keystore copy rather than orphaning the store.

**Not observable on device:** that `enableStoreLock` deletes the keystore copy (§3's "the one
mistake that makes this whole feature theatre"). A release build blocks `run-as`, so this stays
verified by reading `rust_nightdrop_core.dart` — `_secure.delete(key: _kStoreKeyName)` runs after
`setStorePassphrase`, which is also the right order: a crash between the two leaves a lock file
*and* a keystore copy (recoverable), never neither.

## 8. Not yet done

* **Wrong-secret behaviour was not exercised on device** — only in `cargo test`. The growing
  0.5→5s failure delay and the generic failure text are untested against a real keyboard.
* **The duress slot** (§4) — `#3`, not started.

## 9. Deliberately not doing

* **Hardware-throttled Keystore unlock** — kills #3 (§4). Revisit only if duress is dropped.
* **Locking the Tor state dir** or arti's own on-disk state: out of scope, and arti needs it
  while the foreground service runs.
* **Migration** from an existing unlocked install: the install base is empty, so the dangerous
  re-key path is simply absent. If that changes, this needs designing before shipping.
