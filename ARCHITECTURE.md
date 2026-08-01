# Night Drop — Architecture

A privacy-first 1:1 messenger. No server-side keys, no logs, P2P over an anonymity
network, with messages stored on-device by default. Anonymous identities only.

This document is the design source of truth. It precedes implementation; sections
marked _(planned)_ describe intended structure, not existing code.

---

## 1. Goals & Non-Goals

**Goals**
- End-to-end encryption where **only sender and receiver** can read messages.
- **No server-side keys, no logs.** Any server component handles only opaque,
  E2E-encrypted blobs and learns as little metadata as possible.
- **P2P first**; minimal server usage only where strictly necessary.
- **Anonymous identities only** — no phone numbers, emails, or accounts.
- **On-device storage by default**; optional 24h server storage to reduce device
  space, with a **visible in-chat warning to both parties** while it is active.
- Cross-platform: iOS, Android, Windows, Linux, macOS — from one codebase.
- Communication tunneled through **Tor** (pluggable for other anonymity networks).
- Accept **privacy-coin** donations (e.g. Monero).

**Non-Goals (v1)**
- Group chats (the design leaves room for MLS later; v1 is strictly 1:1).
- Account recovery via a central service. Identity is device-held (see §7).
- Any feature requiring a persistent, identity-linked server account.

---

## 2. Technology Decisions

| Concern | Choice | Rationale |
|---|---|---|
| App framework | **Flutter (Dart)** | One codebase for all 6 targets; lightest for a solo maintainer; UI has no web renderer (no XSS surface). |
| Security core | **Rust**, called from Dart via FFI | All key-handling, ratchet, transport, and storage crypto live here. Memory-safe; audited once; identical on every platform. |
| Anonymity transport | **Tor via `arti`** (Rust Tor), embedded as a library | Works on iOS without a separate daemon; no system Tor dependency. Wrapped behind a pluggable `Transport` interface. |
| E2E protocol | **Signal Double Ratchet** via `vodozemac` | Audited, forward secrecy + post-compromise security; ideal for 1:1. X3DH-style initial agreement. |
| Stranger authorization | **PAKE (e.g. SPAKE2)** for short codes | Shared "bouncer" secret is proven, never transmitted; also defeats MITM. |
| Local storage | Encrypted local DB (key in OS keystore) | At-rest protection of message history and identity keys. |
| Donations | Privacy coins (Monero first) | Anonymity for donors; no custody, just published addresses. |

The **golden rule:** anything security-critical (keys, ratchet state, plaintext,
transport, at-rest encryption) lives in the Rust core. Dart/Flutter never touches
raw key material or plaintext beyond what it must render.

---

## 3. Component Overview _(planned layout)_

```
night-drop/
├── app/                 # Flutter application (UI, navigation, platform shells)
├── core/                # Rust security core (compiled to a per-platform lib)
│   ├── identity/        # keypair generation, anonymous identity, QR/short-code
│   ├── crypto/          # vodozemac wrapper: X3DH + Double Ratchet
│   ├── pake/            # SPAKE2 bouncer handshake for short codes
│   ├── transport/       # pluggable Transport trait; arti (Tor) implementation
│   ├── relay_client/    # client side of the relay: rendezvous mailbox + 24h blobs
│   └── storage/         # encrypted local store + keystore integration
├── relay/               # Minimal server (Rust): rendezvous mailbox + store-and-forward
├── website/             # Marketing / features site (static)
└── ARCHITECTURE.md
```

The Dart↔Rust boundary is a small, explicit FFI surface (e.g. via `flutter_rust_bridge`):
high-level calls like `createIdentity`, `beginPairing`, `acceptPairing`,
`sendMessage`, `pollRelay`. Plaintext crosses the boundary only at the UI edge.

---

## 4. Identity Model

- An identity is a **long-term keypair** generated on-device. No registration.
- Default display name is **"Anon"** for both parties. Each user may set a
  **per-chat** display name (their own name, scoped to that conversation).
- Identities are **device-held**. Losing the device means losing the identity and
  history unless the optional encrypted backup (§7) is enabled.

---

## 5. Pairing & Authorization

Two strangers must establish a chat before any message or ratchet keys finalize.
There are two entry paths; **both result in a mutually authorized, E2E session.**

### 5a. QR code — pre-authorized
- The QR encodes a **one-time pre-key bundle + capability token** (and the onion
  address / rendezvous pointer).
- Scanning it is an explicit act of authorization, so the session can begin
  encrypting immediately — no extra secret needed.
- Pre-key bundles are one-time use; scanning consumes the token.

### 5b. Short code — rendezvous mailbox + PAKE "bouncer"
The short code resolves to the peer via a **minimal, untrusted rendezvous mailbox**
(see §5c) and is gated by a **PAKE secret** that carries all the security. The code is
structured like Magic Wormhole's — a non-secret slot plus secret words:

```
   4-cedar-lantern-river
   ^ ^^^^^^^^^^^^^^^^^^^^
   |   PAKE secret words  → never sent to the server; feed SPAKE2 (the bouncer)
   mailbox slot (nameplate) → the rendezvous lookup key; not secret
```

Flow (**interactive SPAKE2**, `core/src/pake` + `node::run_join_handshake` /
`node::service_pending_invites`):
1. **Inviter** picks a **slot** + secret words, reads out the full `slot-secret-words`
   code out-of-band, and *stages* the invite locally (their onion address + a fresh
   pre-key bundle). Nothing decryptable-by-the-code is posted; the background poller
   simply watches the slot for a joiner.
2. **Joiner** enters the code and posts a **SPAKE2 opener** (keyed on the secret words)
   to the slot's joiner leg. The inviter answers with its own SPAKE2 message plus the
   invite payload **sealed under the SPAKE2 shared key**. Both sides now hold the same
   key **iff** they used the same words.
3. The joiner opens the sealed payload — the AEAD tag is the **key-confirmation** step,
   so a wrong code (or a MITM who lacks it) simply fails to open — then dials the onion
   over Tor. The recipient still shows an explicit authorization prompt before the first
   message. This authorizes the stranger **and** prevents MITM.

Because SPAKE2 is a PAKE, the secret words are never sent and **no offline dictionary
attack** is possible: an observer of the rendezvous traffic cannot test a candidate code
without a fresh online handshake, and the sealed payload is readable only by a party who
completes SPAKE2 with the right words. The rendezvous is therefore **never trusted** — a
guessed or random slot yields only un-attackable protocol messages. (The earlier scheme
sealed the payload under an Argon2 key from the words with a fixed salt, which *was*
offline-attackable for low-entropy codes; `TODO.md` #3 replaced it.) The tradeoff is that
pairing is now **interactive**: the inviter must be reachable to answer, which the poller
handles automatically while a code is outstanding.

In both flows the recipient sees an explicit **authorization prompt** before the
first message is delivered.

### 5b′. Safety-number verification (defense in depth)
PAKE/QR already authenticate the first handshake, but users can additionally confirm no MITM
sat on the pairing channel by comparing a **safety number** — `SHA-256` over the *sorted* pair
of long-term identity keys (domain-separated), rendered as 12×5 digits. Because it's symmetric,
**both devices show the identical number**; a mismatch means different keys on the two ends. A
QR of the raw 32-byte fingerprint enables scan-to-verify. State is a per-contact `verified`
flag (persisted). Since a contact is keyed by its identity key, the key can't silently change
mid-chat — a re-paired contact is a new, `unverified` contact, which is itself the "new identity"
signal. All derivation is in `core/` (`Node::safety_number` / `safety_qr` / `verify_safety_qr`);
Dart only renders the string and toggles the flag. See `docs/design/key-verification.md`.

When one side toggles `verified`, `set_verified` also sends the peer an **authenticated
`Frame::Verified` control frame** (state carried by *which* of `MARK_VERIFIED`/`MARK_UNVERIFIED`
decrypts on the ratchet, so there's no tamperable plaintext flag). The receiver sets a separate
`peer_verified` field and shows an **informational** note ("the other person marked this
verified") — it **never** touches the receiver's own `verified`. This is deliberate: verification
is not transitive, so a compromised peer can't forge a verified badge on the other's screen; each
side must still compare the number itself. `peer_verified` is per-contact, persisted, and reset
on a re-pair (new session) exactly like `verified`.

### 5c. The rendezvous mailbox
- A **stateless meeting point** that allocates ephemeral slots and relays the short-TTL
  **SPAKE2 handshake messages** for each (a joiner leg and an inviter leg per slot),
  deleted on pickup. **No keys, no logs**; it sees only PAKE protocol messages and a
  payload sealed under a key it cannot derive, reached over Tor (no IPs).
- It is **not a separate service** — it is one extra endpoint on the same minimal
  relay box (§6, `relay/`), keeping server footprint to a single component.
- It is used **only for first contact via short code**. The QR path (§5a) embeds the
  onion address directly and never touches the rendezvous. After pairing, peers
  exchange any **updated onion addresses in-band** over the E2E channel (or via the
  §6 relay under an unlinkable derived handle), so the rendezvous is never needed
  again. **Implemented** (#11): on startup the node compares its live onion to the
  last-persisted one and, if it changed (e.g. a rebuilt keystore), sends each contact an
  E2E `Address` frame (`node::announce_address`); the peer updates where it routes replies.

---

## 6. Messaging & Transport

- **Hot path:** onion-to-onion. Each client reaches the peer via Tor (`arti`); when
  both are online, messages flow directly, ratcheted per-message.
- **Bridges (censorship circumvention):** where the public Tor relays are IP-blocked, the
  core loads **vanilla bridge lines** from `bridges.txt` in the Tor state dir and routes
  through them (arti `bridge-client`). Where even bridge IPs are DPI-blocked,
  **obfs4/Snowflake pluggable transports** disguise the traffic: a PT bridge line in
  `bridges.txt` plus a `transports.txt` mapping the transport to its client binary (arti
  `pt-client`, launched on demand). Bundling the PT binaries — especially on mobile — is the
  remaining follow-up. See `docs/bridges.md`.
- **Diagnostics never carry identity.** Two logging channels, deliberately: `devlog!` prints
  identity keys, invite codes, and decrypted names, so it is compiled out of release builds —
  Android's logcat persists them for any `adb`-connected observer. `diag!` (`core/src/diag`) is
  the opt-in field channel that *may* run in a release build, and so records **what happened, not
  who with**: counts, outcomes, and which leg of a protocol ran — never keys, onion addresses,
  codes, slots, or names. It is off unless a build explicitly enables it (`NIGHTDROP_DIAG=1`, via
  `--diag` on the install scripts). Anything identity-linked belongs in `devlog!`.
- **Sending never blocks on the network.** `send` advances the ratchet and stores the message
  under the core lock (the ordered, security-critical step stays synchronous), but on a
  non-synchronous transport (`Transport::is_synchronous()` — false for Tor, true for the in-memory
  transport) it **defers the opaque-byte delivery** to the background poller instead of dialing
  inline. Composing a message returns instantly with the message stored "queued"; the poller
  attempts direct-peer delivery (relay fallback) on its next tick and flips the status. Only the
  transport delivery moves off the hot path — the crypto boundary is unchanged.
- **Network dials are time-bounded so the UI never hangs on them.** `send` runs while the core
  lock is held, so an unbounded dial to an offline peer would freeze every other FFI call and the
  poller for as long as arti retries (minutes, at a high `HS_CONNECT_ATTEMPTS`). Direct peer dials
  are capped (`PEER_DIAL_TIMEOUT`) so an offline send fails fast to the relay fallback; relay dials
  are capped (`RELAY_DIAL_TIMEOUT`) without defeating persistence, because the callers that must
  punch through a flaky path (pairing, the relay-retry poller) loop over their own schedule with a
  fresh bounded attempt each time. A capped send is never a lost message — only deferred delivery.
- **The relay self-heals; it is not babysat.** arti can keep the relay process alive while its
  descriptor publisher or introduction points wedge (seen after multi-day uptime: process up,
  onion dark, clients get "could not reach relay") — so `Restart=always` alone never recovers it,
  because nothing crashed. The relay therefore watches its **own** onion reachability
  (`RunningOnionService::status`) and exits after a sustained outage so systemd restarts it fresh
  (re-establishing intro points, republishing). A weekly `RuntimeMaxSec` backstops any slow
  degradation the watchdog misses. The onion keystore is preserved across restarts, so the address
  never changes. The relay runs **6 introduction points** (vs arti's default 3) and device onions
  run **4**, so losing an intro point to relay churn leaves the service reachable instead of dark.
- **Wedged entry guards self-recover, on both the relay and the app.** After weeks of a persistent
  Tor state dir, arti's confirmed entry guards can churn out of the network; a client stuck on them
  can neither publish its onion nor reach the relay, and a plain re-bootstrap reuses the same
  guards, so it can't recover — this is the state that once had to be cleared by hand. Recovery is
  to delete the guard/circuit state (`guards.json`, `circuit_timeouts.json`) while **keeping the
  onion keystore** (stable `.onion`), so the next bootstrap picks fresh guards. The relay escalates
  to this automatically: the watchdog counts consecutive unhealthy restarts and, after a plain
  restart (which preserves guard stickiness and fixes intro-point wedges) fails to restore
  reachability, resets guards on the next start. The app does the same in-process: if its onion
  hasn't published within ~150 s (a healthy publish finishes well under that), it resets guards and
  rebuilds the core with fresh ones — at most once per launch, and only while the onion is
  unpublished, so a working session is never disrupted.
- **One Tor instance per state dir.** arti takes an **exclusive on-disk lock** on its state
  directory, so at most one core may be live at a time. Any path that replaces a running core
  — restoring a backup, retrying a failed launch, creating a new identity after one — must call
  `NightdropCore::shutdown()` first, which drops the transport *and* the relay (its dialer holds
  a clone of the same arti client) and so releases the lock synchronously. Dropping the core is
  not sufficient: the poller thread keeps the transport alive until its next tick, and across the
  FFI boundary the Rust object is only freed by a Dart finalizer at an unpredictable time. Two
  live instances fail with `State already locked`, which must never be reported to the user as a
  bad password.
- **Offline / space-saving path:** a **minimal relay** stores **E2E-encrypted blobs
  only**, for at most **24h**, when a peer is offline or when the user opts into
  server storage to save device space.
  - The relay never holds keys and cannot read content.
  - Metadata is minimized; blobs are addressed by ephemeral, unlinkable handles.
  - When server storage is active, **both parties see a persistent in-chat warning**
    that messages are stored remotely.
- **Multi-relay / self-hosting (#17):** a recipient may advertise an **extra relay set** on
  top of the shared primary default (`my_relays`, announced in-band as `Frame::Relays`, held by
  the peer as a contact's `peer_relays`). Because `mailbox_handle(recipient_ik)` is
  **relay-agnostic** (the same handle works on every relay) and every queued blob is already
  sealed under a recipient-derived key, a sender simply seals **once** and posts the identical
  blob to the primary **plus** the recipient's set (`queue_on_relays`); the recipient drains all
  of them and **de-duplicates by content hash** (`seen_relay_blobs`), and an edit/unsend recalls
  **every** copy. This buys **availability + censorship-resistance** if a relay is down or
  blocked — it is explicitly **not** an anonymity layer and relays never gossip (anonymity stays
  with Tor). No new trust, keys, or metadata surface. Full rationale:
  `docs/design/multi-relay-mailboxes.md`.
  - **Relay-health feedback.** Each poll records whether our own advertised relays answered
    (`relay_health()`); if a self-hosted relay goes dark the app warns the user and nudges them to
    add a backup relay, so degraded redundancy is visible instead of silent. And if opt-in server
    storage is on but no relay is reachable to store a copy (the message still went peer-to-peer),
    the chat's storage banner downgrades to "delivered but not stored" rather than implying a
    server copy exists.
  - **Signed relay directory — rotate the relay set without an app update (#17 tail).** Each relay
    can serve an **operator-signed** relay list (`Request::GetDirectory` → one-line
    `SignedDirectory` JSON). The operator holds an **Ed25519** key whose public half is **baked
    into the app** (`directory::DIRECTORY_PUBKEY`); on every relay poll the app fetches the list
    from whatever relay it can still reach (primary **or** any advertised/discovered relay),
    verifies the signature against that baked-in key, and — only if the payload's monotonic
    `version` is newer than what it already trusts — adopts the relays as `discovered_relays`
    (shared defaults it drains, pairs over, and posts to, alongside `my_relays`). Because trust is
    anchored in a key **only the operator holds**, a hostile relay cannot inject relays, and the
    monotonic version blocks rollback to a stale list. This closes the "lost the primary relay's
    onion key → every user is stranded" failure: publish a new signed list (carrying the new
    onion) from *any* still-live relay and every app migrates itself. The feature is **inert until
    a real key is baked in** (the all-zero default key verifies nothing). Operator flow:
    `nightdrop-relay gen-directory-key` (mint the key, paste the public half into the app, rebuild)
    → `nightdrop-relay sign-directory <priv> <version> <onion…>` → drop the output as
    `<state>/relay-list.json` on each relay. `discovered_relays` + `directory_version` persist in
    the encrypted store and travel in backups.
  - **Private (restricted) relays — self-host for just your circle (§3.2).** A relay can gate its
    onion to **authorized clients only** using the same Tor **restricted discovery** as onion client
    authorization (#22), applied to the relay's onion instead of a chat onion. When
    `<state>/authorized-clients/` holds ≥1 authorized-client key the relay runs **PRIVATE**: an
    unauthorized client cannot even fetch its descriptor, let alone post. This preserves the
    identity-blind invariant — the relay authenticates at the **Tor layer** with x25519 client keys,
    never learning chat identities. Flow: each device runs `create_relay_access_key(relay_onion)`
    (`NightdropCore` FFI → arti `generate_service_discovery_key`, private half stored in the local
    keystore and presented automatically on later dials) and shows the operator the public
    `descriptor:x25519:…`; the operator runs `nightdrop-relay authorize-client <name> <key>`
    (revoke/list mirror it). An empty authorized set = a normal **PUBLIC** relay (today's default) —
    we never restrict with zero clients (that would lock everyone out). Authorizing/revoking after
    launch is picked up live (the directory is watched); the first authorization / last revocation
    flips PUBLIC↔PRIVATE and needs a restart. In-band auto-distribution of access keys (owner's
    contacts authorized automatically over the paired channel) is a deliberate future step: it needs
    either co-location or an authenticated remote-authorize op, so v1 keeps authorization a manual,
    SSH-`authorized_keys`-style operator action. See `RELAYS.md`.
- **Transport is pluggable:** Tor is the default implementation of the `Transport`
  trait; the interface allows adding other anonymity layers (I2P, Snowflake, etc.)
  where Tor is network-blocked.
- **Offline / local-network path (`transport::lan`):** a Briar-style **LAN transport** for when
  there is no internet, or Tor is blocked, but paired devices share a network (same room, same
  Wi-Fi, a field hotspot). It advertises the device's detected LAN IP so a peer dials it directly;
  pairing by QR in the same room needs no discovery step, and the existing in-band address rotation
  (§5c) keeps the endpoint fresh as DHCP changes it. Content stays end-to-end encrypted + padded, so
  a local observer sees only that two IPs talk, never what — but LAN is **not anonymized**, so it is
  a censorship/blackout fallback, not a Tor replacement. Constructor: `NightdropCore::new_lan`.
  **Deliberately no auto-discovery beacon:** broadcasting a raw identity key on the LAN would leak
  presence + a stable identity (violating the anonymity invariant); privacy-preserving discovery
  (a rotating per-contact token, Briar-style) and a Bluetooth path (needs platform channels) are
  tracked follow-ups, as is the app-level "Local Wi-Fi mode" toggle.
- **Metadata protection — fixed-size framing:** every `Frame` is length-prefixed and
  **zero-padded to a fixed size bucket** (`core/src/wire::encode`, `WIRE_VERSION` 2) before it
  touches the transport, so a passive observer can't use frame length to distinguish a short text,
  a rename, or a control ack — they all look identical on the wire. This blunts the "Alice sends
  2 KB → Bob receives 2 KB" size-correlation channel that onion routing alone leaves open. (Coarse
  buckets for media; timing/volume correlation is a separate, harder problem — see the transport
  alternatives note and the mixnet discussion.)
- **Authenticated control plane:** the peer-state control signals `Closed`, `Ack`, `BackedUp`,
  `Verified`, and `Screenshot` are **E2E-authenticated** — each carries an encrypted fixed marker on the ratchet, and the
  receiver acts only if it decrypts to the expected value (`node::MARK_*`, `verify_control`). This
  closes a spoofing/replay gap: without it, anyone who knew your identity key could forge a
  "the other person deleted this chat" (`Closed`), a false delivery `Ack`, a fake backup
  transparency scare, or a fabricated screenshot accusation (`Screenshot`) — that last one being a
  way to manufacture distrust between two people who have no other channel to check. Message *content* was always ratchet-authenticated; this extends the same
  guarantee to the signals that mutate chat state. `Closed` and `Screenshot` are additionally
  **held for retry** (`pending_control`) when neither the peer nor a relay is reachable as they are
  raised: both report a one-off event the user cannot resend by hand. Design record for the
  screenshot signal, including what detection can and cannot see:
  `docs/design/screenshot-transparency.md`.
- **Onion client authorization (reachability gate):** the Tor transport can restrict *who may even
  fetch our onion descriptor* to paired contacts, using v3 **restricted discovery**
  (`core/src/transport/{client_auth,tor}.rs`, `--features tor`). Each side hands the other a **public**
  client descriptor-encryption key during pairing and on address rotation (`Frame::ClientKey`,
  `node::announce_client_key`); we write authorized peers' keys into a directory arti watches, and
  `delete_chat`/`logout` revoke them. This is **defense-in-depth**, not a new secrecy boundary (the
  ratchet already provides that): it raises the cost of frame injection, address scanning, and presence
  probing by stopping the *connection*, not just the payload. It stays **off until the first contact is
  authorized** (empty set ⇒ normal public onion — no lockout), latches on the next launch, and first
  contact under restriction rides the relay fallback. **Live-validated over real Tor**
  (`tor_smoke::restricted_onion_admits_authorized_client_and_refuses_unauthorized`, `#[ignore]`d): an
  authorized client is admitted to a restricted onion and an unauthorized one is refused. The client
  half rides arti's **experimental** restricted-discovery API, so that test is the upgrade canary and
  restriction stays opt-in until exercised across physical devices — see `docs/design/onion-client-auth.md`.
- **Framing & versioning:** peers exchange `Frame`s (`core/src/wire`) as a versioned envelope
  `{"v":2,"f":<frame>}` (`WIRE_VERSION`), length-prefixed and padded as above. Everything in a
  frame is already E2E-encrypted or public handshake material — the transport and relay
  see only these bytes. The version lets two installed builds detect an incompatible
  protocol change and reject it with a clear error rather than misparse it; bump it on any
  breaking change to `Frame` or the framing. The relay's own request/response lines carry a **separate**
  `RELAY_VERSION` (§11.2), so the two protocols evolve independently.

**Storage default:** messages are stored **locally on the device**. Server storage
is **opt-in**, time-boxed to 24h, and always surfaced to both users.

> **Build plan:** the relay store-and-forward, 24h time-bomb, delivery acks, and
> offline notifications are specced in **§11** (single relay; distributed deferred).

---

## 7. Key & Identity Backup

Anonymous + device-held means no recovery exists unless the user opts into a backup.
Backups are an **encrypted export** of the identity (and selected state). There are
three transfer mechanisms, each covering a different scenario.

> **Build plan:** **Lite (default)** vs **Full** backup modes, single-chat scoped backups,
> the per-chat backed-up flag, the peer "backed up this chat" signal, and Closed-on-logout
> for un-backed chats are specced in **§11**. A backup also folds in the **onion keystore** so
> restore reproduces the same `.onion` (already implemented; see §11.5 / git history).

### 7a. Encrypted file export — user-held password
- The app produces an **encrypted backup file**. The encryption password is
  **randomly generated on the device** and **shown to the user exactly once** as a
  recovery code; the user must record it (it is their responsibility).
- The password is **never sent anywhere and never persisted** (not on disk, not in
  the keystore). It exists in RAM only while the password window is open and is
  **zeroized the moment that window closes**.
- Import on any install requires the user to re-enter the recorded password.

### 7b. Device-to-device transfer — no human-handled password
- Old device → new device over an **authenticated channel** (QR + PAKE). The backup
  key is handed directly between devices; nothing is transcribed by the user.
- **Requires the old device to still be working and present.** If the old device is
  lost or wiped, this path is unavailable — use 7c.

### 7c. Server-stored backup — opt-in, time-boxed
- Uploads the **encrypted backup blob** to the relay so it can be recovered on a
  **fresh device** after a wipe or phone change. The server holds an opaque,
  E2E-encrypted blob and **never receives the password** (no server-side keys).
- **Retention: default 24h, user-extendable up to 36h** at upload time. The exact
  expiry **timestamp** is shown, and the user must **acknowledge** that the backup is
  stored remotely and will be deleted at that time.
- **Coupling:** recovery on a fresh device requires the **user-held password from
  7a** — the server cannot decrypt and device-to-device is unavailable once the old
  device is gone. Therefore **enabling the server backup forces the 7a "shown once,
  record this" flow** and an explicit acknowledgment that **losing the password means
  losing the backup**. This is intentional and must be made loud in the UI.
- **Status (#9): done.** **Create:** `NightdropCore::create_server_backup` generates the one-time
  password, posts the opaque blob to the relay (24h default, clamped to 36h), and returns the
  exact expiry; the home-screen "Back up to server" menu shows the password once with the
  mandatory record-it + exact-expiry acknowledgment. **Restore:** `restore_server_backup_tor`
  bootstraps Tor, fetches the blob by its password-derived handle, and rebuilds the identity;
  the device comes back on a **new onion** (the relay is only reachable after bootstrap, so the
  keystore can't be pre-seeded), which the #11 startup announcement then propagates to contacts.
  Wired into onboarding as "Restore from server backup". Caveat: the relay copy is drained on
  fetch, so a wrong-password attempt consumes it — same "lose the password, lose the backup"
  contract as create.

All backup files/blobs are encrypted at rest; the password is the only thing that can
open them, and only the user (7a) or the paired device (7b) ever holds it.

### 7d. App lock — the at-rest key behind a user secret

By default the 32-byte at-rest key lives in the OS keystore, so the app opens the history with no
user interaction. That is the right default (nothing to forget, nothing to lose) but it means
possession of an unlocked device is possession of the history. The **opt-in app lock** re-wraps
that key under `Argon2id(secret, salt)` — 64 MiB / t=3, well above the §7 backup parameters
because this secret is *user-chosen* — and stores `{salt, params, wrapped}` in `store-key.lock`
beside the state blob. Enabling a lock **deletes the keystore copy**; without that the lock would
be decoration. The sealed state format is unchanged, and there is no recovery path: the secret is
the only way in, by design.

The user picks **PIN or passphrase**, and the distinction is real, not cosmetic: a short PIN
defeats someone who picks up the unlocked phone, but ~20 bits cannot survive an attacker who
copies the lock file off the device, at any KDF cost. Only a passphrase covers that case. The UI
states this on each option rather than presenting them as equivalent conveniences.

Lock lifetime follows the existing background-delivery toggle, with no new setting: **off** → the
key is dropped on lock, so nothing can read the store until the next unlock; **on** → it stays
resident, because a foreground service that cannot decrypt cannot deliver. That trade is inherent
to receiving while locked (Signal makes the same one).

Design record, including how this constrains the planned duress wipe: `docs/design/app-lock.md`.

---

## 8. Threat Model (summary)

> **Assurance status.** This threat model states what the design *intends* to protect against. It
> has **not** been validated by an independent external security audit. The cryptographic
> primitives are audited libraries and the security-critical code is isolated and fuzzed, but the
> integration (pairing/PAKE, relay, at-rest storage, FFI) has not been independently reviewed — an
> external audit is a prerequisite before relying on these guarantees in adversarial settings. See
> `SECURITY.md` §Audit status.

**Protect against**
- Network observers / the relay operator reading message content → E2E + Tor.
- The relay correlating who-talks-to-whom → minimized metadata, unlinkable handles,
  onion addressing, 24h cap.
- Stranger spam / unsolicited contact → QR pre-auth and short-code PAKE gating.
- Device theft (at rest) → encrypted local store, keys in OS keystore.
- MITM during pairing → PAKE for short codes; scanned bundle for QR.

**Explicitly out of scope (v1)**
- A fully compromised endpoint (malware with root) — cannot be defended in software.
- Global passive adversary defeating Tor itself.
- Recovery of history after device loss without an opt-in backup (§7). A user who
  never recorded the 7a password and lost their device cannot recover — by design.

### 8a. Metadata resistance — what leaks and what doesn't

Content secrecy is the easy part; metadata is where "private messengers" usually
fall down. This section is deliberately blunt about the residual leaks so users and
reviewers can judge the real guarantees rather than an implied absolute. Nothing
here weakens the §-level invariants — it states their limits.

**What is hidden**
- **Message content** — E2E Double Ratchet; only sender and receiver hold keys. No
  server ever sees plaintext or keys (invariant).
- **Real-world identity** — no phone number, email, or account; identity is a
  device-held keypair (invariant). Handles are opaque and unlinkable to a person.
- **Network path / IP** — all transport is Tor by default (invariant); the relay and
  the peer see a `.onion`, not an IP or location.
- **Frame sizes** — wire v2 pads every frame to a fixed size, so an on-path observer
  learns nothing from length (not message size, not "typing vs. media").
- **Control-plane authenticity** — `Closed`/`Ack`/`BackedUp`/`Verified`/`Screenshot`/`Approved` carry a
  ratchet-encrypted marker, so they can't be spoofed or replayed by the relay.
- **Onion reachability** — restricted discovery (#22) gates the onion descriptor to
  authorized contacts, so a stranger cannot even confirm a user's service is online.

**What the relay operator CAN still observe** (it only ever holds opaque, padded,
E2E blobs — but observation ≠ decryption):
- That **some** blob of the fixed padded size arrived for a given mailbox slot at
  time _T_, and that **someone** later polled/retrieved that slot. It cannot read it,
  size it meaningfully, or tie it to a real identity.
- **Liveness/timing** of a mailbox: rough activity patterns (when a slot receives or
  is drained). v1 ships **no cover traffic**, so timing is not obfuscated — an
  adversary who watches a slot sees the cadence of real events, just not their content.
- With **multi-relay fan-out** (#17), the same hash-dedup'd blob is posted to the
  recipient-chosen relay set at about the same time. This buys availability and
  censorship-resistance; the cost is that a **colluding set of those relays** could
  correlate the timing/size of one delivery across them — still never content or
  identity, and the set is chosen by the recipient, not imposed.

**Residual correlation risks** (honest limits, out of scope to fully defeat in v1)
- **Traffic-analysis / timing correlation.** An adversary positioned to watch both
  ends' relay traffic (or a global passive adversary against Tor) can attempt to
  correlate send/receive timing across a conversation. We do not add latency or
  cover traffic in v1, so this is not defended — it is the classic limit of a
  low-latency anonymity network.
- **Rendezvous pairing.** The short-code mailbox (§5c) sees only ciphertext; the slot
  is a **non-secret** lookup key. A passive observer learns "two parties exchanged
  something under slot _N_ at time _T_," never the secret words, the identities, or
  the result. QR pairing avoids the rendezvous entirely.
- **Store-and-forward window.** Opt-in server storage / offline blobs sit at the relay
  for **up to 24h** (§6, §11), during which the relay knows a slot has pending data of
  the fixed size. Local-first delivery avoids the relay entirely when both peers are
  online.
- **Disappearing messages** are cooperative: they delete on both cooperating clients
  on schedule, but cannot claw back a copy from a peer that has been modified to
  retain it. This is a UX/hygiene feature, not an exfiltration defense.

**Deliberately avoided leaks**
- **No third-party push.** Background delivery uses a local Android foreground service
  (#13), not FCM/APNs — so there is no push-provider metadata trail (who is notified,
  when) as there is in most mobile messengers. (iOS, which is hard to do without a
  push intermediary, is out of scope for v1.)
- **No analytics, crash reporters, or ad SDKs.** The dependency tree is audited to
  avoid anything that reaches a network/analytics endpoint — see `DEPENDENCIES.md`.

---

## 9. Donations

Published privacy-coin addresses (Monero first; optionally others). No custody, no
tracking, no identity linkage. Surfaced in-app and on the website.

---

## 10. Website

A static features/marketing site that explains the app's strengths — privacy,
no-logs, P2P/Tor, local-first storage, anonymous identities — **without naming
competitors**. Hosts the donation addresses and download links.

Addresses/copy/links come from one source of truth, `config/app_config.json`, synced to
`app/assets/app_config.json` and `website/config.js` by `make config`.

---

## 11. Relay store-and-forward & backup evolution — implementation plan

Status: **planned, not yet built.** This section is the agreed source of truth for the next
build. The current Tor build pairs **direct onion-to-onion with no relay** (the `relay/` box
exists with a rendezvous mailbox + 24h store-and-forward but is **not wired into the Tor
path**). Distributed/federated relays and erasure-coded blob splitting are **explicitly
deferred** — single relay only for now. Brute-force of E2E content is **not a threat** and is
**not** a design driver (X25519 + AES-256/ChaCha20 are not brute-forceable); the rationale for
short retention is **metadata minimization, exposure window, and user control**, not "anti
brute force."

### 11.1 Locked decisions
- **Time-bomb (24h):** always on the **relay queue** (undelivered blob older than 24h → reaped).
  On the **device**, auto-deletion applies **only to ephemeral / opt-in server-storage chats**,
  **never** to normal local-first history.
- **Delivery state via ack** (not a mailbox-gone guess): `queued → delivered | expired | recalled`.
- **Backup modes:** **Lite (default)** and **Full**.
  - **Lite** = identity + onion key + contacts (`.onion` + names) + **session pickles**. **No**
    message history, **no** media. Restore is seamless (same onion, chats resume); a leaked Lite
    backup reveals only identity + contact graph (unavoidable for any restorable backup), and
    forward secrecy means session pickles can't decrypt **past** messages.
  - **Full** = Lite + message history + media (the current §11-pre behavior).
- **Single-chat backup:** a backup file may be **scoped to one chat** (still identity-owned).
  Restore phase 1 = standard *replace* restore; **import-into-existing-identity (merge)** is a
  later phase.
- **Backed-up flag** per chat: set when a chat is included in any backup (whole or single).
- **Closed-on-logout:** on identity deletion, chats **without** the backed-up flag send a
  `Closed` signal to the peer (via the relay, since the onion is about to die); backed-up chats
  stay silent (the user may restore them).
- **Backup transparency signal:** when a chat is backed up, signal the peer ("🗄️ backed up this
  chat") — text reflects scope (Lite "no message content" vs Full "messages included").
- **Receiving after restore:** a message into an existing (restored) authorized chat shows in
  the list + notification with **no accept prompt** (already authorized) and **does not
  auto-open** the chat.

### 11.2 Relay protocol (single relay, over Tor)
The relay is a **standalone binary** (the `relay/` crate) that **embeds arti and publishes its
own `.onion`** with its **own keystore, separate from any chat identity** (not linkable). Because
it is an onion service it is reachable **from any network — LTE, NAT, café Wi-Fi — with no LAN,
port-forwarding, or public IP** (Tor solves reachability; the old LAN/TCP pain is gone). Clients
dial it **through their own Tor client** (`RelayClient` must dial via the Tor client, not plain
TCP). Dev: run it on the dev box with a **persisted state dir so its `.onion` is stable** across
restarts; its address goes in config (`config/app_config.json` `relay` field, overridable by
`NIGHTDROP_RELAY`). Ships externally unchanged (drop on a VPS; same onion via its state dir).

All payloads are opaque E2E blobs; the relay learns no keys, no plaintext, no identity. Mailbox
handle = an unlinkable derived key both peers can compute (per §5c style), never the
identity/onion. **Implemented** (`node::mailbox_handle` / `relay_wrap`): the handle is a
truncated, domain-separated SHA-256 of the recipient's long-term identity key (senders know
their contact's key; the receiver knows its own), and every queued frame is additionally
**sealed** (ChaCha20-Poly1305 under a key derived from the recipient's identity key) before
posting — wire frames carry routing metadata (sender identity keys; the sender's onion in
`Hello`) that is fine peer-to-peer but must not sit readable on the relay. Only the recipient
(or a party who already knows their identity key, i.e. their contacts) can even parse an
envelope; message content inside remains Double-Ratchet E2E as always.

The relay enforces **resource limits** (`RelayLimits`): a max blob size, per-mailbox depth and
byte caps, and a global byte ceiling — all **reject-new**, so a flooder can neither OOM the
in-memory store nor evict a victim's queued mail (the poster gets an error; their direct P2P
path is unaffected). Requested TTLs are **clamped** server-side to the 24h cap, keeping the
§6 promise even against a hostile client. Rejections are visible in the dev flow-log.

Each request and response is one JSON line wrapped in a versioned envelope —
`{"v":1,"req":<request>}` / `{"v":1,"resp":<response>}` (`RELAY_VERSION`). A client and relay on
mismatched versions reject each other with a clear error instead of misparsing; the relay
protocol versions **independently** of the peer-to-peer `WIRE_VERSION` (§6). Bump it on any
breaking change to the request/response shape.

Operations (idempotent, authenticated only by capability tokens, never identity):
- `post(handle, blob, ttl≤24h) -> msg_id, delete_token` — enqueue a blob. Returns a per-message
  `delete_token` (random secret) the **poster** keeps for recall.
- `peek(handle) -> count` — **content-free**; returns only how many blobs wait. Used by the
  background/low-data check. (No blob bytes, minimal data.)
- `fetch(handle) -> [(msg_id, blob)]` — pull queued blobs (receiver). Relay marks them for
  delete-on-fetch (or deletes after ack — see below).
- `recall(handle, msg_id, delete_token)` — sender deletes a **still-queued** (unfetched) blob.
  Only whoever holds `delete_token` can delete → no identity, no DoS.
- `reaper` — background loop on the relay deletes any blob with `age > ttl`.

### 11.3 Delivery + notification state machine (client)
- **Send while peer offline / server-storage on:** `post()` → mark message **queued**; show the
  sender badge "Queued on relay (delivers when they're online; expires in 24h)."
- **Receiver online / app-open / background `peek` says count>0:** `fetch()` → decrypt locally →
  insert into chat → **local generic notification** ("New message" — no content leaves the
  device) → send a **silent delivery `ack`** back (E2E control frame, itself store-and-forwarded;
  acks raise no notification and are never acked).
- **Sender gets ack:** badge `queued → delivered`.
- **24h passes, no ack:** badge `queued → expired` ("not delivered").
- **Sender recall:** `recall()` while unfetched → blob gone, receiver never notified; badge
  `queued → recalled` ("unsent"); the "stored on relay" indicator disappears on the sender.
- The "stored on relay / queued" indicator also disappears once **delivered** or **expired**.

### 11.4 Time-bomb (ephemeral chats, device side)
- Only for chats in **opt-in server-storage / ephemeral mode** (both parties opted in, persistent
  warning shown). A client loop checks each ephemeral message's age; `age > 24h` → destroy on the
  device. The relay reaper independently destroys the server copy. Normal local-first history is
  **untouched**.
- Honest limits to document in UI: cannot force-delete the **peer's** device copy, and a hostile
  relay could retain a blob — but it is undecryptable ciphertext, and once the **honest** network
  expires it, an honest fetch can't recover it.
- **User-set disappearing timer** (shipped, TODO #10): an independent per-chat timer
  (`disappearing_secs`, 0 = off) that does **not** require server storage. It is a **shared**
  setting — changing it sends an E2E `Disappearing` frame so both devices mirror the value and a
  notice records the change — and `sweep_time` deletes messages older than the **shorter** of this
  timer and the 24h ephemeral bomb. Same honest limits apply (age is measured from send/receipt).

### 11.5 Backup content matrix
| Item | Lite (default) | Full | Single-chat (Lite/Full) |
|---|---|---|---|
| Identity key + onion keystore | ✓ | ✓ | ✓ |
| Contacts (`.onion` + names) | all | all | the one chat |
| Session pickles | ✓ | ✓ | ✓ (that chat) |
| Message history | ✗ | ✓ | per mode |
| Media bytes | ✗ | ✓ | per mode |

**Lite/Full — implemented** (#7): `Node::backup_with_mode(password, full)` filters what
`export()` includes — Full bundles media; Lite clears each chat's `history` and skips media
(both keep the onion keystore). Exposed via the `full` flag on `create_backup` /
`create_server_backup`, offered as a Lite/Full choice in the backup menu.

**Single-chat scoped backup — implemented** (#8): `Node::backup_chat(contact_id, password,
full)` seals a blob carrying **only** that conversation (its `collect_media_for` attachments
when Full); `Node::merge_from_backup(blob, password)` folds it into the **live** identity —
inserting a chat we lack, or, for one we already have, appending only history messages we're
missing (deduped by `msg_id`; the live session is never rewound) and sealing any carried media
into the store. Exposed as `create_chat_backup` / `merge_backup`; UI in the chat overflow and
the home backup menu. The per-chat backed-up flag + logout/transparency signals (§11.6) are the
rest of #7.

### 11.6 Logout & the "abandoned chat" signal
On identity deletion: for each chat **without** the backed-up flag, `post()` a `Closed` to the
peer's mailbox (relay) before wiping; backed-up chats are left silent. This is why a peer who
messages a **backed-up-then-deleted** identity gets queued (and delivered on restore within 24h),
whereas an **un-backed** chat's peer is told it's deleted.

**Implemented** (#7): `Contact.backed_up` / `Contact.peer_backed_up` (persisted, `#[serde(default)]`).
`Node::mark_backed_up(ids, full)` — called after every `create_backup` / `create_chat_backup` /
`create_server_backup` — sets `backed_up` on the covered chats and, for a **Full** backup only
(Lite copies no messages), sends each peer a `Frame::BackedUp` transparency signal. The receiver
sets `peer_backed_up` and shows a persistent in-chat banner (mirroring the server-storage
warning) that their messages may persist in the sender's backup. `Node::logout` (surfaced as
`NightdropCore::logout`, called by the app **before** it wipes local files) posts `Closed` to every
un-backed, non-closed chat's peer and clears all chats; backed-up chats stay silent so the peer's
mail queues on the relay until we restore within its 24h window.

### 11.7 Build phases (do in order; each shippable)
1. **Relay store-and-forward** wired into the Tor build: `RelayClient` dials the relay onion via
   the Tor client; `post/peek/fetch/recall` + reaper; queued/delivered/expired badge via ack;
   fetch-on-open + background `peek` → notification; device time-bomb for ephemeral chats only.
   **Done** (drain uses `take` rather than `peek`+`fetch`; fetch-on-open = immediate relay poll on
   launch/foreground; expired badge + ephemeral time-bomb run in `Node::sweep_time` on the relay
   cadence, driven by per-message timestamps).
2. **Sender recall** — **superseded by message *editing*** (shipped): a sent text can be edited
   within **15 minutes**, or at any age while still **queued** (the relay blob is `recall`ed via
   its `delete_token` and the new text posted in its place, so the peer never sees the draft).
   Delivered edits go as an E2E `Edit` frame naming the message's random `msg_id`; both sides
   show an **"edited"** tag. **Unsend ("delete for both")** — shipped — reuses this exactly: a
   queued message is `recall`ed to nothing (the peer never receives it); a delivered one gets an
   E2E `Unsend` frame, and both sides replace it with a `kind == "deleted"` tombstone.
3. **Backup modes** (Lite default / Full) + **backed-up flag** + **Closed-on-logout for un-backed
   chats** + **backup transparency signal** to peers.
4. **Single-chat scoped backup**, then **import-into-existing-identity (merge)** restore.
5. Also shipped from the locked decisions: the **server-storage toggle is mirrored to the peer**
   (E2E `Storage` frame) so both parties see the persistent warning (§6 invariant).

### 11.8 Notifications scope (v1)
**Wake-from-killed is dropped** (it needs APNs/FCM device tokens → an anonymity leak, rejected).
- App **alive** (foreground or backgrounded-but-running): background `peek` finds queued → local
  notification.
- App **killed**: no notification until reopened; **fetch-on-open** then delivers the queue.
- Deferred option (not v1): an **Android foreground service** keeps the `peek` alive in the
  background **without** any push provider (peek is over Tor) — reliable background notifications
  at the cost of a persistent notification. iOS cannot, by design.

**Implemented** (#13): opt-in, `core/background_delivery.dart` over `flutter_foreground_task`. The
service keeps the app **process** at foreground priority (a persistent notification) so the
already-running main-isolate poller keeps doing its Tor `peek` + local notifications — the service
task itself does nothing (`ForegroundTaskEventAction.nothing()`), so there is **no second Tor
core** and no push provider. Started/stopped from the app lifecycle (`app.dart`), toggled by a
"Background delivery…" home-menu switch (prompts for the Android-13+ notification permission), and
stopped on logout. Manifest declares a typed `dataSync` foreground service + `FOREGROUND_SERVICE`,
`FOREGROUND_SERVICE_DATA_SYNC`, `WAKE_LOCK`. Build-verified; on-device confirmation of background
wake (especially once the Activity is swiped away and the engine may detach) is still pending.

### 11.9 Dev observability (relay flow log)
A relay **flow log**, gated by a dev flag (`--dev` / `NIGHTDROP_RELAY_DEV=1`), **off in production**:
writes human-readable lines to **stdout + a tailable file** (`relay.log`), one per operation,
showing **everything the relay can see** — timestamp, op (`POST/PEEK/FETCH/RECALL/REAP`), handle
(truncated), `msg_id`, **blob size + short hash only** (never the bytes), ttl/expiry, per-handle
queue depth, result, source = "anonymous (Tor)". Doubles as proof the relay knows only opaque
handles/sizes/timings — no identities, content, or keys.

**TUI dashboard (planned dev tool):** a live in-terminal dashboard (Rust `ratatui` + `crossterm`)
layered on the **same event stream** as the flow-log — a header (onion address, uptime, queued /
reaped totals), a per-mailbox table (handle, depth, oldest age, sizes), and a live flow pane. It
is a thin presentation layer over the log (no extra relay logic), enabled by the same dev flag
and stripped from production. Build order: flow-log first (raw source), dashboard right after.

### 11.10 Invariant compliance
All of the above keep: opaque E2E blobs only, no server-side keys/logs, no persistent
identity-linked metadata (mailbox handles are unlinkable, capability tokens carry no identity),
opt-in + 24h cap + persistent warning for server storage, Tor by default. The one area needing
care is the **notification path**: keep it **content-free** (local notifications generated
on-device; the relay only ever returns a `peek` count). Timely push to a **killed** app (esp.
iOS) is a separate, later problem with its own metadata tradeoffs (see §6 / deferred).
