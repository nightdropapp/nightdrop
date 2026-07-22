# Design/research — Post-quantum key agreement (PQXDH)

**Status:** **Option D implemented** (hybrid-encapsulated pairing payload, §4 below / `core/src/pqkem.rs`);
Options A/B still tracked. Relates to the anonymity/injection review (TODO #23) and `ARCHITECTURE.md` §3
(crypto). The point of this doc is to (a) state the exposure honestly, (b) show what is *already* safe,
and (c) give a phased plan that keeps the "prefer audited crates" invariant instead of hand-rolling PQ
crypto.

## 1. Where we are today

- Crypto core is **vodozemac 0.8** (Matrix's pure-Rust Olm/Megolm): **X3DH-style** initial key
  agreement + **Double Ratchet**, all over **Curve25519 / Ed25519** — entirely **classical**.
- As of 2026, vodozemac has **no PQXDH / PQ key agreement** (verified: upstream still ships the
  classical Olm ratchet). Signal's PQXDH (X25519 **+ CRYSTALS-Kyber/ML-KEM**) is the reference for
  what "done" looks like; Matrix/vodozemac PQ work is roadmapped but not released.

## 2. The actual threat: harvest-now-decrypt-later (HNDL)

No cryptographically-relevant quantum computer (CRQC) exists, so there is **no live break** today.
The real exposure is **HNDL**: an adversary who can *record* ciphertext now (a relay operator, a
network observer, a seized backup) could decrypt it *later*, once a CRQC can break Curve25519. For a
privacy messenger whose whole point is protecting high-risk users over long time horizons, HNDL on
the **initial handshake** is the exposure worth taking seriously.

## 3. What is already quantum-adequate (don't over-scope)

- **The symmetric ratchet** — message keys derived via HKDF/HMAC, AEAD with ChaCha20-Poly1305 /
  AES-256 — is **not** meaningfully threatened. Grover only halves brute-force security; 256-bit
  symmetric keys keep a ~128-bit PQ margin. **No change needed here.**
- So PQ work is **only** about the *asymmetric* parts:
  1. the **initial key agreement** (X3DH over X25519) — the HNDL-critical one, since that root
     secret protects the whole session; and
  2. the **DH ratchet steps** (Curve25519 DH on each ratchet turn) — for ongoing forward
     secrecy / post-compromise security against a *future* quantum adversary.

PQXDH (Signal's first step) fixes (1). A fully PQ ratchet (Signal's later "PQ3", or a KEM-based
ratchet) also fixes (2). (1) is where ~all the HNDL value is; (2) is a longer-horizon refinement.

## 4. Options

**A. Wait for vodozemac/Matrix PQ, adopt upstream.** Lowest risk to the "audited crates" invariant;
zero hand-rolled PQ. Con: timeline is not ours. **Recommended as the primary track — watch upstream
and adopt when it lands.**

**B. Hybrid PQXDH at pairing (interim, HNDL mitigation).** Keep vodozemac's ratchet, but at pairing
mix a **ML-KEM-768** shared secret into the initial root key: both sides exchange an ML-KEM public
key inside the material they already exchange (QR `pair?` payload / SPAKE2-sealed short-code payload),
encapsulate, and fold the KEM secret into the session KDF. An attacker must break **both** X25519 and
ML-KEM to recover the root — HNDL on the handshake is closed. **Blocker:** vodozemac's X3DH/session
establishment is internal; injecting external key material needs a vodozemac API (or a thin,
carefully-scoped fork/upstream PR). Track this as the enabling dependency — do **not** work around it
by bolting a static-key outer layer (that would lack forward secrecy).

**C. Migrate to a PQ-capable protocol lib.** e.g. an MLS impl (OpenMLS) with a PQ ciphersuite, or
libsignal (PQXDH/PQ3). Large migration, and MLS is group-oriented — our documented door for groups,
not for 1:1 v1. Out of scope now; revisit if/when groups (MSC-style MLS) are on the table anyway.

**D. Smaller self-contained win now: PQ-protect the *pairing payload*.** ✅ **Implemented** (§4.1).
Independently of the ratchet, the **short-code rendezvous** payload (prekeys, onion address) — the part
that actually crosses the untrusted relay — is now sealed under a **hybrid** key. The joiner ships a
one-time **ML-KEM-768** public key in its SPAKE2 opener; the inviter encapsulates; the seal key is
`HKDF(SPAKE2_secret ‖ ML-KEM_secret)`, so recovering the payload needs breaking **both** the classical
PAKE and ML-KEM. Lives in `core/src/pqkem.rs` (RustCrypto `ml-kem`, FIPS 203) + the handshake in
`core/src/node.rs`. The **QR** path is deliberately *not* wrapped: the QR is scanned optically, in
person — it never crosses the network, so there is nothing to harvest. This is the stepping stone to
(B); it does **not** touch the ratchet (still (B)/upstream's job).

## 5. Crate choices (if/when we implement B or D)

Use a **vetted** ML-KEM implementation, never hand-rolled (CLAUDE.md invariant): RustCrypto's
`ml-kem` (FIPS 203) or `pqcrypto`/`pqcrypto-mlkem`. Always **hybrid** (classical **+** PQ, secrets
combined via HKDF) — never PQ-only — so a flaw in the young PQ primitive can't be *worse* than today.
Any new primitive needs the explicit justification the conventions require; this doc is the start of it.

## 6. Recommended plan

1. **Track upstream (Option A)** as the primary path; adopt vodozemac PQ when released.
2. **File the enabling dependency** for Option B: a vodozemac API to inject extra key material into
   session establishment (upstream issue/PR). Until it exists, B is blocked — say so, don't fake it.
3. ✅ **Done:** shipped **Option D** (hybrid-encapsulate the short-code pairing payload) as the
   self-contained interim. Option B follows once the vodozemac hook (step 2) exists.
4. Leave the symmetric ratchet alone (already adequate); scope PQ strictly to the asymmetric handshake.

## 7. Honest bottom line

This is a **real but non-urgent, long-horizon** gap: no live break exists, the symmetric layer is
fine, and the fix for the part that matters (handshake HNDL) is **gated on vodozemac exposing a hook**
or on an upstream PQ release. The responsible move is to **track upstream, keep everything hybrid, and
not hand-roll a PQ ratchet** — not to rip out an audited classical stack for a bespoke one.

## Sources
- Signal, "Quantum Resistance and the Signal Protocol" (PQXDH = X25519 + CRYSTALS-Kyber): https://signal.org/blog/pqxdh/
- vodozemac (Olm/Megolm, pure Rust; classical): https://github.com/matrix-org/vodozemac
- Survey of PQ support in crypto libraries (2025): https://arxiv.org/html/2508.16078v1
