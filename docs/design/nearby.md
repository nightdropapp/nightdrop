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
you need to turn this on." Since the beacons (§5a) it also says that for two minutes after joining
a network the device sends a signal there that only the contact can tell is theirs (on any network
where they have a device), and that shows others a Night Drop with this setting on is present (`nearbyBody` in `app_en.arb`). Turning it off
is immediate and needs no confirmation.

Per chat, the banner shows one of:

| Ours | Theirs | Banner |
|---|---|---|
| off | off | — |
| off | on | "They'd like to also connect on the same Wi-Fi" (with the menu item to turn it on) |
| on | off | "Same Wi-Fi: waiting for them to turn it on" |
| on | on, not reachable | "Same Wi-Fi: on — not on the same network right now" |
| on | on, reachable | "Same Wi-Fi: connected" |

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
  redrawn every time it does. It never crosses the local network: keys derived from it prove a
  connection is the contact's (§5) and make our beacons (§5a). It is sent even
  with no local network, so the contact can recognise our beacon when we meet on one with no
  internet.
* Re-announced when our local address changes (checked each tick).
* 0.2 apps ignore an unknown `Captures` marker, so this needs no wire version.

A contact's announcement is accepted only if the address is a private IPv4 (10/8, 172.16/12,
192.168/16, 169.254/16) and the port is 32768 or above (the ephemeral range our listener binds in);
anything else is ignored. **Without the address rule, a contact could announce a public address and
make our app connect to it outside Tor, revealing our public IP.** Without the port rule, they could
make it send its opening line to a service on our network (a printer's raw port, a router's admin
page). Changes count against the daily allowance like other control frames.

## 5. The local connection

The dialer connects to `ipv4:port` (connect timeout 2 s) only when **our** chat has Nearby on, the
contact's announcement is on, and our own local address is in the same subnet (/24 for 192.168,
/16 for 172.16/12 and 169.254, /8 for 10). Then:

A challenge-response on the chat's two tokens, with
`proof(t, role, Nd, Nl) = HKDF-SHA256(salt "nightdrop/nearby/proof/v1", ikm t, info role|Nd|Nl)`,
32 bytes, base64url (43 characters); nonces are 16 random bytes, base64url (22 characters):

1. dialer → listener: `ND-NEARBY/3 ` + `Nd` + `\n`.
2. listener → dialer: a fresh `Nl` + `\n`.
3. dialer → listener: `proof(listener's token, "D", Nd, Nl)` + `\n` (the token the listener
   announced).
4. listener: if it matches no token it issued to a chat with Nearby on (each checked, constant
   time), close. Else reply `proof(dialer's token, "L", Nd, Nl)` + `\n` (the token the dialer
   announced to it).
5. dialer: if the reply is not that proof of its own token, close and fall back to Tor.
6. Frames flow as on any transport: length-prefixed, padded, end-to-end encrypted.

Nothing a listener on the network sees is reusable: the listener's nonce is new each connection, so
a recorded proof does not open another one, and the tokens never cross the network (they also
root the beacon key, §5a). Until 2026-11-09 (never shipped) the opening line was a fixed
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
exchange alone, no message. The chat's "connected" state is a success either way in the last two
minutes. After a local attempt fails, sends skip the local path for a minute — otherwise each would
wait out the 2 s connect first when the two are apart.

## 5a. Finding each other without the internet (beacons)

§4's announcement travels over Tor. Two phones that move to a new network with no internet (a
hotspot in the field, a router with no uplink) could not tell each other their new addresses, so
the chat stayed "not on the same network" until the internet came back (found on the S23/S25,
2026-11-09). So, while a chat has Nearby on, each device also broadcasts a **beacon** on each of
its local networks:

```
"NDB1" | nonce (8 random bytes) | port (2, big-endian) | tag (16)
tag = HKDF-SHA256(salt "nightdrop/nearby/beacon/v1", ikm = our token,
                  info = window (8) | sending IPv4 (4) | port (2) | nonce)[..16]
window = unix time / 600 s
```

* Sent to port 41873 at the network's own broadcast address (taken as the /24) and at
  `255.255.255.255`, from a socket bound to each usable interface's address. The limited broadcast
  alone never reached a phone on another phone's hotspot (S23 → S25, 2026-11-09); the subnet one
  follows Android's route for the hotspot. One per chat where both have Nearby on (nobody could
  use one for a chat whose contact has it off), every 5 s for **two
  minutes** after listening starts or our networks change, **then none**; and once (at most every
  5 s) after we find a contact, who may not have found us. A device staying on a network does not
  keep telling it a Night Drop is there: a contact who arrives later starts their own burst, and
  our answer follows within a second.
* Received on UDP 41873 (shared with any other Night Drop on the device, `SO_REUSEADDR`), only from
  a private address in our subnet, at most 50 checked a second (the rest dropped unread). A beacon is checked against the token of each chat whose
  contact has Nearby on, for the window before, the current one and the next (clock skew), in
  constant time. A match names where that contact listens: the sending address and the port in the
  beacon, which must still be 32768 or above.
* What a beacon finds is held in memory only and used to dial (§5) in place of the announced
  address; the contact's next announcement over Tor replaces it. Finding a contact also resets the
  back-off, so the next send tries the new address. An address that worked in the last minute is
  kept: a contact on two of our networks beacons from both, and switching on each would have the
  two devices answering each other without end.
* **What it shows.** To anyone else a beacon is 30 random-looking bytes that never repeat (fresh
  nonce), carrying no identity, onion, or token: a listener learns that some device on the network
  runs Night Drop with Nearby on, and its port, during the two minutes after it joins (and when it
  answers a contact). It cannot tell which chat, or link beacons across
  networks or across turning Nearby off and on (a fresh token each time). Within one network the
  device's address and port already link its traffic.
* **What the contact can do with it.** The contact holds the token, so a device of theirs (or one
  they give the token to) left on any network recognises our beacon there: they learn when we join
  that network, not only when we are in the same room. Inherent in a beacon only they can
  recognise; bounded by the two-minute bursts and by turning Nearby off (a new token next time).
  `SECURITY.md` says so.
* **What it does not allow.** Forging one needs the contact's token, which never crosses the
  network. A recorded beacon replayed from another address does not verify
  (the address is in the tag); replayed from the same address within the window it says nothing
  new. A beacon only chooses where we dial: the connection still has to pass §5's exchange, and
  everything on it is end-to-end encrypted.
* **Needs:** both turned Nearby on, and each heard the other's token, while they could still reach
  each other over Tor (the same condition as before for the old address). Two people who have
  never had Nearby on together cannot find each other offline.

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
