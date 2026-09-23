# Design — Small group chat, owner-run, pairwise

**Status:** design agreed 2026-09-22, not implemented and not scheduled before 0.3.
**v1 is still strictly 1:1** (`ARCHITECTURE.md`, `CLAUDE.md` conventions) — this note records a
shape and its costs, it does not license group assumptions in current code.
**Depends on:** `mailbox-handles.md` (the fan-out leak is unacceptable without it).
**Relates to:** `key-verification.md`, `cover-traffic.md`.

## 1. Shape: pairwise fan-out, no group key

A group message is sent as **n separate 1:1 messages** over the Double Ratchet sessions that
already exist between each pair. There is no group key, no group ratchet, and no new cryptographic
primitive — which is the point. `CLAUDE.md` requires explicit justification for any new primitive,
and "we wanted group chat" is not one.

**MLS is the correct answer to group messaging** and remains the door `ARCHITECTURE.md` leaves
open. It is also a large, stateful protocol with real implementation risk, and adopting it to serve
groups of under a dozen people would be the tail wagging the dog. Pairwise fan-out reuses code that
is already shipped and already audited.

What that costs, stated rather than discovered:

* **O(n) work per message** — nine ciphertexts, nine deposits, for one thing typed.
* **No group forward secrecy.** Each pair is forward-secret on its own; there is no group secret to
  compromise, and equally none to rotate. §5 is the consequence.
* **No total order.** Without a server to serialise, each member sees messages in their own arrival
  order. In a small group this is mostly invisible and occasionally confusing.

## 2. Membership: an owner, no admins

The **owner** is whoever created the group — concretely, the person whose QR or mnemonic the others
joined from. Only the owner adds members. There are no admins and no delegation.

This is deliberate. Delegation needs a way to express and revoke authority, which in a leaderless
offline-first mesh means either consensus or a key hierarchy. Both are large; both fail badly when
half the members are offline. A single owner is the design a ten-person group actually needs.

## 3. Joining: in-band, no new pairing surface

Shawn's question was whether groups need their own joining page. They do not, and should not have
one.

A group invite is a `GroupInvite` frame sent **over an existing 1:1 channel** — so you can only be
added to a group by someone you have already paired with, through the ordinary QR or short-code
path. There is nothing new to scan and no second pairing surface to get wrong.

The consequence is a feature: **a stranger cannot be added to a group.** The owner must pair 1:1
first. That keeps the *authorization before first message* invariant intact at the outer edge.

## 4. The invariant this bends, and how far

Pairwise fan-out needs a session between **every pair**, not just between each member and the
owner. So the owner acts as an **introducer**, handing each member the others' addresses and keys —
the machinery for which already exists (`Frame::Address`, `Frame::Relays`).

**That is a real weakening and it must be named.** Elsewhere in Night Drop a first message requires
a QR bundle or a PAKE secret; here a member accepts a peer because the owner vouched for them. An
owner who lies — or whose device is compromised — can introduce an impostor.

It is bounded rather than waved away:

* the introduction comes over an **authenticated, confidential** channel, so only the owner can
  make it, not the relay and not the network;
* **out-of-band verification already exists** (`key-verification.md`, `Frame::Verified`), so a
  member can confirm a peer independently;
* therefore group members must be shown as **unverified until verified**, visibly, rather than
  presented as equivalent to a hand-paired contact.

Trusting the owner is the price of not running MLS. A user can reasonably accept it for a circle
they already trust — which is the only group this design is for — provided the product says so.

## 5. Removal is not revocation

With no group key there is nothing to rotate, so removing a member means every remaining member
stops sending to them. A removed member:

* keeps **all history** they already received — unavoidable in any design;
* keeps receiving from **anyone who has not yet applied the removal** (offline, or running a
  modified client that ignores it).

So removal is **convention, not enforcement**, and the UI must say that plainly rather than showing
a reassuring "removed". This is the sharpest argument for MLS later, where removal is a genuine key
operation — worth recording as the thing that would justify the migration.

Losing the owner **freezes** the group: members keep talking, nobody can be added or removed. There
is no succession, for §2's reasons. The recovery is to make a new group, which is honest and
obvious, where a half-working succession protocol would be neither.

## 6. Size, and where the limit comes from

A group does **not** get its own cap. It spends from the per-person contact budget in
`mailbox-handles.md` §8: a group of ten costs nine contacts, and someone in several groups pays for
each. A per-group cap of 10 looks like a bound and is not — eight such groups is ~70 contacts.

## 7. Interaction with the rest

* **Mailbox handles are a precondition.** A fan-out of nine deposits to nine *static* handles
  publishes the group's membership to the relay in one burst. Per-pair handles mean those nine
  deposits are not visibly related; the burst itself remains (`mailbox-handles.md` §6).
* **Relay TTL bites harder in a group.** A member offline for more than 24 hours misses a message
  outright. In 1:1 the sender sees it undelivered; in a group the conversation simply moves on
  without them, and nothing currently makes that visible.
* **Per-chat disappearing timers are per *pair*.** Nine pairwise sessions can hold nine different
  horizons for what the user thinks is one conversation. A group timer has to be set across all of
  them and reconciled when they disagree — and the same applies to per-message burn
  (`burn-messages.md`), where one member on an older client is enough to keep the message.

## 8. Open

* Does a group message show per-member delivery ("4 of 9"), or nothing? Per-member is honest and
  also a small presence leak to the sender.
* Whether membership changes are gossiped between members or only announced by the owner. Owner-only
  is simpler and makes the owner a single point of failure for state, not just for trust.
