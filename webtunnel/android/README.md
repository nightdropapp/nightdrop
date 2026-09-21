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

**libc++ is linked statically** (`ANDROID_STL c++_static` in the toolchain). BoringSSL has C++,
and the NDK's default shared STL would make `libnightdrop.so` depend on `libc++_shared.so`, which
cargokit does not bundle — so the app `dlopen`s the lib and crashes at startup with *"library
libc++_shared.so not found"*. Static libc++ makes the one native lib self-contained. (Found by the
on-device test 2026-09-20; a cross-compile alone does not catch a load-time dependency.)

`android-24` matches the app's `minSdk`. `BUILD_TESTING OFF` only skips BoringSSL's tests and
benchmarks; libcrypto/libssl build normally, so the result is unaffected and deterministic.

When the app's Android build (cargokit/flutter_rust_bridge) is set to enable the `webtunnel`
feature, it must export `CMAKE_TOOLCHAIN_FILE` (this file) and the per-ABI `CC_*` for each of
its ABI builds — this is the remaining wiring, and the same env must be reproducible for
F-Droid.

## Building the app with WebTunnel (opt-in)

cargokit (`app/rust_builder`) builds the core with `--features tor` from `core/cargokit.yaml`.
A small Night Drop customization in `cargokit/build_tool/lib/src/builder.dart` additionally
enables `--features webtunnel` and exports this toolchain + `ND_ANDROID_ABI` **only when
`NIGHTDROP_WEBTUNNEL=1`** — so default and F-Droid builds are unaffected. To build a
WebTunnel-capable APK:

```sh
export NIGHTDROP_WEBTUNNEL=1
cd app && fvm flutter build apk --debug   # or install-android-app.sh for a test build
```

This is off by default; do not enable it in a release/F-Droid build until the F-Droid
reproducibility of the added BoringSSL is confirmed.

## Reproducible builds (F-Droid)

F-Droid's recipe has `binary:`, so it rebuilds the app and compares byte for byte against the APK
we publish. A mismatch is reported in their repo, under our name, as *not reproducible*. Adding a
C dependency to a previously pure-Rust native library is exactly the kind of change that can break
that, so this is what was checked (2026-09-20, arm64, release profile).

**The toolchain is already there.** The recipe's `sudo:` step installs `cmake` along with
`build-essential clang pkg-config rustup`, and pins `ndk: r28c` — which is 28.2.13676358, the
version this cross-compile was proven with. Nothing new is needed to build BoringSSL in their
container.

**The build is deterministic.** `libcrypto.a` and `libssl.a` come out byte-identical across a
`cargo clean -p boring-sys` and full rebuild.

**But it embedded the build path, and that needed fixing.** The recipe's
`--remap-path-prefix=$CARGO_HOME=/cargo` is a *rustc* flag and does not reach a C compiler.
BoringSSL's error macros embed `__FILE__`, which lands in `.rodata` and **survives stripping**:
~190 absolute paths in the shipped `libnightdrop.so`, naming both the build directory and cargo's
`boring-sys-<metadata-hash>` directory.

Those would usually still match, because F-Droid builds at a fixed `/build/nightdrop` and our
release APKs come from `fdroid/build-locally.sh` at that same path. "Usually" is the wrong
standard for a byte comparison, and it makes reproducibility depend on build *location* — which
it never did while the native code was pure Rust. `boringssl-toolchain.cmake` now passes
`-ffile-prefix-map=$OUT_DIR=/nd-boringssl` for C, C++ and ASM, after including the NDK toolchain
(which resets the `*_FLAGS_INIT` variables, so setting them before it has no effect). Measured on
the stripped release archive: **190 absolute paths → 0**, all remapped, and still deterministic.

What remains in the linked `.so` is `$CARGO_HOME/registry` and the rustup toolchain, both of which
the recipe's existing rustc remapping handles in a real F-Droid build.

**Confirmed by a real `fdroid build`, twice (2026-09-21).** The arm64 release entry was built in
the buildserver container from this branch with `NIGHTDROP_WEBTUNNEL=1`, and then built again. The
two APKs are **byte-identical** (`sha256 9cb663ad96a0…`), and the library inside carries BoringSSL
(`X25519MLKEM768`), the WebTunnel Rust (`listener-secret`, `webtunnel/src/socks.rs`), a static
libc++ (`NEEDED` is only liblog/libdl/libm/libc) and **no build-location strings at all** — 144
`/nd-boringssl` paths, zero `/build/nightdrop`, zero `/home/vagrant`. So the remap does better
than making the paths match: it removes them.

### Building an unpushed branch in the container

`fdroid` clones from the recipe's `Repo:`, and the container does not bind-mount the working tree,
so testing a branch normally means pushing it. It does not have to. Put a bare clone inside the
buildserver's own home volume and point `Repo:` at it:

```sh
git clone --bare /path/to/repo ~/.cache/fdroid-local/nightdrop.git
podman run --rm -v fdroid-vagrant:/home/vagrant:z -v ~/.cache/fdroid-local:/mnt/out:z   registry.gitlab.com/fdroid/fdroidserver:buildserver-trixie   bash -c 'cp -a /mnt/out/nightdrop.git /home/vagrant/ && chown -R vagrant:vagrant /home/vagrant/nightdrop.git'
# then in a COPY of the recipe: Repo: /home/vagrant/nightdrop.git, commit: <sha>,
# and run with SKIP_BINARY=1 VERCODE=<one abi>   (the feature is on by default now)
```

It must live in the volume, not in `/mnt/out`: under rootless podman the host user maps to
container root, so `vagrant` sees a root-owned tree and git refuses it with *"detected dubious
ownership"*. Note also that `~/.cache/fdroid-local/apk/` accumulates APKs from previous runs —
check the timestamp before concluding anything about "the" APK.


## The feature is on by default (2026-09-21)

`NIGHTDROP_WEBTUNNEL=1` is no longer needed: cargokit builds the core with `--features webtunnel`
unless `NIGHTDROP_WEBTUNNEL=0` says otherwise. Measured cost on arm64 release:
`libnightdrop.so` 17.6 MB → 19.8 MB, APK 45.4 MB → 47.7 MB, **+2.2 MB (5.1 %)**.

Two things that have to move together, and the reason the env var still gates both: the
`--features webtunnel` flag and the Android BoringSSL toolchain environment. Setting the feature
without the env builds BoringSSL against the NDK's defaults, which links the *shared* libc++ and
produces an APK that dies at `dlopen`. That was the original bug; keeping them on one condition is
what stops it coming back.

**Untried: iOS and macOS.** BoringSSL has only ever been cross-compiled for Android and built
natively for Linux. Whoever builds for Apple platforms first should expect to need a toolchain file
there as Android did — and can use `NIGHTDROP_WEBTUNNEL=0` to get a working build meanwhile.
