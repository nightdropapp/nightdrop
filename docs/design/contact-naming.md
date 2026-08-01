# Design draft — Telling your contacts apart

**Status:** 🟢 implemented (2026-08-01). Not yet exercised on a device.
**Relates to:** `ARCHITECTURE.md` §4 (per-chat names), the key-verification design (safety
numbers), and `#5` multiple identities, which multiplies the problem.

## 1. Problem

Every contact starts as **"Anon"**, and nothing makes them change it. With two or three chats the
list reads Anon, Anon, Anon. Reported from real use: *"it makes it impossible to know which chat to
look at."*

This is not cosmetic. In a messenger whose entire premise is who is on the other end, an
indistinguishable contact list means **sending a message to the wrong person** — a confidentiality
failure produced by the UI, not by the crypto.

The gap is specific: `set_my_name` lets you rename *yourself* in a chat (announced to the peer), but
there is no way to label *them*. `their_name` is whatever they chose, or "Anon" forever.

## 2. A local nickname — the actual fix

A name **you** give a contact, stored on your device and **never sent**. Only you know that this key
belongs to the person you met; the peer cannot supply that knowledge and should not be asked to.

Offered at the moment of approval, when you have just paired and know exactly who it is, and
editable later from the chat. No protocol change, no metadata on the wire.

## 3. A derived tag for contacts you haven't named

A nickname only helps if it is set. The default must still distinguish, so an unnamed contact shows
its **identity tag**: 6 characters derived from a hash of their identity key, rendered in monospace
next to the name — `Anon · K7QF2M`.

**Derived, not random.** A random per-contact label would also disambiguate, but it would be *stable
across an identity change* — hiding exactly what you want to see. A tag derived from the identity
key changes when the key changes, so a contact who reappears as a different identity looks
different. That is the whole reason to prefer it.

**Shown even when the peer has set a name.** Two contacts can both call themselves "Alex". The tag
is suppressed only when *you* have given a local nickname, because that is the point at which you
have vouched for who this is.

## 4. What the tag is not

It is **not verification**, and the UI must never let it look like verification. Two traps:

* **False familiarity.** Users start reading "same tag = same person". The safety number is the
  check; the tag is a disambiguator.
* **Grinding.** 6 characters is ~30 bits. Accidental collision across a handful of contacts is
  negligible, but an attacker who *wants* a matching tag can generate keys until one matches. That
  is cheap, and it is why the tag can never carry trust.

So it is styled as an identifier, not a name — monospace, muted, always beside the name rather than
replacing it — and the verification screen keeps the full safety number as the only thing that
answers "is this really them".

## 5. Deliberately not doing

* **No auto-generated fake names** ("Blue Fox"). They read as names, which invites exactly the
  false familiarity §4 warns about, and they would be indistinguishable from a peer's chosen name.
* **No avatars derived from the key.** Same trap in picture form, and harder to compare carefully.
* **Not sending the nickname.** It is yours. Sending it would leak how you think of people, for no
  gain — the peer already knows who they are.
