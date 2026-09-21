#!/usr/bin/env bash
# Run the JA4 fingerprint test ON an Android device, against the real on-device BoringSSL.
#
# webtunnel/tests/fingerprint.rs asserts that connect()'s ClientHello still carries the Chrome
# JA4 we validated by capture. Run by `cargo test` it proves that for the HOST build — x86-64
# Linux — which is not what ships. The fingerprint is the whole defence against a censor that
# blocks by how traffic looks, so "it matches on my laptop" is not the claim that matters.
#
# This cross-compiles that same test to arm64 and runs it on the phone. No network: the test
# connects to a listener it binds itself, so it works on any connected device.
#
#   ./webtunnel/android/run-fingerprint-on-device.sh [adb-serial]
#
# Needs: the NDK (see README), a connected device, and `rustup target add aarch64-linux-android`.
set -euo pipefail

SERIAL=${1:-}
ADB=(adb)
[ -n "$SERIAL" ] && ADB=(adb -s "$SERIAL")

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$REPO_ROOT"

: "${ANDROID_NDK_ROOT:=$HOME/android-sdk/ndk/28.2.13676358}"
[ -d "$ANDROID_NDK_ROOT" ] || { echo "no NDK at $ANDROID_NDK_ROOT (set ANDROID_NDK_ROOT)" >&2; exit 1; }
export ANDROID_NDK_ROOT
export ANDROID_NDK_HOME=$ANDROID_NDK_ROOT
export PATH="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH"
export CMAKE_TOOLCHAIN_FILE="$REPO_ROOT/webtunnel/android/boringssl-toolchain.cmake"
export ND_ANDROID_ABI=arm64-v8a
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=aarch64-linux-android24-clang
# Without these the test EXECUTABLE links the shared libc++ and dies on the device with
# "library libc++_shared.so not found" — the same failure the app hit. The cdylib gets them
# from cargokit's builder.dart; a bare `cargo test` has to set them itself.
export BORING_BSSL_RUST_CPPLIB=c++_static
export RUSTFLAGS="${RUSTFLAGS:-} -Clink-arg=-lc++abi"

echo "==> building fingerprint test for aarch64-linux-android"
BIN=$(cargo test -p webtunnel-client --features chrome-proto \
        --target aarch64-linux-android --test fingerprint --no-run --message-format=json 2>/dev/null \
      | python3 -c '
import json,sys
for line in sys.stdin:
    try: m=json.loads(line)
    except ValueError: continue
    if m.get("reason")=="compiler-artifact" and m.get("executable") and m["target"]["name"]=="fingerprint":
        print(m["executable"])
' | tail -1)
[ -n "$BIN" ] && [ -x "$BIN" ] || { echo "could not locate the built test binary" >&2; exit 1; }

echo "==> pushing $(basename "$BIN")"
"${ADB[@]}" push "$BIN" /data/local/tmp/nd-fingerprint >/dev/null
"${ADB[@]}" shell chmod 755 /data/local/tmp/nd-fingerprint

echo "==> running on device"
out=$("${ADB[@]}" shell /data/local/tmp/nd-fingerprint --nocapture 2>&1) || true
echo "$out"
"${ADB[@]}" shell rm -f /data/local/tmp/nd-fingerprint || true

grep -q "test result: ok" <<<"$out" || { echo "FAILED on device" >&2; exit 1; }
echo "==> on-device JA4 matches the validated Chrome profile"
