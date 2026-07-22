# Run your own Night Drop relay

A Night Drop relay is a tiny, stateless **store-and-forward mailbox + rendezvous** service.
It exists so two people can pair by short code and so offline messages have somewhere to wait
(up to 24h). Everything it holds is an **opaque, end-to-end-encrypted blob** under an unlinkable
handle — it never sees keys, plaintext, identities, or addresses, and it keeps **no logs**. See
`ARCHITECTURE.md` §6 and `docs/design/multi-relay-mailboxes.md` for the design.

> For the big picture — the four ways relays reach a client, private vs. public relays, and the
> signed directory — see **[`../RELAYS.md`](../RELAYS.md)**. This file is the hands-on run guide.

Running your own relay (and pointing your contacts at it) means **no single operator is a
chokepoint** for your pairings or offline delivery (§3.1). Short-code pairing and delivery both
**broadcast across your configured relay set** (the baked-in default plus any extras), so losing
any one relay — to an outage or a block — doesn't stop new pairings, as long as both sides still
share one live relay.

## What you need

- A machine that can stay online (a $5 VPS, a Raspberry Pi at home, a spare box).
- The Rust toolchain (`rustup`).
- Outbound network access for Tor. **No inbound ports, no public IP, no domain, no TLS
  certificate** — the relay publishes its **own v3 onion service** via embedded `arti`, so it's
  reachable over Tor without any of that.

## Start it

```sh
# From the repo root:
NIGHTDROP_RELAY_STATE=/var/lib/nightdrop-relay cargo run --release -p nightdrop_relay
```

On first run it bootstraps Tor (can take a minute) and prints its address:

```
nightdrop-relay onion: <your-relay>.onion
```

The onion address is derived from a key kept in `NIGHTDROP_RELAY_STATE` (default: `./relay-state`),
so it stays **stable across restarts**. Back that directory up if you want to keep the same
address; delete it to rotate to a fresh one. The address is also written to
`<state>/onion` so tooling can read it without scraping logs.

Environment toggles:

| Var | Effect |
|-----|--------|
| `NIGHTDROP_RELAY_STATE` | State/onion-key dir (default `relay-state`). Determines the stable address. |
| `NIGHTDROP_RELAY_TUI`   | Run the live dashboard (metadata only — never blob bytes). |
| `NIGHTDROP_RELAY_DEV`   | Dev flow-log to stdout + `relay.log` (metadata only). Leave **off** in production. |

## Run it as a service (recommended)

For anything beyond a quick test, run it supervised so it restarts on crash and comes back
after a reboot. The installer builds the release binary and installs a hardened systemd unit:

```sh
# System-wide (VPS / dedicated box; needs sudo). State → /var/lib/nightdrop-relay.
relay/deploy/install-relay.sh

# Or per-user, no sudo (a personal always-on box). Keeps the onion in the repo's
# relay-state/ if present, so it matches the address baked into your app builds.
relay/deploy/install-relay.sh --user

relay/deploy/install-relay.sh --status      # service state + onion
relay/deploy/install-relay.sh --uninstall   # add --user to remove the per-user one
```

The committed unit lives at [`relay/deploy/nightdrop-relay.service`](deploy/nightdrop-relay.service)
(runs as a transient `DynamicUser` with a persisted `StateDirectory`, drops all capabilities,
and sets neither `NIGHTDROP_RELAY_DEV` nor `NIGHTDROP_RELAY_TUI` — no logs, no dashboard). The
`--user` mode generates the equivalent unit for the systemd *user* manager and enables linger.

## Use it in the app

In **Settings → Relays**, add your relay's `.onion` address to your **extra relays**. Your
contacts add it too (share it out-of-band). From then on, pairing and offline mail fan out to
your relay alongside the default — so your group isn't dependent on any single operator.

## Rotate the relay set without an app update (signed directory, §3.1)

Manually pointing every contact at a new `.onion` (above) is fine for a small circle, but if you
run the **default** relay baked into the app, losing its onion key would strand everyone. The
**signed relay directory** fixes that: a relay can serve an operator-signed list of the current
relays; the app fetches it on every poll, verifies it against a key **baked into the app**, and
migrates itself. A hostile relay can't forge the list (it isn't the signer), and a monotonic
`version` blocks rollback.

Set it up **once** per deployment:

```sh
# 1) Mint the signing key. Prints a PUBLIC key (paste into the app) + a PRIVATE key (keep secret).
nightdrop-relay gen-directory-key
#    → paste the printed array into core/src/directory.rs  DIRECTORY_PUBKEY, then rebuild the app.
```

Then, whenever the relay set changes (a relay rotated its onion, you added a backup relay):

```sh
# 2) Sign the current list. Bump <version> on every change.
nightdrop-relay sign-directory <private-key> <version> aaaa.onion bbbb.onion  > relay-list.json

# 3) Drop it on each relay you run (served automatically on next start / already-running pickup
#    is on next launch):
cp relay-list.json  "$NIGHTDROP_RELAY_STATE/relay-list.json"
```

Apps that can still reach **any** relay in the old or new set pick up the new list, verify it, and
adopt the new relays — so even if the primary's onion is gone, publishing from a surviving relay
brings everyone across. Until you bake a real key in, the feature is inert (the default all-zero
key verifies nothing), and nothing changes.

## What it does and doesn't see

- **Sees:** opaque sealed blobs, unlinkable mailbox/rendezvous handles, blob sizes, TTLs, and the
  fact that *someone* (an anonymous Tor client) posted or fetched. That's it.
- **Never sees:** encryption keys, message plaintext, who is talking to whom, real identities, or
  IP addresses (every client reaches it over Tor), and reaps expired blobs on a timer (server-side
  24h time-bomb).
- **Persists to disk** (in `NIGHTDROP_RELAY_STATE`): its onion key, and — by default — the
  **store-and-forward queue** (`queue.json`), so a restart or crash doesn't drop queued mail. Only
  the same **opaque, already-encrypted, time-boxed blobs under unlinkable handles** that were in
  RAM are written; anything past its TTL is dropped on load, so nothing identity-linked and nothing
  outliving its 24h window ever hits disk. Set `NIGHTDROP_RELAY_EPHEMERAL=1` for strict RAM-only
  (nothing but the onion key on disk).

The privacy invariants are not configuration — the relay has no code path that could log or persist
identity-linked metadata or plaintext. If you fork it, keep it that way.
