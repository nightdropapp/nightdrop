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
  every builder resolves the identical `rustc`. ✅ ⚠️ **But cargokit bypasses it for the app
  build:** `app/rust_builder/cargokit/build_tool/lib/src/builder.dart` runs
  `rustup run <toolchain> cargo build` with `<toolchain>` taken from `core/cargokit.yaml`'s
  options, which only accept `stable`/`beta`/`nightly` — and `rustup run` overrides the
  toolchain file. So `flutter build apk` builds the core with whatever `stable` currently is,
  not with 1.96.0. It has matched so far only because this machine's `stable` *is* 1.96.0;
  it silently stops matching the day stable moves. The F-Droid recipe patches that line (see
  `fdroid/README.md`); local release builds still need the same care.

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

⚠️ **That test was same-machine, same-path, and that caveat mattered more than it looks.** Across
*different* paths, 9 entries differ: `libapp.so` (the Flutter AOT snapshot), `libdartjni.so` and
`libflutter_zxing.so`, on all three ABIs. Pinning toolchain versions does not fix it and neither
do the Rust remap flags, which only cover our own crate. The fix is the F-Droid recipe's fixed
build path (§ *Remaining work* item 4). Since 0.1.8 the published APK verifies from two different
build paths.

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

⚠️ **For the Android build the flags must go in `CARGO_ENCODED_RUSTFLAGS`, not `RUSTFLAGS`.**
cargokit's `android_environment.dart` sets `CARGO_ENCODED_RUSTFLAGS` itself (an `-L` libgcc
workaround for the NDK), and that variable overrides plain `RUSTFLAGS` *entirely* in cargo — so
exporting `RUSTFLAGS` before `flutter build apk` is silently ignored for the core lib. cargokit
does preserve and append to the encoded variable, so this works:

```sh
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$PWD=.$(printf '\037')--remap-path-prefix=$CARGO_HOME=/cargo"
```

(`\037` is the unit separator cargo expects between encoded flags.) Verified 2026-07-27 by
building the app in the real F-Droid buildserver container: with `RUSTFLAGS` the resulting
`libnightdrop.so` still had 779 `/home/vagrant/.cargo/registry` paths and zero `/cargo` ones.

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
4. ~~**Cross-machine / cross-path.**~~ ✅ **Resolved in 0.1.8.** Two separate defects:
   the remap flags never reached cargo at all (they must be set via `CARGO_ENCODED_RUSTFLAGS`,
   see above), and even once they did they only covered *our* crate — the Flutter AOT snapshot
   and the CMake-built plugin libs still embedded the absolute build directory. The recipe now
   moves the checkout to a fixed path before building, as `templates/build-flutter.yml`
   prescribes. Verified by building 0.1.8 at two unrelated paths and matching the published APK
   both times (`WS_PATH=… ./fdroid/build-locally.sh`). NDK is pinned at r28c.
5. ~~**Release builds don't apply the flags.**~~ ✅ **Resolved in 0.1.8.** The published `v0.1.6`
   APK was built without the remap flags (~826 `/home/shawn/…` strings in the shipped `.so`), so
   it could never be byte-matched; `v0.1.7` had the flags but not the fixed path. Releases are
   now *built by the F-Droid recipe itself* via `./fdroid/build-locally.sh` and only signed
   afterwards, so the release path cannot diverge from the recipe by construction. Sign with
   `apksigner --alignment-preserved`, or the re-aligned zip breaks signature copying. Full
   release procedure: `fdroid/README.md`.
6. **KGP plugin (`TODO.md` #5).** `flutter_foreground_task` applies its own Kotlin Gradle Plugin —
   resolve (bump to a migrated release, or vendor with the upstream migration diff) to keep the
   Gradle build clean and future-proof. Didn't block reproducibility, but worth clearing.

## Getting into F-Droid

The build recipe is **submitted** ([MR !43625](https://gitlab.com/fdroid/fdroiddata/-/merge_requests/43625))
and mirrored byte-for-byte at [`../fdroid/app.nightdrop.yml`](../fdroid/app.nightdrop.yml); see
[`../fdroid/README.md`](../fdroid/README.md) for its shape and how to validate it locally. It
extracts the Flutter version from `app/.fvmrc`, provisions Rust 1.96.0 + the Android targets,
patches cargokit's hardcoded `stable`, builds at a fixed path with the repro
`CARGO_ENCODED_RUSTFLAGS`, and pins the signing key. `binary:` is declared, so F-Droid rebuilds
and verifies against the developer-signed APK. The **store listing** (title, summary, full description, and the
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
