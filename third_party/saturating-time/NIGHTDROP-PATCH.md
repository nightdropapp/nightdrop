# Patched copy of saturating-time 0.5.0

> **Unused since arti 0.48 (0.2.2, 2026-11-14).** arti 0.48 requires saturating-time 0.6.0, so this
> 0.5.0 patch no longer applies (cargo warns "patch ... was not used in the crate graph"). 0.6.0
> contains the fix itself: `find_limit` treats a step that makes no progress like `None`
> (`Some(st) if st != res`), and `saturating_add` uses `unwrap_or_else(max_value)`. Checked by
> reading the 0.6.0 source and on Windows 11 (the VM): `SystemTime::max_value()` and a saturating
> add past the end return in 6 µs with 0.6.0, while the same program with 0.5.0 hung until killed
> after 20 s. Still to do before removing this directory and its `[patch.crates-io]` entry: a
> Windows bridge test of 0.2.2 (the condition below).

Upstream: <https://crates.io/crates/saturating-time> (now maintained inside arti), MIT OR Apache-2.0.
Used through `[patch.crates-io]` in the workspace `Cargo.toml`.

**Why:** on Windows, `find_limit` loops forever. `SystemTime` there counts 100ns ticks, so a step
under 100ns rounds to zero and returns `Some` unchanged; the search never reaches the 1ns `None`
that ends it. arti calls `max_value()` while parsing bridge descriptors, so Tor over bridges never
bootstrapped on Windows: one core at 100%, logs frozen (found 2026-09-30 in a Windows 11 VM, stack
taken with cdb). Upstream: arti#2678, arti#2726. **Still unfixed in 0.5.0** (2026-10-01, the version arti 0.47.0's
tor-netdoc requires): this note used to say 0.5.0 carried the fix, the patch was dropped on that
basis for the arti 0.47 upgrade, and the first Windows bridge test of the 0.1.28 branch
(2026-10-05) hung exactly as before — one core busy, arti's files frozen. Checked the 0.5.0 source:
`find_limit` still has no progress check and `saturating_add` still computes the limit eagerly.
Re-applied to 0.5.0 the same day (one hunk with fuzz 1, reviewed by hand).

**What changed** (the patch another reporter posted on arti#2726, applied here; we did not send one
upstream):

- `internal.rs`: `SaturatingTime` requires `PartialEq`, and `find_limit` returns when a step makes
  no progress. Plus a test with a simulated 100ns clock that did not terminate before.
- `lib.rs`: `unwrap_or(max_value())` became `unwrap_or_else(max_value)` (and `min_value`), so an
  ordinary saturating add no longer computes the limit eagerly.

No effect on Linux or Android, where `SystemTime` has 1ns resolution and the new check never fires.

**Remove this directory and the `[patch.crates-io]` entry** only after reading the new
saturating-time's `find_limit` and seeing the fix there, and after a Windows bridge test passes
without it. A release note or issue comment saying "fixed" is not enough: that is how it was
dropped once already.
