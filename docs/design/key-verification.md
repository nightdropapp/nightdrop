# Design draft — Safety numbers & key-change verification

**Status:** ✅ **implemented** (TODO #18; see `ARCHITECTURE.md` §5b′). Kept here as the design
record. Core: `Node::safety_number` / `safety_qr` / `verify_safety_qr` / `set_verified` +
`Contact.verified`. UI: `app/lib/src/features/chat/verify_screen.dart`. Phase 3 (QR
scan-to-verify) shipped in the first pass.
**Relates to:** `ARCHITECTURE.md` §5 (pairing/authorization), the invariant "authorization
before first message." This is defense-in-depth *on top of* the existing PAKE/QR pairing.

## 1. Problem

Pairing already authenticates the **first** handshake: SPAKE2 binds the short-code session to a
shared secret, and QR carries the pre-key bundle over a visual channel. What's missing is a way
for two humans to **confirm out-of-band that no MITM sat on the pairing channel**, and a clear
**verified/unverified** state per contact — i.e. Signal-style *safety numbers*.

Note a property we get for free: **a contact's id *is* their long-term identity key**
(`contact_id == peer_ik`). So the identity key **cannot silently change within a chat** — a
different key is a different contact-id, which re-enters the authorization flow. That means we do
**not** need mid-session key-swap detection (unlike systems that key by phone number). The work
is purely: *derive a comparable fingerprint, let users mark it verified, and surface the state.*

## 2. What we're defending against
- A **compromised pairing channel** — a tampered QR, or a rendezvous/relay that somehow subverts
  the short-code exchange — where an attacker substitutes their own key. SPAKE2 already makes
  this very hard; the safety number is the belt-and-suspenders check a careful user can perform.
- **Re-pairing confusion** — you re-pair with someone who reinstalled (a genuinely new identity)
  vs. an impostor. Verification state makes "this is a *new*, unverified identity" explicit.

Not defended (out of scope, honest): a user who never compares the number. Safety numbers are an
opt-in, user-driven check.

## 3. Mechanism

### 3.1 Safety number (fingerprint)
A deterministic value derived from **both** long-term identity keys, identical on both devices:

```
material = sort_bytes(own_ik_raw, peer_ik_raw)          // canonical order → same on both sides
digest   = SHA-256("nightdrop/safety/v1" ‖ material[0] ‖ material[1])
number   = digits(digest)                                // e.g. 60 decimal digits, 12 groups of 5
```

- Rendered as 12 space-separated 5-digit groups (Signal's format) — easy to read aloud/compare.
- Also expose the raw 32-byte digest as a **QR** so a pair can *scan* instead of read: scanning
  the other device's QR compares digests and, on match, marks verified automatically.
- Pure function of two public keys → **no new protocol, no network, no key material at risk.**
  Computable entirely from `own_ik` + `contact_id`.

### 3.2 Verification state
Per contact: `verified: bool` (default `false`), persisted.
- User compares the number out-of-band (in person / trusted voice) or scans the QR, then taps
  **"Mark as verified."**
- Because a changed identity key is a new contact-id, `verified` is naturally scoped to the exact
  key; a re-paired contact starts `unverified` again. No separate "key changed!" alarm is needed
  — the new contact simply isn't verified yet, and the UI says so.

## 4. Surface

### 4.1 Core (Rust)
- `fn safety_number(&self, contact_id) -> String` — the grouped-digits string (pure).
- `fn safety_qr(&self, contact_id) -> String` — base64url of the 32-byte digest for the QR.
- `fn verify_safety_qr(&mut self, contact_id, scanned: String) -> bool` — compare + set
  `verified` on match.
- `set_verified(contact_id, bool)`; add `verified` to the Contact DTO + `PersistedChat`
  (`#[serde(default)]`).
- All derivations live in `core/` (key material never crosses to Dart) — only the *rendered
  string* and a bool cross the FFI.

### 4.2 App (Dart)
- Extend the existing chat app-bar **"Their identity"** action (`chat_screen.dart`,
  `_showIdentity`) into a **Verify screen**: shows the safety number, a "Scan to verify" button
  (reuses the `flutter_zxing` scanner), a "Show my QR" (`qr_flutter`), and a **Mark verified**
  toggle.
- **Badges:** a small ✓ on verified contacts (home list + chat title); an unobtrusive
  "Unverified — tap to verify" affordance otherwise. No nagging banner (verification is optional).
- Reuse the QR scanner/renderer already in the app — no new deps.

## 5. Persistence
`PersistedChat.verified: bool` (`#[serde(default = false]`). Nothing else; the number is derived
on demand, never stored.

## 6. Threat model / limits
- Safety numbers detect a MITM **on the pairing channel** *if users compare them*. They don't
  replace SPAKE2/QR — they audit it.
- The number is symmetric (both keys, sorted) so both devices show the **same** value; a mismatch
  = different keys on the two ends = something substituted a key. That's the signal.
- No downgrade risk: an attacker can't make the number "match" without holding the same keys.
- Zero metadata to any server (fully local derivation).

## 7. Phasing
1. **Core derivation + DTO/persistence + FFI** (`safety_number`, `verified`). Unit tests: same
   number on both ends; changes iff a key changes; stable ordering.
2. **Verify screen** (display + Mark verified) and badges.
3. **QR scan-to-verify** (mutual, using the existing scanner/renderer).

## 8. Open questions
- Format: 60 decimal digits (Signal-familiar) vs. a word list (BIP39-style, friendlier to read
  aloud). Suggest digits for v1, words later.
- Should enabling opt-in **server backup / peer backup** (#7) surface verification state too
  (e.g. "verify before trusting a restored contact")? Probably a later cross-link.
- Whether to *require* verification before the first message for high-security mode (an optional
  stricter setting on top of the existing authorization gate).
