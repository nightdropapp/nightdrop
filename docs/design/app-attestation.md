# Design draft — "Official app" verification of a contact (0.3)

**Status:** 🟡 design only (2026-10-18), for 0.3. Not built. §8 lists what must be measured on a
device before any code is written; §8.1 can cancel the whole feature.
**Asked for by:** the maintainer, 2026-10-18, after Block screenshots became two-sided (a contact's
app honours it cooperatively, and a modified app need not): "it would be nice to know if the hash or
something is different to let the other person know that it's not the official app".
**Relates to:** `docs/design/screenshot-transparency.md` (the cooperative block this would back up),
`docs/design/key-verification.md` (the safety number, which proves *who*, not *what software*),
`ARCHITECTURE.md` §1a (anonymous identities; security-critical code in Rust; audited crates),
`docs/trust-boundaries.md`.

## 1. The question, and why a hash cannot answer it

"Is my contact running the official Night Drop?" A modified app controls everything it says about
itself, so anything it **reports** — a hash of its own files, the version string `Frame::Version`
already carries, a copy of the official APK's signature — can be the official value whatever is
really running. The public repository and reproducible builds let anyone check what the official
app *is*; they cannot show what runs on someone else's device. So no self-report, however it is
computed, proves anything, and this design does not add one.

The only thing a modified app cannot fake is a statement made by **hardware it does not control**.
On Android that exists: Keystore **key attestation**.

## 2. What Android key attestation proves

An app asks the Android Keystore for a new key and passes a **challenge**. The secure hardware (TEE
or StrongBox) returns a certificate chain for that key: a leaf whose extension
(`1.3.6.1.4.1.11129.2.1.17`, *KeyDescription*) records, among other things,

* `attestationChallenge` — the challenge, so the statement is fresh and bound to this exchange;
* `attestationSecurityLevel` — Software, TrustedEnvironment or StrongBox;
* `attestationApplicationId` (tag 709) — the **package name and the SHA-256 digests of the signing
  certificates** of the app that asked;
* `rootOfTrust` (tag 704) — `verifiedBootState` (Verified / SelfSigned / Unverified / Failed) and
  `deviceLocked`;
* `osVersion`, `osPatchLevel`, `vendorPatchLevel`, `bootPatchLevel`;

signed up a chain that ends at **Google's hardware attestation root** keys, which are published and
can be bundled in the app. No Google service, no Play Services, no network call is needed to make or
check it.

A copy of Night Drop rebuilt from modified source must be **re-signed with a different key** (ours,
`d08b8e64…`, is offline), so its attestation names a different signing-certificate digest — and on
a phone with a locked bootloader and verified boot, the app cannot change what the Keystore writes.
F-Droid's builds carry **our** signature (they are reproducible and F-Droid publishes our signed
APK), so both distribution channels attest as official.

Caveat, stated now and in the UI: `attestationApplicationId` is filled in by Android's keystore
service rather than the secure hardware itself. It is only as trustworthy as the OS, which is why
the verdict also requires `verifiedBootState == Verified` and `deviceLocked == true` (§4).

## 3. What it cannot do

* **Desktop and iOS** have no equivalent that works without the vendor's servers (iOS App Attest is
  verified through Apple; Windows and Linux offer nothing an ordinary app can use). Contacts there
  are always **"can't tell"**.
* **Rooted phones, custom ROMs, unlocked bootloaders** fail the boot-state check, though their owners
  may be entirely honest. They read **"can't tell"**, never "not official".
* **Leaked attestation keys.** Some device models' factory keys have leaked; whoever holds one can
  forge an attestation for any app. Google publishes a revocation list (§5.3); between a leak and its
  listing, a forgery passes.
* **It proves the app, not the person.** An official app on a phone whose owner photographs the
  screen is still official.

So the honest product is a **badge**, never a warning: "official app" for some Android contacts, and
"can't tell" for everyone else — which will be the common case, so it must not read as suspicious.
The one strong negative is a valid attestation naming a **different signer** (§4): that is a
modified or repackaged Night Drop, and is worth saying plainly.

## 4. Verdicts

| Verdict | When | Shown as |
|---|---|---|
| **Official** | chain verifies to a bundled root; challenge matches; not revoked; security level TEE or StrongBox; package `app.nightdrop`; the only signing digest is ours; `verifiedBootState == Verified`; `deviceLocked` | ✓ "Official Night Drop, checked by their phone's hardware" (with the date) |
| **Not official** | everything above holds **except** the package or the signing digest | ⚠ "Their app is not the official Night Drop (it is signed by someone else). Their copy may not do what this one does — including blocking screenshots." |
| **Can't tell** | anything else: no answer, sharing off, desktop/iOS, software-only attestation, unlocked or unverified boot, chain or challenge failure, revoked key | nothing in the header; in the contact's details: "Can't tell whether their app is official" with the reason class |

"Can't tell" is the default and the quiet case. A **failed** chain or challenge is *also* "can't
tell", not "not official": a broken attestation proves nothing either way, and treating it as an
accusation would let a network glitch slander a contact.

Our own **test app ids** (`app.nightdrop.rc`, `.test`) are not official by this rule — correct, and
it doubles as the end-to-end test of the negative path.

## 5. Protocol

### 5.1 Exchange

Over the existing end-to-end session, so no relay or observer sees it, and only with a contact
whose `Frame::Version` says 0.3 or later (older builds cannot decode a new frame; sending one would
cost them that frame):

1. **Request** — `Frame::AttestRequest { from, message }`, sealed: a 32-byte random nonce.
2. **Response** — `Frame::Attestation { from, message }`, sealed: either a decline marker (sharing is
   off, or the platform cannot) or the certificate chain (DER, typically 3–6 KB; capped at 16 KB).
3. The responder generates a **fresh** P-256 key for every response, with challenge
   `SHA-256("nightdrop/attest/v1" ‖ nonce ‖ requester identity key ‖ responder identity key)`, reads
   the chain, and deletes the key. Binding both identity keys stops a chain obtained in one chat being
   replayed into another; a fresh key per response keeps the leaf from linking responses together.

Requests are rate-limited (one per contact per day, plus one after either side updates) and count
against the daily allowance (`limits::Allowance`), so this cannot be used to make a phone do Keystore
work on demand.

### 5.2 Verification — in Rust

Security-critical code lives in the Rust core (§1a). The verifier parses the chain with the
RustCrypto `der`/`x509-cert` crates (to be reviewed for the audited-crates rule before use; no
`unsafe`, no network), checks each signature up to one of the **bundled** Google roots, decodes
*KeyDescription* by hand against the published ASN.1 schema, and applies §4. It is a pure function
of (chain, expected challenge, roots, revocation snapshot, now) and is fuzzed like the media parsers.

### 5.3 Roots and revocation, without a network call

* The Google attestation roots are compiled in (the current RSA root and the ECDSA root introduced
  for Remote Key Provisioning).
* Google's revocation list (`android.googleapis.com/attestation/status`) is **not** fetched by the
  app — that would tell Google when, and from where, someone checked a contact. Instead the release
  process fetches it, and it is shipped **signed with our update key** alongside the relay list on the
  onion site (same channel, same signature and expiry rules as `update.json`), refreshed weekly. An
  app whose snapshot is older than 30 days reports "can't tell" rather than "official".

### 5.4 Stored result

Per contact: the verdict, the date, the attested OS patch level (to show "checked on …"), nothing
else. The chain itself is discarded after verification. Re-checked when either side updates and at
most monthly otherwise.

## 6. Privacy and defaults

An attestation tells the verifier the phone's **Android version, patch levels, security level and
bootloader state**, and the intermediate certificates identify the **device family or batch**. That is
metadata about the user, handed to a contact, which Night Drop otherwise never discloses. Hence, by
the maintainer's rule (security on by default; the user chooses to lower their privacy):

* **"Let contacts check my app is official"** — sharing — **off by default**, explained in one line
  where it is turned on: what it reveals and to whom. A contact who asks while it is off gets a
  decline, and sees "can't tell".
* **Checking contacts** — costs the user nothing — **on**: requests go out on the cadence of §5.1.

Device ID attestation (brand, model, serial, IMEI) needs a privileged permission ordinary apps do not
hold and is **never** requested.

## 7. UI

* Chat header: a small ✓ after the contact's name for **Official**; nothing otherwise.
* **Not official**: a persistent banner in that chat (the text in §4), and the chat's capture block
  (`holdBlock`) stays as-is — it is our own app enforcing it, which still works; the banner tells the
  reader that the *other* side may not be doing the same.
* Contact details / safety-number screen: the verdict, its date, and for "can't tell" the reason
  class (their app doesn't share it · their device can't prove it · couldn't be checked).
* Settings: the sharing switch (§6).

## 8. Before writing code — measure on a device

1. **Can it link identities?** With Remote Key Provisioning (Android 12+, required on recent phones)
   attestation keys are provisioned per device and rotate. If the certificates a phone presents are
   unique to that phone for their lifetime, two contacts comparing notes could tell that two Night
   Drop identities live on the same device — which `ARCHITECTURE.md` §1a (anonymous identities)
   rules out. Measure on the S23 and S25: generate several attestations from two app ids, compare the
   intermediates and their lifetimes. **If they are device-unique, this feature does not ship** (or
   ships only with a stronger warning the maintainer approves).
2. The real chain sizes, generation time and battery cost on both phones.
3. That `attestationApplicationId` on an F-Droid install names our signature (it should: F-Droid
   distributes our signed APK).
4. The revocation list's size and format, for §5.3.

## 9. Alternatives considered

* **Self-reported hash or signature** — rejected, §1: a modified app reports whatever it likes.
* **Play Integrity API** — rejected: needs Google Play Services and a round trip to Google per check;
  incompatible with F-Droid and with the no-Google-calls stance.
* **Reproducible builds alone** — already done; they prove what the official app *is*, which this
  design relies on, but not what a contact runs.
* **iOS App Attest** — out: verification goes through Apple's servers.

## 10. Work, roughly

Rust verifier + fuzzing + fixtures from real devices (the bulk); two frames and their trust-boundary
rows; Kotlin key generation on the existing channel; the signed revocation snapshot in the release
process; UI and settings; device tests on both phones, including a test-id build as the "not
official" case. Gated on §8.1.
