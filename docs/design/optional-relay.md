# Design — Optional relay, and "direct only"

**Status:** draft 2026-10-05, for 0.2 (the protocol release). Not implemented. Decisions marked
**(decided)** were made by Shawn on 2026-10-03; everything else is a proposal until reviewed.
**Relates to:** `multi-relay-mailboxes.md` (#17, the relay set this generalises),
`mailbox-handles.md`, `onion-client-auth.md` (§5 below depends on it), `cover-traffic.md`,
`ARCHITECTURE.md` §5c, §6, §7c and §11. Prompted by issue #16 ("How is this app secured compared
to other apps?"), where the always-on Night Drop relay was the part that needed the most
explaining.

## 1. What changes

Today every install uses the **Night Drop relay**. It is the implicit *primary*: always posted to,
always polled, never shown as removable, and the relay directory (`directory.rs`) adds its signed
siblings. "My relays" (#17) holds only *extra* relays on top of it.

In 0.2 **(decided)**:

* The Night Drop relay is an **ordinary entry** in "My relays": preselected, removable, and
  re-addable later.
* **An empty list means "direct only":** no relay is used at all, for receiving *or* sending.
* Contacts are **told** each other's complete relay set, including "none", and see it in the chat.
* The choice is offered **once at identity creation** (§7), beside the cover-traffic prompt.

What the relay was providing, so it is clear what direct only gives up: delivery while the
recipient is offline (24 h store-and-forward), short-code pairing (the rendezvous, §5c), the
server backup (§7c), opt-in per-chat server storage, the cover-traffic mailbox, and first contact
with a restricted onion (§5 below).

## 2. The relay list

* An entry is either the **Night Drop relay**, a symbolic entry that stands for the baked-in
  default plus whatever the signed directory currently lists, or an **onion address** the user
  added. The symbolic form matters: the directory exists so the Night Drop relays can rotate
  without an app update, and a contact's copy of "Night Drop" must follow that rotation rather
  than freeze a list of onions from the day it was announced.
* **At most 4 entries**, which settles #17's open question. Every sender posts each message to
  every entry, so this bounds the fan-out cost.
* **Empty = direct only.** No special flag: the list itself is the state, so there is no way for
  "direct only" and a non-empty list to disagree.
* **Migration from 0.1.x:** an upgraded identity starts with `[Night Drop] + my_relays`, which is
  exactly what it was already using. Nothing changes for anyone who does not open the screen.
* **Removing a relay keeps draining it for 24 h** (the relay TTL). Mail already queued there for
  us would otherwise be lost. Posting to it stops at once.
* **The directory is refreshed only while the Night Drop entry is in the list.** A user who removed
  it still posts to contacts who use it (§3), using the baked-in default plus the last directory
  they accepted. See §11 on staleness.

## 3. Telling contacts

`Frame::Relays` today carries only the extras, and the receiver adds its *own* primary. In 0.2 it
is replaced by **`Frame::RelaySet`**: E2E like every control frame, and carrying the sender's
**complete** set:

```
RelaySet { nightdrop: bool, relays: Vec<onion> }   // both empty/false = direct only
```

Rules:

1. **A sender posts to exactly the recipient's announced set.** There is no implicit relay any
   more, so a contact who removed Night Drop really has nothing on it.
2. **A direct-only sender posts to no relay at all (decided),** even when the recipient has relays.
   "Direct only" means this device never talks to a relay, so it is a property of *your* traffic,
   not only of your mailbox.
3. **Unknown is not empty.** A contact whose set has not arrived yet is treated as "hold" (§4),
   never as "use Night Drop". In 0.2 that window is short: the set travels inside pairing (the
   joiner's `Hello`, and the sealed short-code response) and as the inviter's first frame after it.
4. Sent on pairing, whenever the list changes, and once at startup if it changed while the app was
   closed. This is the same once-per-change behaviour `announce_relays` has today.

The chat shows the contact's state where it affects delivery:

* the contact is **direct only**: *"{name} receives messages only while their app is online.
  Messages wait on this device until you're both online."*
* **you** are direct only: the same line from your side, once in each chat, plus a home-screen
  note under "My relays".

## 4. Holding messages for direct delivery

With no relay on either side of a pair, a message to an offline peer stays **on the sender's
device**, marked **"Waiting for {name} to come online"**, and is retried directly.

* **Persistent.** This generalises `pending_relay` (`node.rs`, retried by `flush_pending_relay`).
  That queue already re-queues messages that reached neither the peer nor a relay, but only in
  memory: a restart drops the retry and leaves the message "queued" forever. That is tolerable
  when a relay usually takes the copy within seconds, and not when holding is the only path, so
  the held queue is stored with the chat. It holds only messages explicitly marked held, *not*
  everything in the "sent" state: `awaiting_receipt`'s doc comment records why seeding a retry
  queue from history sent a burst of duplicates.
* **Retry:** back off from 1 min to a cap of 15 min, on the poll cadence. Every retry is a
  descriptor fetch plus a circuit, so it has a battery cost, which has to be measured the way
  `background-traffic.md` measured the mailbox checks before the cap is fixed.
* **Flush at once on any inbound frame from that peer:** hearing from them proves they are online
  right now, and costs nothing extra.
* **Control frames** (receipts, `Version`, `Address`, `ClientKey`, edits, unsends, burns) follow
  the same path. Relay recalls (`relay_receipts`) simply never exist for these messages.
* **Ordering:** held messages go out in send order, one peer at a time, as the relay drain
  already delivers them.

Both sides must be online at the same moment. For two direct-only phones that mostly means both
apps open, or both running background delivery on Android. Say so plainly in the UI, rather than
leaving a message that "sometimes" arrives.

## 5. Pairing without a relay: the restricted-onion problem

**Without this section, direct only breaks the second pairing.** Once a device has one contact its
onion is **restricted** (`onion-client-auth.md` §7a): only clients whose key it has authorized can
fetch its descriptor. A new contact's key reaches it in the pairing `Hello`, and that `Hello` gets
through today only because a refused direct dial falls back to the relay. With no relay, the
joiner's dial is refused and pairing fails, silently, for every contact after the first.

Fix: **the side being dialled hands out a credential in advance.** A client-auth key is an x25519
pair, and the transport can already import a secret for a peer's onion (`insert_client_key`, used
today to restore keys after a restart). So:

* **QR (inviter → joiner):** when showing a QR, the inviter mints a keypair, **authorizes the
  public half on its own onion**, and adds the secret to the QR (`&ck=…`, 32 bytes). The joiner
  inserts it for the inviter's onion before dialling. The QR is already a secret, pre-authorized
  bundle; this extends "pre-authorized" from the session to the descriptor.
* **Reply (joiner → inviter):** the joiner does the same in reverse. It mints a keypair, authorizes
  it on its own onion, and puts the secret inside its `Hello`, which is E2E as a pre-key message.
  The inviter can then reach the joiner even if the joiner's onion is restricted too.
* **Two separate gates, and the pass opens only the first.** Onion client auth decides who can
  *reach* the device. Chat approval decides who can *talk* to the user. A pass gets a `Hello`
  delivered and nothing else. That `Hello` lands as a **chat request the user must Accept**,
  exactly as every inbound `Hello` does today (`frames.rs`: `require_authorization` is set on every
  real constructor, and the branch logs "held as a request pending approval"). Nobody is approved
  automatically. Showing a QR is not consent to whoever scans it.
* **Tighten onion authorization to follow the user's decision.** Today a pending request's
  `ClientKey` is authorized on our onion as soon as it arrives (`frames.rs`, the `ClientKey` arm
  authorizes any contact with a chat, *pending included*). In 0.2 a request gets onion access
  only when the user taps **Accept**, and a **Decline** leaves it with none. The same goes for
  the joiner's reply pass: we hold it unused until Accept.
* **Lifetime:** the QR pass is **single-use**. It is revoked as soon as a `Hello` arrives that
  used that QR's one-time key, which is single-use anyway, and after 24 h if nobody scans it. A
  QR screenshot posted online therefore lets at most one stranger knock, as a request the user
  can decline, within a day. The joiner's reply pass is replaced by the normal `ClientKey`
  exchange after Accept, then revoked.
* This applies to **every** pairing in 0.2, not only to direct-only users. It removes the relay from
  first contact for everyone, so one path is tested instead of two.

**Short codes need the rendezvous**, which lives on a relay, so a direct-only user can neither
create nor join one (decided). QR stays available. See §11 for a possible one-off exception.

## 6. What else depends on the relay

| Feature | With relays | Direct only |
|---|---|---|
| Offline delivery (24 h) | yes | no, held on the sender (§4) |
| Short-code pairing | yes | no, QR only (§5) |
| Server backup (§7c) | yes | no; file export and device transfer still work |
| Per-chat server storage | when **both** sides have relays | hidden, with the reason |
| Cover traffic | optional | off and hidden (no mailbox to cover) |
| Relay directory refresh | while Night Drop is listed | no |
| Update check (§10a, our own onion) | yes | yes, not a relay |
| "Get bridges" (moat) | yes | yes, not a relay |

**Per-chat server storage** posts to the *recipient's* relays and is sent by the *sender*, so it
needs relays on both sides. Its persistent in-chat warning is unchanged where it is available.

## 7. Onboarding

One setup step before `createIdentity` (decided), because the choice changes the first thing the
new identity does on the network. Doing it afterwards runs into the `_Root` routing problem
recorded in TODO.

```
How should messages reach you while you're offline?

(•) Night Drop relay                    recommended
    Encrypted messages wait up to 24 h on our relay. It cannot read them.
( ) My own relay                        [ onion address ]   [Check]
( ) Direct only
    No server at all. You receive messages only while the app is online,
    and you can add contacts by QR code only.

[ ] Cover traffic   (only shown when a relay is chosen; see cover-traffic.md)
```

Everything here can be changed later in "My relays". Removing the last relay later shows the same
consequences as a confirmation, listing what turns off (§6).

## 8. Invariants and threat model

* **No server-side keys, no logs:** unchanged. Fewer servers involved.
* **Local-first:** strengthened. Direct only is local-only.
* **Tor by default:** unchanged. Every path is still Tor.
* **Authorization before first message:** unchanged. §5's credentials grant reachability, not
  messaging, and they ride on the QR and on the E2E `Hello`, the two channels that are already
  authorized.
* **Server-storage warnings:** unchanged where server storage exists.

| Concern | Relay (as today) | Direct only |
|---|---|---|
| Relay sees mailbox timing and volume | yes, per-pair handles | **nothing**: no relay |
| Contacts learn when you are online | partly, from direct delivery | **more**: held retries succeed exactly when you come online |
| Message lost if both are never online together | after 24 h | held until the chat is deleted |
| Pairing exposure | rendezvous sees PAKE messages | none (QR only) |

The online-time point is the real cost and belongs in `SECURITY.md` and `website/limits.html`:
a direct-only user's contacts see their messages arrive the moment that user comes online.

## 9. Implementation plan (0.2, in order)

1. **Core list model:** a `RelayEntry { NightDrop | Onion(String) }` list, a cap of 4, the
   migration in §2, and the 24 h drain-out. `queue_on_relays` stops taking an implicit primary.
2. **`Frame::RelaySet`** replacing `Frame::Relays`, the set inside pairing, and "unknown = hold".
3. **The held queue (§4)**, generalised from `pending_relay`, with retry and flush-on-inbound.
4. **Pairing credentials (§5)** for QR and `Hello`.
5. **Gate the features in §6** in the core, with the UI showing the reasons.
6. **UI:** the onboarding step, "My relays" with the Night Drop entry, chat banners, and the
   "Waiting for … to come online" status.
7. **Docs:** `ARCHITECTURE.md` (§5c, §6, §7c, §11.1's "single relay"), `SECURITY.md`,
   `website/limits.html`, and `multi-relay-mailboxes.md` marked as superseded where it differs.

**Tests:**
* Two in-process nodes, both direct only. A message sent while the peer is down is held and
  delivered when it comes back. The same holds across a restart of the sender.
* A restricted onion pairs a second contact by QR with no relay. This needs real Tor: extend
  `tor_smoke` the way `onion-client-auth.md` did, because it is the case that would fail silently.
* A relay sender and a direct-only recipient: nothing is posted, and the message is held.
* A direct-only sender and a relay recipient: nothing is posted, and the message is held.
* Removing a relay still drains mail already queued there.
* On hardware, two phones, one direct only: the battery cost of held retries, measured like
  `background-traffic.md`.

## 10. Not in scope

Peer-to-peer relaying through contacts, and any discovery of relays, as #17 already ruled out.
Changing relay TTLs. Groups (`group-chat.md`, 0.3), which will inherit whatever each pair uses.

## 11. Open questions

* **One-off short code for a direct-only user**, using the Night Drop rendezvous once with explicit
  consent. That would make direct only less isolating, at the price of a relay contact the user
  said they wanted to avoid. Default: no.
* **Directory staleness** for a user who removed Night Drop but sends to contacts who use it. Their
  copy of the directory ages, because only a listed relay is asked for it. Options: refresh it
  anyway when posting to a contact's Night Drop entry (they are talking to that relay regardless),
  or accept the baked-in default plus the last list.
* **An "I'm online" ping** from a direct-only device at startup, so contacts flush held messages at
  once instead of waiting up to 15 min. Faster, but it announces presence to every contact on each
  launch. Default: no ping; rely on flush-on-inbound and the retry cap.
* Whether the retry cap should be shorter while the app is in the foreground.
