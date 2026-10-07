# Design — Per-pair, epoch-rotating mailbox handles

**Status:** design agreed 2026-09-22; implemented on branch `mailbox-v2` 2026-09-26 and shipped in **0.1.26** (planned for 0.1.25). Where
the build departs from or sharpens the text below, §9 says so.
**Relates to:** `ARCHITECTURE.md` §6 (relay, store-and-forward) and §11.2, `multi-relay-mailboxes.md`
(#17), `cover-traffic.md` (#4). Group chat (0.3) depends on this but does not block it.

## 1. The leak

`core/src/node.rs`:

```rust
fn mailbox_handle(recipient_identity_key: &str) -> String {
    h.update(b"nightdrop/mailbox/v1");
    h.update(recipient_identity_key.as_bytes());
    format!("mbx:{}", base64_handle(&h.finalize()[..15]))
}
```

A **static** truncated SHA-256 of a **long-term** identity key. Its docstring calls it unlinkable,
and it is — *to the identity*. The relay cannot reverse it to an onion address or a public key.

But it never changes. `mbx:ABC` is the same person for the life of that identity, so a relay (or
whoever seizes one) can accumulate:

* **a contact graph**, from which handles receive deposits close together in time;
* **a per-mailbox behavioural profile** — activity, volume, hours — which `cover-traffic.md` §1
  already names as the threat cover traffic exists to blunt.

`ARCHITECTURE.md` §11.2 currently describes these as *"ephemeral, unlinkable handles"*. They are
neither. **That line is wrong and must be corrected in the same change** — an overclaim in the
authoritative document is worse than the leak, because it is what a reader relies on.

This is **not** a group-chat problem. It exists in 1:1 today. Groups only make it louder: a
fan-out to nine mailboxes is nine correlated deposits at one instant.

## 2. Rejected: padding the fan-out

The obvious mitigation — always deposit to 10 mailboxes regardless of real group size — fails.
Dummy handles are **never drained**, while real ones are collected; a relay separates them after a
few rounds. Padding with extra blobs to the *same* real handles hides the member count but leaves
the co-occurrence set intact, and membership is the more valuable leak. Scratched.

## 3. Rejected: epoch rotation alone

`H(ik_recipient ‖ epoch)` keeps one handle per person and breaks linkage across days. It looks
sufficient and is not: a **stable group re-links itself by its own co-occurrence pattern**. Nine
handles that appear together every morning are a fingerprint whatever they are called today.

## 4. The scheme

```
handle = "mbx:" ‖ b64( HKDF(mailbox_secret_AB, "nightdrop/mailbox/v2" ‖ epoch_day)[..15] )
```

* **Per-pair.** Alice→Bob and Carol→Bob produce *different* handles, so the relay cannot tell two
  deposits are even for the same recipient. This is what kills the co-occurrence graph, in 1:1 and
  in groups alike.
* **Per-epoch.** One UTC day. Readers accept the **neighbouring epoch** on either side, the same
  tolerance Tor uses for blinded onion keys, so clock skew needs no negotiation.
* **Computable by both ends** with no extra round trip: each side holds the pair secret.

**Cost:** a recipient polls one handle per contact instead of one in total — and with #17, one per
contact *per relay*. That cost grows linearly and is the reason for a stated contact cap; see §8.

### 4.1 Where the secret comes from

**Not `Session::session_id()`.** vodozemac computes it as
`SHA256(identity_key ‖ base_key ‖ one_time_key)` (`olm/session_keys.rs`) — all three public, and
the base key and one-time key **travel in the pre-key message**. Anyone who observed session setup
could recompute the handle, which hands back exactly the linkage being removed. Nothing else on
`Session` is both stable and secret: the root key ratchets forward.

So the secret is **ours to derive and persist**: at `create_outbound_session` /
`create_inbound_session`, take a value both sides compute identically, run it through HKDF with a
`nightdrop/mailbox/v2` label, and store it in the contact record beside the session. It never
ratchets, so the handle is stable for the life of the contact.

### 4.2 Contacts that already exist

Their ratchets have moved on; no such secret can be recovered after the fact. Rather than strand
them on `v1` for ever, the pair **agrees one over the channel they already have** — they are
mutually authenticated and confidential, so a small key-agreement frame is safe and needs no user
action. Migration then proceeds per contact rather than as a flag day.

## 5. Migration — the part that can lose messages

A 0.1.22 sender posts to a `v1` handle. A 0.1.23 recipient polling only `v2` never sees it, and
with a 24-hour TTL the blob expires unnoticed. **Silent message loss is far worse than the leak
being fixed**, so the transition is the design, not an afterthought.

1. **Announce the capability**, exactly as `Frame::Captures` announces screenshot reporting. Peers
   record per contact whether `mailbox-v2` is supported.
2. **Post `v2` only to contacts known to support it**; `v1` to everyone else.
3. **Poll both** `v1` and every `v2` handle throughout the transition.
4. **Tell the user.** A chat with a `v1`-only peer shows a persistent notice — the same shape as
   the existing "this device cannot report screenshots" warning — saying the other person is on an
   older version, that their messages are addressed less privately, and that support will be
   removed in a future release. The person who can act is the one on the old version, so the
   notice must be visible on **both** sides.
5. **Drop `v1` after two releases**, and say so in the changelog of each.

## 5a. Draining must be isolated, or the scheme is undone

**Added after review, 2026-09-22. The claim stands; the mechanism was corrected the same day — §5c.**

Unlinkable deposits are worthless if collection re-links them. `drain_relay_mailboxes`
(`core/src/node.rs`) currently takes a **single** handle. Per-pair handles make it one request per
contact — and if those go over one circuit, the relay sees a single client asking for `H1…H10` and
learns exactly the set this design removed from the deposit side.

So per-pair handles are **conditional on how draining is scheduled**. The mechanism exists:
`arti-client` 0.43 offers `StreamPrefs::isolate_every_stream()` and `set_isolation(token)`, so
polls can be placed on separate circuits at whatever granularity is chosen. How much to buy is
§5c — the first answer given here, one circuit per handle, was the wrong end of the dial.

If draining is not isolated at all, per-pair handles buy far less than they appear to, and the
honest thing is to say so rather than ship the appearance of a fix.

## 5b. What "ephemeral" and "unlinkable" would actually mean

Worth stating precisely, since `ARCHITECTURE.md` claimed both without defining either.

| Can the relay link… | v1 (shipped) | v2 (this design) | per-message |
|---|---|---|---|
| a handle to an identity or onion | no | no | no |
| two senders to the same recipient | **yes** | no | no |
| today's handle to yesterday's | **yes** | no | no |
| two messages, same pair, same day | **yes** | **yes** | no |

**Unlinkable** is therefore not one property: v2 achieves it across senders and across days, and
deliberately not within a pair-day.

**Ephemeral** would mean a handle is used once. Reachable with a hash ratchet —
`handle_n = HKDF(pair_secret, n)`, sender incrementing, reader scanning a window ahead — at the
cost of polling a window of unknown depth per contact per relay, and a resync path for when a
sender outruns the window. Not proposed for 0.1.23; recorded so the next person knows the ceiling
and what it costs, rather than assuming v2 is the end of the road.

## 5c. How much isolation — and the trap in re-randomising

Three corrections to the first pass, in the order they matter.

**Handles rotate per *epoch*, not per round.** An epoch is a UTC day; a poll round is minutes. So
the same handle set is polled on the order of **288 times** (5-minute rounds) before anything
rotates. Rotating *circuits* between rounds therefore buys almost nothing — the relay re-identifies
the set by the **handles**, which are unchanged, not by the circuit that asked. Circuit rotation is
near-free and worth doing anyway; it is simply not the control. The only question that matters is
whether handles are polled **together**.

**Per-handle isolation is the expensive end and not obviously the right one.** It costs
contacts × relays circuit builds *per round* — at 50 contacts and 5-minute rounds, ~14,400 circuit
builds a day, on a phone, over Tor. That is not a latency cost to be endured; it is a different
product.

**The workable middle is a fragmented poll:** fix a partition of the handle set at the start of each
epoch, give each fragment its own circuit, and randomise when fragments go out. The relay then
learns "these ~k handles share a client" and no more. Accepting circuit reuse within a fragment for
**5–30 minutes** is what makes this affordable, and is the trade explicitly agreed.

**The trap: do not re-randomise the partition each round.** It reads as more privacy and is less.
Over an epoch's ~288 rounds a relay intersects the fragments it has seen and reassembles the full
set from co-occurrence frequency — handles belonging to one client land together far more often
than chance. A partition that is **fixed for the epoch** leaks a bounded, stateable amount; a
partition that churns leaks everything, slowly. Re-draw it only when the handles rotate.

For the same reason the **stagger must be random per round**. A fragment that always polls seven
minutes after another one has announced their relationship without ever sharing a circuit.

**Bucketed dummy polls** are worth adding and worth not overselling. Pad the polled set to a bucket
boundary with handles that do not exist — a poll for a nonexistent mailbox and a poll for an empty
one are indistinguishable, so this hides the contact count from a relay looking at one round. It
degrades against a long-lived one: a handle polled all week that never receives anything is
probably a dummy (or a very quiet contact). Cheap, keeps the count fuzzy, does not make it private.

And note it does **not** transfer to deposits — that is §2's failure, and the asymmetry is the whole
reason it works here. A dummy *poll* is free because empty is a normal answer. A dummy *deposit* is
never drained, which is what gives it away.

## 6. What this does not fix

* **The fan-out burst.** N deposits at one instant is still N deposits. Jitter and #17 spread it;
  they do not remove it. Handles being unrelated means the relay learns *that* a burst happened,
  not *who* it binds together.
* **The hot path is untouched — and does not need touching.** Onion-to-onion delivery never
  reaches a relay (`cover-traffic.md` §2), and an onion address is not geolocatable the way an
  IPv4 address is.
* **Cover traffic remains opt-in.** It blurs per-mailbox volume and timing, which is exactly the
  residual profile this scheme cannot erase, so 0.1.23 also offers it at onboarding beside the
  background-delivery prompt — **before** `createIdentity`, because `_Root` swaps the screen out
  from under a dialog awaited after it (see the comment on `_offerBackgroundDelivery`). Showing
  both prompts *during* identity creation is a worthwhile but separate change: it alters `_Root`'s
  routing condition, which was hardened on 2026-09-21 after a raced relaunch offered onboarding
  over a live identity.

## 7. Consequences elsewhere

`multi-relay-mailboxes.md` §2 states handles are relay-agnostic — still true, a pair's handle is
the same on every relay. What changes is §4.2's `drain_all(handle)`: a recipient now drains **one
handle per contact** on each relay, so the poll count becomes contacts × relays.

## 8. The polling budget, and why the cap is per person

A recipient's work per round is **contacts × relays**, and every one of those is a request over
Tor. That is the real constraint on this design, so the limit belongs in the design rather than
being discovered on a phone.

**The cap is on total contacts, not on group size.** A per-group cap of 10 sounds like it bounds
the problem and does not: someone in eight groups of ten has up to ~70 contacts while no single
group is oversized. Polling cost, battery, and the fragment count all follow the *total*, so that
is what must be bounded. Groups inherit the limit instead of setting it.

**Proposed: 50 contacts.** With ~8-handle fragments (§5c) that is 7 circuits per round per relay,
which a phone can carry. It is a **UI limit, not a cryptographic one** — nothing in the wire format
enforces it, and a modified client can exceed it and simply pay for it. Stating it as a product
limit is honest; implying the protocol enforces it would not be.

At the cap the app should refuse a new contact with a reason, not fail quietly — and the reason is
worth giving plainly, because "this app limits you to 50 contacts so that a relay cannot rebuild
your address book" is a sentence that explains the product.

## 9. As built (0.1.26)

Where the implementation differs from, or makes concrete, the sections above.

**Switching is gated on proof, not on an announcement (§5.1–5.2).** Each side contributes 32 random
bytes in a `Frame::MailboxKey` over the existing session; the secret is HKDF over both, bound to
both identity keys. A side posts v2 only after the peer sends a confirmation hash proving it
derived the same secret. Announcing a capability alone would let a sender post before the
recipient could compute the handle. A changed contribution (a peer restored an old backup) drops
the pair back to v1 at once and re-agrees. An older build drops the frame undecoded and never
confirms, so it stays on v1 in both directions with no further signalling.

**The notice cannot be on both sides (§5.4).** The side that can act is running a build that has
no such notice and cannot be given one. So the newer side shows a persistent banner worded to be
passed on ("ask them to update"). The only other path to the older side is its own update prompt.
The banner is raised only on evidence: the peer has been active (any authenticated frame, a silent
ack included) more than 10 minutes after our contribution reached them or a relay, and has never
sent theirs. A current build replies to a contribution on receipt and announces its own on every
launch, so that silence means the frame was dropped. A contact who is merely offline shows no
activity and is never flagged. The 10 minutes cover a reply crossing a relay behind frames sent
before ours was read.

**Fragments (§5c).** Seven buckets per epoch, chosen by a keyed hash of a persisted per-device seed,
the epoch and the contact, so a contact's bucket is fixed for the day, survives a restart, and
adding a contact moves nobody else. Each bucket is an arti isolation group. The static v1 handle is
polled in a group of its own, since beside anything it would name the owner of those v2 handles.
Yesterday's, today's and tomorrow's handles each poll in **their own day's** partition, never
together, which would link a pair across days. Each epoch is padded with dummies to a multiple of
8 (minimum 8), stable for the epoch like real handles. Job order is shuffled every round with a
0–300 ms random gap (Tor only). A relay that fails is skipped for the rest of the round; otherwise
each fragment would wait out the 30 s dial timeout in turn. A new relay request, `take_many`,
drains one fragment in one round-trip; older relays get one `take` per handle.

**The cost is higher than §8 estimated, and each group is an onion connection, not a circuit.**
§8 counts 7 circuits per round per relay. Because each day of the three-day window needs its own
partition, the build uses up to **1 + 3 × 7 = 22** isolation groups per relay (16 with one contact,
since the dummies spread across buckets). And arti 0.43 keeps onion-service state **per isolation
group** (`tor-hsclient` `state.rs`: descriptor, hsdir circuits and intro history are never shared
across isolations), so every group is a full onion connection with its own descriptor fetch and
hsdir, intro and rendezvous circuits.

Measured on a Galaxy S25 over real Tor, 2026-09-26, 16 groups against one relay: the first round
after launch is cold, and about 40–50% of fragments fail at the connect ("Unable to download hidden
service descriptor", ~20 s each). Once the groups are warm, a round is 15–16 of 16 in a few seconds
(slowest 3–4 s), and that held across a UTC midnight, when every group is new. Two consequences
were built in: a relay is abandoned for a round only after two misses with no answers (skipping at
the first miss meant no group ever warmed, and no round succeeded for 15 minutes), and a connected
exchange fails after 60 s without progress (a stalled one hung the poller for good).

**The three-day window is polled only where it is needed (built 2026-09-26).** Today's handles are
polled all day. Tomorrow's are polled only in the last 3 hours of the UTC day (`SKEW_MARGIN_SECS`),
for senders whose clocks run fast. Yesterday's are polled until a drain that **started at least 3
hours after midnight** answers on every one of yesterday's fragments on every relay; then the epoch
is retired for the rest of the day. That covers senders whose clocks run slow, and a device that was
offline across midnight keeps polling yesterday however late it comes back. So most of the day costs
1 + 7 groups per relay, with the same number of handles per group as before (about 7 contacts at the
cap). Group tokens stay keyed by (epoch, bucket), so tomorrow's groups are already warm at midnight
and yesterday's are today's from the day before: the rollover opens no cold connections.

The cost is in clock tolerance. The full three-day window tolerated nearly a day of clock error;
this tolerates 3 hours. Mail from a sender whose Unix time is off by more than that goes to a
handle nobody polls and expires unread. Timezones do not matter (epochs are UTC days of Unix time,
the same instant everywhere); only a clock that is actually wrong does, which NTP-synced phones and
desktops rarely are. Direct delivery never uses a handle, so it is unaffected.

Rejected: **rotating buckets** (a pair's handle for day *e* in bucket `(h(pair) + e) mod 7`). It
cuts groups the same way without any clock trade, but handles per group are 3 × contacts ÷ groups,
so each group would tie together about 21 contacts' handles instead of 7. Fewer groups over the
same handles always means bigger groups. The only real saving is polling fewer handles.

**Posting is isolated too.** Posts and recalls ride one isolation group per recipient, so a relay
cannot tell that deposits for two people came from one sender by the circuit they share.

**The cap (§8)** is 50 open, approved chats. A pending request is not a contact yet, and
re-pairing an existing contact is not new. It is checked before anything reaches the network,
both when connecting and when approving, and the refusal states the reason.

**Verified on devices (phone + desktop, 2026-09-27).** New build against old, both directions: the
banner appeared on the newer side and messages kept flowing on v1. New against new: both sides
confirmed agreement, with delivery receipts in 1–10 s. Offline mail under a v2 handle: the desktop
queued a message "under a v2 handle" with the phone app closed, and the phone drained it on its
first cold round (10/10 fragments answered). The receipt came back after 110 s, nearly all of it
the time the phone app spent closed.
