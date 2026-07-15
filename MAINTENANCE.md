# Night Drop — Maintenance & Update Guide

How to update, verify, and release this app when you're not the person who built it.
Read `CLAUDE.md` (non-negotiable invariants) and `ARCHITECTURE.md` (design source of
truth) before changing anything security-related; `BUILD_AND_DEPLOY.md` covers the
day-to-day build/deploy commands. This file covers **maintenance**: toolchain setup,
dependency updates, the rename machinery, verification, and the release checklist.

---

## 1. The 60-second mental model

- `app/` — Flutter UI. It talks only to the abstract `NightdropCore` seam
  (`app/lib/src/core/nightdrop_core.dart`). No crypto, no keys, no transport here — ever.
- `core/` — Rust security core (identity, Double Ratchet, PAKE, Tor, storage). Exposed
  to Dart via flutter_rust_bridge; the FFI surface is `core/src/api.rs`.
- `relay/` — untrusted store-and-forward server binary (opaque encrypted blobs only).
- `app/rust_builder/` — vendored cargokit plugin that compiles `core/` and bundles
  `libnightdrop` into every platform build. **Vendored third-party code — do not
  "fix" its analyzer warnings, and exclude it when running `flutter analyze`.**
- `config/app_config.json` — single source of truth for app/website copy and donation
  addresses. `make config` syncs it to `app/assets/app_config.json` and
  `website/config.js`. Never edit the two derived files directly.

---

## 2. Toolchain prerequisites

| What | Why | Notes |
|---|---|---|
| Rust (stable, rustup) | builds `core/` and `relay/` | pinned via `rust-toolchain.toml` |
| Flutter SDK (stable, Dart ≥ 3.6) | builds `app/` | |
| `clang cmake ninja pkg-config libgtk-3-dev libsecret-1-dev libavformat-dev libavcodec-dev libavutil-dev libswscale-dev` | Linux desktop build | `libsecret-1-dev` is required by flutter_secure_storage's Linux backend; the ffmpeg `-dev` set by fc_native_video_thumbnail's — CMake configure fails without them |
| Real JDK (with `javac`) + Android SDK 36 + NDK | Android build | a headless JRE is **not** enough for Gradle. `app/android/local.properties` must point `sdk.dir` at the SDK |
| `xvfb` | headless integration tests on Linux | `sudo apt install xvfb` |
| `flutter_rust_bridge_codegen` (cargo install) | only when `core/src/api.rs` changes | **must match the `flutter_rust_bridge` version pinned in `app/pubspec.yaml`** |
| macOS + Xcode | iOS/macOS builds only | not buildable on Linux |

---

## 3. The verification loop (run after every change)

```sh
make core-test        # 82 Rust tests: crypto, ratchet, PAKE, relay, persistence
make clippy           # must be clean
make app-test         # builds the core, then all Flutter tests — includes
                      # rust_bridge_test.dart, which loads the real libnightdrop.so
cd app && flutter analyze lib test integration_test    # must be "No issues found"
flutter build linux --debug                            # catches CMake/plugin breakage
xvfb-run -a flutter test integration_test -d linux     # full onboarding→pair→chat flow
                                                       # against the REAL Rust core
```

The integration test is the only thing that exercises the real widget tree against the
real core — it has caught bugs the unit tests can't (e.g. a notify-during-first-build
crash). Don't skip it because it needs a display; that's what xvfb is for.

Single tests: `cargo test -p nightdrop <name>` /
`cd app && flutter test --plain-name "<name>"`.

---

## 4. Updating Flutter packages

Procedure:

1. `cd app && flutter pub outdated` — note which direct deps have new majors.
2. Raise the constraints in `app/pubspec.yaml`, then `flutter pub upgrade`.
3. `flutter analyze lib test integration_test` and fix what it reports.
4. Run the full verification loop (§3), **including** `flutter build linux` — plugin
   majors often change native build requirements, which analyze/tests won't catch.

Hard rules and known traps:

- **`flutter_rust_bridge` is pinned exact** (no caret) because the Dart package, the
  generated bindings (`core/src/frb_generated.rs`, `app/lib/src/rust/`), and the
  codegen binary must all be the same version. To upgrade it: bump the pin,
  `cargo install flutter_rust_bridge_codegen` at the same version, `make gen-bridge`,
  commit the regenerated files together.
- **`nightdrop` (path: rust_builder)** is the local cargokit plugin, not a pub
  package. Leave it alone during upgrades.
- Prefer the latest **stable** (pub may offer a beta as "resolvable" — don't take it).
- Migrations already done once (for reference if a revert ever resurrects them):
  file_picker ≥ 9 uses static `FilePicker.pickFiles/saveFile` (no `.platform`);
  flutter_local_notifications ≥ 19 uses named params (`initialize(settings:)`,
  `show(id:, title:, body:, notificationDetails:)`); flutter_secure_storage ≥ 10
  dropped `AndroidOptions(encryptedSharedPreferences:)` (data migrates automatically).
- **`flutter_foreground_task` ≥ 9** (opt-in background delivery, #13) drives
  `core/background_delivery.dart` and the typed `dataSync` `<service>` +
  `FOREGROUND_SERVICE*`/`WAKE_LOCK` entries in `AndroidManifest.xml`. Its 9.x API differs from
  8.x (`ForegroundTaskEventAction`, `TaskHandler.onStart(_, TaskStarter)` /
  `onDestroy(_, isTimeout)`, `startService(serviceId:)`); match the installed major. The feature
  is **build-verified only** — its actual background-wake behaviour needs a real device (see §11).

## 5. Updating Rust crates

- `cargo update`, then `make core-test`, `make clippy`, **and**
  `cargo build -p nightdrop --features tor` — the Tor path is feature-gated and a
  plain build/test will not compile it.
- Crypto/transport crates (`vodozemac`, `arti`, `spake2`, `chacha20poly1305`,
  `argon2`) are deliberately chosen audited crates. Major-version bumps here are
  security-relevant changes: read their changelogs, and never swap one for an
  unaudited alternative (CLAUDE.md invariant).
- TLS for Tor is **native-tls (system OpenSSL), not rustls** — rustls 0.23 needs an
  explicitly installed crypto provider. The two `install_default()` calls in
  `core/src/transport/tor.rs` and `relay/src/main.rs` exist for arti's internals;
  keep them.
- The `tor` feature bundles SQLite from source, so no system `libsqlite3` is needed.

## 6. Changing the Dart↔Rust surface

Any change to the `pub` items in `core/src/api.rs`:

1. `make gen-bridge` — regenerates `core/src/frb_generated.rs` and `app/lib/src/rust/`
   (both are committed).
2. Mirror the change in the abstract seam `app/lib/src/core/nightdrop_core.dart`, then in
   `RustNightdropCore` (real) and `MockNightdropCore` (UI-only tests). Keep all three in sync.
3. Run §3. `rust_bridge_test.dart` is the canary for a stale bridge.

---

## 7. App identity (name / bundle id)

Current identity: **`app.nightdrop` / "Night Drop"**. For Android and Linux it is
a **build-time variable** — a pre-release rename needs no source edit:

```sh
GHOST_APP_ID=org.example.chat GHOST_APP_NAME="New Name" scripts/install-android-app.sh
GHOST_APP_ID=org.example.chat scripts/install-desktop-app.sh
```

Where it's wired:

- `scripts/install-android-app.sh` exports `GHOST_APP_ID`/`GHOST_APP_NAME` as Gradle project
  properties (`ORG_GRADLE_PROJECT_appId` / `..._appName`); `scripts/install-desktop-app.sh`
  exports `GHOST_APP_ID` for the Linux CMake.
- `app/android/app/build.gradle.kts` reads `appId`/`appName` with the defaults, and
  fills the `${appName}` placeholder in `AndroidManifest.xml`. The Kotlin `namespace`
  (and `MainActivity.kt`'s package, `app.nightdrop`) is intentionally **fixed** —
  it names code, not the shipped identity.
- `app/linux/CMakeLists.txt` reads `GHOST_APP_ID` from the environment.
- **iOS/macOS have no variable** — edit `PRODUCT_BUNDLE_IDENTIFIER` in
  `app/ios/Runner.xcodeproj/project.pbxproj` (Runner + RunnerTests, 3 configs each)
  and `app/macos/Runner/Configs/AppInfo.xcconfig`. Windows metadata lives in
  `app/windows/runner/Runner.rc` and the window title in `windows/runner/main.cpp`.
- Also update the hardcoded id in `BUILD_AND_DEPLOY.md`'s adb commands.

**Caveat:** to Android, a new `applicationId` is a *different app*. It installs
alongside the old one (no upgrade, no data migration) — uninstall the old id manually.

---

## 8. Shell scripts (`scripts/`)

| Script | Purpose |
|---|---|
| `scripts/install-desktop-app.sh` | build the Linux release bundle (Tor + relay baked in via `--dart-define`) and install it as a desktop app (`.desktop` entry + hicolor icons + `~/.local/bin/nightdrop`); `--no-build`, `--run`, `--uninstall`; honors `GHOST_APP_ID`/`GHOST_APP_NAME` |
| `scripts/install-android-app.sh` | build + install + launch the APK (Tor + relay baked in) on a connected device; honors `GHOST_APP_ID`/`GHOST_APP_NAME`; `--release`, `--build-only`, `--install-only`, `--wireless IP PORT` |
| `scripts/onion-website.sh` | serve `website/` behind a Tor v3 onion service (stable `.onion` via persisted state) |
| `scripts/install-onion-service.sh` | install the onion-website as a systemd user service (starts on boot) |
| `scripts/setup-pluggable-transports.sh` | write `transports.txt` for obfs4/snowflake bridges |
| `scripts/deploy-vps.sh` | rsync `website/` to the VPS + reload nginx |

Quick local previews are one-liners in `README.md` (relay dev TUI: `NIGHTDROP_RELAY_TUI=1
cargo run -p nightdrop_relay`; website: `python3 -m http.server --bind 127.0.0.1 --directory
website 8000` — **loopback only by design**). Dev desktop runs use `make app-run`.

All build/install scripts are location-independent (they derive the repo root from their own
path) and overridable via `FLUTTER_HOME`, `PROJECT_ROOT`, `ADB`, `ANDROID_SDK`. After editing,
at minimum run `bash -n <script>`; use shellcheck if available. Watch the `set -e` +
`grep` trap: a grep that matches nothing exits 1 and kills the script — append
`|| true` when an empty result is a valid outcome.

---

## 9. Known build traps (each has burned an hour before)

- **CMake "cannot copy to /usr/local: Permission denied"** on `flutter build linux`:
  a stale cache from an earlier *failed* configure. `rm -rf app/build/linux/x64/<mode>`
  and rebuild. Never sudo it.
- **Android build stops with "No Android SDK found"** even though builds worked
  before: check `app/android/local.properties` still points at a real SDK and that
  `java`/`javac` exist. Machine cleanups tend to eat these.
- **`flutter analyze` (bare) reports ~185 errors** — they're all in the vendored
  `app/rust_builder/cargokit/`. Analyze `lib test integration_test` instead.
- **Gradle 9**: `app/rust_builder/cargokit/gradle/plugin.gradle` is locally patched
  (Gradle 9 removed `Project.exec()`). A blind cargokit re-vendor loses the patch.
- **Old artifacts lie**: `app/build/**` can contain manifests/bundles from before a
  rename or upgrade. When grepping for stale identifiers, exclude `app/build/`,
  `**/ephemeral/`, `.plugin_symlinks`, `.dart_tool`.
- **Tor is slow on first run** (~30–60 s bootstrap; onion reachable after ~1–3 min).
  The UI's "publishing your address" banner reflects `onion_ready()` — that's normal,
  not a hang.

---

## 10. Things that look odd but are intentional (don't "fix")

- **QR scanning uses `flutter_zxing` (ZXing), not `mobile_scanner`/ML Kit** — deliberately,
  to keep **Google Play Services out of the APK**. ML Kit barcode scanning drags in
  `play-services-mlkit-barcode-scanning` + `firebase-*`/`play-services-base` transitive infra;
  ZXing is Apache-2.0, on-device, and pulls no Google runtime (verify with a `classes*.dex`
  scan for `com/google/android/gms` after any scanner change). Don't swap it back for the
  "nicer" ML Kit scanner without accepting that trade-off. There is no FCM/push anywhere.
- **The composer paste button is text-only** (built-in `Clipboard`), by design. It used
  `pasteboard` for image-paste, but that was one of the plugins tripping the Flutter **KGP**
  warning, so it was dropped; images go through the attach button (which also compresses them
  for Tor). Don't re-add `pasteboard`/`super_clipboard` for paste-an-image without re-checking
  the KGP warning **and** the `super_native_extensions`↔`file_picker` `win32` conflict (both
  currently block it — see `TODO.md` #5). Note the KGP warning is **not** fully gone —
  `flutter_foreground_task` (#13) still trips it; removing `pasteboard` just took it off the list.
- **Plaintext only at the UI edge.** If a change makes Dart touch key material,
  ratchet state, or unencrypted persistence, it's wrong — move it into `core/`.
- **`MediaCache.wipe()` on logout** (`app/lib/src/core/media_cache.dart`): decrypted
  attachments are memoized in RAM and decrypted videos land as `ghost-media-*` files
  in the OS temp dir; logout must clear both. Anything new that writes decrypted
  bytes anywhere must be added to this wipe.
- **`_Root` defers `core.start()` to a post-frame callback** (`app/lib/src/app.dart`):
  `start()` can notify synchronously and notifying mid-build crashes. Don't inline it.
- **The relay logs nothing by default.** The flow-log/TUI exist only behind
  `NIGHTDROP_RELAY_DEV` / `NIGHTDROP_RELAY_TUI` and show metadata only — never blob bytes.
  Keep it that way (invariant: no server-side logs).
- **Relay blobs are double-wrapped** (`node::mailbox_handle` / `relay_wrap`): mailboxes
  are addressed by a hash of the recipient's identity key (never an onion address), and
  queued frames are sealed so the relay can't read sender identity keys or `Hello`
  addresses. Don't post raw `wire::encode` bytes to the relay; don't put addresses in
  handles. `poll_relay` silently skips blobs that don't unseal — that's deliberate
  (garbage in a mailbox must not wedge the drain).
- **Multi-relay fan-out (#17) seals once, posts many, dedups by hash.** `queue_on_relays`
  computes the sealed blob **once** and posts the *identical* bytes to the primary + each of
  the recipient's `peer_relays`; the receiver drains the primary + its own `my_relays` and
  de-dups by SHA-256 of the blob (`seen_relay_blobs`), so a message fanned to N relays surfaces
  once. Two gotchas to preserve: (1) an edit/unsend must recall **every** stored copy — iterate,
  do **not** `.any()` (it short-circuits after the first success and strands siblings); (2)
  `fetch`/`take` on the relay **drain**; use `peek` (count only) for non-destructive checks
  (a test that "verifies then polls" with `fetch` will eat its own blob). A relay being
  unreachable must never abort the drain/fan-out from the others (best-effort, ≥1 success).
- **Relay failures are surfaced, not just swallowed.** `poll_relay` records per-relay
  reachability for our own `my_relays` into `Node::relay_reachable`; `relay_health()` exposes it
  (`api::RelayHealth`), and the home screen shows a "your relay is offline — add a backup"
  banner (`_RelayHealthBanner`) when a self-hosted relay stops answering. Separately, when opt-in
  server storage is on but a send can't reach any relay to store the copy (peer was reached
  directly), the send sets the chat's ephemeral `remote_storage_healthy = false`, surfaced on
  `Contact.remote_storage_healthy` — the chat's storage banner then switches to an error tone
  ("delivered but not stored"). Both flags are in-memory (recomputed), never persisted. If you add
  a new relay code path, keep these updated or the UI will silently go stale again.
- **Wire frames are length-prefixed + zero-padded to fixed buckets** (`wire::encode`/`decode`,
  `WIRE_VERSION` 2) so frame *length* leaks nothing (metadata protection). The bytes are **not
  raw JSON** anymore — a 4-byte big-endian length prefix precedes the envelope, then zero padding.
  Never parse wire bytes as JSON directly (a couple of old tests did — they now read `bytes[4..4+len]`).
  If you add a big new frame type, check the bucket schedule (`PAD_BUCKETS`/`PAD_BLOCK`) so common
  frames still collapse to one size. Changing the framing means bumping `WIRE_VERSION`.
- **`Closed`/`Ack`/`BackedUp` are authenticated on the ratchet** (`node::MARK_*`, `authed_control`
  sender / `verify_control` receiver). They carry an encrypted marker; the receiver acts only if it
  decrypts to the expected constant — this is what stops a party who knows your identity key from
  spoofing "chat deleted"/fake-ack/fake-backup or replaying an old one. If you add a new
  state-changing control frame, authenticate it the same way — don't reintroduce a plaintext
  `{from}`-only signal. (`Approved`/`CodeInUse` stay plaintext by design: pairing-time, self-correcting.)
- **The only send-failure error string is** `"peer offline and no relay accepted the message"`
  (raised only when the direct send failed **and** every relay — primary + fan-out set — refused).
  The Dart UI cleans the bridge's `AnyhowException(...)` wrapper via `cleanCoreError` and preserves
  the composer draft on failure (`chat_screen._send`). Don't reintroduce `_input.clear()` before
  the send resolves.
- **The relay poll is on a timed cadence** (15 s foreground / 60 s background, plus an
  immediate catch-up on foregrounding — `RELAY_POLL_*` in `api.rs`). Each poll is a full
  Tor round-trip, so don't "make it snappier" by shortening it: online messages already
  arrive push-style over the direct Tor stream; the relay only bounds offline-mail
  latency.
- **`devlog!` in `node.rs` compiles to nothing in release builds.** Those logs contain
  identity keys, invite codes, and decrypted display names — fine on a dev box, never in
  logcat on a user device. New core logging that touches such data must use `devlog!`,
  not `eprintln!`.
- **The demo core** (`NightdropCore::new()`, in-process echo peer) is what non-Tor,
  non-networked launches use. It's for UI development; real two-device operation is
  Tor mode (`GHOST_TOR=1`) or TCP networked mode (`GHOST_LISTEN`+`GHOST_RELAY`).

---

## 11. Release checklist

1. **Replace the placeholder Zcash address** in `config/app_config.json`
   (`zs1example…replacethis…`) or remove that donation entry; fill `links.source`.
   Then `make config` and commit the synced files.
2. **Android release signing**: `app/android/app/build.gradle.kts` still signs release
   builds with the **debug key** (see the TODO in `buildTypes`). Create a real
   keystore and signing config before shipping.
3. Bump `version:` in `app/pubspec.yaml` (`x.y.z+build`).
4. Confirm the app identity (§7) — it's permanent once users install.
5. Full verification loop (§3) plus `flutter build apk --release` and
   `flutter build linux --release`, on a machine with the Android SDK/JDK.
5a. **On-device validation** of opt-in background delivery (#13): enable it in the home menu,
   background the app, send from a second device, confirm the persistent notification stays up
   and the message notification arrives over Tor. Check both backgrounded and swiped-away cases
   (the latter may pause the engine — a known limitation, §11.8). CI/desktop can't cover this.
6. Deploy the relay per `BUILD_AND_DEPLOY.md`. Its `.onion` is pinned by
   `relay-state/` (`NIGHTDROP_RELAY_STATE`) — **losing that directory changes the relay
   address**, and the address is baked into Android builds via
   `--dart-define=GHOST_RELAY=...`, so treat `relay-state/` as production state.
7. Website: `python3 -m http.server --bind 127.0.0.1 --directory website 8000` to preview;
   deploy the static `website/` dir.
8. Sanity-pass the invariants in `CLAUDE.md` against your diff — no server-side
   keys/logs, local-first storage, 24h cap + warning banner, Tor by default,
   authorization before first message, backups user-owned.
