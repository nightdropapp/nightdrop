# Dependency audit — no phone-home

Night Drop's privacy claims (ARCHITECTURE.md §8, §8a) are only as strong as its
dependency tree. A single library that quietly contacts an analytics, push, or ad
endpoint would leak exactly the metadata the design goes to great lengths to avoid.
This document records the audit of every direct dependency for network / phone-home
behavior, the transitive concerns worth calling out, and a checklist to re-run when
dependencies change.

**Bottom line:** the **only** network egress in the whole app is Tor. There is no
clearnet path by design (invariant: "Tor by default, never hardcode a non-anonymized
network path"). No Firebase, no FCM/APNs push SDK, no Google Play Services (GMS), no
analytics, no crash reporter, no ad SDK appears anywhere in the resolved tree.

## How this was audited

- Read every direct dependency in `app/pubspec.yaml`, `core/Cargo.toml`, and
  `relay/Cargo.toml` and classified its network behavior from its documented purpose.
- Scanned the resolved lockfile (`app/pubspec.lock`) for known offenders:
  `firebase`, `crashlytics`, `sentry`, `analytics`, `gms`, `play-services`, `admob`,
  `facebook`, `amplitude`, `mixpanel`, `segment`, `adjust`, `appsflyer` → **no matches**.
- Grepped the Android project for `google-services` / Firebase / GMS Gradle plugins
  → **none**.
- Confirmed no third-party push: notifications are local only (see below).

Reproduce with: `flutter pub deps`, `cargo tree -p nightdrop --all-features`, plus the
lockfile grep above.

## Rust core (`nightdrop`) and relay

The core is the security boundary; its dependencies are audited crypto crates and the
Tor stack. None makes a network call except the Tor stack, whose entire job is the
anonymized transport.

| Dependency | Purpose | Network? |
|---|---|---|
| `anyhow` | error handling | none |
| `vodozemac` 0.8 | Olm/Megolm X3DH + Double Ratchet (matrix.org, audited) | none |
| `spake2` 0.4 | PAKE bouncer for short-code pairing | none |
| `ml-kem` 0.3 | ML-KEM-768 (FIPS 203) hybrid PQ pairing | none |
| `hkdf`, `sha2`, `chacha20poly1305`, `argon2`, `ed25519-dalek` | RustCrypto primitives (at-rest AEAD, KDF, backup hashing, directory signing) | none |
| `rand`, `zeroize`, `base64`, `serde`, `serde_json` | RNG, memory wipe, encoding | none |
| `flutter_rust_bridge` 2.12 | Dart↔Rust FFI | none (in-process) |
| **`arti-client` + `tor-*` 0.43** (`tor` feature) | embedded Tor client + onion service | **Tor only** — this *is* the transport; connects to the Tor network and the configured relay/peer onions, never clearnet, never analytics |
| `rustls` 0.23 (ring) | TLS *inside* Tor circuits | no independent egress |
| `tokio`, `futures` | async runtime for the Tor stack | none of its own |
| `libsqlite3-sys` (bundled) | arti's **local** on-disk state store | none (local file) |

The **relay** (`relay/`) depends on the core plus the same arti stack; it publishes
its own onion service and store-and-forwards opaque, E2E-encrypted, fixed-size blobs.
It has no analytics and no clearnet listener.

## Flutter app (`app/`)

Every direct dependency is local-only except the Rust core it wraps (which networks
solely over Tor, above).

| Dependency | Purpose | Network? |
|---|---|---|
| `flutter_rust_bridge` / `nightdrop` (cargokit) | FFI to the core; builds `libnightdrop` | none (Tor is inside the core) |
| `flutter_zxing` 2.3 | **on-device** QR decode (Apache-2.0 ZXing) | none — chosen over ML Kit specifically to avoid Google Play Services |
| `permission_handler` 12 | runtime camera-permission request + open-settings | none |
| `qr_flutter` | render pairing QR locally | none |
| `flutter_local_notifications` 22 | **local** new-message/request notifications | none — not FCM/APNs, so no push-provider metadata trail |
| `flutter_foreground_task` 9 | opt-in Android foreground service for background delivery (#13) | none of its own (delivery goes through the core over Tor) |
| `flutter_secure_storage` 10 | at-rest key in the OS keystore (Keychain/Keystore) | none |
| `file_picker`, `path_provider`, `open_filex` | OS file picker, local paths, open received file in system viewer | none |
| `image`, `fc_native_video_thumbnail` | local downscale/recompress + video thumbnail | none |
| `cupertino_icons`, `flutter_launcher_icons` | icons (the latter is build-time only) | none |

### Transitive notes

- `flutter_zxing` pulls **`camera`** (CameraX on Android) and **`image_picker`** — both
  AndroidX/local, no phone-home. CameraX is not GMS.
- `permission_handler` pulls platform federated packages
  (`permission_handler_android/apple/html/windows`) — all local.
- No package in the tree pulls Firebase, GMS, an analytics SDK, a crash reporter, or an
  ad SDK (verified against the lockfile).

## Deliberate choices that keep the tree clean

These are not accidents; each avoids a common metadata leak:

- **ZXing over ML Kit** for QR scanning → no Google Play Services runtime.
- **Local notifications over FCM/APNs** → no push-provider metadata (who is notified,
  when). (This is also why iOS background delivery is out of scope for v1 — it's hard to
  do without a push intermediary.)
- **rustls (ring) over native-tls** → no system/vendored OpenSSL, and TLS exists only
  *within* Tor circuits.
- **Tor as the sole egress** → there is no code path that opens a clearnet socket.

## Re-audit checklist (run on any dependency bump)

1. `grep -iE 'firebase|crashlytics|sentry|analytics|gms|play-services|admob|facebook|amplitude|mixpanel|segment|adjust|appsflyer' app/pubspec.lock` → expect no matches.
2. `grep -rniE 'google-services|firebase|gms' app/android` (excluding `build/`) → expect none.
3. Confirm the Android merged manifest declares **only** `INTERNET`/`CAMERA`/notification
   permissions — no `ACCESS_FINE_LOCATION`, contacts, or unexpected permissions.
4. For any **new** direct dependency, add a row above and justify its network posture;
   reject anything that phones home or bundles an SDK that does.
5. Re-run `cargo tree -p nightdrop --all-features` and confirm no new crate opens a
   clearnet socket outside the `tor` feature.
