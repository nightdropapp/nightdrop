# Design draft — Keeping the onion identity off disk

**Status:** 🟡 design, not started.
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

**Still open:** the restricted-discovery directory (#22) is **not** part of the keystore and is not
covered by this change. `client-auth/<id>.auth` is one file per authorized contact, written by us
into the Tor state dir. Those hold *public* client keys, so the material is not secret — but the
**count of contacts** still leaks, and the filenames are per-contact ids. Worth a follow-up decision
once this lands; it is a smaller leak than plaintext addresses but the same class.
