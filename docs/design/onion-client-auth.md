# Design — Onion-service client authorization (restricted discovery)

**Status:** **done — live-validated over Tor.** The arti-free **authorized-client key-directory
manager** (`core/src/transport/client_auth.rs`), the **arti wiring** (`core/src/transport/tor.rs` — a
watched `DirectoryKeyProvider` + client-side key generation, behind `--features tor`), the **transport
seam** (`Transport::make_client_key`/`authorize_client`/`revoke_client`), and the **pairing key
exchange** (`wire::Frame::ClientKey`, `node::announce_client_key`, authorize-on-learn /
revoke-on-delete) all landed. Covered offline by
`node::tests::pairing_exchanges_onion_client_keys_and_delete_revokes_them`, and **end to end over real
Tor** by the `#[ignore]`d `tor_smoke::restricted_onion_admits_authorized_client_and_refuses_unauthorized`
(2026-07-09, ~20 min): an authorized client is admitted to a restricted onion and delivers a frame; an
unauthorized one is refused — exercising the **experimental** client-side `generate_service_discovery_key`
path (§3) for real. See §9 for exactly what shipped. Grounded in the **arti 0.43** API present in the
tree (`tor-hsservice` / `arti-client` / `tor-hsclient` 0.43.0). Relates to `ARCHITECTURE.md` §5–§6 and
the anonymity/injection review (TODO #22).

## 1. Goal

Restrict *who can even open a connection to our onion service* to our **paired contacts**. Today any
party who learns our `.onion` can connect and feed frames to the core (`transport::tor` accepts any
inbound frame). Client authorization means only holders of an authorized key can fetch/decrypt our
onion descriptor and reach us at all — defense-in-depth against:

- **Control-frame / junk injection** at the transport (complements the now-authenticated control
  plane, TODO #20, and the E2E ratchet — this stops the *connection*, not just the payload).
- **Onion-address scanning / enumeration** and unsolicited-connection DoS.
- **Presence probing** by a party who scraped your address from a leaked backup or a contact.

It is **defense-in-depth**, not a new confidentiality boundary: message secrecy already comes from the
Double Ratchet. This raises the cost of even talking to us.

## 2. The Tor mechanism (v3 restricted discovery)

Tor v3 onion services support *client authorization* a.k.a. **restricted discovery**: the service
encrypts its published descriptor to a set of authorized client **x25519 descriptor-encryption keys**
(`KS_hsc_desc_enc`). A client without an authorized key cannot decrypt the descriptor, so it cannot
learn the introduction points — it can't connect, and the HSDir can't even confirm the service to it.

## 3. arti 0.43 API (verified present in-tree)

**Service side** (`tor-hsservice`):
- `OnionServiceConfigBuilder::restricted_discovery(RestrictedDiscoveryConfig)`.
- `RestrictedDiscoveryConfig` is built from a key provider:
  - `StaticKeyProvider` — an in-memory `BTreeMap<HsClientNickname, HsClientDescEncKey>` (public keys).
  - `DirectoryKeyProvider` — a **watched directory** of client key files. This is the one we want:
    it lets the authorized set change **without rebuilding the service**, which matters because
    contacts are added continuously after `launch_onion_service` runs once at bootstrap.

**Client side** (`arti-client` `TorClient`, keymgr-backed):
- `generate_service_discovery_key(selector, hsid) -> HsClientDescEncKey` — generate + store *our*
  keypair for connecting to peer `hsid`, returning the **public** key to hand the peer.
- `insert_service_discovery_key` / `get_service_discovery_key` / `rotate_service_discovery_key` /
  `remove_service_discovery_key`.
- On `connect((peer_onion, port))`, arti automatically uses the stored keypair for that `hsid`.

**⚠️ Maturity — the client half is experimental.** `tor-hsservice/restricted-discovery` (the service
side) is a *stable* feature. But `arti-client`'s `restricted-discovery` feature — required for the
client-side `generate_service_discovery_key` family — pulls `__is_experimental`: a **semver-unstable
API that arti may change or remove in any release.** Since our device is *both* a service and a
client (we must connect to peers' restricted onions), the whole feature depends on this experimental
API. Committing the app's Tor reachability to it means pinning arti and accepting churn on upgrades —
a conscious call the maintainer should make, not a default. This is the main reason the arti wiring is
deferred rather than shipped.

**Key-file format (verified, and what `client_auth.rs` produces):** the authorized-keys directory
holds one file per contact named `<nickname>.auth` (extension exactly `auth`), whose contents are the
contact's key string `descriptor:x25519:<base32>` (arti's `HsClientDescEncKey::to_string()`), which
arti re-parses with `HsClientDescEncKey::from_str`. `RestrictedDiscoveryConfigBuilder` is reached as a
**sub-builder** off `OnionServiceConfigBuilder` and set with `.enabled(true)`, a
`DirectoryKeyProviderBuilder::default().path(CfgPath::new_literal(dir))`, and
`watch_configuration(true)` so added/removed files are picked up **without** relaunching the service.

## 4. How it maps onto Night Drop pairing

The asymmetry to internalize: to reach a peer we need **our** keypair *for their onion*; for a peer to
reach us they need **their** public key in *our* authorized set. So each side both generates a key
(as a client of the other) and authorizes a key (as a service to the other). Per pairing:

1. Both sides already exchange onion addresses (QR `pair?` payload / `Hello.address`).
2. Once A knows B's onion `hsid_B`: `keyA_pub = clientA.generate_service_discovery_key(hsid_B)`.
   Symmetrically B computes `keyB_pub` for `hsid_A`.
3. Each sends the other its `desc_enc` **public** key, alongside the existing handshake material.
4. Each writes the peer's public key into its **`DirectoryKeyProvider` directory** (one file per
   contact, named by a stable nickname derived from the contact id), authorizing that peer.
5. arti republishes our descriptor encrypted to the updated authorized set; the peer, holding the
   matching secret, can now decrypt it and connect. Nobody else can.

**QR (pre-authorized) pairing** carries `keyA_pub` in the QR payload directly — clean, since the QR
is delivered over a trusted channel. **Short-code** pairing exchanges it inside the SPAKE2-sealed
payload, so the rendezvous never sees it. A key change (rebuilt keystore) rides the existing in-band
`Frame::Address` rotation (§5c), extended to also carry a fresh `desc_enc` public key.

## 5. Key lifecycle & storage

- **Our per-peer client keypairs** live in arti's **keymgr** (keyed by peer `hsid`) — arti persists
  and uses them; we don't hand-roll storage. Rotate via `rotate_service_discovery_key` on re-pair.
- **Peers' public keys we authorize** live as files in the `DirectoryKeyProvider` directory under the
  Tor `state_dir`. Deleting a contact removes their file (revokes them); logout wipes the directory.
- These keys are **not** the identity/safety-number keys and carry no new *identity* authority — they
  gate reachability only. Losing them degrades to "unreachable until re-paired", never to a security
  break.

## 6. Failure modes & UX

- **One-way authorization window:** between learning a peer's onion and both sides writing each
  other's key, connections fail. Mitigate by bundling the key with the address in the *same* handshake
  message, and by falling back to the **relay** path (already the offline behavior) until direct
  reachability is established.
- **Backward compatibility:** gate behind a flag. A service with an **empty** authorized set stays a
  normal public onion (today's behavior), so existing pairings keep working during rollout. Only
  enable restriction once both peers advertise support (a capability bit in the handshake).
- **Do not lock yourself out:** never enable restriction before at least one authorized key is
  present, or the service becomes unreachable by everyone.

## 7. Live validation (done)

The live Tor path — the one part the offline suite structurally cannot cover — is now validated by
`tor_smoke::restricted_onion_admits_authorized_client_and_refuses_unauthorized` (`#[ignore]`d;
`--features tor`; ~20 min against real Tor, run 2026-07-09):

1. A launches a **public** onion, then relaunches on the **same keystore** with B's key present →
   restricted descriptor at the **same** `.onion`.
2. B mints its key with the **experimental** `generate_service_discovery_key`, A authorizes it via
   `client_auth::authorize`, and B — holding the matching secret in its keymgr — **connects and
   delivers a frame**.
3. C, which never exchanged a key, **cannot reach A**. The refusal is asserted only *after* B
   succeeds, so it reflects the restriction, not a flaky network.

This exercises the real descriptor publish/decrypt + keymgr path, not just the node's call into
`authorize_client`/`revoke_client` (which the offline
`node::tests::pairing_exchanges_onion_client_keys_and_delete_revokes_them` already covers).

**Keep this test as the upgrade canary** for arti's experimental `arti-client/restricted-discovery`
(`__is_experimental`) API — re-run it on any arti bump. Remaining nuance before flipping restriction on
*by default*: exercise it across **two physical devices** (the validation ran two cores on one host), not
just one machine.

## 7a. Rollout safety (how it avoids a flag day / lockout)

- **Public until first authorization.** `onion_service_config` enables restricted discovery **only
  when `authorized_count > 0`**. A fresh device (no contacts) launches a normal public onion, exactly
  as before — so there is no flag day and no way to ship a build that's unreachable by everyone.
- **Restriction latches on the next launch.** The `enabled` flag is decided at service-launch time;
  the directory is `watch_configuration(true)`, so keys added mid-session republish the descriptor to
  the new set, but the *first* device to gain a contact only becomes restricted on its next start.
- **First contact under restriction uses the relay.** A new joiner's direct onion connect is blocked
  until we authorize them; `node::deliver` already falls back to the relay mailbox, so the pairing
  `Hello` (and the `ClientKey` in it) still lands, we authorize them, and direct reachability follows
  on the next republish. QR and short-code pairing both keep working.

## 9. What shipped

- **`core/src/transport/client_auth.rs`** (default build, unit-tested) — the authorized-keys directory
  manager: `client_nickname` (slug-safe), `authorize`/`revoke`/`is_authorized`/`authorized_count`,
  `validate_key`. Writes exactly the `<nickname>.auth` files arti's `DirectoryKeyProvider` reads; holds
  only the **public** keys peers supply.
- **`core/src/transport/tor.rs`** (`--features tor`) — `onion_service_config` builds a watched
  `DirectoryKeyProviderBuilder` over that directory and enables `restricted_discovery` when
  `authorized_count > 0`; `make_service_discovery_key(peer_onion)` mints/stores our per-peer client key
  via `TorClient::generate_service_discovery_key`. `bootstrap` gained a `client_auth_dir` argument
  (`{state_dir}/client-auth`, wired in `api.rs`).
- **Transport seam** (`core/src/transport/mod.rs`) — `make_client_key`/`authorize_client`/
  `revoke_client` on the `Transport` trait (default no-ops), implemented by `TorTransport`, so the node
  drives client auth without depending on arti types.
- **Pairing exchange** (`core/src/wire`, `core/src/node.rs`) — `Frame::ClientKey { from, client_key }`
  carries a side's (public) client key for the *other's* onion. `node::announce_client_key` sends it on
  pairing (`connect_with_bundle` and the `Hello` handler's reverse leg) and on address rotation (the
  `Frame::Address` handler re-mints for the new onion). The receiver authorizes it **only for a contact
  it already has** (no key-planting by strangers). `delete_chat` and `logout` revoke.

## 8. Scope note

This authorizes *reachability*. It does **not** replace: the E2E ratchet (content secrecy),
authorization-before-first-message (§5, who becomes a contact), the authenticated control plane
(TODO #20), or safety-number verification (#18, MITM detection at pairing). It sits alongside them.
