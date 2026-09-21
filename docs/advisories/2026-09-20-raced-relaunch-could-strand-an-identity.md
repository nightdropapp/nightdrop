# A raced relaunch could present a working install as a fresh one

**Date:** 2026-09-20
**Severity:** High for the person it happens to — the identity and every chat become
unreachable from inside the app. Nothing is destroyed on disk, but nothing in the UI says so.
**Affected:** Every build whose launch path can be re-entered while it is still running —
0.1.21 and earlier. Found on `bridges-0.2` while testing in-app bridges, but the defect is in
`start()`/`createIdentity()`, which are the same on `main`.
**Not affected:** Message content, the relay, and anything on the wire. This is a local
state-handling defect.

## What happened

Testing the in-app bridge editor on a phone, the "reconnect" action was tapped several times in
a row. Each tap tears the core down and rebuilds it; nine core lifecycles started inside twenty
seconds over the same state file and Tor state directory.

`start()` has no re-entrancy guard. One run cleared `_booting` in its `finally` while another was
still building, which left the app in a combination the UI had no case for:

```
identity == null    isBooting == false    loadError == false
```

`_Root` renders exactly that as `OnboardingScreen` — the fresh-install screen. The person is then
shown "create my identity" as the only thing on offer, on a device that already has one. Taking
it calls `createIdentity()`, whose first act is `_setAsideOldState()`.

The existing protection did not fire. `loadError` guards the case where a state file *exists and
will not open*, and it is careful: it preserves the bytes and offers a recovery screen. Here the
state file was fine and was never read — the launch that would have read it lost a race — so
nothing looked wrong to the code that decides whether onboarding is safe.

## Why the data survived anyway

Two earlier decisions, both made for different reasons, contained this:

* `_setAsideOldState()` **renames rather than deletes**, so the identity persisted as
  `nightdrop-state.bin.replaced-<ms>`.
* `_ensureStoreKey()` reads the existing key before generating one
  (`_secure.read(...) ?? randomStoreKey()`), so the new identity reused the same at-rest key and
  the set-aside blob stays decryptable.

The sealed onion key is the gap: `onion-key.sealed` is **overwritten, not set aside**. Restoring
the state file therefore returns the identity and chats under a *new* `.onion`, with every peer
still holding the old address — the same failure `ARCHITECTURE.md` §11 describes for a backup
that omits the onion keystore. Anything that sets the state aside should set the sealed onion key
aside with it.

## The fix

* `start()` is no longer re-entrant: concurrent calls coalesce onto one in-flight future, so two
  runs can never interleave over the same state file.
* `createIdentity()` refuses to displace a state file that is on disk unless the user was shown
  the recovery screen and chose to replace it. `dismissLoadError()` — the "set up new identity"
  button, and the only place that choice is actually made — is what grants it. A fresh install has
  no state file and is unaffected; onboarding reached by accident now fails loudly instead of
  quietly setting an identity aside.
* The check only refuses on a confirmed sighting of the file. If the path cannot be resolved it
  proceeds, because refusing to create an identity is its own way of locking someone out.

Covered by `rust_bridge_test.dart`, which now asserts both halves: refused without consent,
allowed with it.

## What to take from it

The lesson is not "add a mutex". It is that **`identity == null` was being used to mean "new
device"**, when it also means "we have not found out yet" and "the attempt to find out failed".
Onboarding is destructive, so it needs positive evidence that the device really is new — the
absence of a loaded identity is not that evidence. The same reasoning already appears one branch
away, where the code refuses to treat an unreadable state file as a fresh install.
