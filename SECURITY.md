# Security Policy

Night Drop is a privacy-and-security product: a vulnerability here can expose exactly
what the design promises to protect. We take reports seriously and want to make
responsible disclosure easy.

> **Before this is live:** point `nightdrop.app` at the site so `SECURITY.md`,
> `/.well-known/security.txt`, and `/pgp.txt` resolve, and make sure
> `security@nightdrop.app` is a monitored inbox. Until the domain serves these, the
> channel below is configured but not yet reachable.

## Reporting a vulnerability

**Please report privately — do not open a public issue for a security bug.**

- Email: **security@nightdrop.app** (monitored inbox for security reports).
- Encrypt sensitive reports with our PGP key:
  fingerprint `079B A016 9201 A8AB 11F3  2385 884E ACB8 89D0 2002`.
  Public key: <https://nightdrop.app/pgp.txt> (also referenced from `security.txt`).

If you cannot use email, open a **private** report through the source host's
confidential/security advisory channel (e.g. a GitHub/GitLab security advisory) once the
public repository exists.

Please include, as far as you can:

- A clear description of the issue and its security impact.
- Step-by-step reproduction, a proof-of-concept, and the affected version/commit.
- The platform (Android / Linux desktop) and build (release, F-Droid, self-built).
- Any suggested fix or mitigation.

## What's in scope

The security-critical surfaces, in rough priority order:

- **The Rust core (`core/`)** — identity keys, the Double Ratchet, the SPAKE2 short-code
  handshake, the at-rest store encryption, and the Tor transport.
- **The wire protocol and frame parsing** — anything reachable with attacker-controlled
  bytes from a relay or an unpaired peer (memory-safety, panics/DoS, auth bypass).
- **The relay protocol** (`relay/`) — the store-and-forward mailbox, rendezvous, and the
  guarantee that a relay only ever handles opaque, E2E-encrypted blobs with no keys,
  plaintext, or persistent identity-linked metadata.
- **Pairing & authorization** — QR pre-auth and short-code PAKE, "authorization before
  first message", and MITM resistance / safety-number verification.
- **Backups** — the user-owned password model (§7): the password is never persisted or
  sent to a server; the server-stored blob is opaque.
- **The invariants in `CLAUDE.md`** — a report that breaks one of them (server-side keys
  or logs, plaintext leaving the Rust core, a non-anonymized network path, etc.) is
  always in scope.

See `ARCHITECTURE.md` (§8 threat model, §8a metadata resistance) for the intended
guarantees, and `DEPENDENCIES.md` for the no-phone-home posture.

## What's out of scope

These are documented limitations, not vulnerabilities (see `ARCHITECTURE.md` §8/§8a):

- A fully compromised endpoint (malware with root/jailbreak) or an attacker with the
  unlocked device beyond the at-rest device-theft model.
- A global passive adversary de-anonymizing Tor itself, and low-latency traffic-analysis
  / timing correlation (v1 ships no cover traffic — stated, not fixed).
- Loss of history after device loss without an opt-in backup, or a lost/forgotten backup
  password — data loss by design.
- Reports that require the user to disable protections, install a malicious build, or act
  against explicit in-app warnings.
- Spam/DoS purely from sending your own well-formed, authorized traffic (but resource
  exhaustion of the relay via malformed or unauthenticated input **is** in scope).

## Safe harbor

We will not pursue or support legal action against researchers who, in good faith:

- make a genuine effort to avoid privacy violations, data destruction, and service
  disruption to others while researching;
- only interact with accounts/identities/devices they own or have explicit permission to
  test; and
- give us a reasonable chance to remediate before public disclosure.

If in doubt, ask first — we'd rather help you test safely than have you hold back.

## Our commitment

- We aim to acknowledge a report within **3 business days** and give an initial
  assessment within **10 business days**. (Best-effort; this is a small project.)
- We practice **coordinated disclosure**: we'll agree a timeline with you, fix the issue,
  and — with your consent — credit you in the release notes and this repository.
- Because the build is designed to be **reproducible** (`docs/reproducible-builds.md`),
  fixes can be independently verified against the published source.

## Cryptography note

Night Drop deliberately uses audited primitives (`vodozemac`, `arti`, a vetted PAKE) and
keeps all key material in the Rust core. Reports of hand-rolled crypto, primitive misuse,
nonce/key reuse, or ratchet/state-machine flaws are especially welcome.
