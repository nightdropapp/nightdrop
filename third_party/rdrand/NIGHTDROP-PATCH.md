# Patched copy of rdrand 0.9.0

Upstream: <https://crates.io/crates/rdrand> (<https://github.com/nagisa/rust_rdrand>), ISC.
Used through `[patch.crates-io]` in the workspace `Cargo.toml`. arti's `tor-llcrypto` depends on
it: `CautiousRng` mixes the CPU's hardware RNG into every key it generates.

**Why:** on aarch64 Linux and Android, `has_rand()` decided whether RNDR exists by executing
`mrs ID_AA64ISAR0_EL1` from user space. Linux emulates that read only since kernel 4.11; older
kernels deliver SIGILL and the whole app dies with no message. The first key `CautiousRng` makes
(a new onion identity at onboarding, or introduction-point keys on every start) therefore killed
Night Drop on phones still running a 4.9 kernel. Found 2026-11-10 from issue #22's tombstone
(LineageOS 18.1 on a Xiaomi Mi 9 Lite: `SIGILL ILL_ILLOPC`, `*pc=0xd5380608`, in
`CautiousRng::try_fill_bytes` called from `IptManager`'s key generation).

**What changed** (`src/lib.rs`, `has_rand()` only): on Linux and Android it now reads
`getauxval(AT_HWCAP2) & HWCAP2_RNG`, the kernel's own statement that RNDR is usable (5.6+). An
older kernel never sets the bit, so RNDR is reported absent and `CautiousRng` uses its other
sources (the system RNG, which it requires, and the rest). FreeBSD, NetBSD, UEFI and bare metal
keep the original register read. Nothing else in the crate changed, and nothing on x86.

Check after any arti or rdrand update: the arm64 library must contain no `mrs …, ID_AA64…`
instruction (`llvm-objdump -d libnightdrop.so | grep 'mrs.*ID_AA64'` prints nothing).

**Remove this directory and the `[patch.crates-io]` entry** only after reading the new rdrand's
aarch64 detection and seeing that it no longer executes the register read on Linux/Android, and
after that check passes without the patch.
