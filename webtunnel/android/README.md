# Cross-compiling `webtunnel-client` (with BoringSSL) to Android

The `chrome-proto` feature pulls BoringSSL via `boring-sys`, which builds the whole BoringSSL
CMake project — including the test-only `third_party/benchmark`, whose regex-backend detection
runs a *target* binary and so cannot work when cross-compiling (`Failed to determine the source
files for the regular expression backend`). BoringSSL gates that tree behind `BUILD_TESTING`,
so `boringssl-toolchain.cmake` forces it off and then includes the NDK's own toolchain. It is
selected by exporting `CMAKE_TOOLCHAIN_FILE`, which makes `boring-sys` defer to it
(`boring-sys` build.rs returns early when a toolchain file is provided).

Proven on 2026-09-20 with **NDK 28.2.13676358** for all three F-Droid ABIs (arm64-v8a,
armeabi-v7a, x86_64); the produced BoringSSL objects are genuine ARM/ARM64/x86-64 ELF.

## Recipe (one ABI shown; repeat per ABI)

```sh
export ANDROID_NDK_ROOT=$HOME/android-sdk/ndk/28.2.13676358
export ANDROID_NDK_HOME=$ANDROID_NDK_ROOT
export PATH=$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH
export CMAKE_TOOLCHAIN_FILE=$(pwd)/webtunnel/android/boringssl-toolchain.cmake

# arm64-v8a
export ND_ANDROID_ABI=arm64-v8a
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=aarch64-linux-android24-clang
cargo build -p webtunnel-client --features chrome-proto --target aarch64-linux-android
```

Per-ABI settings:

| ABI | rust target | `ND_ANDROID_ABI` | linker / `CC_*` clang |
|-----|-------------|------------------|-----------------------|
| arm64-v8a | `aarch64-linux-android` | `arm64-v8a` | `aarch64-linux-android24-clang` |
| armeabi-v7a | `armv7-linux-androideabi` | `armeabi-v7a` | `armv7a-linux-androideabi24-clang` |
| x86_64 | `x86_64-linux-android` | `x86_64` | `x86_64-linux-android24-clang` |

**armv7 needs the compiler set explicitly** — `ring`'s `cc-rs` guesses
`arm-linux-androideabi-clang`, which the NDK does not ship; export
`CC_armv7_linux_androideabi=armv7a-linux-androideabi24-clang` (and `CXX_*=…-clang++`,
`AR_armv7_linux_androideabi=llvm-ar`).

`android-24` matches the app's `minSdk`. `BUILD_TESTING OFF` only skips BoringSSL's tests and
benchmarks; libcrypto/libssl build normally, so the result is unaffected and deterministic.

When the app's Android build (cargokit/flutter_rust_bridge) is set to enable the `webtunnel`
feature, it must export `CMAKE_TOOLCHAIN_FILE` (this file) and the per-ABI `CC_*` for each of
its ABI builds — this is the remaining wiring, and the same env must be reproducible for
F-Droid.
