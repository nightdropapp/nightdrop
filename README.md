# Night Drop

A privacy-first **1:1** messenger: anonymous identities, end-to-end encryption where
only sender and receiver can read messages, P2P over Tor, and **local-first** storage.
No accounts, no server-side keys, no logs.

Free, with no accounts and nothing to sell — it runs on donations: see [Support](#support).

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the full design and threat model, and
[`MAINTENANCE.md`](MAINTENANCE.md) for how to update, verify, release and operate it
(toolchain, dependency upgrades, the release procedure, publishing, device testing).

## Where to get it

- **Web:** [nightdrop.app](https://nightdrop.app/)
- **Onion:** `z6xw2ywlybjeskki4jons5ujepc2pedp5qkgvtbyxorie46qnjpnzqqd.onion`

The onion site serves the same pages and the same builds, over Tor, with no exit node in the
path — open it in Tor Browser. Reaching a v3 `.onion` at all proves you reached *us*: the
address **is** the site's public key, so there is no certificate authority to trust and nothing
to spoof. Prefer it if you would rather not tell your network, your ISP, or a CDN that you are
downloading a privacy tool.

Downloads on both are the same artifacts, and every release is signed — see
[`docs/reproducible-builds.md`](docs/reproducible-builds.md) to verify a build yourself.

## Status

**Feature-complete and verified end to end** (Rust and Flutter test suites passing, `cargo
clippy` and `flutter analyze` clean; `MAINTENANCE.md` §3 is the loop):

- **E2E crypto** (`core/crypto`, `identity`, `pake`): Signal Double Ratchet via
  `vodozemac` (X3DH + ratchet), a SPAKE2 "bouncer", anonymous device identities.
- **Wire + transport** (`core/wire`, `transport`): a framed protocol over a pluggable
  `Transport`; two real `Node`s pair and converse. **Embedded Tor** via `arti`
  (`transport::tor`, `tor` feature) — verified by a live test that bootstraps a circuit
  and publishes a real `.onion` (`core/tests/tor_smoke.rs`, `#[ignore]`d).
- **Censorship circumvention** (0.1.22): in-app **Tor bridge** configuration and an
  **in-process WebTunnel** client — Tor carried inside ordinary HTTPS with a Chrome-identical
  TLS fingerprint (BoringSSL), no separate process and no daemon. Reachable **before** you
  create an identity, which is when a blocked user needs it. Verified against a network
  configured to block Tor, and against an IDS with 52,311 signatures (zero alerts). Limits are
  stated plainly in `SECURITY.md`: never tested against a national firewall, and traffic
  *shape* is not disguised. See [`docs/design/android-bridges.md`](docs/design/android-bridges.md).
  From 0.1.28 the bridges screen can also **get WebTunnel bridges from the Tor Project** in one
  tap — the app's one direct, non-Tor request, made only after a consent dialog that says so.
- **Live event-driven core**: `NightdropCore::new_with_transport` runs a background poller
  that delivers unsolicited inbound messages and emits a push-event stream. A
  deterministic integration test drives two real cores over an injected transport + relay
  (pairing, authorization, bidirectional + offline delivery).
- **Relay** (`core/relay_client`, `relay/`): untrusted rendezvous mailbox + 24h
  store-and-forward; offline delivery; opaque encrypted blobs only.
- **Short-code pairing**: interactive **SPAKE2** over the rendezvous mailbox — the secret
  words never leave the device and the relay can't offline-attack the code; QR pairing is
  pre-authorized.
- **Authorization** (§5): a stranger can't message you until you approve the request.
- **Persistence** (`core/storage`): encrypted-at-rest store; identity/sessions/history
  survive a restart.
- **Backup** (§7): password-encrypted export/import (Argon2) and opt-in server backup (24h
  default / 36h max) with **restore over Tor** (`restore_server_backup_tor`). Direct
  device-to-device transfer (§7b) is planned, not built.
  **Lite/Full** content modes, **single-chat scoped backup + merge-restore**, a per-chat
  **backed-up flag** driving a peer **transparency warning** and an un-backed **logout
  Closed-signal** (§11.6).
- **Messaging extras**: **unsend / delete-for-everyone** + edit, per-chat **disappearing
  messages** (shared, synced), in-band **onion-address rotation** (§5c), **safety-number
  verification** (compare a Signal-style number out-of-band, or scan the QR).
- **Multi-relay mailboxes** (#17): advertise an **extra relay set** on top of the shared
  default; senders seal once and **fan the same blob out to every relay**, the receiver
  drains all and **de-duplicates by hash**, and edits/unsends **recall every copy**. Buys
  availability + censorship-resistance (a down/blocked relay doesn't drop mail) without adding
  trust or metadata — relays still see only opaque, recipient-sealed blobs, and anonymity stays
  with Tor. Edit your set from the home menu → **"My relays…"**.
- **Cover traffic** (opt-in): decoy posts that blur per-mailbox volume and timing from the
  relay. Off by default and measured before shipping — ~78 mAh a night
  ([`docs/design/cover-traffic.md`](docs/design/cover-traffic.md) §6).
- **Post-quantum pairing** (`core/pqkem`): **ML-KEM-768 (FIPS 203)** mixed into the short-code
  rendezvous seal, so a *harvest-now-decrypt-later* adversary must break **both** SPAKE2 and
  ML-KEM. Scoped deliberately: it protects the **rendezvous payload**, a true hybrid that never
  weakens the classical guarantee. The QR path never crosses the network, so it is not wrapped.
- **Screenshot transparency**: the app does **not** block screenshots (it cannot, honestly) and
  instead *tells the other person* when one is taken. Best-effort by construction — Android 14+
  only, blind to cameras and screen recording — and the UI never implies otherwise.
- **App lock + duress wipe**: a lock code to open the app, and a **separate code that wipes**
  identity and history instead of unlocking.
- **Media messages**: sealed attachments, streamed and stored encrypted at rest.
- **Silence detection**: tells you when nothing has arrived for long enough that the *transport*
  is the likely explanation, rather than leaving you to guess.
- **Update checks over Tor** (`core/update`): the manifest is fetched from our own onion site
  only — a v3 onion authenticates itself, and each download is checked against the manifest's
  SHA-256 before it lands. There is deliberately **no** clearnet fallback, so the update check can
  never become the thing that deanonymizes you.
- **Dart ↔ Rust bridge** + **cargokit** (`app/rust_builder`): the **Linux desktop GUI
  builds** (bundles `libnightdrop`) and the **Android APK builds** (cargokit
  cross-compiles the core into `arm64-v8a`, `armeabi-v7a`, `x86_64`) — both verified.
- **App** (`app/`): onboarding (create / restore file / restore server) → pairing (QR scan +
  short code) → approve request → chat, per-chat rename, 24h server-storage toggle with a
  warning banner, disappearing-timer picker, backup (Lite/Full, per-chat, server), opt-in
  Android **background delivery** foreground service (#13), donations.
- **Website** (`website/`): static features/marketing site.

**Environment notes for running for real:**
- **Android** needs a real **JDK** (not a headless JRE — Gradle needs `javac`) and SDK 36
  + NDK; `app/rust_builder/cargokit/gradle/plugin.gradle` is **patched for Gradle 9**
  (which removed `Project.exec()`), and `app/rust_builder/android/build.gradle` uses
  `compileSdk 36`.
- **iOS/macOS** builds need a Mac with Xcode (not buildable on Linux).
- **Windows** builds run on Windows (Flutter cannot cross-compile it); toolchain, long-path
  setup and known limits are in `docs/building-windows.md`.
- A bare `flutter run` starts the **demo core** (an in-process peer, two real `Node`s) for UI
  work. Real builds pass `--dart-define=NIGHTDROP_TOR=1` and the relay address (the install
  scripts and the F-Droid recipe do), which selects `new_with_transport` — same `NightdropCore` API.
- The `#[ignore]`d Tor and `integration_test/` suites run where a network / device (or
  `xvfb`) is available.

## Layout

```
app/            Flutter UI (Dart). Talks only to the abstract NightdropCore seam.
app/rust_builder cargokit plugin: builds core/ and bundles libnightdrop per platform.
core/           Rust security core: api, node, wire, identity, crypto, pake, transport
                (+ transport/tor behind `tor`), relay_client, storage.
relay/          Minimal server binary: rendezvous mailbox + 24h store-and-forward.
website/        Static marketing/features site.
scripts/        Build/install + publish helpers (desktop, Android, AppImage, Windows, website).
webtunnel/      In-process WebTunnel client (Tor inside HTTPS, Chrome-identical TLS).
moat/           "Get bridges" client for the Tor Project's bridge distributor.
third_party/    Vendored, patched crates (each with a NIGHTDROP-PATCH.md).
fdroid/         F-Droid recipe and the reproducible-build tooling.
docs/           Design records (docs/design/), field advisories, operations.
```

## Prerequisites

- **Rust** (stable, via rustup) — https://rustup.rs
- **Flutter SDK** (stable, Dart >= 3.6) — https://docs.flutter.dev/get-started/install
- For the **desktop GUI** only: the platform toolchain (Linux: `clang cmake ninja
  pkg-config libgtk-3-dev libsecret-1-dev libavformat-dev libavcodec-dev libavutil-dev
  libswscale-dev` — libsecret for flutter_secure_storage, the ffmpeg set for video
  thumbnails). Not needed to build the core or run the tests below.
- To regenerate bindings after changing `core/src/api.rs`:
  `cargo install flutter_rust_bridge_codegen` (pinned to the version in `pubspec.yaml`).

## Verify the core + bridge (no GUI required)

```sh
make core-test      # cargo test -p nightdrop  — crypto, ratchet, PAKE, loopback
make core-build     # builds target/debug/libnightdrop.so
make app-test       # flutter test — widget test + Dart↔Rust bridge test (loads the .so)
```

`make app-test` runs `rust_bridge_test.dart`, which loads the built `libnightdrop.so`
and drives the real core from Dart — verifying the FFI without needing a display.

## Run the app (GUI)

Requires the desktop toolchain above (or a mobile device/emulator), plus a native-lib
build hook so the bundle ships `libnightdrop.so` (the standard approach is
flutter_rust_bridge's `rust_builder`/cargokit plugin — see
https://cjycode.com/flutter_rust_bridge). Then:

```sh
make bootstrap     # flutter create (adds android/ios/windows/linux/macos) + pub get
make app-run       # cd app && flutter run
```

## Local dev helpers

Run a **dev relay** locally (live TUI dashboard; its `.onion` persists across restarts in its
own state dir, see [`ARCHITECTURE.md`](ARCHITECTURE.md) §11.9):

```sh
NIGHTDROP_RELAY_TUI=1 make relay-run    # state in relay-state-dev/
```

Never point a second process at `relay-state/`: on the maintainer's machine that is the
production relay's keystore, already served by the `nightdrop-relay` user service.

Preview the **website** locally (loopback only — don't expose the dev server to the LAN):

```sh
python3 -m http.server --bind 127.0.0.1 --directory website 8000
```

Build/install and publishing helpers live in [`scripts/`](scripts/); `MAINTENANCE.md` §8 lists
them, with the environment variables that change what they build.

## Regenerating the bridge

The bridge is already generated and committed (`core/src/frb_generated.rs`,
`app/lib/src/rust/`). After editing the `pub` surface in `core/src/api.rs`, regenerate:

```sh
make gen-bridge    # flutter_rust_bridge_codegen generate
```

The app depends only on the abstract `NightdropCore` (`app/lib/src/core/nightdrop_core.dart`);
`RustNightdropCore` (real) and `MockNightdropCore` (UI-only) both implement it.

## Security note

All security-critical logic (keys, Double Ratchet, PAKE, Tor, at-rest crypto) lives in
the Rust `core/` — never in Dart. Prefer audited crates (`vodozemac`, `arti`, a vetted
PAKE) over hand-rolled cryptography. See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the full
design, threat model, and non-negotiable invariants.

**No external audit yet.** Night Drop builds on audited cryptographic *libraries*, but the
integration as a whole (pairing/PAKE, relay, at-rest storage, FFI boundary) has **not** had an
independent security audit. See [`SECURITY.md`](SECURITY.md#audit-status) — treat the app as
promising and improving, not battle-tested, until that's done.

## Support

Night Drop takes no payments, runs no accounts, and has nothing to sell — so donations are the
only funding. Monero and Zcash keep a donation fully private; Bitcoin is accepted through a
silent payment address, which keeps donations unlinkable but not invisible.

**Monero (XMR)** — the default: it doesn't leak the sender, the receiver, or the amount to anyone
watching the chain.

```
49yRv29r6yHYBGZH4z1uGTXg68VFYX4Zf1cWopevd32YLUwj86mXddNe8bCTaZKcRQYDRdHJrcL6uAiCRKH1AMrDTQNNZZm
```

**Zcash (ZEC)** — a shielded-only address (Sapling + Orchard). Send from a shielded wallet, such as
Zodl, so the payment stays private.

```
u1q6yxrsr95z7md9fgsnxhjc39dsqd4yvtedhkxq5hmackxxf38pfp8p7jdv0fs6uhp5wzxycykw7s4hyzgcyprkcue87f8afq3fhl0hg4fr8c6h7nalhhnx62qrzt3ucfjpxq6mx4cxmxy0q70ef5yhf4dhen6fgslsntl85zju00ulhk
```

**Bitcoin (BTC)** — a [silent payment](https://bips.dev/352/) address: each donation lands at a
fresh address, so donations can't be linked to each other. Amounts and the sending wallet stay
public, as with any Bitcoin payment. Your wallet must be able to send to `sp1` addresses (for
example Cake, Sparrow, BlueWallet or Wasabi).

```
sp1qqvf5treewvewtlyvqggffn2maf44m5xgfrglaelw9yr59x9ehjrhcqeteyf7pznevdrt43xn60q3nesh9a3szvs37se3rhs8ugeuw2rxtqayfsuu
```

These are the same addresses shown in the app (**Support Night Drop**) and on the website: the
canonical copy lives in [`config/app_config.json`](config/app_config.json) and `make config` syncs
it into `app/assets/app_config.json` and `website/config.js`. Change them there and re-run
`make config` — this README is the one copy that isn't generated, so update it in the same commit
(`app/test/donations_test.dart` fails if it drifts).

## License

Licensed under the GNU Affero General Public License v3.0 or later
(`AGPL-3.0-or-later`) — see [`LICENSE`](LICENSE).
