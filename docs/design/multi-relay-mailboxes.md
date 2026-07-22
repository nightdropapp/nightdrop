# Design — Self-hostable & multi-relay mailboxes

**Status:** ✅ **implemented** (`core/src/node.rs`: `my_relays`/`set_my_relays`,
`queue_on_relays`, `announce_relays`, `Frame::Relays`, `seen_relay_blobs` dedup; UI: "My relays…"
on the home menu). This document is retained as the design rationale.
**Relates to:** `ARCHITECTURE.md` §5–§6 (transport/relay), the non-negotiable invariants in
`CLAUDE.md` (no server-side keys/logs; Tor by default; local-first).

## 1. Goal & non-goals

Let users **run their own relay** and let a chat use **several relays redundantly**, for
availability and censorship-resistance — *without* adding trust, new metadata surface, or
relay-to-relay communication.

- **Goal:** a recipient chooses which relay(s) host their offline mailbox; senders fan out to
  that set; either side tolerates some relays being down or hostile-by-omission.
- **Non-goal:** relays are **not** an anonymity layer and do **not** gossip. Anonymity comes
  from Tor (onion-to-onion hot path). We do not chain relays into a home-grown onion network
  (smaller anonymity set + more latency — see the rationale note in the chat that spawned this).
- **Non-goal (v1):** automatic relay discovery/marketplace. Relays are addresses users paste or
  receive during pairing.

## 2. Why this is cheap and safe here

Two existing properties make federation nearly free:

1. **Mailbox handles are relay-agnostic.** `mailbox_handle(recipient_ik) =
   sha256("nightdrop/mailbox/v1" ‖ recipient_ik)` (`core/src/node.rs`). The **same** handle
   works on **every** relay — no per-relay addressing, no server-side account.
2. **Relays are already fully untrusted.** `relay_wrap(recipient_ik, frame)` seals every queued
   blob under a recipient-derived key; the relay sees only opaque bytes addressed by a hash. A
   new relay operator therefore learns nothing a sender didn't already accept the baked-in relay
   could see.

So "add a relay" reduces to "post/poll the same handle on more than one endpoint."

## 3. Model

- A chat's **offline delivery target is the *recipient's* relay set** (delivery is always *to*
  the recipient's mailbox). The recipient owns the choice; the sender posts to the recipient's
  advertised relays.
- Each device has:
  - `my_relays: Vec<RelayAddr>` — where *my* mailbox lives (what I advertise to contacts).
  - per-contact `peer_relays: Vec<RelayAddr>` — where *their* mailbox lives (learned during
    pairing / via in-band update).
  - a **baked-in default relay** stays as the bootstrap/fallback so first-contact pairing works
    out of the box and `my_relays` defaults to `[default]`.
- **No gossip.** Redundancy is pure **client-side fan-out**: sender posts each sealed blob to
  every relay in `peer_relays`; recipient polls every relay in `my_relays` and de-dups.

## 4. Wire / API changes

### 4.1 Advertising a relay set
- **Pairing payloads** carry the inviter's relay set:
  - QR: extend `nightdrop://pair?addr=…&ik=…&otk=…` with `&relays=<b64url csv of onion addrs>`.
  - Short-code path: include the relay set inside the (already E2E-sealed) invite response
    (`build_invite_response`), not in the rendezvous cleartext.
- **In-band updates** (mirrors onion rotation #11 / `Frame::Address`): new
  `Frame::Relays { from, message }` where `message` is the E2E-encrypted, comma-joined relay
  list. Receiver replaces `peer_relays` and shows an optional system notice. Sent by
  `announce_relays_if_changed()` on startup and on user edit, exactly like `announce_address`.

### 4.2 Node/relay surface
- Replace `relay: Option<RelayClient>` with `relays: Vec<RelayClient>` **for my own polling**,
  plus a way to build a `RelayClient` per `peer_relays` entry on the send path. A thin
  `RelayPool` wrapper keeps call sites tidy:
  - `RelayPool::post_all(handle, blob, ttl) -> Result<usize>` — post to every relay; **success
    if ≥1 accepts** (best-effort). Returns per-relay receipts.
  - `RelayPool::drain_all(handle) -> Vec<Vec<u8>>` — `take` from every relay, concatenated.
- `deliver()`'s relay-fallback branch posts via the **recipient's** pool (built from
  `peer_relays`), not a single global relay.
- `poll_relay()` drains **all** of `my_relays` and de-dups (see §5).

### 4.3 App/FFI
- `set_my_relays(Vec<String>)`, `my_relays() -> Vec<String>`, and validation
  `probe_relay(addr) -> bool` (reachable over Tor + speaks the protocol version).
- Contact DTO gains `peer_relays: Vec<String>` (display only).
- Settings UI: add/remove/reorder my relays; "run your own" help text; a per-relay reachability
  check. Persisted in the encrypted store.

## 5. De-duplication & receipts

The same sealed blob lands in N mailboxes, so the recipient must not process/notify N times.

- Each queued frame already rides inside a sealed blob; add a random **`post_id`** to the
  relay-wrap envelope (not the inner E2E frame). Recipient keeps a small **seen-`post_id` set**
  (bounded, TTL = relay max TTL = 24h) and drops duplicates on drain.
- **Delivery ack** (`Frame::Ack`) is E2E to the sender and already flips queued→delivered; it
  stays correct regardless of which relay the recipient drained from.
- **Unsend/edit recall**: `relay_receipts` becomes **per-relay** (recall the undelivered blob
  from *every* relay it was posted to). If recall fails on some relay, fall back to the existing
  `Edit`/`Unsend` frame path (already the behaviour when a receipt is lost).

## 6. Persistence
`PersistedState`/`PersistedChat` gain (all `#[serde(default)]` for forward-compat):
- top-level `my_relays: Vec<String>`
- per-chat `peer_relays: Vec<String>`
No key material changes; these are plain addresses.

## 7. Threat model delta

| Concern | Before (1 baked relay) | After (recipient-chosen set) |
|---|---|---|
| Read plaintext / keys | No (E2E + relay_wrap) | **No** — unchanged |
| Forge/inject | No (E2E) | **No** — unchanged |
| Drop mail (availability) | Single point of failure | **Mitigated** by redundancy (≥1 relay suffices) |
| See blob size/timing on hosted mailboxes | The one relay | Only relays the **recipient** chose; recipient controls exposure |
| Link mailbox handle → identity | Needs the identity key | **Unchanged** — handle is an unlinkable hash |
| Colluding relays | n/a | Can compare *handle activity* across the set, but handles stay unlinkable to identity; mitigate by not reusing the same custom relay across unrelated contacts if paranoid |

Invariants: **preserved.** No server-side keys/logs (relays still opaque); local-first
unchanged; Tor-by-default (relays dialed over Tor); no new trust delegation. The relay set must
travel only in QR or **E2E** frames — never in rendezvous/relay cleartext.

## 8. Phasing
1. **Configurable single relay** (unblocks self-hosting): surface the already-parameterised
   relay address in settings + `set_my_relays`/persistence; still one relay per chat. Small.
2. **Redundant multi-relay:** `RelayPool` fan-out + `post_id` de-dup + per-relay `relay_receipts`.
3. **Advertise & sync:** `relays=` in QR + `Frame::Relays` in-band update + `announce_relays_if_changed`.
4. **UX polish:** reachability probe, health/backoff, "run your own relay" docs (ties into
   `BUILD_AND_DEPLOY.md`, which already covers running `relay/`).

## 9. Open questions
- Cap on relay-set size (bound fan-out cost; suggest ≤ 4).
- Should a sender post to **all** peer relays every time, or the first that ACKs plus one spare?
  (All = strongest availability; spare-only = less load. Start with all, revisit.)
- Health/backoff policy for a persistently-down relay (don't wedge the poll cadence).
