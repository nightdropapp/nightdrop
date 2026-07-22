# Relays — what they are, how they work, and how to run your own

A **relay** is the only server Night Drop ever talks to, and it is deliberately as dumb and
untrusted as possible. This document explains what a relay does, the four ways relays reach a
client, and how **anyone** can run one — for themselves, for their contacts, or for the public.

If you just want to *use* Night Drop, you never need a relay: the app ships with one baked in. This
document is for people who want to **run** relays or understand the trust model.

---

## 1. What a relay is (and what it is not)

A relay is an **untrusted box that holds opaque, already-encrypted blobs** under short-lived,
unlinkable handles. It plays two roles with one store:

- **Rendezvous mailbox** — first contact by short code. The two sides drop SPAKE2-encrypted blobs
  under a slot handle and drain them once. The relay is just a dead-drop; all security is in the
  secret words, never in the relay.
- **Store-and-forward** — when the recipient is offline (or opts into 24h server storage), the
  sender queues an encrypted blob under the recipient's (unlinkable) mailbox handle until they
  drain it. Capped at 24h, then a background reaper deletes it.

It publishes **its own v3 Tor onion service**, so it's reachable from any network (LTE, NAT, no
port-forwarding, no domain, no TLS certificate).

### The invariants — what a relay can and cannot see

| The relay **sees** | The relay **never sees** |
|---|---|
| Opaque sealed blobs (ciphertext) | Encryption keys or message plaintext |
| Unlinkable mailbox / rendezvous handles | Who is talking to whom, or real identities |
| Blob sizes and TTLs | IP addresses (every client arrives over Tor) |
| That *some* anonymous Tor client posted/fetched | Anything that outlives its 24h TTL (reaped) |

These are **not configuration** — there is no code path in the relay that logs or persists
identity-linked metadata or plaintext. A relay operator, even a malicious one, learns none of the
above. This is why it's safe to use a relay you don't control, and why running one for others
carries little responsibility: you are hosting encrypted noise.

Persisted to disk (in the relay's state dir): its **onion key** (so the address is stable) and, by
default, the **store-and-forward queue** (`queue.json`, so a restart doesn't drop queued mail — only
the same opaque, time-boxed blobs that were in RAM). `NIGHTDROP_RELAY_EPHEMERAL=1` makes it strict
RAM-only.

---

## 2. The four ways a relay reaches a client

Night Drop clients can learn about relays through four channels, which stack. A message or pairing
**fans out across every relay a client knows** (seal once, post the identical blob to all; the
recipient de-duplicates by content hash and a recall removes every copy). More relays = more
availability and censorship-resistance. Relays never gossip and this is **not** an anonymity layer —
anonymity always stays with Tor.

| # | Channel | Who controls it | Trust anchor | Good for |
|---|---------|-----------------|--------------|----------|
| 1 | **Baked-in default** | The app operator (build-time `--dart-define=NIGHTDROP_RELAY`) | Shipped in the app | The out-of-the-box shared relay |
| 2 | **Signed directory** | The app operator (holds the directory key) | Ed25519 key baked into the app | Rotating / expanding the *public* default set without an app update |
| 3 | **Your extra relays** (`my_relays`) | You, per-device | Announced in-band to your paired contacts | A relay for **you and the people you talk to** |
| 4 | **A contact's relays** (`peer_relays`) | Your contact | Learned in-band from them | Reaching a contact via *their* relay |

Channels 3 and 4 are two sides of the same in-band mechanism (#17): when you set your extra relays,
the app announces them to your contacts over the authenticated E2E channel; they hold them as your
`peer_relays` and fan your offline mail out to them. **No stranger ever learns a relay you added
this way.**

---

## 3. Running your own relay (the basics)

Anyone can run a relay. You need a machine that stays online (a $5 VPS, a Raspberry Pi, a spare box)
and the Rust toolchain — **no inbound ports, no public IP, no domain, no TLS**.

```sh
# Supervised install (systemd), stable onion, restarts on crash + across reboot:
relay/deploy/install-relay.sh          # system-wide (VPS, sudo)
relay/deploy/install-relay.sh --user   # per-user, no sudo (a personal always-on box)
```

It prints its address on first bootstrap:

```
nightdrop-relay onion: <your-relay>.onion
  mode: PUBLIC — reachable by anyone with the address
```

Full details, hardening, and env toggles: **[`relay/README.md`](relay/README.md)**.

Now pick how people should reach it — the next three sections are the three real deployment shapes.

---

## 4. A relay for you and your contacts (private to your circle)

**Goal:** you self-host a relay that only you and the people you message use. This is the most common
self-hosting case, and most of it works with **no special setup** — it's channel #3 above.

### 4a. The simple version (private by obscurity of the address)

1. Run a relay (section 3), note its `.onion`.
2. In the app: **Settings → My relays**, add the `.onion`, save.
3. The app announces it in-band to your contacts; from then on your pairings and offline mail fan
   out to your relay alongside the shared default.

The relay's address is only ever shared over your authenticated contact channels, so **strangers
don't learn it exists**. The relay still holds only opaque blobs, so even if someone found the
address, they'd learn nothing — but they *could* use it as free storage.

### 4b. The locked-down version (restricted discovery — §3.2)

If you want the relay to be **unusable by anyone outside your circle** — not just undiscoverable —
turn it **PRIVATE**. This uses Tor **restricted discovery**: the relay encrypts its onion descriptor
only to authorized clients, so an unauthorized client can't even find the service, let alone post.
It authenticates at the **Tor layer** with per-device x25519 keys and never learns any chat identity
— the no-server-identities invariant is intact.

**One-time, per device that should have access (you + each contact):**

```
# In the app (on each device): generate this device's access key for the relay.
#   → Settings surfaces createRelayAccessKey(<relay.onion>), which prints:
#     descriptor:x25519:XXXXXXXX…
```

**On the relay (operator authorizes each key, SSH-`authorized_keys`-style):**

```sh
nightdrop-relay authorize-client alice-phone  descriptor:x25519:XXXXXXXX…
nightdrop-relay authorize-client my-laptop     descriptor:x25519:YYYYYYYY…
nightdrop-relay list-clients                    # count + mode (PUBLIC/PRIVATE)
nightdrop-relay revoke-client alice-phone       # remove access later
```

- The **first** authorization flips the relay PUBLIC→PRIVATE; restart it once to apply.
- Later authorizations/revocations are picked up **live** (the directory is watched).
- Revoking the **last** client returns it to PUBLIC (restart to apply).
- arti stores each device's private key locally and presents it automatically on future dials, so
  once authorized, the device just works. The key travels in the device's encrypted backup.

**How keys get to the operator (v1):** manually — the contact reads their `descriptor:x25519:…`
string to you (in-chat, out-of-band, however), and you authorize it. This mirrors how self-hosted
SSH/WireGuard access works. Automatic in-band distribution (your contacts get authorized over the
already-paired channel) is a planned enhancement; it's deferred because doing it safely needs either
the relay co-located with your app or an authenticated remote-authorize operation, and we won't add
an admin credential to the relay lightly.

> **Trade-off:** a PRIVATE relay is unusable by not-yet-paired people, so it **cannot serve as a
> rendezvous mailbox for first contact by short code** with someone new — short-code pairing needs a
> relay both sides can reach *before* they're paired. Keep at least one PUBLIC relay (e.g. the baked-in
> default) in your set for first contact; use the PRIVATE relay for ongoing store-and-forward.

---

## 5. A public relay for anyone to use

**Goal:** you run a relay and want to help the whole network — anyone can use it.

Just run it (section 3) and leave it PUBLIC (the default — no authorized clients). Then get people to
add it. Two ways:

- **Ad-hoc:** share the `.onion` out-of-band; people add it under **Settings → My relays**. No
  blessing needed — it's their choice to trust your relay for availability (they lose nothing if it's
  malicious, since blobs are opaque).
- **Official default:** if you run *the* app deployment, add it to the **signed directory** (next
  section) so every app picks it up automatically. A third party can't inject a relay into the signed
  list — that gate is what stops a malicious relay from making itself an app-wide default. The list
  is a curation decision by whoever holds the directory key.

Abuse is bounded by the relay's flood limits (`RelayLimits`: per-blob, per-mailbox, and total byte
caps; 24h TTL). A public relay is hosting time-boxed encrypted noise for anonymous clients.

---

## 6. The signed directory — rotating the public default set (§3.1)

The signed directory lets the app operator **change the shared relay set without shipping an app
update**. Relays serve an operator-signed list; the app fetches it on every poll from whatever relay
it can still reach, verifies the signature against a key **baked into the app**, and adopts the
relays if the list's version is newer. This closes the "lost the primary relay's onion key → everyone
stranded" failure: publish a new signed list from any surviving relay and every app migrates itself.

### One-time setup (per deployment)

```sh
nightdrop-relay gen-directory-key
#  → paste the printed PUBLIC key into core/src/directory.rs DIRECTORY_PUBKEY, rebuild the app.
#  → the PRIVATE key is saved to <state>/directory-signing-key — keep it secret AND backed up.
```

### Whenever the relay set changes — add, remove, or rotate relays

```sh
# Sign a new list containing EVERY relay you want active (it's a full replacement), auto-bumping
# the version, and restart the local relay to serve it:
relay/deploy/sign-directory.sh --restart  relay-1.onion  relay-2.onion  relay-3.onion
```

Then **copy the resulting `<state>/relay-list.json` to every relay's state dir** (the script prints
the exact `scp` lines). Distributing to all relays is what makes rotation resilient: a client that
can only reach a surviving relay still gets the update.

Key rules:

- **Include every active relay each time** — omitting one *removes* it from clients on their next poll.
- **Bump the version** — the script does this automatically (clients ignore anything not newer;
  this is the rollback protection).
- **Guard the private key** — it's the linchpin. Lose it and you can't sign updates, which forces
  the very app-update-to-change-relays situation the directory exists to avoid. Back it up alongside
  each relay's onion key.

`install-relay.sh` also copies a present `relay-state/relay-list.json` into the deployed state dir, so
a fresh relay serves your directory from first boot.

---

## 7. Which kind should I run? (decision guide)

| You want… | Do this |
|---|---|
| To just message people | Nothing — the baked-in relay works |
| A relay for **you + your contacts**, minimal setup | Run a relay, add it under **My relays** (§4a) |
| A relay only your circle can **use**, not just discover | Run it PRIVATE with `authorize-client` (§4b) |
| To help the network, anyone welcome | Run it PUBLIC, share the address (§5) |
| To change the app's **default** relays for everyone | Sign it into the directory (§6) — needs the directory key |
| Redundancy so no single relay is a chokepoint | Run several and list them all (§6), or add several under My relays |

---

## 8. Reference — operator commands

All are subcommands of the `nightdrop-relay` binary and run **without** bootstrapping Tor.

| Command | Purpose |
|---|---|
| `gen-directory-key` | Mint the directory signing keypair (once per deployment) |
| `sign-directory <priv> <version> <onion…>` | Sign a relay list (or use `relay/deploy/sign-directory.sh`) |
| `authorize-client <name> <descriptor:x25519:…>` | Authorize a device on a PRIVATE relay |
| `revoke-client <name>` | Revoke a device's access |
| `list-clients` | Count authorized clients + show PUBLIC/PRIVATE mode |

Client-side (in the app, `NightdropCore` FFI): `createRelayAccessKey(relayOnion)` mints this device's
access key for a PRIVATE relay and returns the public `descriptor:x25519:…` to hand the operator.

State dir layout (`NIGHTDROP_RELAY_STATE`, default `relay-state/`):

```
relay-state/
├── arti-state/, arti-cache/     # Tor state; the onion key lives here (stable address)
├── onion                        # the published .onion (convenience, written on start)
├── queue.json                   # persisted store-and-forward queue (opaque blobs; unless EPHEMERAL)
├── relay-list.json              # the signed directory this relay serves (optional, §6)
├── directory-signing-key        # operator's PRIVATE directory key (secret! optional, §6)
└── authorized-clients/*.auth    # authorized client keys → PRIVATE mode (optional, §4b)
```

Everything under `relay-state/` is gitignored. Back up the onion key (to keep your address), the
`directory-signing-key` (to keep signing), and — if you run a PRIVATE relay — `authorized-clients/`.

---

## 9. Design notes & pointers

- The trust model, threat model, and rationale live in **[`ARCHITECTURE.md`](ARCHITECTURE.md)** §6
  (transport/relay), §3.1 (signed directory), §3.2 (private relays), and #17 (multi-relay fan-out).
- Multi-relay mailbox design: `docs/design/multi-relay-mailboxes.md`.
- Onion client authorization (the restricted-discovery machinery private relays reuse):
  `docs/design/onion-client-auth.md`.
- The relay authenticates clients **only at the Tor layer** (x25519 restricted-discovery keys),
  never with chat identities — so "no server-side keys, no identities" holds even for PRIVATE relays.
