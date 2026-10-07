# Night Drop — Maintenance & Release Handbook

How to update, verify, release and operate this app when you're not the person who built it.
Read `ARCHITECTURE.md` (design source of truth, including the non-negotiable invariants in §1a)
before changing anything security-related; `BUILD_AND_DEPLOY.md` covers day-to-day dev builds.
This file covers everything else: toolchain, dependency updates, verification, the release
procedure, publishing, device testing, the services that run Night Drop, and the conventions for
where knowledge lives.

Anything here that cost a session to learn says so. Those notes are the point of the file.

---

## 1. The 60-second mental model

- `app/` — Flutter UI. It talks only to the abstract `NightdropCore` seam
  (`app/lib/src/core/nightdrop_core.dart`). No crypto, no keys, no transport here — ever.
- `core/` — Rust security core (identity, Double Ratchet, PAKE, Tor, storage). Exposed
  to Dart via flutter_rust_bridge; the FFI surface is `core/src/api.rs`.
- `relay/` — untrusted store-and-forward server binary (opaque encrypted blobs only).
- `webtunnel/`, `moat/` — the in-process WebTunnel client and the "Get bridges" client.
- `third_party/` — vendored, patched crates (`tor-hsservice`, `saturating-time`); each carries a
  `NIGHTDROP-PATCH.md`.
- `app/rust_builder/` — vendored cargokit plugin that compiles `core/` and bundles
  `libnightdrop` into every platform build. **Vendored third-party code — do not
  "fix" its analyzer warnings, and exclude it when running `flutter analyze`.** It carries two
  local patches: Gradle 9 (§9) and the WebTunnel feature switch (§8).
- `config/app_config.json` — single source of truth for app/website copy and donation
  addresses. `make config` syncs it to `app/assets/app_config.json` and
  `website/config.js`, and writes `app/lib/src/core/app_version.dart`. Never edit the derived
  files directly.
- `fdroid/` — the F-Droid recipe and the reproducible-build tooling (`fdroid/README.md`).
- `website/` — the static site, served live from disk by the onion service (§12).

---

## 2. Toolchain prerequisites

| What | Why | Notes |
|---|---|---|
| Rust (rustup) | builds `core/` and `relay/` | pinned **exactly** in `rust-toolchain.toml` |
| Flutter SDK | builds `app/` | pinned **exactly** in `app/.fvmrc` (the F-Droid recipe reads it). The scripts use `FLUTTER_HOME` (default `~/flutter`), **not `PATH`** — see §8 |
| `clang cmake ninja pkg-config libgtk-3-dev libsecret-1-dev libavformat-dev libavcodec-dev libavutil-dev libswscale-dev` | Linux desktop build | `libsecret-1-dev` is required by flutter_secure_storage's Linux backend; the ffmpeg `-dev` set by fc_native_video_thumbnail's — CMake configure fails without them |
| Real JDK (with `javac`) + Android SDK 36 + NDK | Android build | a headless JRE is **not** enough for Gradle. `app/android/local.properties` must point `sdk.dir` at the SDK. Builds need `ANDROID_HOME` **and** `JAVA_HOME` set |
| `xvfb` | headless integration tests on Linux | |
| `flutter_rust_bridge_codegen` (cargo install) | only when `core/src/api.rs` changes | **must match the `flutter_rust_bridge` version pinned in `app/pubspec.yaml`** |
| podman | `fdroid/build-locally.sh` (release builds) | ~40 GB |
| macOS + Xcode | iOS/macOS builds only | not buildable on Linux |
| Windows (a VM works) | the Windows installer | `docs/building-windows.md` |

---

## 3. The verification loop (run after every change)

```sh
make core-test        # Rust tests: crypto, ratchet, PAKE, relay, persistence
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

Single tests: `cargo test -p nightdrop <name>` / `cd app && flutter test --plain-name "<name>"`.

Two traps in the loop itself:

- **`--features tor` targets are compiled by nothing automatic.** Neither `cargo test` nor
  `clippy --all-targets` builds `core/tests/tor_smoke.rs` or `relay_e2e.rs`, so they can sit
  broken for a whole session. When touching transport, build them explicitly:
  `cargo test -p nightdrop --features tor --test relay_e2e --test tor_smoke --no-run`. They are
  not in the pre-commit hook because they pull arti.
- **A pipe hides the exit code.** `cmd 2>&1 | tail -5` reports the exit status of `tail`, so a
  failed build looks like a successful one. `cargo build --release -p nightdrop_relay --features
  tor` once failed outright ("does not contain this feature") and was reported as exit 0, one step
  away from deploying a three-day-old binary as new. Redirect to a file and check `$?`, or set
  `pipefail`, whenever the exit status is load-bearing.

---

## 4. Updating Flutter packages

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
  file_picker ≥ 9 uses static `FilePicker` calls; file_picker 12.3 is federated and
  `saveFile()` returns a Uri; flutter_local_notifications ≥ 19 uses named params;
  flutter_secure_storage ≥ 10 dropped `AndroidOptions(encryptedSharedPreferences:)`.
- **`flutter_foreground_task`** (opt-in background delivery, #13) drives
  `core/background_delivery.dart` and the `<service>` + `FOREGROUND_SERVICE*`/`WAKE_LOCK`
  entries in `AndroidManifest.xml`; match its installed major's API. From 11 it uses Flutter's
  built-in Kotlin, which cleared the last Kotlin Gradle Plugin warning from the Android build.
  Its background-wake behaviour needs a real device (§13).

### Updating the Flutter SDK itself

A Flutter bump is a deliberate, tested task, never a side effect: prefer the latest stable, but
move only when something applies — a security fix (the strongest reason), FFI/native-interop
changes (we are heavy flutter_rust_bridge users), Android build/R8/split-ABI changes (the per-ABI
F-Droid split is fragile), Impeller on Android — weighed against the cost of any AGP-major jump.
An upgrade means: change `app/.fvmrc`, migrate Android build changes (AGP/Gradle/Kotlin), re-verify
F-Droid reproducibility byte for byte (§11), smoke-test on a device, and update every machine that
builds a release (the Windows VM has its own SDK). History: 2026-09-20 stayed on 3.44.6 (no
security fixes); 2026-10-05 moved to 3.47.6 for 0.1.28 (3.47.2 updated the engine's libpng; we
decode images contacts send).

---

## 5. Updating Rust crates

- Prefer a **minimal** update (`cargo update -p <crate>`, `--precise` when the index is stale): a
  plain `cargo update` once moved 180 crates at once. Then `make core-test`, `make clippy`, **and**
  `cargo build -p nightdrop --features tor` — the Tor path is feature-gated and a plain
  build/test will not compile it.
- Crypto/transport crates (`vodozemac`, `arti`, `spake2`, `chacha20poly1305`, `argon2`) are
  deliberately chosen audited crates. Major-version bumps here are security-relevant changes:
  read their changelogs, and never swap one for an unaudited alternative.
- **vodozemac upgrades must keep old installs working.** `core/tests/vodozemac_compat.rs` restores
  state written by vodozemac 0.8 and keeps conversations going; its fixture must never be
  regenerated with a newer vodozemac.
- **TLS for Tor is rustls (ring), not native-tls**, so the core cross-compiles to Android without
  OpenSSL. rustls 0.23 needs an explicitly installed crypto provider: the
  `ring::default_provider().install_default()` calls in `core/src/transport/tor.rs` and
  `relay/src/main.rs` exist for arti's internals; keep them. On Android arti also needs an
  explicit writable `state_dir` and relaxed fs-mistrust (`tor.rs`).
- The `tor` feature bundles SQLite from source, so no system `libsqlite3` is needed.
- **Vendored patches (`third_party/`) are dropped only on evidence.** Before removing one because
  "upstream fixed it", read the upstream release's *source*. A patch note claimed
  `saturating-time` 0.5.0 contained the Windows fix; the patch was dropped on that word, and the
  first Windows bridge test hung exactly as before. 0.5.0 does not contain it.
- Re-run the dependency audit in `DEPENDENCIES.md` on any bump that adds or changes a crate.

---

## 6. Changing the Dart↔Rust surface

Any change to the `pub` items in `core/src/api.rs`:

1. `make gen-bridge` — regenerates `core/src/frb_generated.rs` and `app/lib/src/rust/`
   (both are committed).
2. Mirror the change in the abstract seam `app/lib/src/core/nightdrop_core.dart`, then in
   `RustNightdropCore` (real) and `MockNightdropCore` (UI-only tests). Keep all three in sync.
3. Run §3. `rust_bridge_test.dart` is the canary for a stale bridge.

**"Surface" includes doc comments and the names of private fns** — both are copied into the
generated Dart, so editing a `///` on a `pub fn`, or adding a private helper in `api.rs`, drifts
the committed bindings even though no signature changed and everything still compiles. Nothing
fails; the checked-in files just quietly stop matching. Re-run `make gen-bridge` before
committing.

---

## 7. App identity (name / bundle id)

Current identity: **`app.nightdrop` / "Night Drop"**. For Android and Linux it is
a **build-time variable** — a pre-release rename needs no source edit:

```sh
NIGHTDROP_APP_ID=org.example.chat NIGHTDROP_APP_NAME="New Name" scripts/install-android-app.sh
NIGHTDROP_APP_ID=org.example.chat scripts/install-desktop-app.sh
```

Where it's wired:

- `scripts/install-android-app.sh` exports `NIGHTDROP_APP_ID`/`NIGHTDROP_APP_NAME` as Gradle project
  properties (`ORG_GRADLE_PROJECT_appId` / `..._appName`); `scripts/install-desktop-app.sh`
  exports `NIGHTDROP_APP_ID` for the Linux CMake.
- `app/android/app/build.gradle.kts` reads `appId`/`appName` with the defaults, and
  fills the `${appName}` placeholder in `AndroidManifest.xml`. The Kotlin `namespace`
  (and `MainActivity.kt`'s package, `app.nightdrop`) is intentionally **fixed** —
  it names code, not the shipped identity.
- `app/linux/CMakeLists.txt` reads `NIGHTDROP_APP_ID` from the environment.
- **iOS/macOS have no variable** — edit `PRODUCT_BUNDLE_IDENTIFIER` in
  `app/ios/Runner.xcodeproj/project.pbxproj` (Runner + RunnerTests, 3 configs each)
  and `app/macos/Runner/Configs/AppInfo.xcconfig`. Windows metadata lives in
  `app/windows/runner/Runner.rc` and the window title in `windows/runner/main.cpp`.

**Caveat:** to Android, a new `applicationId` is a *different app*. It installs
alongside the old one (no upgrade, no data migration) — uninstall the old id manually.

That is also the way to put a **test build beside a real install**, e.g.
`NIGHTDROP_APP_ID=app.nightdrop.wttest NIGHTDROP_APP_NAME="ND WT Test"`. On Linux the test id gets
its own data folder (`~/.local/share/<id>`) and its own keyring entry (`<id>.secureStorage`);
remove both when done (`secret-tool clear account <id>.secureStorage`).

---

## 8. Shell scripts (`scripts/`)

| Script | Purpose |
|---|---|
| `install-desktop-app.sh` | build the Linux release bundle (Tor + relay baked in via `--dart-define`) and install it as a desktop app; `--no-build`, `--run`, `--uninstall` |
| `install-android-app.sh` | build + install + launch the APK on a connected device; `--release`, `--universal`, `--build-only`, `--install-only`, `--diag`, `--wireless IP PORT`. **`--release` publishes** (§13) |
| `build-appimage.sh` | build the Linux AppImage into `website/applications/linux/` — which **is** the onion site (§12); `--no-build` repackages the existing bundle |
| `build-windows-installer.ps1` | build `NightDropSetup.exe` on Windows (`docs/building-windows.md`) |
| `deploy-website.sh` | publish binaries to the onion site: refuses non-release-signed APKs, regenerates + GPG-signs `SHA256SUMS`, writes `applications/android/index.html` (§12) |
| `gen-update-manifest.sh` | write `website/update.json` (`make update-manifest`) — **publish time only** (§12) |
| `deploy-clearnet.sh` | rsync `website/` (minus `applications/`) to this machine's nginx web root (`docs/hosting.md`) |
| `onion-website.sh` | serve `website/` with nginx behind a Tor v3 onion service, with an end-to-end self-dial watchdog |
| `install-onion-service.sh` | install the onion website as the `nightdrop-onion` systemd user service |
| `setup-pluggable-transports.sh` | write `transports.txt` for obfs4/snowflake bridges (desktop) |
| `watch-relay-queue.sh` | watch the relay's queue file |

Quick local previews are one-liners in `README.md`. Dev desktop runs use `make app-run`.

The scripts derive the repo root from their own path, **but every one of them honours
`PROJECT_ROOT`, `FLUTTER_HOME`, `ADB` and `ANDROID_SDK` from the environment first** — and that is
where they bite:

- **The maintainer's `~/.bashrc` exports `PROJECT_ROOT`** (the live checkout), so running a *worktree's* copy of a
  script still builds the live tree, writes the AppImage into the live `website/applications/linux/`
  and re-signs the manifest. On 2026-09-30 that put an untagged 0.1.26 AppImage on the onion site
  for a few minutes. Pass `PROJECT_ROOT=<worktree>` explicitly; `git status` cannot catch it
  (`applications/` is gitignored).
- **A worktree builds a release with no relay, silently.** `build-appimage.sh` and
  `install-android-app.sh` bake the relay address from `$PROJECT_ROOT/relay-state/onion`, which is
  gitignored and so absent from a fresh worktree: they print one info line and build a P2P-only app
  (no short codes, no offline delivery), exit 0. The F-Droid APKs and the Windows installer take it
  from `fdroid/app.nightdrop.yml` instead and are immune. Copy **only** `relay-state/onion` (the
  public address — never the keys beside it) into the worktree first. On 2026-10-06 the 0.1.28
  universal APK and AppImage were built this way and caught before publishing.
- **`FLUTTER_HOME`, not `PATH`.** It defaults to `~/flutter`, so a side-by-side SDK on `PATH` is
  ignored: the 0.1.28 universal APK first came out on Flutter 3.44.6 (without the libpng fix 3.47.6
  was adopted for) while the shell's `PATH` said 3.47.6. Set `FLUTTER_HOME` to the SDK matching
  `app/.fvmrc`, and check the result rather than the command (§11.6).
- **WebTunnel is on in every app build.** The patched cargokit (`builder.dart`) adds
  `--features webtunnel` unless `NIGHTDROP_WEBTUNNEL=0`; a bare `cargo build` leaves it off.

After editing a script, at minimum run `bash -n <script>`; use shellcheck if available. Watch the
`set -e` + `grep` trap: a grep that matches nothing exits 1 and kills the script — append
`|| true` when an empty result is a valid outcome.

---

## 9. Known build traps (each has burned an hour before)

- **CMake "cannot copy to /usr/local: Permission denied"** on `flutter build linux`:
  a stale cache from an earlier *failed* configure. `rm -rf app/build/linux/x64/<mode>`
  and rebuild. Never sudo it.
- **Android build stops with "No Android SDK found"** even though builds worked
  before: check `app/android/local.properties` still points at a real SDK and that
  `java`/`javac` exist. Machine cleanups tend to eat these.
- **Plugins needing a newer compileSdk.** `file_picker` → `flutter_plugin_android_lifecycle`
  demands compileSdk 36 while Flutter's default is lower; `app/android/build.gradle.kts` overrides
  it in a `subprojects { afterEvaluate { … } }` block that must be registered **before**
  `subprojects { evaluationDependsOn(":app") }`. Ordering matters.
- **`flutter analyze` (bare) reports ~185 errors** — they're all in the vendored
  `app/rust_builder/cargokit/`. Analyze `lib test integration_test` instead.
- **Gradle 9**: `app/rust_builder/cargokit/gradle/plugin.gradle` is locally patched
  (Gradle 9 removed `Project.exec()`). A blind cargokit re-vendor loses the patch.
- **Old artifacts lie**: `app/build/**` can contain manifests/bundles from before a
  rename or upgrade. When grepping for stale identifiers, exclude `app/build/`,
  `**/ephemeral/`, `.plugin_symlinks`, `.dart_tool`.
- **Release builds are already minified and resource-shrunk** by Flutter's default — declaring
  `isMinifyEnabled`/`isShrinkResources` changed the artifact by four bytes. Comparing a local build
  against a *published* APK proves nothing about R8 (both are minified); only local-vs-local with
  the flag flipped does (`false` gives a 22.4 MB dex against 3.5 MB). R8 damage is runtime-only and
  silent — a green build is not evidence; smoke-test QR scanning, notifications and background
  delivery on hardware.
- **Tor is slow on first run** (~30–60 s bootstrap; onion reachable after ~1–3 min).
  The UI's "publishing your address" banner reflects `onion_ready()` — that's normal,
  not a hang.

---

## 10. Things that look odd but are intentional (don't "fix")

- **QR scanning uses `flutter_zxing` (ZXing), not `mobile_scanner`/ML Kit** — deliberately,
  to keep **Google Play Services out of the APK**. ML Kit barcode scanning drags in
  `play-services-mlkit-barcode-scanning` + `firebase-*`/`play-services-base` transitive infra;
  ZXing is Apache-2.0, on-device, and pulls no Google runtime (verify with a `classes*.dex`
  scan for `com/google/android/gms` after any scanner change). There is no FCM/push anywhere.
- **The composer paste button is text-only** (built-in `Clipboard`), by design. Image paste used
  `pasteboard`, dropped because it tripped the Flutter Kotlin Gradle Plugin warning; images go
  through the attach button (which also compresses them for Tor). Before re-adding
  `pasteboard`/`super_clipboard`, re-check that warning **and** the
  `super_native_extensions`↔`file_picker` `win32` conflict that also blocked it.
- **Plaintext only at the UI edge.** If a change makes Dart touch key material,
  ratchet state, or unencrypted persistence, it's wrong — move it into `core/`.
- **`MediaCache.wipe()` on logout** (`app/lib/src/core/media_cache.dart`): decrypted
  attachments are memoized in RAM and decrypted videos land as `nightdrop-media-*` files
  in the OS temp dir; logout must clear both. Anything new that writes decrypted
  bytes anywhere must be added to this wipe.
- **`_Root` defers `core.start()` to a post-frame callback** (`app/lib/src/app.dart`):
  `start()` can notify synchronously and notifying mid-build crashes. Don't inline it.
- **The relay logs nothing about users.** The flow-log/TUI exist only behind
  `NIGHTDROP_RELAY_DEV` / `NIGHTDROP_RELAY_TUI` and show metadata only — never blob bytes. The
  default output is its onion address and a watchdog heartbeat. Keep it that way (invariant: no
  server-side logs).
- **Relay blobs are double-wrapped**: mailboxes are addressed by derived handles (never an onion
  address or identity key), and queued frames are sealed so the relay can't read sender identity
  keys or `Hello` addresses. Don't post raw `wire::encode` bytes to the relay; don't put addresses
  in handles. `poll_relay` silently skips blobs that don't unseal — deliberate (garbage in a
  mailbox must not wedge the drain).
- **Multi-relay fan-out (#17) seals once, posts many, dedups by hash.** `queue_on_relays`
  computes the sealed blob **once** and posts the *identical* bytes to the primary + each of
  the recipient's `peer_relays`; the receiver drains all and de-dups by SHA-256 of the blob
  (`seen_relay_blobs`). Two gotchas to preserve: (1) an edit/unsend must recall **every** stored
  copy — iterate, do **not** `.any()` (it short-circuits after the first success and strands
  siblings); (2) `fetch`/`take` on the relay **drain**; use `peek` (count only) for
  non-destructive checks. A relay being unreachable must never abort the drain/fan-out from the
  others (best-effort, ≥1 success).
- **Relay failures are surfaced, not just swallowed.** `poll_relay` records per-relay
  reachability for our own `my_relays` (`relay_health()`), and the home screen warns when a
  self-hosted relay stops answering. When opt-in server storage is on but a send can't reach any
  relay to store the copy, the chat's storage banner switches to "delivered but not stored". Both
  flags are in-memory, never persisted. A new relay code path must keep them updated.
- **Wire frames are length-prefixed + zero-padded to fixed buckets** (`wire::encode`/`decode`,
  `WIRE_VERSION` 2) so frame *length* leaks nothing. Never parse wire bytes as JSON directly. If
  you add a big new frame type, check the bucket schedule (`PAD_BUCKETS`/`PAD_BLOCK`). Changing the
  framing means bumping `WIRE_VERSION`.
- **State-changing control frames are authenticated on the ratchet** (`node::MARK_*`,
  `authed_control` / `verify_control`): the receiver acts only if an encrypted marker decrypts to
  the expected constant. A new state-changing control frame must be authenticated the same way.
  (`Approved`/`CodeInUse` stay plaintext by design: pairing-time, self-correcting.)
- **The only send-failure error string is** `"peer offline and no relay accepted the message"`.
  The Dart UI cleans the bridge's `AnyhowException(...)` wrapper via `cleanCoreError` and preserves
  the composer draft on failure. Don't clear the input before the send resolves.
- **The relay poll is on a timed cadence** — 15 s foreground, 5 min background, 2 s while pairing,
  plus an immediate catch-up on foregrounding (`RELAY_POLL_*` in `api.rs`). Each poll is a full Tor
  round-trip, so don't "make it snappier": online messages already arrive push-style over the
  direct Tor stream; the relay only bounds offline-mail latency.
- **`devlog!` compiles to nothing in release builds.** Those logs contain identity keys, invite
  codes, and decrypted display names. New core logging that touches such data must use `devlog!`,
  not `eprintln!`; `diag!` is the release-safe channel and must never carry identity
  (`ARCHITECTURE.md` §6).
- **The demo core** (`NightdropCore::new()`, in-process echo peer) is what a bare `flutter run`
  uses. Every real build passes `--dart-define=NIGHTDROP_TOR=1` (the scripts and the recipe do).

---

## 11. Cutting a release

**The order below is the point** — every step out of order has burned a release round at least
once. F-Droid-specific detail (recipe shape, validation, local builds) is in `fdroid/README.md`.

### 11.1 Why the order matters: F-Droid builds from our tags by itself

F-Droid MR !43625 merged on 2026-08-14, so `app.nightdrop` is in fdroiddata `master` and **no
per-release MR exists**. The recipe has `AutoUpdateMode: Version` + `UpdateCheckMode: Tags
^v[\d.]+$`: the bot watches this repo's tags, reads the version from `app/pubspec.yaml`
(`UpdateCheckData`), and generates the three per-ABI build entries itself (`VercodeOperation`).
Tag `vX.Y.Z` and it happens — typically ~4 h to the bot, ~3 days to f-droid.org. That shifts work
onto us with no second chance:

- **The tag must match `^v[\d.]+$` and `app/pubspec.yaml` must be right at that commit.**
- **The published GitHub binaries are load-bearing**: the recipe has `binary:`, so F-Droid rebuilds
  and compares against the APKs we publish. Sign with `--alignment-preserved` or a genuinely
  reproducible build is published as *not* reproducible, in their repo, under our name.
- **Never let a tag point at code we have not built and verified.** A bad release flows to F-Droid
  users automatically.
- **Tags are immutable** (GitHub ruleset "Release tags are immutable", 2026-10-01, no bypass): a tag
  on the wrong commit cannot be moved — it means a new version, or disabling the ruleset by hand.
  Ruleset "Protect main history" likewise blocks force-push and deletion of `main`.

### 11.2 Version codes

`app/pubspec.yaml` `version: X.Y.Z+N`; per-ABI codes are `N*10 + abi` (1 armeabi-v7a, 2 arm64-v8a,
3 x86_64) and **the universal APK is slot 4** (`app/android/app/build.gradle.kts`), so base 414
gives 4141/4142/4143 + 4144. **Codes must never go backwards** — Android refuses the update and
installs silently stop arriving; check the new low code against the last published high one. The
base jumped 16 → 402 when the scheme was adopted and must never go back below that. The recipe's
`VercodeOperation` must encode the same formula, listed in the same ascending order as its
`Builds:` (`'%c * 10 + 1'`, `+ 2`, `+ 3`), or F-Droid's `checkupdates` CI fails.

### 11.3 Steps

1. **Bump** `version:` in `app/pubspec.yaml`, then `make config` (regenerates `app_version.dart`;
   skipping it ships an About screen showing the previous version).
2. **Changelogs for every per-ABI code** — `fastlane/metadata/android/en-US/changelogs/<N>1.txt`,
   `<N>2.txt`, `<N>3.txt`, not the base. F-Droid reads them from the **tagged** commit, so a miss
   ships a release with no "What's New" that cannot be fixed afterwards. Until 0.2, every changelog
   repeats the 0.2 break notice (older versions lose offline mail, `docs/design/mailbox-handles.md`
   §5). `./fdroid/check-metadata.sh` must pass and end `rewritemeta: canonical`.
3. **Commit and push `main` without a tag.** Point the recipe's three `commit:` fields (full
   40-char hash) and `versionName`/`versionCode`/`CurrentVersion*` at the release commit, re-run
   `check-metadata.sh`.
4. **Build and verify before tagging** — two fresh builds from a local bare clone (the recipe's
   `Repo:` pointed at it; the clone must be owned by uid 1000 via `podman unshare chown`, else git
   refuses "dubious ownership"), then one from GitHub:
   `SKIP_BINARY=1 FDROID_LOCAL_ARTIFACTS=<dir> ./fdroid/build-locally.sh --fresh`.
   `--fresh` is **not optional**: the cached volume holds the previous clone, and the new commit
   not being in it fails as `VCSException: Git checkout of '<sha>' failed`, reading like a bad
   hash. Builds share a volume, so run them one after another.
5. **Sign each APK with `--alignment-preserved`** (`fdroid/README.md` has the command), and
   check each against the others with fdroidserver's `verify_apks` (the fdroidserver checkout
   lives in the `fdroid-vagrant` podman volume), with a deliberately mismatched pair as a
   control. Unsigned builds differ only inside the throwaway signing block; that is expected.
6. **Tag** `git tag -a vX.Y.Z <release commit> -m "Night Drop X.Y.Z"` and push the tag.
7. **Build the rest**, with the §8 traps in mind: the universal APK
   (`NO_DEPLOY_WEB=1 install-android-app.sh --release --universal --build-only`), the AppImage
   (`build-appimage.sh`), and the Windows installer in the VM (`docs/building-windows.md`).
8. **Check every artifact's contents** before publishing (§11.6).
9. **Publish the GitHub release** — 15 assets: 4 APKs, the AppImage, `NightDropSetup.exe`, a `.asc`
   for each, `SHA256SUMS`, `SHA256SUMS.asc`, `nightdrop-signing-key.asc` — generated by running
   `scripts/deploy-website.sh` against an unserved `PROJECT_ROOT` (a worktree), then verified the
   way a user would (`gpg --verify`, `sha256sum -c`).
10. **Publish the website** — `scripts/deploy-website.sh <files>` on the live tree, then
    `make update-manifest`, commit `website/update.json`, then `scripts/deploy-clearnet.sh`.
11. **Verify**: `/releases/latest/download/` resolves to the new files; `curl` the onion
    (`http://127.0.0.1:8787/update.json`) and clearnet `update.json`; a day later, fdroiddata
    `master`'s `CurrentVersion` has moved (if not, suspect `UpdateCheckData` or
    `VercodeOperation`, not the release).

### 11.4 Before a release, also

- Sanity-pass the invariants (`ARCHITECTURE.md` §1a) against the diff.
- On-device validation of anything touching background delivery (§13): backgrounded and
  swiped-away cases. CI/desktop can't cover it.
- The relay is production state: its `.onion` is pinned by `relay-state/` and baked into every
  build, so **losing that directory changes the relay address** for every install.

### 11.5 Credentials a release uses (locations only — never echo a value)

| What | Where |
|---|---|
| Android release keystore | `~/.android/nightdrop-release.jks`; signing properties in `app/android/key.properties` (gitignored, chmod 600). Copy it into a worktree only for the build and delete it right after |
| Release cert SHA-256 | `d08b8e64…2c0ac` — also `AllowedAPKSigningKeys` in the recipe and `RELEASE_CERT_SHA256` in `deploy-website.sh` |
| GPG release key | `security@nightdrop.app`, `079B A016 9201 A8AB 11F3 2385 884E ACB8 89D0 2002`; passphrase normally cached in `gpg-agent` — check with a throwaway `--pinentry-mode error` signature before assuming a human is needed. The **private** key is not in the repo (its location: `MAINTENANCE.local.md`) |
| Directory-signing key | `relay-state/directory-signing-key` (Ed25519; a different key from the GPG one) |
| GitHub | `gh`, authenticated as `nightdropapp` |
| Security mailbox | `security@nightdrop.app`, Proton Mail on the custom domain with **our own key**: reports stay PGP-encrypted to the published key and are decrypted locally; Proton's address-key encryption stays off |
| GitLab (fdroiddata fork) | `gitlab_token.txt` at the repo root (gitignored); use as a `PRIVATE-TOKEN:` header against the API. `glab auth status` (its own stale token) and `ssh -T git@gitlab.com` (no key registered) both fail and **say nothing about this token** — on 2026-08-10 they were taken as proof the work could not be done from here. Rarely needed since the merge |

### 11.6 Check what you built, not what you ran

Every one of these has shipped, or nearly shipped, wrong once:

```sh
# version inside a desktop bundle (the runner binary's mtime is a false negative)
strings -a <bundle>/lib/libapp.so | grep -oE '0\.1\.[0-9]+\+[0-9]+'
# which Flutter a binary really has: each SDK's bin/cache/engine.stamp is embedded in
# libflutter.so / libflutter_linux_gtk.so / flutter_windows.dll
strings -a lib/arm64-v8a/libflutter.so | grep -c "$(cat $FLUTTER_HOME/bin/cache/engine.stamp)"
# the relay address is baked in
strings -a lib/arm64-v8a/libapp.so | grep -c "$(cat relay-state/onion | cut -d. -f1)"
# APK version and signer
aapt2 dump badging x.apk | head -1; apksigner verify --print-certs x.apk
```

---

## 12. The website, publishing, and update.json

Two sites, one `website/` directory (`docs/hosting.md` has the hosting detail):

- **The onion site serves `website/` live from disk** (nginx behind `nightdrop-onion.service`), so
  writing a file there **is** deploying it. It additionally serves `applications/` (the binaries)
  and `update.json`, the app's only update channel (`ARCHITECTURE.md` §10a).
- **The clear-web site** is a copy, rsynced by `scripts/deploy-clearnet.sh` without
  `applications/`: clearnet downloads go to GitHub Releases via `config.js`.

Traps, each of which has happened:

- **Checking out a branch is a deploy.** `git switch`/`checkout`/`rebase`/`cherry-pick` rewrite
  `website/` in place, and the onion serves whatever the working tree holds. On 2026-09-30 a branch
  cut before a title change put the old site back live twice in one afternoon. Before switching,
  `git diff --stat HEAD <branch> -- website`; if non-empty, bring `main`'s `website/` into the
  branch first or treat the switch as a deploy. Check what is live with
  `curl -s http://127.0.0.1:8787/index.html`.
- **`website/update.json` is generated at publish time only** (`make update-manifest`), after the
  new binaries are in `website/applications/`. It is deliberately *not* part of `make config`: it
  used to be, and bumping the version then announced the release instantly while the site still
  served the previous APKs **under the new version number with matching hashes** — the download
  verified, installed, changed nothing, and re-prompted forever. Restore it with
  `scripts/gen-update-manifest.sh`, never `git checkout` (only correct when HEAD is clean).
- **Never `git add -A` while a manifest is staged for testing.** That published a nonexistent
  0.1.18 for ~20 minutes on 2026-08-06.
- **A download link must never pin a release tag.** `website/config.js` once hardcoded
  `/releases/download/v0.1.15/…`, so every clearnet Linux download served 0.1.15 for three days
  after 0.1.16 shipped (`docs/advisories/2026-08-05-linux-download-pinned-to-0.1.15.md`). Use
  `/releases/latest/download/…`.
- **Publish binaries with `scripts/deploy-website.sh`, never by copying them in.** Copying works —
  which is the trap. The script refuses any APK not signed by the release key, regenerates
  `SHA256SUMS` over everything present and GPG-signs it, and generates `android/index.html` from
  the APKs themselves (version via `aapt2`, size, hash). Don't hand-edit that page.
- **Restarting an onion service rotates its introduction points**, and clients keep the old
  descriptor until they refetch — so a restart is never free, for the relay, the website onion or
  anything else. Anything automated that restarts one must require *sustained* failure, or it
  thrashes (the relay's restart counter once reached 34 that way).

---

## 13. Device testing

- **`install-android-app.sh --release` PUBLISHES.** On success it runs `deploy_website()`, copying
  the APK into `website/applications/android/` — live on the onion site. Only `NO_DEPLOY_WEB=1`
  stops it. For a test build, build and `adb install -r` by hand.
- **Never install a debug-signed APK over a release install.** A release APK carries the published
  signer, so `adb install -r` keeps identity and chats; a debug APK forces an uninstall first, which
  **destroys the identity**. Check with `apksigner verify --print-certs` before installing.
  Downgrading versionCode also forces an uninstall. Use a separate app id (§7) for experiments.
- **Wireless adb**: the port rotates whenever wireless debugging is toggled (the test phone's IP is
  a static DHCP lease, in `MAINTENANCE.local.md`) — discover it with `adb mdns services | grep
  _adb-tls-connect`, never trust a remembered address. If connect fails while mDNS still
  advertises, the host is unpaired: open *Wireless debugging → Pair device with pairing code* on
  the phone, then `adb pair <ip>:<pairing port, from adb mdns services> <6-digit code>` and
  `adb connect <ip>:<connect port>`. Pin the serial with `-s`: the device appears as several adb
  entries. The scripts find the SDK through `ANDROID_SDK`, then `ANDROID_HOME`/`ANDROID_SDK_ROOT`,
  then `~/Android/Sdk`.
- **The desktop app is single-instance** (`app/linux/runner/my_application.cc`): a second launch
  hands off to the running one and exits, so kill the old process before testing a new build.
- **Flutter renders to a canvas**, so `uiautomator dump` returns no text. Drive the UI with
  `adb exec-out screencap -p` plus cropping, and verify each tap landed before the next.
- **A black screenshot is not a blank screen.** `screencap` returns black for anything inside
  Samsung Secure Folder and for our own app while backgrounded (`FLAG_SECURE`).
- **An adb-injected screenshot (`input keyevent 120`) does not fire Android's
  `ScreenCaptureCallback`.** Only a hand-taken screenshot tests that path.
- **Never trust a component's own opinion of its health — prove it end to end.** The same mistake
  happened three times: the Tor guard heal keyed off `is_fully_reachable()` (bootstrap *progress*,
  not liveness) and destroyed healthy guard sets; the relay watchdog trusted the same signal and
  reported healthy for 90 minutes while every client timed out; the website onion had no check
  and went dark while systemd called it active. The fix each time was an **end-to-end self-dial**
  plus a heartbeat logged every cycle — counters are worthless without evidence the log is growing.
- **The test phone** is a Galaxy S25 on wireless adb (serial and address: `MAINTENANCE.local.md`).
- **AppImage window icon**: GTK resolves it by theme name, so the AppDir ships
  `usr/share/icons/hicolor/*/apps/app.nightdrop.png` and `AppRun` prepends `$HERE/usr/share` to
  `XDG_DATA_DIRS` (`scripts/build-appimage.sh`). Checking it on the build machine is misleading,
  because `~/.local/share/icons` is searched first: test with `XDG_DATA_HOME` pointed at an empty dir.
- **A deleted chat is usually a deleted chat.** A recurring "chat deleted" report turned out to be
  real deletes of abandoned test chats, not a bug and not caused by closing the app.

---

## 14. Services on the host

The relay and both website onions run on the maintainer's machine as **systemd user units** —
`systemctl --user`, not `systemctl` (which reports them inactive while they run fine):

| Unit | What | Notes |
|---|---|---|
| `nightdrop-relay` | the production relay (`~/.local/bin/nightdrop-relay`, state `relay-state/`) | `Restart=always`, so a stale PID never means dead; read `journalctl --user -u nightdrop-relay` and look for the `self-dial reached` heartbeat every 5 min. Temporary diagnostics go in a drop-in under `nightdrop-relay.service.d/` — never `NIGHTDROP_RELAY_DEV` on the live relay |
| `nightdrop-onion` | the onion website (`scripts/onion-website.sh 8787`: nginx + tor) | serves `website/` from disk; local check at `http://127.0.0.1:8787/` |
| `nightdrop-traffic*.timer` | snapshots GitHub traffic and release downloads | |

Both onions are stable across restarts, but a restart still rotates introduction points (§12): a
client that bootstrapped *before* a relay restart can keep failing on stale introduction points
until it refetches the descriptor (restarting the client app does). To probe the relay end to end
from here: `NIGHTDROP_RELAY=$(cat relay-state/onion) cargo test -p nightdrop --features tor --test
tor_smoke -- --ignored --nocapture relay_is_reachable`. Running `scripts/onion-website.sh` by hand
while `nightdrop-onion` is up fails ("another Tor process … same data directory", port 8787 busy) —
the expected conflict, not a fault.
Deploying a new relay build: back up the binary, install, restart once, then confirm a
`self-dial reached` line — systemd's "active" proves nothing (§13).

---

## 15. Where knowledge lives

- `ARCHITECTURE.md` is authoritative; when code and doc disagree, reconcile them in the same change.
- **Everything project-related belongs in a committed file.** `CLAUDE.md` and `TODO.txt` are
  gitignored: `CLAUDE.md` may only *summarise* rules whose full text lives in a committed file it
  names, and `TODO.txt` holds future work and nothing else.
- **Private values** — LAN addresses, device serials, key locations — go in the gitignored
  `MAINTENANCE.local.md`, because the repo is public; committed docs name that file instead.
- Where things go: user-facing limits and residual risks → `SECURITY.md` and `website/limits.html`;
  design and rationale → `ARCHITECTURE.md` or `docs/design/`; something that broke in the field →
  `docs/advisories/`; procedures and traps → this file; why one change was made → the commit message.
- **Nothing referenceable may live in `TODO.txt`.** It tracks *future work* — features, fixes,
  things still to do — and nothing else. A limitation, a warning, a design decision, an incident, a
  measured number, a gotcha: if anyone would later need to *look it up*, it belongs in the file
  that owns that subject. The test is not "is this useful?" but "would someone go looking for
  this?" — TODO is read once by whoever picks up the work, then rewritten.
- **Delete finished items from `TODO.txt` outright** — not struck through, not moved to a "done"
  section, deleted the moment the work lands, and stale items corrected as soon as they are
  noticed. Whatever was worth keeping is already in the commit, the doc, or the advisory.
- **Verify before acting on a claim.** A note, comment, changelog, issue saying "fixed", or an
  earlier conclusion is a claim, not evidence: check the primary source (§5 has the cost of not
  doing so).
- v1 is **1:1 only**; don't add group-chat assumptions (`docs/design/group-chat.md`, not before 0.3).
