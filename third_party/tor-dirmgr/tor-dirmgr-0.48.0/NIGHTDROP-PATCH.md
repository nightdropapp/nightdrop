# Night Drop patch: allow patched rusqlite

Arti 0.48.0 pins `rusqlite` below 0.41.0. That release of `rusqlite` still selects
`libsqlite3-sys` 0.38.x, which bundles SQLite affected by RUSTSEC-2026-0459. The
development change for Arti widens this upper bound to `<0.42.0`; this local fork makes
that narrow compatibility change so the workspace can select `rusqlite` 0.41 and
`libsqlite3-sys` 0.39.x while retaining the bundled SQLite feature.

Only the generated dependency bound in `Cargo.toml` differs from the published crate.
Remove this fork and the root `[patch.crates-io]` entry once a released Arti accepts the
patched `rusqlite` line. Recheck with `cargo tree -i libsqlite3-sys` and `cargo audit`.
