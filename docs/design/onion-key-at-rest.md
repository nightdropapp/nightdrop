# Design draft — Keeping the onion identity off disk

**Status:** 🟡 partly done. The **cleanup half is implemented** (2026-08-02): deleted chats and
logout now drop the peer client keys, and logout clears the Tor state on every platform. The
**ephemeral-keystore half is still design** — see §3.
**Relates to:** `docs/design/app-lock.md` §8 (which lists arti's state as out of scope — this
revisits that), `docs/multiple-identities.md` §1, and `ARCHITECTURE.md` §4.

## 1. What a cloned phone gets today

The app lock seals the message store. **arti's keystore is not sealed**, and it sits in the same
app-private directory:

```
arti-state/keystore/hss/nightdrop/ks_hs_id.ed25519_expanded_private
    -----BEGIN OPENSSH PRIVATE KEY-----          ← our onion identity, in the clear

arti-state/keystore/client/<56-char-v3-onion>/ks_hsc_desc_enc.x25519_private
    ×9 on this device                             ← one per contact, named by THEIR address
```

So imaging the device yields, with no crypto broken and regardless of any app lock:

* **the key that *is* our address** — enough to impersonate the identity, or to prove the device
  and the onion are the same party;
* **the onion address of every contact ever paired with** — a contact list, as addresses, in
  plaintext directory names.

The second is the worse leak and was found while investigating the first. `app-lock.md` §8
deliberately scoped arti's state out; that decision predates knowing what the directory actually
contains.

## 2. What is available

Both pieces are in the arti version already pinned, and `tor-hsservice`, `tor-keymgr` and
`arti-client` are already direct dependencies:

* **`ArtiKeystoreKind::Ephemeral`** (`tor-keymgr::config`) — an in-memory primary keystore,
  selected by config. Nothing it holds touches disk.
* **`TorClient::keymgr() -> &KeyMgr`** — public, with `KeyMgr::get` / `KeyMgr::insert`, and
  `HsIdKeypairSpecifier` from `tor-hsservice` to name the identity key.

Both the service key and the `client/<peer-onion>/` entries live under the **same keystore root**,
so one change removes both from disk.

## 3. Shape

The onion identity key moves into our own sealed store — the same blob the app lock already
protects — and is handed to arti in memory at startup.

1. **Config:** primary keystore kind = `Ephemeral`.
2. **First run:** no key saved. Let arti generate one as it does now, then read it back with
   `KeyMgr::get::<HsIdKeypair>(&HsIdKeypairSpecifier::new(nickname))` and persist it in the store.
3. **Later runs:** before launching the onion service, `KeyMgr::insert` the saved key so the
   service comes up on the **same address**.
4. Client (contact) keys follow the same path: they already live in the keystore, so they become
   in-memory too and are re-inserted from our store.

**No new user step.** The key is loaded from a store the app already opens at startup. When an app
lock is set, arti is not started until after unlock anyway (`start()` returns early and sets
`needsUnlock`), so the ordering already holds.

## 4. The failure that must not happen

If the insert is skipped or fails and the service launches anyway, arti generates a **new** identity
— a new address — and every existing contact is stranded with no way to reach us and no notice. That
is worse than the leak being fixed.

So:

* Launch is **gated** on the key being present: if a key exists in our store and cannot be inserted,
  fail the startup loudly rather than proceeding to generate a fresh one.
* A test asserts that a second startup with a populated store yields the **same** `HsId`, and that a
  simulated insert failure does **not** produce a new address.
* First-run generation is the *only* path that may mint an identity, and only when the store has no
  key at all.

**Amended 2026-08-02, after it bricked an app on a device.** "Fail loudly" was applied to *every*
start-up, including the creation of a new identity — and a new identity has a new store key, under
which any leftover file cannot unseal. A wipe that left the file behind therefore failed the one
path the user had left ("set up a new identity" on the load-error screen), so the app could not be
recovered from inside at all.

The rule is right but was scoped too widely. It applies **only when an identity is being restored**
— i.e. when the state file exists. When a new identity is being created the sealed key is not
consulted at all, and is overwritten once the fresh identity has a key of its own. Both halves are
pinned by `a_stale_onion_key_blocks_a_restore_but_never_a_new_identity`, confirmed to fail without
the fix. The general lesson: a fail-closed rule protecting an existing identity must not also guard
the path that exists precisely because there is no identity worth protecting.

**Amended again, on review of the same change.** Scoping the *sealed* key to restores left the
other source of an identity unscoped: arti's **on-disk keystore**. A new identity consulted no
sealed key, but `keystore_is_on_disk` still answered "yes" for any leftover `arti-state/keystore`,
so arti launched the new identity on the **old** `.onion` — reachable by everyone who knew the
address the user was walking away from, linkable to the identity they believed they had left, and
then sealed into the new store as if it were the new one. Two ways to reach it: an install from
before this design whose user picks "set up a new identity" before the migration has ever run (the
load-error screen is exactly where they land), and any wipe that fails to delete `arti-state`,
which has been observed on Android.

The rule is therefore symmetric, and lives in `drop_superseded_keystore`: the on-disk keystore may
speak for us **only while restoring an identity we have no sealed key for** — the migration run,
and the run after a backup restore. In every other case it is removed *before* bootstrap. Pinned by
`a_leftover_arti_keystore_is_never_inherited_by_a_new_identity`.

## 5. What it does and does not buy

**Does:** a cloned device no longer yields our onion secret key, our address, or our contacts'
addresses. Those become as protected as the message store — i.e. behind the app lock, with the same
honest caveat that a short PIN cannot resist an offline attack (`app-lock.md` §2).

**Does not:** make the app invisible. `guards.json`, `circuit_timeouts.json`,
`hss/<nickname>/ipts.json`, `iptpub.json` and the directory cache still hit disk, so a clone still
shows that Night Drop runs here and operates a hidden service. It stops the clone learning *which*
service and *whose* addresses.

**Also does not:** help while the app is running with the key resident in memory — this is at-rest
protection, not anti-forensics against a live device.

## 6. Costs

* **More experimental arti surface.** We already build with `experimental-api` + `keymgr` for
  restricted discovery; this adds `ephemeral-keystore`. arti is pinned, so an upgrade could break
  it — a real maintenance cost, and the reason to keep the integration small and well tested.
* **Regenerated per-run key material.** Blinded ids and descriptor signing keys are derived per time
  period and currently persisted; in memory they are regenerated at each launch. Expected to be
  cheap, but worth measuring against startup time.
* **A migration.** Existing installs have the key on disk. First run after the upgrade should import
  it into the store and then **delete the on-disk copy** — with the caveat that deletion on flash is
  not a guarantee, so the honest line is "new installs are clean; upgraded ones are best-effort".

## 7. Checked, and still open

**Checked:** `hss/<nickname>/iptpub.json` and `ipts.json` hold only opaque introduction-point ids
(`lid`, a hash) and expiry times — no onion address, ours or anyone's. Confirmed by reading them and
by grepping for any 56-character v3 address: none. So with the keystore in memory the address really
is off disk, and what remains is only "a hidden service runs here".

**Closed 2026-08-02:** `client-auth/` is now removed wholesale on logout, alongside `arti-state`.
It was only ever public client keys, but one file per contact is still a contact count, and it had
no reason to outlive the identity.

## 8. The cleanup half, as implemented

Independent of the ephemeral keystore, and worth having regardless — the keys had no removal path at
all:

* `Transport::forget_peer_key(peer_onion)`, implemented for Tor via
  `remove_service_discovery_key`. Deliberately keyed by the **address**, not the contact id, because
  that is how arti names the directory.
* `delete_chat` reads the peer address out *before* dropping the chat (it is the only place we hold
  it) and forgets the key alongside the existing `revoke_client` — the two directions of #22.
* `logout` does the same for every chat, so the duress wipe inherits it.
* The Dart wipe deletes `arti-state` on **every platform**. It was Android/iOS only, which meant a
  desktop logout kept the onion identity key it was supposedly deleting, plus one directory per peer
  named by their address.

Two tests, both confirmed to fail with the calls removed: a deleted chat forgets both directions,
and logout forgets *every* peer rather than the last one.
