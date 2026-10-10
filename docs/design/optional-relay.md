# Design — Optional relay, and "direct only"

**Status:** design agreed 2026-10-05, for 0.2 (the protocol release). **Built 2026-10-09/10**
(§9, each step says what and where); not yet tested on two devices or released. Decisions marked
**(decided)** were made by the maintainer on 2026-10-03, 2026-10-05, 2026-10-09 or 2026-10-10.
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
* **At most 4 entries in *your own* list**, the relays where *you* receive. This settles #17's
  open question. Someone writing to you posts each offline message to every entry, so the cap
  bounds how many copies one message makes. It does **not** limit how many relays you can send
  *to*: each contact has their own list, and you post to theirs. Ten contacts on ten different
  self-hosted relays all get their offline mail. The same cap applies to a list a contact
  announces to us (extra entries are ignored), so a hostile contact cannot make us post a message
  a thousand times.
* **Relays never see anyone's list.** Lists travel only between contacts, end to end encrypted
  (§3). The Night Drop directory is the opposite direction and the only thing a relay serves: a
  list of *Night Drop's own* relays, signed by us. Nothing about any other relay is ever sent to
  a Night Drop relay. Someone running their own relay shares it with their contacts, and only the
  app does that, automatically and E2E, when they add it to their list.
* **Empty = direct only.** No special flag: the list itself is the state, so there is no way for
  "direct only" and a non-empty list to disagree.
* **Migration from 0.1.x:** an upgraded identity starts with `[Night Drop] + my_relays`, which is
  exactly what it was already using. Nothing changes for anyone who does not open the screen.
* **Removing a relay keeps draining it for 24 h** (the relay TTL). Mail already queued there for
  us would otherwise be lost. Posting to it stops at once.
* **The directory comes from our onion website, not from the relays** (decided 2026-10-05).
  Today it is served only by relays (`GetDirectory`, asked of the primary, the discovered relays
  *and* the user's own relays). So if every relay a user knows goes down, they can no longer learn
  the replacement, which is exactly the case the directory exists for. Instead:
  * The signed list is published as a file on the onion site (`website/relays.json`, the existing
    `SignedDirectory` format, Ed25519-signed with the offline directory key, monotonic version).
    It is public information, already in every app build, so it is **signed, not encrypted**:
    encryption would protect nothing, and the signature is what stops a compromised website or
    anyone else from pointing apps at their own relay.
  * The app fetches it **over Tor from `UPDATE_ONION`** (`Transport::onion_get`, as the update
    check does), at most **once a day**, with the same record-the-attempt rule so a down site never
    turns into a retry on every launch.
  * It is **not** gated like the update check. F-Droid installs skip the update check because
    F-Droid is their updater; the relay list is not an update, it is how the app finds its relay.
  * **Every user with a relay list fetches it,** including someone who removed the Night Drop
    entry but writes to contacts who use it (§3). Their copy must follow a relay rotation, or that
    mail goes to a dead address and is held. This replaces the earlier "refresh while sending",
    and no relay is ever asked for the list. A direct-only user skips the fetch, since they never
    post to a relay.
  * The relays keep answering `GetDirectory` through 0.2, for 0.1.x clients. After that it can go,
    and our self-hosters' relays stop being asked for a list that was never theirs.

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
5. **Contacts paired before 0.2 are not unknown (decided 2026-10-09).** On upgrade each existing
   contact's set is seeded from what we already post to: Night Drop plus the extras they announced
   in `Frame::Relays`. Their first `RelaySet` replaces it. Without the seed, rule 3 would hold
   every existing chat until both people happened to be online at once, and the `RelaySet` that
   ends the hold would itself have no route. Only contacts paired under 0.2 start unknown.

The chat shows the contact's state where it affects delivery:

* the contact is **direct only**: *"{name} receives messages only while their app is online.
  Messages wait on this device until you're both online."*
* **you** are direct only: the same line from your side, once in each chat, plus a home-screen
  note under "My relays".

## 4. Holding messages for direct delivery

With no relay on either side of a pair, a message to an offline peer stays **on the sender's
device**, marked **"Waiting for {name} to come online"**, and is retried directly.

* **Persistent.** This generalises `pending_relay` (now `Node::held`, retried by `flush_held`).
  That queue already re-queues messages that reached neither the peer nor a relay, but only in
  memory: a restart drops the retry and leaves the message "queued" forever. That is tolerable
  when a relay usually takes the copy within seconds, and not when holding is the only path, so
  the held queue is stored with the chat. It holds only messages explicitly marked held, *not*
  everything in the "sent" state: `awaiting_receipt`'s doc comment records why seeding a retry
  queue from history sent a burst of duplicates.
* **Retry (decided 2026-10-05):** back off from 1 min to a cap of **5 min while the app is open**
  and **15 min in the background**, on the poll cadence. Every retry is a descriptor fetch plus a
  circuit, so the background cap has a battery cost that should be measured the way
  `background-traffic.md` measured the mailbox checks.
* **No "I'm online" signal (decided 2026-10-05),** but build it switched off. A direct-only device
  could tell its contacts it has just come online, so they flush held messages at once instead of
  waiting for their next retry. It is not wanted now: it would announce presence to every contact
  on each launch. The implementation includes it behind a single core constant
  (`ANNOUNCE_ONLINE_ON_START = false`, beside the other announce settings in `node.rs`), with a
  test that runs both settings, so turning it on later is a one-line change, not a design.
  **Found while building it (2026-10-09):** the signal already existed. Burn support, app version,
  address and screenshot capability went to every contact on every launch, trying their device
  first. **Decided:** they are sent on change only. Each chat records what it confirmed
  (`Chat::told_*`), so a launch sends nothing new; the constant restores the per-launch sends.
  Built 2026-10-09 (`node/tests_announce.rs`).
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
* This applies to **every** pairing in 0.2, not only to direct-only users, so the sealed
  short-code response carries the inviter's pass exactly as the QR does. It removes the relay from
  first contact for everyone, so one path is tested instead of two.

**Short codes need the rendezvous**, which lives on a relay, so a direct-only user can neither
create nor join one (decided), not even once through the Night Drop rendezvous (decided 2026-10-05).
QR stays available.

**Short codes meet only on shared relays (decided 2026-10-09).** Both sides look for each other on
their own lists (`Node::rendezvous_relays`), never on a relay they removed. A code therefore works
when the two lists share a relay, which by default is Night Drop. Someone who removed Night Drop
and kept only their own relay can pair by short code with people on that relay, and by QR with
everyone else. A join that finds nobody says so and points to the QR code, rather than blaming
the network.

## 6. What else depends on the relay

| Feature | With relays | Direct only |
|---|---|---|
| Offline delivery (24 h) | yes | no, held on the sender (§4) |
| Short-code pairing | yes | no, QR only (§5) |
| Server backup (§7c) | yes | no; file export and device transfer still work |
| Per-chat server storage | when **both** sides have relays | hidden, with the reason |
| Cover traffic | optional | off and hidden (no mailbox to cover) |
| Relay directory (from the onion website, §2) | daily | no (never posts to a relay) |
| Update check (§10a, our own onion) | yes | yes, not a relay |
| "Get bridges" (moat) | yes | yes, not a relay |

**Per-chat server storage** posts to the *recipient's* relays and is sent by the *sender*, so it
needs relays on both sides. Its persistent in-chat warning is unchanged where it is available.

**Cover traffic after v1 mailboxes (decided 2026-10-09).** Cover traffic posted to our own v1
mailbox. 0.2 drops v1, and real mail already goes to per-pair handles, so chaff on a mailbox
nobody else posts to would cover nothing. In 0.2 each cover post goes to the current inbound
handle of one contact picked at random, on our own relays, so it lands in the mailboxes real mail
uses. With no contacts yet it goes to a self handle derived the same way.

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

0. **Relay directory from the onion website (§2). Built for 0.1.28.** Independent of everything below and not a
   protocol change, so it ships in **0.1.28** (decided 2026-10-05): `relays.json` on the site, published by
   `nightdrop-relay sign-directory`; a daily `onion_get` in the core; relays kept as the fallback.
1. **Core list model:** a `RelayEntry { NightDrop | Onion(String) }` list, a cap of 4, the
   migration in §2, and the 24 h drain-out. `queue_on_relays` stops taking an implicit primary.
   **Built 2026-10-09** (`Node::relays`, `post_route`; tests in `node/tests_relay_list.rs`). A
   0.1.x state's extras past the cap drain out rather than vanish, and a direct-only device
   already posts to no relay.
2. **`Frame::RelaySet`** replacing `Frame::Relays`, the set inside pairing, and "unknown = hold".
   **Built 2026-10-09** (`WIRE_VERSION` 3; tests in `node/tests_relay_set.rs`). The joiner's set
   is the `Hello` plaintext (`HelloBody`), the short-code response carries the inviter's as `rs=`,
   and a QR joiner holds until the inviter's first `RelaySet`. Each chat keeps the set it was last
   confirmed to hold of ours (`relays_told`), so a change, or the first 0.2 launch, is re-sent on
   later ticks until it lands. The one exception to "unknown is held" is the QR `Hello` itself,
   which still goes to Night Drop until step 4 lets it go direct.
3. **The held queue (§4)**, generalised from `pending_relay`, with retry and flush-on-inbound.
   **Built 2026-10-09** (`Node::held`, persisted in the state; tests in `node/tests_held.rs` and
   `tests_outbox.rs`). As built:
   * Anything that reaches neither the peer nor a relay is held in send order. Messages show
     **"waiting"** ("Waiting for {name} to come online") when no relay connects the pair, and
     "queued" otherwise. Control frames to a contact with no relay route (a rename, a timer, an
     approval, an edit) are held with them instead of being lost or reported as failed.
   * A contact with no relay route gets its oldest held frame only, directly on a fresh
     connection, on a per-contact backoff of 1, 2, 4 then 5 min (15 in the background); when it
     lands the rest follow at once. Every other retry to that contact (`pending_control`,
     unconfirmed messages, standing announcements) waits on the same backoff, so an offline
     contact is dialled once per retry, not every 15 s tick.
   * Any authenticated frame arriving **directly** resets the backoff and flushes at once
     (`Node::heard_from`, from `pump`; counted by `Chat::alive_count`, since `last_seen` has
     one-second resolution). A relay-drained frame does not count: it may be hours old.
   * Editing or unsending a message still held edits the held copy in place or drops it; nothing
     is sent. "waiting" never expires; deleting the chat drops its held frames.
   * The v2 mailbox agreement is skipped for a contact with no relay route (mailboxes live on
     relays).
   * Not yet measured: the battery cost of the background retries on two phones (§9 tests).
4. **Pairing credentials (§5)** for QR and `Hello`. **Built 2026-10-09** (tests in
   `node/tests_pass.rs`, which simulate restricted discovery, and on real Tor
   `tor_smoke::a_pass_minted_by_the_onion_admits_whoever_holds_it`). As built:
   * The inviter mints a pass (`Transport::mint_pass`, not installed anywhere), authorizes it as
     `pass:<one-time key>`, and puts the secret in the QR and the short-code response as `ck=`.
     Persisted with the state; revoked when a `Hello` uses that one-time key, or after 24 h
     (`expire_passes`, from `sweep_time`).
   * The joiner installs the pass for the inviter's onion and sends, inside the E2E `Hello`, its
     own client key for the inviter's onion and a reply pass for its own (authorized at once for
     the inviter). It switches from the pass to its own key when the inviter's first `RelaySet`
     arrives: only approved chats are sent one, and it is authenticated, unlike `Approved`.
   * **Decided 2026-10-09: the reply pass stays the inviter's key** for the joiner's onion; it is
     not replaced after Accept. Replacing it needs the joiner's descriptor republished for the new
     key, and with no relay to cover that gap a direct-only pair could not reach each other.
     Holding a secret the joiner minted gives the inviter nothing but that reach.
   * Both keys wait on the request (`peer_key_pending`, `pass_pending`, persisted) and are applied
     on Accept (`apply_pending_keys`); Decline drops them. A `ClientKey` frame from a request is
     held the same way, no longer authorized on arrival.
   * A request is also told nothing about us before Accept: burn support, version, screenshot
     capability and the relay set all wait for it, rather than telling whoever scanned a QR.
   * The `Hello` goes directly only, for everyone, held and retried like any relay-less frame.
   * A transport without `mint_pass` (none ship) falls back to the 0.1 `ClientKey` exchange.
5. **Gate the features in §6** in the core, with the UI showing the reasons. **Built 2026-10-10**
   (`api::relay_features`, `Contact::peer_relay_state` / `server_storage_available`; tests in
   `node/tests_relay_list.rs` and `api.rs`). Short codes refuse with a pointer to the QR code, and
   a join that finds nobody now says the two may share no relay. Server storage turns on only
   with relays on both sides. A direct-only device skips the relay-list fetch. **Server backup
   needs the Night Drop relay in the list** (decided while building, 2026-10-10): a new device
   restoring one knows no other relay to look on, so a list without Night Drop cannot offer it.
6. **UI:** the onboarding step, "My relays" with the Night Drop entry, chat banners, and the
   "Waiting for … to come online" status. **Built 2026-10-10** (`app/test/optional_relay_test.dart`):
   * Setup: Night Drop (recommended) / my own relay / direct only, before `createIdentity`;
     `new_tor(initial_relays)` puts the list in place before the new identity's first poll. Cover
     traffic is offered only with a relay.
   * "My relays" (`features/relays/my_relays_screen.dart`): the Night Drop entry can be removed and
     added back, the cap is shown, each added relay has its access key, and removing the last one
     first lists what turns off. Only `.onion` addresses are accepted (the old editor's `host:port`
     would bypass Tor). The design's **[Check]** button is not built: there is no reachability
     probe yet, and a relay that does not answer shows as "Not answering" after the next poll.
   * Home: a direct-only note. Chat: one-line notices for a direct-only contact, an unknown set
     (after approval only), and our own direct only; the storage switch explains why it is off.
   * Unavailable features stay in the menus and explain themselves, with a way to "My relays",
     rather than vanishing: a missing option reads as broken, an explained one does not.
7. **Docs:** `ARCHITECTURE.md` (§5c, §6, §7c, §11.1's "single relay"), `SECURITY.md`,
   `website/limits.html`, and `multi-relay-mailboxes.md` marked as superseded where it differs.
   **Done 2026-10-10**, except that the `limits.html` change waits in
   `docs/release/0.2-website.patch`: the onion site serves `website/` live, so it is applied at
   publish time (`MAINTENANCE.md` §11 step 10).

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

None remaining.

**Decided 2026-10-09:** existing contacts are seeded, not unknown (§3 rule 5); cover traffic posts
to a random contact's pair handle (§6); short codes meet only on relays both lists share (§5).

**Decided 2026-10-05:** no one-off short code for direct-only users (they pair by QR only); no
"I'm online" signal, built switched off (§4); retry cap 5 min while open, 15 min in the background
(§4); the relay directory is fetched from the onion website, not from relays (§2).
