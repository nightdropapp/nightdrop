# Reproducible builds & F-Droid — status and recipe

Goal: a third party (and F-Droid) can rebuild the shipped APK **bit-for-bit from source** and
verify it matches the developer-signed release. This is the trust gate for the privacy audience
(`IMPROVEMENT_PLAN.md` §5.1, `TODO.md` #15).

This is harder than a plain Android app because the build has two halves — a **Flutter** app and a
cross-compiled **Rust** core (`libnightdrop.so`, built by cargokit through the NDK). Each half must
be deterministic.

## What's already in place

- **License:** AGPL-3.0-or-later. No proprietary/Google deps (no Play Services / Firebase / ML Kit;
  `flutter_zxing` is Apache-2.0). ✅ F-Droid FOSS-compatible.
- **Release signing wired:** `app/android/app/build.gradle.kts` reads `app/android/key.properties`
  (gitignored; keystore lives outside the repo). ✅
- **Deterministic `versionCode`:** read from `pubspec.yaml` `version: x.y.z+N` (a committed integer,
  **not** derived from git/time). Bump `N` monotonically each release. ✅
- **Pinned Rust toolchain:** `rust-toolchain.toml` pins an **exact** `1.96.0` (not `stable`), so
  every builder resolves the identical `rustc`. ✅

## Proven: the release APK is content-reproducible

Two **clean release-APK builds** were compared entry-by-entry. Result:

- **All 349 entries byte-identical** — `libapp.so` (Flutter AOT), `libnightdrop.so` for **every ABI**
  (arm64 / armv7 / x64), `classes*.dex`, `flutter_assets/`, resources. The Flutter AOT snapshot, the
  Rust core, dex, and assets are all deterministic with the pinned toolchain.
- **Zip entry order + timestamps identical.**
- The **only** difference is a tight ~7.8 KB region at EOF — the **APK Signing Block** (the RSA
  signature, non-deterministic by design). F-Droid strips and re-creates the signature (or copies the
  developer's via `apksigcopier`), so this is *exactly* the part that doesn't count toward
  reproducibility verification.

In other words the build is reproducible today for F-Droid's purposes: everything F-Droid verifies is
already bit-for-bit. (Test was same-machine, same-path; the remaining variable is a *different
machine* — covered by pinning the toolchain/NDK/Flutter versions and the remap flags below.)

## Proven: the Rust core is reproducible

The core lib is deterministic across rebuilds, and — with path remapping — across **different build
directories** (the case F-Droid actually exercises). Verified:

```sh
# Same source built in two different paths, with the reproducibility flags, produce an
# IDENTICAL libnightdrop.so:
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
RUSTFLAGS="--remap-path-prefix=$PWD=. --remap-path-prefix=$CARGO_HOME=/cargo" \
  cargo build --release -p nightdrop
```

Without the remap the lib differs by ~12% across paths (build-dir + registry paths leak into
symbol/debug encodings — plain `strings` misses them). **`trim-paths` (the clean profile option) is
still nightly-only as of 1.96, so `--remap-path-prefix` is the stable knob.** These flags belong in
the F-Droid build recipe / a repro build wrapper — they can't be a committed `.cargo/config.toml`
(the build path is machine-specific), so they're set from `$PWD`/`$CARGO_HOME` at build time.

cargokit invokes `cargo` and honours `RUSTFLAGS` from the environment, so exporting them before
`flutter build apk` propagates to the core lib.

## Remaining work

Most of the "diffoscope grind" turned out to be a non-issue — the content is deterministic out of
the box. What's proven vs. left:

1. ~~**NDK cross-compile target lib.**~~ ✅ **Proven.** `aarch64-linux-android` `libnightdrop.so` is
   bit-for-bit identical across build paths (remap flags + NDK clang); the linker emits **no
   build-id**. All ABIs (arm64/armv7/x64) are also identical *inside the release APK* above.
2. ~~**Flutter AOT + assets.**~~ ✅ **Proven** — `libapp.so`, kernel/assets, and dex are byte-identical
   across two clean release builds. Pin the **Flutter SDK commit** in the recipe to hold this across
   machines.
3. ~~**APK packaging.**~~ ✅ **Proven** — entry order + zip timestamps identical; only the signature
   block differs (F-Droid re-signs). No `SOURCE_DATE_EPOCH` fiddling needed here.
4. **Cross-machine + cargokit `RUSTFLAGS`.** The proofs above are same-machine. The last technical
   check: on a *different machine / build path*, confirm the F-Droid recipe's remap `RUSTFLAGS`
   actually reach cargokit's `cargo` invocation (it reads env `RUSTFLAGS`, so exporting before
   `flutter build apk` should suffice). Pin the **NDK version** (tested: 28.2) too.
5. **KGP plugin (`TODO.md` #5).** `flutter_foreground_task` applies its own Kotlin Gradle Plugin —
   resolve (bump to a migrated release, or vendor with the upstream migration diff) to keep the
   Gradle build clean and future-proof. Didn't block reproducibility, but worth clearing.

## Getting into F-Droid

The build recipe is **drafted** at [`../fdroid/app.nightdrop.yml`](../fdroid/app.nightdrop.yml) — a
reference copy of the `fdroiddata` metadata (provisions Flutter 3.44.6 + Rust 1.96.0 + the Android
targets, exports the repro `RUSTFLAGS`, runs `flutter build apk --release`, and pins the signing
key for reproducible verification). The **store listing** (title, summary, full description, and the
v1 changelog) is in place under [`../fastlane/metadata/android/en-US/`](../fastlane/metadata/android/en-US/),
which F-Droid pulls automatically — only screenshots remain to be dropped in (see the images README
there). Remaining steps:

1. Fill the recipe's TODOs: a **public git repo** with **tagged releases** (semver + monotonic
   `versionCode`), and a **stable production relay `.onion`** to bake in (the placeholder is the
   current dev relay).
2. Add **screenshots** to `fastlane/metadata/android/en-US/images/phoneScreenshots/` (captured with
   throwaway test identities so no real data is shown).
3. **Stand up a local F-Droid repo first** with `fdroidserver` — build the app in F-Droid's offline
   environment on your own machine to validate the recipe and no-network-at-build compliance (Cargo
   crates + pub packages must fetch deterministically) *before* submitting to `fdroiddata`.
4. Submit the metadata, flagged for **reproducible** verification, and publish the recipe so third
   parties can reproduce independently.

## Verify locally (any time)

```sh
# Rust core, cross-path reproducibility (should print identical md5s):
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
RUSTFLAGS="--remap-path-prefix=$PWD=. --remap-path-prefix=$CARGO_HOME=/cargo" cargo build --release -p nightdrop
md5sum target/release/libnightdrop.so
# …copy the source to a different path, build again with the remap, compare.
```
