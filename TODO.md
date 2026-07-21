## Pre-publish

- [ ] **External security audit** — before a wide public launch, get an independent audit of the
      integration (pairing/PAKE, relay, at-rest storage, Dart↔Rust FFI). The primitives are audited
      libraries, but the whole has not been reviewed. Publish the audit + fixes when done. Tracked
      in SECURITY.md §Audit status, ARCHITECTURE.md §8, README, and website/limits.html.

## Fixed (2026-07-17) — verify on your next pass

1. ~~Importing backup has been failing on mobile because it exceeded 120s~~
   **Same root cause as #2**, not a timeout problem. Two arti instances were contending for the
   Tor state dir; on mobile that blew the 120s `BOOTSTRAP_TIMEOUT` before the bootstrap could
   reach the onion service, which is where desktop reported the lock outright. Verified: restore
   on the phone now completes in <30s, and the address publishes faster too. `BOOTSTRAP_TIMEOUT`
   was left alone — the evidence says 120s is fine for a single instance.
2. ~~Importing server and file backup failing on desktop, asking to verify the password~~
   Never a password problem. arti holds an **exclusive lock** on its state dir, and any path that
   replaced a running core (restore, `retryStart`, create-identity-after-a-failed-start) built a
   second instance while the first was still alive — the Dart side only cleared its reference,
   which frees nothing. Now `NightdropCore::shutdown()` releases it synchronously, and every
   construction site closes the old core first. The "corrupted file" symptom was the same error
   misreported: the UI blamed the password for *every* restore failure, and now only says that
   when the core actually reports `decrypt failed`. See `ARCHITECTURE.md` §6.

## Open

3. Since the verification code is the same for both person, once it's confirmed on one device, it
   should confirm on the other
4. queue.json keeps the data instead of deleting it once the relay information is pulled by the
   client
   — Not reproduced yet: `drain_handle` and `save_store` both look correct, so suspect the flush
   trigger. Needs a repro.
5. Have a self contained bundle that can be downloaded and run on any system without needing the
   lib and data folder to put on the website for download
   — Note: `website/applications/linux/night_drop` is a stale pre-rename binary; this task should
   replace it outright.
6. The phone never received the chat request when the PC wrote the secret
7. I have had some issues using the secret to send message requests

   #6/#7 — **partially addressed.** Ruled out a leg mismatch (joiner posts `rdv:{slot}:j` / polls
   `:i`; inviter drains `:j` / posts `:i` — consistent). Fixed one real bug: the inviter's read is
   destructive, so an opener consumed without an answer ever being posted stranded the joiner for
   its whole timeout. The joiner now re-posts while waiting (`RENDEZVOUS_REPOST`), so pairing
   self-heals. Two candidates remain and need a repro to tell apart:
   - **RAM-only invites** (`node.rs`, `pending_invites`): the inviter answers only while its
     poller runs. If the phone's screen slept while the code was typed on the PC, Android freezes
     the process and the opener is never answered. Silent — nothing is logged.
   - **Onion not yet published**: the SPAKE2 handshake isn't the chat request. After pairing, the
     PC dials the phone's `.onion` to send Hello — *that's* the request. If the descriptor isn't
     up (1–3 min after launch, or the app was backgrounded), no request ever appears.

   **To diagnose:** both devices are now installed with opt-in diagnostics (`--diag` on the
   install scripts → `NIGHTDROP_DIAG=1`). Reproduce the pairing exactly as before, with:
   - phone: `adb logcat -s nd-diag`
   - desktop: run `~/.local/lib/nightdrop/night_drop` from a terminal (lines are `[nd-diag] …`)

   What to look for — the sequence names the culprit:
   - no `invite: took N opener(s)` on the phone → the opener never reached a shared relay
   - `invite: answered … posted to 0/N relays` → ANSWER LOST (the destructive-take case)
   - `join: TIMED OUT` on the PC → the phone never answered at all
   - `tor: … restricted discovery ON` + `deliver: direct dial failed` + no `pair: inbound Hello`
     → restricted discovery (#22) is keeping a new peer out and the relay fallback isn't landing

   These lines carry protocol outcomes only — no keys, codes, onion addresses, or names (see
   `ARCHITECTURE.md` §6). The identity-linked `devlog!` lines remain compiled out of release.

   **Diagnosed 2026-07-20 — it was the RELAY, not the app.** The diagnostics showed
   `join: opener posted to 0/1 relays`: the PC couldn't reach the relay to post its opener. A
   reachability probe (`relay_is_reachable_over_tor` in `core/tests/tor_smoke.rs`, now with a
   DuckDuckGo control) proved:
   - The relay had been up **3 days** and its onion descriptor had gone stale — clients got
     "Onion Service not found." Restarting it (systemd) forces a fresh publish; confirmed
     `descriptor uploaded successfully to 8/8 HSDirs` in the relay's own `RUST_LOG` output. Its
     persisted guard set was a **month** old (Jun 18–Jul 19); I reset it (kept the keystore, so
     the onion address is unchanged).
   - Independently, this host's Tor client HS reachability is **intermittent** right now — the
     DDG control succeeded earlier in the day and failed later, and both the relay's uploads and
     the client's fetches log `could not build circuit to HsDir` with many retries. That
     intermittency is the "sometimes it works" in #7 and is environmental (host/network Tor
     conditions), not app code.

   The app's Tor state was a **red herring** — probing with a copy of it failed only because a
   *fresh* client failed at the same moment (verified with a fresh control). Lesson: always run
   the fresh control before blaming persisted state.

   **RESOLVED 2026-07-20 — root cause was a VPN on the PC + a stale relay.** With the PC on a
   VPN, every Tor circuit was funnelled through the tunnel and throttled: circuits built only
   after 20–72 retries, and hidden-service descriptor fetch/rendezvous failed outright. Turning
   the VPN off measurably fixed circuit-building (a fresh same-host onion-to-onion test then
   passed in 46s). The relay also needed one fresh restart: it had established its intro points
   while the VPN was degrading the network, so its published descriptor pointed at circuits
   clients couldn't reach. After VPN-off + relay restart, the reachability probe passes
   consistently (`RELAY REACHABLE`, `POST OK`). **Guidance: do not run a VPN in front of Tor.**

   Two resilience fixes shipped from this, good regardless of the VPN:
   - `HS_CONNECT_ATTEMPTS` (core/src/transport/tor.rs): raised arti's `hs_desc_fetch_attempts` /
     `hs_intro_rend_attempts` from the default 6 to 32, so a slow-but-working path isn't reported
     as "unreachable."
   - `run_join_handshake` now retries posting the opener for 45s (`POST_ESTABLISH_TIMEOUT`)
     instead of bailing after one failed round.

   Follow-up DONE (2026-07-20): the relay no longer needs manual restarts. It runs a self-healing
   watchdog (exits when its onion stays unreachable, so systemd restarts it fresh), a weekly
   `RuntimeMaxSec` backstop, and 6 introduction points (device onions run 4) for resilience. The
   temporary `RUST_LOG` drop-in has been removed. See `ARCHITECTURE.md` §6.
8. Ways to know how many people downloaded the application from the website
