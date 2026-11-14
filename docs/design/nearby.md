# Design — Nearby: a chat that also connects over the same Wi-Fi (and, later, Bluetooth)

**Status:** Wi-Fi (§1–§9) agreed 2026-10-18 and being built for 0.2.2 (0.2.1 was a security-only release). Bluetooth (§10) is design only.
**Decided by:** the maintainer, 2026-10-18: "give [the LAN transport] a screen to allow it through
Wi-Fi/Bluetooth"; opt-in **per chat**, off by default, both sides must turn it on, used only when
both are on the same network, Tor stays the default, a banner while it is on; Wi-Fi now, Bluetooth
designed.
**Relates to:** `ARCHITECTURE.md` §1a (Tor by default; this is the **second approved direct path**,
recorded there), §6, `docs/trust-boundaries.md`, `core/src/transport/nearby.rs`,
`core/src/node/nearby.rs`.

## 1. What it is for

Two contacts in the same room, on the same Wi-Fi (a home network, a field hotspot, a router with no
internet): messages go device to device over the local network, without Tor. It is faster, works
when the internet or Tor is down, and costs no relay. It is **not anonymous**: anyone watching that
network sees two devices talking (never what), and each of you learns the other's local address
and that you are near each other. So it is off by default and turned on per chat, by both people.

## 2. What it is not

* Not a replacement for Tor. Tor stays the transport and the onion stays the address; Wi-Fi is an
  extra path tried first for one contact, with Tor behind it.
* Not discovery of people. What is broadcast (§5a) is a per-chat beacon only that contact can
  recognise, and that never repeats; a contact's local address is learned only from that contact,
  over the end-to-end session (§4) or from that beacon.
* Not app-wide. Turning it on for one chat reveals nothing to any other contact.

## 3. Consent and the user-visible states

Chat menu: **"Also connect on the same Wi-Fi"**, behind a dialog that says what it reveals:
"Your contact learns your device's address on the local network, and anyone on that network can see
that your two devices are talking (not what you say). Messages stay end-to-end encrypted. Both of
you need to turn this on." Since the beacons (§5a) it also says that for two minutes after Nearby
starts or joining a network the device sends a signal there that only the contact can tell is
theirs (on any network where they have a device), and that shows others a Night Drop with this
setting on is present (`nearbyBody` in `app_en.arb`). Turning it off
is immediate and needs no confirmation.

Per chat, the banner shows one of:

| Ours | Theirs | Banner |
|---|---|---|
| off | off | — |
| off | on | "They'd like to also connect on the same Wi-Fi" (with the menu item to turn it on) |
| on | off | "Same Wi-Fi: waiting for them to turn it on" |
| on | on, not reachable | "Same Wi-Fi: on — not on the same network right now" |
| on | on, reachable | "Same Wi-Fi: connected" |

The chat history also records each change: ours ("You turned on/off…"), their request when ours is
off ("They'd like to also connect on the same Wi-Fi…"), and their turning it off ("They turned off…"),
so an old request is never left as the last word once they have withdrawn it.

## 4. Protocol

A standing announcement on `Frame::Captures` (already E2E-authenticated, decrypted once, announced
on change and re-sent on a contact's update or approval like the capture block):

```
nightdrop/ctl/nearby/v1:<seq>:0                        — off
nightdrop/ctl/nearby/v1:<seq>:1:<token>                — on, but no local network right now
nightdrop/ctl/nearby/v1:<seq>:1:<ipv4>:<port>:<token>  — on, listening at ipv4:port
nightdrop/ctl/nearby/v1:<seq>:1                        — on, no token (development builds only)
```

* `seq`: the sender's increasing sequence (milliseconds), as for the capture block; an older or
  equal one is ignored.
* `token`: 32 random bytes, base64url, **per chat**, drawn when the chat turns Nearby on and
  redrawn every time it does. It never crosses the local network. It remains the legacy connection
  proof and beacon key for peers that have already exchanged this announcement.
* `discovery key`: a separate key derived with HKDF from the pair's confirmed v2 mailbox secret,
  which new pairs receive in the encrypted pairing Hello. It is derived independently on both
  devices, retained with the sealed mailbox state, and never transmitted as a Nearby control
  message. It lets an enabled device make a beacon before the peer's Nearby-on announcement can
  arrive over Tor.
* Re-announced when our local address changes (checked each tick).
* 0.2 apps ignore an unknown `Captures` marker, so this needs no wire version.

A contact's announcement is accepted only if the address is a private IPv4 (10/8, 172.16/12,
192.168/16, 169.254/16) and the port is 32768 or above (the ephemeral range our listener binds in);
anything else is ignored. **Without the address rule, a contact could announce a public address and
make our app connect to it outside Tor, revealing our public IP.** Without the port rule, they could
make it send its opening line to a service on our network (a printer's raw port, a router's admin
page). Changes count against the daily allowance like other control frames.

## 5. The local connection

The dialer connects to `ipv4:port` (connect timeout 2 s) only when **our** chat has Nearby on, a
legacy announcement or an authenticated beacon has supplied the contact's address, and our own
local address is in the same subnet (/24 for 192.168, /16 for 172.16/12 and 169.254, /8 for 10).
The listener exists only while its own user has Nearby on for at least one chat. Then:

A challenge-response on either the common discovery key (`NDB2`) or the chat's two per-toggle
tokens (`NDB1` legacy), with
`proof(t, role, Nd, Nl) = HKDF-SHA256(salt "nightdrop/nearby/proof/v1", ikm t, info role|Nd|Nl)`,
32 bytes, base64url (43 characters); nonces are 16 random bytes, base64url (22 characters):

1. dialer → listener: `ND-NEARBY/3 ` + `Nd` + `\n`.
2. listener → dialer: a fresh `Nl` + `\n`.
3. dialer → listener: `proof(key, "D", Nd, Nl)` + `\n`.
4. listener: check the proof against locally enabled chats' discovery keys and known legacy tokens
   (each checked in constant time); if none matches, close. Otherwise reply `proof(key, "L", Nd,
   Nl)` + `\n`.
5. dialer: if the reply is not that proof of its own token, close and fall back to Tor.
6. Frames flow as on any transport: length-prefixed, padded, end-to-end encrypted.

Nothing a listener on the network sees is reusable: the listener's nonce is new each connection,
so a recorded proof does not open another one, and neither the discovery key nor either legacy
token crosses the network. Until 2026-11-09 (never shipped) the opening line was a fixed
per-chat value: anyone who saw one connection could replay it to push frames at the listener or
hold its 16 connections, and its sameness linked a chat's connections over time.

The exchange filters strangers and wrong hosts (another device holding the same private IP on a
different network); it is not the security. Content, sender and integrity are the Double
Ratchet's, as over Tor. An active attacker on the same network can still relay a connection
between the two or drop traffic, never read or forge a message; a dropped message falls back to
the relay when its receipt does not arrive (30 s), as today.

The listener runs only while at least one chat has Nearby on, binds an ephemeral
port on all interfaces, answers only private addresses in our subnet (16 connections at most), and
keeps its own 16 MiB `InboundBudget`.

**Probing and back-off.** Each tick, a route not heard from for a minute is probed: the token
exchange alone, no message, and each route at most once a minute whatever came of it (the local
side runs every poll tick, and a probe of a contact on another network ends before connecting,
recording no failure). A contact found by beacon, a contact's beacon while they are not known
reachable, or a change of our own networks lets the next probe go at once (and clears the
back-off). The chat's "connected" state is a success either way in the last two
minutes. After a local attempt fails, sends skip the local path for a minute — otherwise each would
wait out the 2 s connect first when the two are apart.

## 5a. Finding each other without the internet (beacons)

The old `NDB1` beacon relies on §4's Nearby-on announcement to exchange its per-toggle token over
Tor. That meant first enabling Nearby while offline could not discover the other phone. New chats
already share a secret from the encrypted pairing Hello, so a separately derived discovery key
allows a beacon to work before that announcement. While the local chat has Nearby on, each device
broadcasts an `NDB2` beacon on each usable local network, whether or not the peer's Nearby-on
announcement has arrived:

```
"NDB2" | nonce (8 random bytes) | port (2, big-endian) | tag (16)
tag = HKDF-SHA256(salt "nightdrop/nearby/beacon/v2", ikm = discovery key,
                  info = window (8) | sending IPv4 (4) | port (2) | nonce)[..16]
window = unix time / 600 s
```

The discovery key is HKDF-derived from the shared pair secret and never appears in a beacon. When a
peer's legacy per-toggle token is known, the app also sends `NDB1` for compatibility with 0.2.2.

* Sent to port 41873 at the network's own broadcast address (taken as the /24) and at
  `255.255.255.255`, from a socket bound to each usable interface's address. The limited broadcast
  alone never reached a phone on another phone's hotspot (S23 → S25, 2026-11-09); the subnet one
  follows Android's route for the hotspot. One per chat where **our side** has Nearby on, every 5 s
  for **two minutes** after listening starts or our networks change, **then none**; and once (at most
  every 5 s) after we find a contact, who may not have found us. The discovery key is available
  from pairing, so this burst can start before we know the peer's Nearby setting. A device staying
  on a network does not keep telling it a Night Drop is there: a contact who arrives later starts
  their own burst, and our answer follows within a second. The listener still runs only while at
  least one local chat has Nearby on.
* Received on UDP 41873 (shared with any other Night Drop on the device, `SO_REUSEADDR`), only from
  a private address in our subnet, at most 50 checked a second (the rest dropped unread). A beacon
  is checked against each locally enabled chat's discovery key; legacy beacons are checked against
  the peer's per-toggle token once it is known. Both checks cover the window before, current, and
  next (clock skew), in constant time. A match authenticates the chat and names where that contact
  listens: the sending address and port in the beacon, which must still be 32768 or above. A packet
  sourced from one of our interface addresses and naming our own listening port is discarded,
  since the shared discovery key also authenticates our own broadcast. Since only a device with its
  own Nearby setting on listens, both people must opt in before connecting.
* What a beacon finds is held in memory only and used to dial (§5) in place of the announced
  address; the contact's next announcement over Tor replaces it. Finding a contact also resets the
  back-off, so the next send tries the new address. An address we dialled successfully in the last
  minute is kept (the contact connecting to us says nothing of where they listen; counting it kept
  a stale port for 44 s on the S23/S25, 2026-11-09): a contact on two of our networks beacons from both, and switching on each would have the
  two devices answering each other without end.
* **What it shows.** To anyone else a beacon is 30 random-looking bytes that never repeat (fresh
  nonce), carrying no identity, onion, or key: a listener learns that some device on the network
  runs Night Drop with Nearby on, and its port, during the two-minute burst after Nearby starts or
  the device joins a network (and when it answers a contact). An observer cannot identify the chat
  from the tag. Within one network the device's address and port already link its traffic.
* **What the contact can do with it.** The contact holds the pair's discovery key, so a device of
  theirs left on any network can recognize the beacon there and learn when we join that network,
  not only when we are in the same room. This is bounded by the two-minute bursts and by leaving
  Nearby off locally; the listener is not started unless the local user enabled the feature.
  `SECURITY.md` says so.
* **What it does not allow.** Forging one needs the contact's token, which never crosses the
  network. A recorded beacon replayed from another address does not verify
  (the address is in the tag); replayed from the same address within the window it says nothing
  new. A beacon only chooses where we dial: the connection still has to pass §5's exchange, and
  everything on it is end-to-end encrypted.
* **Needs:** an existing chat whose devices share the discovery key from pairing (or a completed
  v2 mailbox-key agreement for an older chat), both people locally turn Nearby on, and both devices
  join the same broadcast-capable local network. No internet is needed for beacon discovery or the
  direct connection. A chat without a shared discovery key still needs its Nearby announcement to
  reach the peer over Tor before the legacy beacon path works.

## 6. Transport shape

`NearbyTransport` wraps the Tor transport. `address()` is the onion. `send(peer_onion, frame)`: if a
usable route for that onion exists (§5), try it; on any failure, send over Tor. Inbound frames from
both paths go to one queue; frames identify their sender themselves, so the core needs no change in
how it routes them. The node tells the transport its routes (onion → address, tokens) whenever a
chat's Nearby state changes.

## 7. Persistence and backups

Per chat: `nearby` (ours, bool), `nearby_token` (ours), `peer_nearby` (their address and token, or
none), `peer_nearby_seq`. Persisted and backed up like the capture block. A restore turns our side
**off** (the address and network are the old device's) and tells the contact.

## 8. Trust boundaries

Rows in `docs/trust-boundaries.md`: the nearby marker — exact prefix, bounded length, private
IPv4 only, port 32768 or above, token exactly 32 bytes, sequence newer than the last, counted
against the allowance; the local connection's preamble — fixed length, compared in constant time;
and the beacon (§5a) — exact length, our subnet only, tag checked in constant time, port 32768 or
above.

## 9. Platforms

Plain TCP and UDP sockets: Android (INTERNET only; Android 17's local-network permission must be
checked before 0.3 targets it), Linux, Windows (the firewall may prompt on first listen, for the
TCP listener and the beacon port). The listener, dialer and beacons are Rust (§1a: transport stays
in the core). Some Android Wi-Fi drivers drop broadcasts while the screen is off; a beacon missed
then is sent again, and the phone that is awake finds the other.

### 9.1 Two-device verification (2026-11-13)

Verified on a Galaxy S23 and S25 running the ND RC build. With both on the same Wi-Fi, messages
and photos were exchanged. Then the S23's Wi-Fi was turned off (it has no mobile data), its
hotspot was enabled, and the S25 joined that hotspot. After closing and reopening Night Drop on
both devices, they showed **Connected directly** in about three seconds. Messages and photos then
sent and arrived quickly, often nearly instantly. Both devices were returned to the home Wi-Fi.

This confirms the no-internet hotspot path end to end, including beacon discovery after relaunch,
direct text and media delivery, on two physical Android devices. It does not cover Windows' first
listen firewall prompt or iOS/macOS.

## 10. Bluetooth (design only, not in 0.2.2)

* **Why it is harder:** the radio is owned by the platform. On Android the core cannot open an
  RFCOMM or L2CAP socket itself; Kotlin opens it and passes bytes through a platform channel to the
  Rust transport, which keeps the framing, tokens and everything security-relevant (§1a).
* **Finding each other without broadcasting an identity:** both phones advertise (BLE) a short
  **rotating per-chat tag**, `HMAC(nearby_key, floor(now / 10 min))` truncated to 8 bytes, where
  `nearby_key` is exchanged in the same E2E announcement as the Wi-Fi token. Only that contact can
  recognise it; to anyone else it is random and changes every ten minutes (Briar does the same).
* **Connecting:** the phone that sees its contact's tag opens an L2CAP channel (Android 10+, no
  pairing dialog) to the advertiser and runs the §5 preamble; frames then flow as for Wi-Fi.
* **Permissions:** `BLUETOOTH_ADVERTISE`, `BLUETOOTH_CONNECT`, `BLUETOOTH_SCAN` (Android 12+, with
  `neverForLocation`), asked only when the user turns Bluetooth on for a chat.
* **Costs and limits:** continuous BLE advertising and scanning cost battery, so it runs only while
  the app is open (or for a chosen time); range is a room; throughput suits text and small photos.
  Desktop has no equivalent in this design.
* **Order:** the per-chat switch becomes "Also connect nearby (Wi-Fi and Bluetooth)", reusing §3's
  consent and §4's announcement with one more field.
