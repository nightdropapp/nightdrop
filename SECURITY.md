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

## Audit status

**Night Drop has not yet had an independent, external security audit.** The cryptographic
*primitives* it builds on are audited libraries, and the security-critical code is isolated in one
Rust core with automated tests and fuzzing — but the *integration* (the pairing/PAKE protocol, the
relay, at-rest storage, and the Dart↔Rust FFI boundary) has **not** been independently reviewed. An
external audit is a prerequisite for treating Night Drop as battle-tested; when one is done we will
publish it and any resulting fixes. Until then, treat the app as promising and improving, not
proven — and this is exactly why the reports above are valuable.

### Known hardening items for the auditor

Defense-in-depth items we've already identified in the at-rest storage layer (none is a known
vulnerability under the device-theft threat model, but each is worth an auditor's judgement):

- **AEAD key separation.** The 32-byte store key is currently used both as the ChaCha20-Poly1305
  seal key *and* as vodozemac's pickle-encryption key. The constructions are independent, but
  deriving two purpose-separated sub-keys (HKDF, distinct `info`) would be the textbook-clean form.
  Changing it is an at-rest-format change and needs a migration.
- **Domain separation / associated data.** `storage::seal` binds no associated data, so the state
  file and sibling sealed media files aren't cryptographically bound to their role. A local
  attacker with *write* access could swap or roll back sealed files (all still the user's own data
  — confusion/rollback, not disclosure). Binding each blob to its role via AAD closes it; also a
  format change needing migration.
- **Backup KDF parameters.** Backups use `Argon2id` at the crate default (~19 MiB, t=2). This is
  safe *because backup passwords are randomly generated (~100 bits)*, so entropy — not KDF cost —
  is the load-bearing defense. **Invariant to preserve:** never let a user choose their own
  server-backup password, or the fixed-salt recovery handle becomes brute-forceable by the relay.
- **Screenshot detection is best-effort, and its absence proves nothing.** *(Known limit, by
  design.)* Screenshots are deliberately **not** blocked: a permanent `FLAG_SECURE` only pushes a
  determined person to photograph the screen with a second device, which no software can detect,
  while breaking a legitimate thing users want. Instead a capture is reported — locally and to the
  peer over an authenticated control frame (`Frame::Screenshot`). The app-switcher preview is
  suppressed outright on Android 13+ (`setRecentsScreenshotEnabled`); on older releases that
  suppression is best-effort and can fail, since the only tool available there is a window flag set
  as the app leaves the foreground, which does not reliably beat the system's snapshot. The
  reporting is blind to
  **every Android below 14** (`Activity.ScreenCaptureCallback` is API 34+), **every desktop
  platform**, **screen recording**, and **a camera pointed at the screen**. So a peer who sees no
  notice has learned nothing, and the UI/website must never imply otherwise. What *is* blocked is
  the Recents thumbnail, since that capture has no user intent behind it.
- **App-lock PIN entropy.** *(Known and disclosed, not a bug.)* The opt-in app lock
  (`ARCHITECTURE.md` §7d) accepts either a PIN or a passphrase. A **passphrase** protects the
  history against someone who copies `store-key.lock` off the device; a short **PIN does not**, and
  no key-derivation cost can change that — ~20 bits is exhaustible whatever the cost per guess. The
  lock uses `Argon2id` at 64 MiB / t=3 (far above the backup default above, since this secret
  is user-chosen), and the app rate-limits attempts, but both only slow an attacker typing at the
  phone. A PIN is offered because "stops the person who picked up my unlocked phone" is a real
  threat worth covering; the UI says which threat each option covers instead of implying parity.
  Hardware-throttled unlock (Keystore user-authentication binding) is the only real fix for short
  secrets and is deliberately not used, because it cannot support a duress secret.
- **The wipe code inherits the PIN's limit, and cannot be rehearsed.** *(Known limit, by design.)*
  A second secret at the lock screen deletes the identity instead of opening it (`#3`,
  `docs/design/duress-wipe.md`). Two things it does **not** do. It does not protect a device that
  was **imaged before** you used it: the copy still holds the lock file, and a short PIN there is
  exhaustible exactly as above — duress defends against the person holding your phone, not against
  a forensic copy already taken. And it cannot be practised: entering it destroys everything, every
  time, which is what makes it work under pressure. Arming it self-checks that the code opens the
  duress slot, so it can fail closed rather than silently.
  The lock file is written so an armed wipe code is **indistinguishable** from an unarmed one, and
  the app never displays whether one is set outside the wipe-code screen itself — a "duress: on"
  readout in a menu or banner would tell whoever picks up the phone that it exists. **Your contacts
  are not notified**, and cannot be: the wipe runs at the lock screen with no store key (the duress
  secret unwraps filler, by design), so there is no ratchet state to authenticate a "chat deleted"
  notice with, for anyone. Peers learn from your silence — agree on an out-of-band fallback before
  you need one. A server backup, if you made one, is **not** removed by
  the wipe — it is addressed by a handle derived from the recovery password, which is never
  persisted, so nothing on the device can find it. It stays opaque and expires within its TTL
  (24h default, 36h max), but a written-down recovery password can be compelled inside that window.
- **Store-key zeroization.** *(Addressed.)* The long-lived at-rest key is now wiped on drop and the
  transient decode/generate buffers are zeroized; a live-memory/cold-boot attacker still sees
  plaintext history in RAM, so this is hygiene, not a fix for a disclosure bug.

## Verifying downloads

Every release binary is signed with the Night Drop release key, so you can confirm a download
hasn't been altered.

**Signing key** — `Night Drop Security <security@nightdrop.app>`
Fingerprint: `079B A016 9201 A8AB 11F3  2385 884E ACB8 89D0 2002`
The public key ships with each release as `nightdrop-signing-key.asc` (and is the same key used
to sign the operator-signed relay directory).

**Verify (Linux/macOS):**

```sh
# One-time: import the key and check its fingerprint against the value above.
gpg --import nightdrop-signing-key.asc
gpg --fingerprint security@nightdrop.app

# Verify a specific download:
gpg --verify Night_Drop-x86_64.AppImage.asc Night_Drop-x86_64.AppImage
gpg --verify NightDrop.apk.asc NightDrop.apk

# Or verify the whole release at once via the signed checksum manifest:
gpg --verify SHA256SUMS.asc SHA256SUMS   # authenticity of the manifest
sha256sum -c SHA256SUMS                  # integrity of each file against it
```

A `Good signature` line (from the fingerprint above) means the file is exactly what was released.
The Android APK is additionally signed with the app release key, which Android verifies on install.
