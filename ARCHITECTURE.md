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
- **One identity per install**, deliberately. Compartmentalisation — separate identities for
  separate contexts — is delegated to the operating system: a second copy of the app in an isolated
  profile (Samsung Secure Folder, an Android work profile, or a second user) gets its own storage,
  its own onion and its own contacts, protected by hardware-backed keys we cannot match in-app. An
  in-app switcher could not have offered deniability either, since arti stores the onion secret key
  as a plaintext file outside anything we encrypt, so a second identity is visible to anyone who
  images the device. Tested, with the one real limitation (no background delivery while the profile
  is locked, and mail expiring after 24h): `docs/multiple-identities.md`.

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

Because it is interactive, **the outstanding invite is persisted** (`pending_invites`, alongside
`pending_control`), not merely held in memory. Anything that rebuilds the core mid-pairing — the
guard heal, "Reset Tor connection", a restore — otherwise stops answering the rendezvous while the
inviter's screen still shows the code, so the joiner times out with "the inviter never answered"
and blames the wrong side. That was observed on 2026-08-03 between two fresh identities, which is
the worst case: a new identity's first descriptor publish is slow enough that the 150 s heal
reliably fires *during* pairing, so the people most exposed were new users on their first attempt.
The monotonic `Instant` expiry persists as unix seconds; on load, expired invites are dropped and
the remaining time is clamped to the invite's own TTL so a backwards clock jump cannot extend a
code's window. Side benefit: a short code now also survives an ordinary app restart within its TTL.

In both flows the recipient sees an explicit **authorization prompt** before the
first message is delivered.

**Pending state is persisted, and this is load-bearing.** A request awaiting approval is an ordinary
chat in the store, flagged unapproved. It was not flagged at all until 2026-08-02: the field did not
exist and restore assumed approval, so *any restart promoted an unapproved stranger to a contact* —
the invariant above held right up until the app was closed. A missing flag in an older state file
reads as approved, so upgrades don't demote real contacts. Pinned by
`a_pending_request_is_still_pending_after_a_restart`.

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
  because nothing crashed. The relay therefore proves its own reachability and exits after a
  sustained outage so systemd restarts it fresh (re-establishing intro points, republishing). A
  weekly `RuntimeMaxSec` backstops any slow degradation the watchdog misses. The onion keystore is
  preserved across restarts, so the address never changes. The relay runs **6 introduction points**
  (vs arti's default 3) and device onions run **4**, so losing an intro point to relay churn leaves
  the service reachable instead of dark.

  **Reachability is proved two ways, because one failure mode is invisible to each.**

  *An end-to-end self-dial.* The watchdog dials its own `.onion` over Tor every 5 minutes and runs
  one real `GetDirectory` through its own accept loop — HSDir lookup, introduction, rendezvous,
  response. Each probe uses a **freshly isolated client**, because arti caches a fetched descriptor
  per (service, client isolation): reusing the main client would answer probe after probe out of
  cache, and skipping the lookup is skipping the failure being hunted.

  *The publisher's ring check.* A probe resolves the service under the one time period its own
  consensus calls current, so a descriptor published to one HsDir ring and missing from the other
  is invisible to it — the probe reaches us over the ring that works while clients on the other
  ring find nothing. arti computes exactly this (`upload_result_state`) and reports it as
  `State::DegradedUnreachable`: "definitely not reachable by all clients". The watchdog reads that
  **one variant only**, never the aggregate's `is_fully_reachable()`.

  That distinction is the whole lesson. `is_fully_reachable()` is arti's summary of *bootstrap
  progress* and is wrong in both directions — it read healthy for ~90 minutes on 2026-08-02 while
  every client timed out, and it reads false on services that are serving perfectly, which is what
  drove the systemd restart counter into the thirties. But it was also *accidentally carrying a
  real signal*: on 2026-08-05 this relay restarted itself twice, and both times the journal shows a
  genuine one-sided outage (8/8 and 5/5 on one period, 0/2 on the other). Replacing the aggregate
  wholesale with a self-dial would have discarded a true positive. Reading `DegradedUnreachable`
  narrowly keeps it without the noise: `Bootstrapping` and `Recovering` — the states behind the
  false readings — are matched *ahead* of it during aggregation, so they can only ever mask it,
  never manufacture it.

  **A restart must be paid for with corroborated evidence**, because it rotates the introduction
  points and strands every client holding the current descriptor until it refetches. Two things
  veto one: a real client's stream served since the dark spell began (the service is live, so our
  probe is what's broken — and a failing probe can't forge this, since a success would end the
  spell), and arti reporting our own Tor client offline/filtered/unbootstrapped (a dial made
  through a broken client says nothing about our descriptor, and a restart can't fix someone
  else's outage). That client-status signal is read in this direction *only* — as a reason to do
  nothing, never as a reason to act; using it as a trigger is the same mistake in a different place.

  Both vetoes apply to the **self-dial only**. Neither may excuse the publisher's ring check: it is
  arti's account of its own uploads rather than a guess about the network, and clients still being
  served is precisely what a one-sided outage looks like from the inside — so honouring the veto
  there would mask the failure permanently. Either signal, sustained past `WATCHDOG_MAX_DARK`,
  restarts. The threshold is several probes wide because probe latency is genuinely high-variance
  (measured 3.4s to 56.8s on a healthy relay, and a cold start can time out entirely once before
  settling), so `PROBE_TIMEOUT` is set well past the worst healthy sample: a tight cap would not
  detect an outage sooner, only invent failures on a relay that is fine.
- **Wedged entry guards self-recover, on both the relay and the app.** After weeks of a persistent
  Tor state dir, arti's confirmed entry guards can churn out of the network; a client stuck on them
  can neither publish its onion nor reach the relay, and a plain re-bootstrap reuses the same
  guards, so it can't recover — this is the state that once had to be cleared by hand. Recovery is
  to delete `guards.json` while **keeping the onion keystore** (stable `.onion`) so the next
  bootstrap picks fresh guards, and **keeping `circuit_timeouts.json`**, which holds arti's learned
  circuit-build-time distribution and has nothing to do with guard choice — deleting it only drops
  the replacement client onto conservative defaults until it re-measures. The relay escalates to
  this automatically: the watchdog counts consecutive unhealthy restarts and, after a plain restart
  (which preserves guard stickiness and fixes intro-point wedges) fails to restore reachability,
  resets guards on the next start. The app does the same in-process, at most once per launch.

  **A guard reset is a last resort, and almost nothing is allowed to trigger one.** Entry guards
  bound the probability that a hostile relay ever becomes our entry, so they are meant to be sticky
  for weeks; rotating them spends real anonymity margin, and anything that can *cause*
  unreachability could otherwise force rotation.

  More to the point, arti repairs a bad guard set by itself. Measured 2026-08-04 with a router
  dropping every packet to all four of a device's confirmed guards while the rest of the internet
  stayed up: a cold start still bootstrapped, sampled a replacement guard, reached `Running` and
  published its descriptor — in about 80 seconds. An earlier measurement saw the incremental case,
  a single guard going unreachable and being replaced in 79 s without discarding the persisted set.

  So the only automatic trigger is `direct_path_wedged`: several sends have failed and *neither* the
  direct path nor the relay has ever succeeded this run. That is positive, end-to-end evidence that
  messages are not moving, and it is self-corroborating — a working relay proves Tor works, which
  makes the problem the peer rather than us. Everything else is the user's explicit
  "Reset Tor connection" action.

  **Two triggers were tried and removed, both for inferring "the guards must be bad" from something
  that was not evidence of it.** The first was "our onion descriptor hasn't published": arti's
  aggregate onion-service state is *bootstrap progress*, with `Bootstrapping` and `Recovering`
  outranking the reachable states in its combination rules, and `Recovering` documented there as a
  state in which the service "may be reachable". A phone with its descriptor on 8/8 HSDirs for both
  time periods, 4/4 introduction points and zero upload failures read as not-published for eight
  minutes, so the heal destroyed a healthy guard set 2.5 minutes into every session — and the
  replacement client, with no guards and no learned circuit timings, was slower than the one it
  replaced.

  The second was arti's own `bootstrap_status()`. It cannot answer this question either.
  `BlockageKind::CantReachTor` is unreachable in arti 0.43 — its match arm has no corresponding
  `ConnBlockage` variant — and the kinds that *are* reachable cannot be acted on, because `online`
  is derived from `last_tcp_success` across relay connections only. A dead guard set and a dead
  network are therefore indistinguishable from inside arti, and rotating guards because the device
  is in a lift spends anonymity margin on someone else's problem. The obvious discriminator —
  probing some non-Tor host to see whether the internet works — is barred by §6's own rule against
  hardcoding a non-anonymized network path. Since we cannot distinguish, we do not guess.

  `Transport::published` therefore answers a UI question only ("can others pair with me yet"), and
  is monotonic within a run, since a published descriptor stays valid on the HSDirs for hours.

- **One Tor instance per state dir.** arti takes an **exclusive on-disk lock** on its state
  directory, so at most one core may be live at a time. Any path that replaces a running core
  — restoring a backup, retrying a failed launch, creating a new identity after one, the guard
  heal above — must call `NightdropCore::shutdown()` first, which drops the transport *and* the
  relay (its dialer holds a clone of the same arti client) and so releases the lock
  synchronously. Dropping the core is not sufficient: the poller thread keeps the transport alive
  until its next tick, and across the FFI boundary the Rust object is only freed by a Dart
  finalizer at an unpredictable time. Two live instances fail with `State already locked`, which
  must never be reported to the user as a bad password.
- **Shutdown waits for the background threads, it does not just signal them.** Releasing the lock
  means *every* handle is gone, and two threads hold one past a teardown: the poller, which
  snapshots `RelayClient`s and drains them off the core lock (§1.5.2) — each clone carrying an
  `Arc<TorClient>` + the tokio runtime — and the Tor transport's idle-stream reaper, which holds
  that runtime (and therefore the arti tasks that own the lock) between sweeps. A replacement
  client built while either is alive comes up **read-only** ("Another process has the lock on our
  state files"), and a read-only client cannot persist the fresh guards a heal just picked — so
  the heal repeats, indefinitely. Both threads therefore sleep on a `StopSignal` (`lifecycle.rs`)
  and signal their exit, and both teardown paths wait for that exit **with a bound**: the same
  code runs the duress wipe, where hanging the app is worse than a late lock release, so an
  expiry is logged to diagnostics and execution continues. To keep the bound honest, an in-flight
  relay request is cancelled when the transport closes rather than running out its 30 s dial
  timeout.
- **No network I/O while holding the core lock.** Every blocking network step runs in three phases:
  snapshot what it needs under a brief lock, do the I/O with the lock released, re-acquire only to
  apply the result. That was true of the relay drain (§1.5.2) and is now true of **sends**
  (`plan_pending_sends` → `execute_sends` → `apply_send_outcomes`).

  A send is a peer dial (up to `PEER_DIAL_TIMEOUT`) plus, on failure, a relay post per target (up
  to `RELAY_DIAL_TIMEOUT` each), and it used to run inside `apply_tick` with the lock held. On a
  healthy network that is invisible. On a device whose circuits are timing out it pins the lock for
  minutes, and *everything* queues behind it — UI reads, and the teardown itself. Measured on a
  phone (2026-08-03): the user tapped "Reset Tor connection" and nothing happened at all, because
  `shutdown` could not take the lock the poller was holding for a dial that would never complete.
  The app was least able to recover exactly when it most needed to.

  Two consequences worth keeping: `shutdown` **tries** for the lock with a bound and proceeds
  without it rather than waiting (an unbounded acquire made "bounded shutdown" a lie), and the
  transport is held as an `Arc` so a send can carry a handle across the unlocked window.
  `service_pending_invites`, `flush_pending_control` and `refresh_directory` still do their I/O
  under the lock — the same treatment is owed to them.
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

### 10a. In-app update check — over Tor, to our own onion

Users who install from the onion site or from GitHub have **no update channel**: F-Droid and
Play users get told about a security fix, and everyone else does not. That is the gap this
closes, and it dictates the whole shape of the feature — it exists to carry security fixes, so
it must never become the thing that deanonymizes the user it is trying to protect.

**The path is Tor or nothing.** The app asks *our own* `.onion` for `/update.json`
(`core/src/update.rs`, `UPDATE_ONION`/`MANIFEST_PATH`) through
`Transport::onion_get`. The trait's default implementation returns `None`, and **`None` means
skip the check** — never fall back. There is no clearnet path to disable because there is no
clearnet path to begin with. Reaching a v3 onion authenticates the site by construction: the
address *is* the public key, so there is no CA in the trust path and nothing to spoof.

**The manifest cannot drift from what is served.** `scripts/gen-update-manifest.sh` (run by
`make config`) takes the version from `app/pubspec.yaml` and each APK's SHA-256 from the file
actually sitting in `website/applications/android/`. An APK that is not present is simply
omitted, which the app reads as "a newer version exists, but there is no download to offer" —
tell the user something, promise nothing.

**It must be this device's per-ABI build, never the universal one.** `--split-per-abi` numbers
the per-ABI APKs `base*10 + abi` (4031/4032/4033) and leaves the universal APK on the bare base
(403), so offering the universal build to anyone on a per-ABI install is a *downgrade by
versionCode* and Android refuses to install it — after the whole download, with no way for the
app to learn why. F-Droid ships per-ABI, so that is most users. `update::native_abi` picks from
the architecture the core was compiled for, which is by definition the ABI Android chose at
install time; the Dart side is not consulted because it cannot know better.

**Downloads are verified before they land.** `update::download` fetches that build,
hashes it, and writes to the destination **only** on a match. A file that exists is a file the
user may be one tap from installing, so a partial or mismatched download must never reach that
path. A mismatch is a hard error, not a retry: we asked an authenticated onion for a file it
told us the hash of.

**The app never installs.** Android verifies signatures itself and refuses to replace Night Drop
with anything not signed by our release key, so the worst a compromised site achieves is a
wasted download — not a swapped app. Adding an in-app installer would mean
`REQUEST_INSTALL_PACKAGES` on a privacy tool for a small delta; declined for now.

**It lands where the user can find it, without a permission.** Since the app does not install,
the file has to be reachable by hand — so the verified build is published to the **public
Downloads folder** via `MediaStore.Downloads` (`Downloads.kt`, `PublicDownloads`), which on API
29+ costs no permission at all. `getExternalStorageDirectory()` is *not* the cheap answer it
looks like: it returns a path under `/Android/data/`, which the system Files app and SAF-based
file managers have refused to navigate since Android 11, so a build written there is no more
findable than an app-private one. It remains only as the pre-API-29 fallback, where that
lockdown does not exist and the public folder *would* cost `WRITE_EXTERNAL_STORAGE` — a
permission the manifest deliberately strips.

The download is staged app-privately and only published after the hash matches, so the ordering
invariant holds end to end: **nothing the user can see is unverified.** The MediaStore copy runs
with `IS_PENDING` set for the same reason, and a failed copy deletes its row rather than leaving
a truncated entry. If no route works the file stays where it is and the user is told that path —
a verified build is never discarded over a publishing problem.

**Streamed, not buffered.** `Transport::onion_get_to_file` writes the build to disk as it
arrives, so resident memory stays at one 16 KiB buffer instead of the whole ~45MB — which
mattered because those minutes are exactly when the app is backgrounded and the low-memory killer
is picking a victim. The bytes go to a `<dest>.part` sibling, are hashed by reading the file back,
and are `rename`d into place only on a match; rename within a filesystem is atomic, so no partial
build is ever visible under the final name, and every failure path deletes the scratch file rather
than leaving tens of megabytes behind. Streaming means the response head must be parsed
incrementally rather than split out of one buffer — `update::ResponseHead` does that, and lives in
`update` rather than the transport so the chunk-boundary cases (including a `\r\n\r\n` straddling
two reads) are testable without a live circuit.

**Bounds and lock discipline.** The manifest read is capped at `MAX_MANIFEST_BYTES` (8 KiB) so a
routine background fetch can never OOM the app; the build download has its own, much larger cap
(`MAX_DOWNLOAD_BYTES`) via `onion_get_capped`, kept separate precisely so one cap large enough
for an APK does not silently unbound the every-24h fetch. Timeouts are generous, not tight
(§6: measured onion connect latency ranged 3.4s–56.8s on a healthy network) — a tight cap does
not fail faster in any useful sense, it just turns a slow circuit into "no update exists", which
is the one answer this must never give wrongly. All of it runs off the core lock via
`Node::transport_handle`.

**A failed check says so.** Reporting "up to date" when the site did not answer is a confident
lie on the one screen where the user deliberately asked, and it hides exactly the security fix
the feature exists to surface. `checkForUpdateNow()` returns whether the site answered, and the
UI distinguishes the two.

**A download holds the process at foreground priority** for its whole duration
(`BackgroundDelivery.holdDuring`), because a multi-minute Tor transfer is otherwise fair game for
Doze and App Standby to freeze partway through. Two details are load-bearing: the hold ignores the
background-delivery opt-in, since declining passive message delivery is not declining to finish a
download the user just started; and it is taken while the app is still foreground from the tap,
because Android 12+ refuses to start a foreground service once the app has left. It is
best-effort — if the service cannot start, the download still runs.

Known limit: streaming to `.part` makes *resume* possible — the partial file is right there — but
that is not implemented, so a failed download restarts from zero.

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
- **Send while peer reachable:** the direct dial succeeds → mark message **sent**, badge "Not
  confirmed yet" (a clock, never a tick). This is *not* delivery: the dial proves their onion
  service answered, not that their app processed the frame.
- **Sender gets a per-message receipt (`Frame::Delivered`):** badge `sent|queued → delivered`, for
  **that message id only**. This is the *only* transition into `delivered`.
- **No receipt within `RECEIPT_TIMEOUT` (30 s):** the sender re-seals the message under the **same
  id** and queues it on the relay → badge `sent → queued`, and the ordinary relay lifecycle takes
  over. Once per message. A peer that already has it discards the copy as a duplicate **and
  receipts it**, which is what settles the sender; a peer too old to send receipts never confirms,
  so each of its messages costs one relay copy and shows twice on their screen — the safe direction
  to be wrong in.
- **Sender gets `Ack`:** nothing. See below.

**Only a named receipt confirms a message (2026-08-02, revised on review).** `sent` used to be
terminal and drew no badge, so a message that was merely dialled looked exactly like one that had
arrived — and on a device, one was lost when the core was torn down with the frame still buffered,
while the *next* message arrived normally. Olm decrypts out of order and a gap does not block later
messages, so a later frame is no evidence about an earlier one.

Naming the message in the receipt was necessary but not sufficient: **three** other signals were
still promoting messages to `delivered` on the strength of "the peer is alive" — a direct dial
succeeding, a message arriving from them, and their relay `Ack`. Each could mark a message the peer
had dropped or never collected. All three are gone; `Frame::Delivered` is the sole confirmation.
`Ack` (mailbox-drained, no id) is still **sent**, because a peer on an older build understands only
that, and still **received** as proof of life — but it confirms nothing.

An unconfirmed message is never simply abandoned, either: that is what the relay fallback above is
for. The end state of a message is always one the sender can trust — `delivered` (receipted),
`queued`/`expired` (the relay's own lifecycle), or visibly unconfirmed.

**Receipts are built after decryption, by the handler that accepted the frame** — not from the frame
beforehand. Only `Frame::Message` carries its id in the clear; an attachment's lives inside the
envelope, which is why media had nothing to confirm it. An attachment is receipted by its
`transfer_id` (the id both sides already share, tagged `t:` so it cannot be mistaken for a text
`msg_id`); putting a `msg_id` into the media envelope instead would corrupt every attachment for
peers on older builds, since the payload is trailing data after three length-prefixed fields.
Edits and unsends still carry no receipt — they have no badge of their own.

**A message can arrive twice.** With server storage on, every message is sent directly *and* copied
to the relay, so both reach the receiver; a relay fallback resend is a second copy by construction.
Two layers handle it: identical frames are recognised by hash across **both** intake paths (a replay
of a spent Olm message key cannot decrypt, so this must happen before the ratchet sees it), and a
re-sealed resend — different bytes, same id — is caught by id at the point of acceptance and
receipted without being shown. Frame-hash dedup applies only to user-content frames: several control
frames carry no per-instance ciphertext, so a legitimate repeat is byte-identical (`Approved` on a
re-pair is the one that bites). And a frame that will not process never aborts a relay drain — those
blobs are already destructively taken, so failing the batch loses the rest for good.
- **24h passes, never collected:** badge `queued → expired` ("not delivered").
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
