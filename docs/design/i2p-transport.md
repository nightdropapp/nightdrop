# Design — I2P as a second transport

**Status:** assessed 2026-09-23, **not adopted**. Revisit against §7's conditions.
**Prompted by:** a user email asking whether Tor and I2P could both be offered.
**Relates to:** `ARCHITECTURE.md` §6 (transport), `android-bridges.md` (the censorship path we
did take), `cover-traffic.md`.

## 1. Why this is a fair question

[I2P](https://i2p.net/) is a real anonymity network with a genuine claim, not a curiosity. Its
destinations are self-authenticating addresses reachable without revealing an IP — the same
property Night Drop already relies on from v3 onion services. Offering a choice would mean not
depending on the health, funding or reachability of a single network.

And the seam was built for it. `core/src/transport/mod.rs` says in its own docstring that the
trait *"leaves room for I2P/Snowflake/etc."*, and that holds up on inspection:

* `Address` is a plain `String`. Nothing in the core validates onion format.
* `tor.rs` already bridges an **async** router to the sync `Transport` trait over channels on a
  background thread — precisely the shape an async I2P router would need.
* `relay_dialer`, `onion_get*` and `published` are already `Option`/defaulted, so a transport that
  cannot do one of them degrades rather than breaks.

So this note is **not** "it doesn't fit". It fits. The reasons for declining are elsewhere.

## 2. The prerequisite now exists — and is far too young

Until recently there was no embeddable pure-Rust I2P router, which ended the discussion on its own.
`i2p-rs` is a **SAM client library**: it talks to a router someone else is running, which is a
daemon dependency and a non-starter for the same reason a Tor daemon was (iOS, and not shipping a
second process).

[`emissary`](https://github.com/eepnet/emissary) changes that. It is an embeddable, async,
pure-Rust I2P router — structurally the arti-equivalent. That is the one hard blocker lifted.

But as of this assessment [`emissary-core` is **0.4.0**, published April 2026](https://lib.rs/crates/emissary-core):
five releases, three of them breaking, upstream describing it as experimental and not recommended
for production. It is ~97K SLoC with ~342K more in dependencies, and it implements **NTCP2, SSU2,
I2CP, SAMv3 and the garlic layer itself**.

That last part is the objection that matters. `CLAUDE.md` requires audited crates and explicit
justification for new cryptographic surface. This would place an entire young, unaudited
crypto-and-transport stack **inside the security core** — the one place the project says
security-critical code must live and therefore the one place it must be most conservative. arti is
the Tor Project's own, funded, and scrutinised for years. This is not "somewhat less mature"; it is
a different risk class, and adopting it would trade a known-good dependency for an unknown one in
the component with the least margin for error.

None of that is a criticism of emissary, which looks like good work. It is a statement about
what stage it is at relative to what it would be carrying.

## 3. Choice itself has a cost: the anonymity set

This is the argument that survives even if emissary were mature tomorrow.

Night Drop's user base is small. Splitting it between two networks makes **both halves smaller**,
and for a messenger the anonymity set is not a nice-to-have — it is substantially the product.
Someone who selects the minority network has also made a distinguishing choice, which is the
opposite of what an anonymity tool should ask of its users.

A privacy feature that requires the user to pick correctly has usually failed before it shipped.

## 4. "Or even use both" is the part to reject hardest

Running both simultaneously sounds like belt-and-braces and is a regression.

An identity reachable at **both** a `.onion` and a `.b32.i2p` has handed anyone who sees both
addresses a link between them: same person, two networks. A seized relay holding both for one
contact gets it for free; an adversary observing both networks can correlate by timing. The entire
direction of `mailbox-handles.md` is to reduce what a relay can correlate per identity — publishing
a second address per identity moves the opposite way.

Dual-homing is worse than either network alone. If I2P is ever adopted it should be **exclusive per
identity**, chosen once, never simultaneous.

## 5. Client-only is possible — the original objection was overstated

An earlier version of this assessment said I2P "assumes you are a router" and implied a phone could
not sensibly participate. **That was too strong, and the correction is worth recording.**

I2P supports client-only operation as a first-class mode. In **hidden mode** a router does not
publish its RouterInfo to the netDb, does not accept participating tunnels, and refuses direct
connections to routers in its own country. It is explicitly intended for users who must not route
traffic for others, and it is what any mobile client would use. So a Night Drop client would *not*
have to relay strangers' traffic on a metered, battery-powered device.

What remains true is that it costs something, by I2P's own account:

* **I2P documents hidden mode as reducing anonymity.** The stated mechanism is that a node which
  carries no participating traffic gives its immediate peers cleaner inference — everything
  entering and leaving it is its own, where a participating router's traffic is mixed with
  other people's.
* **Tor has no equivalent penalty.** Being a pure client is the overwhelming norm there; the
  client/relay split is the design. In I2P, routing is the norm and hidden mode is the exception,
  so the mobile-appropriate configuration is also the unusual one.
* At scale, a messenger population that only consumes tunnels is a free-rider on a volunteer
  network — not a blocker, but a thing to be honest about before pointing thousands of phones at it.

So the accurate statement is: **client-only works, would be the right choice, and hands back some
of the anonymity the switch was made to gain.** It weakens the case rather than ending it.

Worth reading before any revisit: recent published work on
[de-anonymising hidden I2P services via behaviour alignment](https://arxiv.org/pdf/2512.15510)
bears directly on a messenger that would run the I2P equivalent of a hidden service. Unreviewed
here; flagged so the next assessment starts from it rather than from vibes.

## 6. The benefit it aims at is already served — and I2P is weak at it

The honest practical case for I2P is *"my network blocks Tor."*

**0.1.22 already answers that**, with in-app bridges and an in-process WebTunnel carrying Tor
inside ordinary HTTPS — shipped, on by default, reproducible, and tested against a blocked network
and against an IDS with 52,311 signatures (`android-bridges.md` §6.5).

And I2P is not strong here. Its bootstrap is **reseed over HTTPS from a small, well-known set of
servers**, which is a straightforward chokepoint, and it has nothing equivalent to Tor's pluggable
transports. I2P is *less often* blocked mainly because censors have less reason to bother — that is
obscurity, not resistance, and obscurity stops working precisely when a tool becomes worth blocking.

Adopting an unaudited stack to answer a question already answered better is the wrong trade.

## 7. What would change the answer

Concrete, so a future reader is not re-litigating taste:

1. **emissary (or a successor) reaching production maturity with independent review** — a stable
   release line, and ideally an external audit of the crypto and transport layers.
2. **Evidence of real users blocked from both Tor and WebTunnel.** This is the one that would
   actually justify it, and it is currently unmeasured. Until someone is genuinely cut off from
   both, I2P is a second answer to a solved problem.
3. A decision to make it **exclusive per identity**, never dual-homed (§4).

(1) is plausible within a year or two. (2) is the gate.

## 8. If it were built, the non-obvious work

Recorded because the transport implementation is the visible part and not the expensive one:

* **`diag.rs` redacts only `.onion`.** `redact_onion` scans for that literal suffix, so a
  `.b32.i2p` destination would pass straight through into diagnostic output. Any second address
  format needs the redactor extended **first**, with tests — this is a leak, not a cosmetic gap.
* **Backups must bundle the I2P destination keys**, exactly as they must already bundle the Tor
  onion keystore. The failure mode is identical and already documented: a restore without the key
  yields a new address, stale peer addresses everywhere, and both sides restored means permanently
  "peer offline".
* **The update check is `onion_get`**, whose `None` contract means *do nothing* — that must stay
  true rather than quietly acquiring a second path (the comment there records a real clearnet leak
  from exactly this mistake).
* Pairing payloads, QR parsing, address-rotation frames (`Frame::Address`) and the relay's
  address handling all assume one address shape per identity.
* Doubling the platform matrix: Android, Linux, and an iOS story for a full router under
  background-execution limits.
