## Pre-publish (open)

- [ ] **External security audit** — before a wide public launch, get an independent audit of the
      integration (pairing/PAKE, relay, at-rest storage, Dart↔Rust FFI). The primitives are audited
      libraries, but the whole has not been reviewed. Publish the audit + fixes when done. Tracked
      in SECURITY.md §Audit status, ARCHITECTURE.md §8, README, and website/limits.html.

## Open — needs a decision

3. **Show the peer's safety-number verification.** Today `set_verified` is **local only** — marking
   a contact verified doesn't tell them. A nice UX add: send the peer an authenticated "I've
   verified our safety number" signal so both sides can see mutual verification (Signal-style).
   Security nuance to preserve: keep it **informational, never auto-trust** — each party must still
   confirm the number themselves, or a compromised device could forge "verified" on the other's
   screen. Optional feature; decide whether you want it for v1.

## Fixed

1. ~~Backup import failing on mobile (>120s)~~ — same root cause as #2 (two arti instances
   contending for the Tor state dir). Restore on the phone now completes in <30s.
2. ~~Backup import asking to verify the password~~ — never a password problem; a second core was
   built over the still-locked Tor state dir. `NightdropCore::shutdown()` releases it synchronously,
   and restore errors are no longer misreported as password failures. See `ARCHITECTURE.md` §6.
4. ~~queue.json keeps the data after a client pulls it~~ — **not a bug.** `Take`/`Fetch` remove the
   mailbox (`drain_handle`) and mark the store dirty, so the flusher rewrites `queue.json` without
   it (within ~500ms via the rate-limited flush). `Peek` is intentionally non-consuming (a
   content-free count) — the likely source of the observation. Reopen only with a concrete repro.
5. ~~Self-contained single-file download~~ — **done.** `scripts/build-appimage.sh` produces one
   `Night_Drop-x86_64.AppImage` (Rust core bundled, no external lib/data folders), signed and
   attached to the v0.1.0 release; the Android APK is the single-file for that platform.
8. ~~Know how many people downloaded the app / visited the source~~ — **done.** Downloads go through
   GitHub Releases (per-file download counts), plus `~/.local/bin/nightdrop-stats` (GitHub traffic +
   downloads) and a weekly `nightdrop-traffic.timer` that snapshots history locally. All private to
   the maintainer, nothing visitor-facing. See the private stats tooling under `~/.local/`.

### 6/7. Pairing / "secret" message-request failures — RESOLVED (2026-07-20)

Root cause: **a VPN in front of Tor** (do not run a VPN over Tor) plus a **stale relay**. With the
VPN off and the relay restarted, the reachability probe passes (`RELAY REACHABLE`, `POST OK`) and
pairing + bidirectional messaging work device-to-device. Fixes shipped along the way, good
regardless: the destructive-take strand (joiner re-posts its opener, `RENDEZVOUS_REPOST`), raised
arti HS connect attempts (6→32), the 45s opener-post retry (`POST_ESTABLISH_TIMEOUT`), the
double-pairing session fix, and self-healing entry-guard reset on both the relay and the app.

Full diagnostic history (kept for reference):
- Diagnostics (`--diag` → `NIGHTDROP_DIAG=1`) surface protocol outcomes only — no keys/codes/onions.
  Read them via `adb logcat -s nd-diag` (phone) or the desktop's stderr (`[nd-diag] …`).
- `join: opener posted to 0/1 relays` was the tell — the PC couldn't reach the relay. The
  reachability probe (`relay_is_reachable_over_tor` in `core/tests/tor_smoke.rs`, with a DuckDuckGo
  control) isolated it to the host's Tor path, then to the VPN.
- The app's Tor state was a red herring — a *fresh* client failed at the same moment. Lesson: run
  the fresh control before blaming persisted state.
- Relay reliability: self-healing watchdog (restarts when its onion goes dark), weekly
  `RuntimeMaxSec` backstop, escalating guard reset, 6 intro points (device onions 4). See §6.
