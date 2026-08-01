# Night Drop

A privacy-first **1:1** messenger: anonymous identities, end-to-end encryption where
only sender and receiver can read messages, P2P over Tor, and **local-first** storage.
No accounts, no server-side keys, no logs.

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the full design and threat model, and
[`MAINTENANCE.md`](MAINTENANCE.md) for how to update, verify, and release the app
(toolchain, dependency upgrades, rename variables, release checklist).

## Status

**Feature-complete and verified end to end** (82 Rust tests + 9 Flutter tests; `cargo
clippy` and `flutter analyze` clean):

- **E2E crypto** (`core/crypto`, `identity`, `pake`): Signal Double Ratchet via
  `vodozemac` (X3DH + ratchet), a SPAKE2 "bouncer", anonymous device identities.
- **Wire + transport** (`core/wire`, `transport`): a framed protocol over a pluggable
  `Transport`; two real `Node`s pair and converse. **Embedded Tor** via `arti`
  (`transport::tor`, `tor` feature) — verified by a live test that bootstraps a circuit
  and publishes a real `.onion` (`core/tests/tor_smoke.rs`, `#[ignore]`d).
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
- **Backup** (§7): password-encrypted export/import (Argon2), device-to-device, and opt-in
  server backup (24h default / 36h max) with **restore over Tor** (`restore_server_backup_tor`).
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
- The demo app still pairs with an in-process peer (two real `Node`s) until you start it
  via `new_with_transport` pointed at Tor + a deployed relay — that swap doesn't change the
  `NightdropCore` API.
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
scripts/        Build/install + deploy helpers (desktop, Android, onion service, VPS).
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

Run the **relay** locally (dev TUI dashboard; its `.onion` persists across restarts via
`relay-state/`, see [`ARCHITECTURE.md`](ARCHITECTURE.md) §11.9):

```sh
NIGHTDROP_RELAY_TUI=1 cargo run -p nightdrop_relay    # or: make relay-run
```

Preview the **website** locally (loopback only — don't expose the dev server to the LAN):

```sh
python3 -m http.server --bind 127.0.0.1 --directory website 8000
```

Build/install helpers (Linux desktop + Android installers, onion service, VPS deploy) live
in [`scripts/`](scripts/); each is self-documenting via `--help`.

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
only funding. **Monero (XMR):**

```
49yRv29r6yHYBGZH4z1uGTXg68VFYX4Zf1cWopevd32YLUwj86mXddNe8bCTaZKcRQYDRdHJrcL6uAiCRKH1AMrDTQNNZZm
```

Monero is the default for the same reason the app exists: it doesn't leak the sender, the
receiver, or the amount to anyone watching the chain. This is the same address shown in the app
(**Support Night Drop**) and on the website: the canonical copy lives in
[`config/app_config.json`](config/app_config.json) and `make config` syncs it into
`app/assets/app_config.json` and `website/config.js`. Change it there and re-run `make config` —
this README is the one copy that isn't generated, so update it in the same commit.

## License

Licensed under the GNU Affero General Public License v3.0 or later
(`AGPL-3.0-or-later`) — see [`LICENSE`](LICENSE).
