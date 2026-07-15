# Fuzzing the security core

`cargo-fuzz` (libFuzzer) harness over the parsers that eat **attacker-controlled input**.
These are the surfaces where a panic is a remote DoS and a logic bug can be worse, so they
are the highest-value fuzz targets in the project.

## Targets

| Target | Function | Why |
|---|---|---|
| `wire_decode` | `wire::decode(&[u8])` | Every frame from a peer/relay is parsed here (length prefix + versioned JSON envelope + padding). The primary network input parser. |
| `relay_handle_line` | `RelayCore::handle_line(&str)` | The relay's request-line handler — its main untrusted-input surface. Malformed/unauthenticated input must not panic or exhaust resources (in scope per `SECURITY.md`). |
| `directory_from_wire` | `SignedDirectory::from_wire` + `verify` | A relay serves the signed directory; parsing happens **before** signature verification, so a malicious relay can shape the bytes. |

## Requirements

Not preinstalled in this repo — set up once:

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
```

## Run

From `core/`:

```sh
cargo +nightly fuzz run wire_decode
cargo +nightly fuzz run relay_handle_line
cargo +nightly fuzz run directory_from_wire
```

Each runs until it finds a crash (writing the input to `fuzz/artifacts/<target>/`) or you
stop it. Reproduce a crash with:

```sh
cargo +nightly fuzz run wire_decode fuzz/artifacts/wire_decode/crash-<hash>
```

## Seed corpus

Curated, valid starting inputs live in **`fuzz/seeds/<target>/`** (committed) — one small
file per interesting shape (each relay op, a valid vs. version-mismatched wire frame, a
base64 directory blob). They give the fuzzer a coverage head-start instead of rediscovering
the input grammar from scratch. Pass them alongside the working corpus:

```sh
cargo +nightly fuzz run wire_decode fuzz/corpus/wire_decode fuzz/seeds/wire_decode
```

`fuzz/corpus/` (the accumulating, mutated corpus) and `fuzz/artifacts/` (crashes) are
git-ignored; only `fuzz/seeds/` is committed.

## Status

Last exercised 2026-07-14 on nightly `1.99.0`, cargo-fuzz `0.13.2`: each target ran a 60s
budget from an empty corpus with **no crashes** — `wire_decode` ~7.4M execs,
`relay_handle_line` ~5.1M, `directory_from_wire` ~11.4M. This is a smoke run; the ongoing
campaign runs in CI (below).

## CI

`.github/workflows/fuzz.yml`:

- **Build gate** (every PR/push touching `core/`): `cargo +nightly fuzz build` plus a
  seed replay (`-runs=0`) — verifies the targets still compile against the core and the
  seeds are valid. No mutation, so it's fast.
- **Campaign** (weekly + `workflow_dispatch`): a coverage-guided run of each target with the
  corpus cached between runs (via `actions/cache`) so it accumulates over time. A crash
  fails the job and uploads the reproducer as an artifact.

When a bug is found, commit a minimized reproducer as a regression test in the core's normal
test suite (not just as a corpus entry) so it's covered on every `cargo test`.
