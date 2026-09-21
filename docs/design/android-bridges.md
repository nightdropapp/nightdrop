# Design draft — Bridges and pluggable transports on Android

**Status:** 🟢 in-app **bridge** configuration implemented (2026-08-01), not yet exercised on a
device. PT binaries are a separate, larger piece (§3) and are **not** included. 🟢 An in-process
**WebTunnel** client (§5) is built in `webtunnel/` instead: steps 1–3 done, the BoringSSL
Android cross-compile proven for all ABIs, and a real `NIGHTDROP_WEBTUNNEL=1` APK built with
BoringSSL + WebTunnel linked in (2026-09-20). Remaining: on-device "block Tor → fall back" test
and F-Droid reproducibility. Off by default, so nothing ships yet.
**Relates to:** `docs/bridges.md` (the file formats and where to get bridge lines),
`ARCHITECTURE.md` §6 (censorship resistance), `core/src/transport/tor.rs`
(`apply_bridges` / `apply_transports`).

## 1. The gap

The core already supports both bridges and pluggable transports. At bootstrap it reads
`bridges.txt` and `transports.txt` from the Tor state directory, and arti launches a PT binary on
demand when a bridge line names one.

On **desktop** the user drops those files in by hand (`scripts/setup-pluggable-transports.sh`
generates the second). On **Android** the state directory is app-private and not writable without
root, so neither file can ever be placed there — and Android is the platform most likely to be
behind a national firewall.

Two distinct missing pieces, worth separating because they cost wildly different amounts:

1. **No way to write the files.** Fixed here: an in-app editor that validates bridge lines and
   writes `bridges.txt` into the app's own state directory.
2. **No PT binaries on the device.** Not fixed here. See §3.

## 2. In-app bridges

A settings screen takes pasted bridge lines — the format the Tor Project hands out, so a user can
paste what they were given without editing it — validates each with the same
`BridgeConfigBuilder` parse the bootstrap uses, and writes the accepted ones.

* **Per-line errors.** A rejected line is shown back with its reason rather than silently dropped.
  Someone copying bridges off a censored connection needs to know *which* line is wrong.
* **Applied on restart.** Bridges are read when the Tor client is built, so saving offers to
  restart the core. Nothing is silently deferred.
* **Never sent anywhere.** Bridge lines are local config, and which bridges you use is exactly the
  thing a censor wants; they stay in the app's private state directory.

## 3. Why PT binaries are not in this change

`obfs4`/`snowflake` need a **client executable**. On desktop it comes from the distro. On Android
there is no `PATH`: the binary would have to ship inside the APK, extracted to
`nativeLibraryDir` and exec'd — the approach Orbot and Tor Browser use.

That is a real project, not an afternoon:

* **Building them.** lyrebird and snowflake-client are Go. Cross-compiling per ABI adds a Go
  toolchain to the build for a Rust + Flutter app.
* **Size.** Several MB per ABI, on APKs that are already 36–48 MB split.
* **F-Droid.** Prebuilt binaries are not acceptable; the recipe would have to build them from
  source, adding Go to a build whose reproducibility was hard-won and is under review right now
  (MR !43625). Landing this carelessly risks that.
* **Unverified assumption.** arti's PT manager spawns a child process. That works on Android for a
  binary in `nativeLibraryDir`, but nothing here has tested it.

So it needs its own design note, and probably its own release.

## 4. What in-app bridges do and don't buy

**Do:** get past a plain block of the public relay list — an ISP or network that blackholes known
Tor relay IPs. Vanilla bridges are unlisted, so they survive that.

**Don't:** get past deep-packet inspection. Where a censor fingerprints the Tor protocol itself and
blocks bridge IPs as it finds them — Iran, China, Russia in practice — vanilla bridges fail, and
that is precisely where obfs4/Snowflake is needed. §3 is the harder half, and it is the half the
worst-censored users need.

The UI must not imply otherwise. A user in Iran who pastes vanilla bridges, sees "3 bridges saved",
and concludes they are safe has been misled by us.

## 5. WebTunnel, in-process, in Rust (2026-09-19)

§3's cost comes from the transport being a separate Go program. arti can also use an
*unmanaged* transport — one already listening on a local port (`proxy_addr` in
`[[bridges.transports]]`, stable config since well before 0.43). So a transport written in Rust
can run inside the core, and there is nothing to bundle, exec, or build with Go.

**Why WebTunnel and not the others.** WebTunnel adds no cryptography of its own: TLS to a real
web server, an HTTP/1.1 `Upgrade: websocket` on a secret path, then raw Tor bytes, with Tor's
link encryption inside. It is small and testable. obfs4 is its own crypto protocol (Elligator2,
an ntor-style handshake); writing it ourselves would be new, unaudited crypto. Snowflake runs
over WebRTC — a project in itself.

**Why not an existing Rust crate.** Checked 2026-09-19. `jmwample/ptrs` (obfs4): the `obfs4`
crate is still `0.1.0-alpha.1` (2024-09), with nothing but dependency bumps since 2024-12, one
author, no audit. `ptrs-gesher` (a fork adding WebTunnel): created 2026-05, one maintainer, and
its WebTunnel TLS is plain rustls — see the fingerprint problem below.

**Step 1 — protocol (done).** `webtunnel/` (`webtunnel-client`, MIT OR Apache-2.0): bridge-option parsing with
lyrebird's semantics (`url`, `addr`, `servername`, `sni-imitation`, `cert-domain`, `cert`),
TLS with WebPKI verification or lyrebird's certificate-chain pin, the upgrade request
byte-identical to Go's, and a SOCKS5 front end speaking arti's pt-spec argument encoding.
Verified against the Tor Project's own server (`webtunnel/interop.sh` builds it from a pinned
commit) behind an nginx-like TLS front: 1–4 MiB echo round-trips, 16 concurrent tunnels, the
SOCKS path as arti drives it. Mutation-checked: reading the 101 in bulk, dropping the secret
check, or accepting any pin each make a test fail.

**Access control.** A loopback port on Android is reachable by every app on the device, and
the SOCKS username/password already carry the bridge options. So the listener requires a
random bridge argument, `listener-secret=…`, which the core appends to each bridge line it
gives arti; anything without it is refused before dialling. arti's in-process transport hook
(`AbstractPtMgr`) would avoid the port entirely but sits behind `experimental-api`, which can
change in any release — revisit if it stabilises.

**Step 2 — the TLS fingerprint (DONE 2026-09-20; boring is the `connect()` TLS layer behind
`chrome-proto`).** rustls's ClientHello is recognisable as rustls, and very little that browses
the web sends it, so a censor could block it for almost no collateral cost. That makes the transport
*correct but not stealthy* until this is solved, and **it must not ship to users before it is**
— a recognisable handshake does not just fail, it can mark the user as a circumventer.

*Why pure Rust cannot do it* (measured, not assumed). A spike compared stock rustls against a
real Chrome hello: even with ciphers/groups reordered, ALPN set, and cert compression enabled,
rustls was missing six of Chrome's ciphers (the legacy CBC/RSA-kx suites rustls *deliberately
refuses to implement*), GREASE (no API), the `X25519MLKEM768` keyshare (needs the `aws-lc-rs`
backend), and several extensions. Reaching a Chrome JA4 from rustls would mean reintroducing
crypto rustls exists to avoid — not a small fork. A from-scratch Rust uTLS is worse: the
ClientHello commits you to the whole handshake (key exchange for every group offered, the 1.3
key schedule, cert-compression *decompression*), i.e. reimplementing a TLS stack, which the
"no hand-rolled crypto" rule forbids. There is no maintained Rust uTLS; every browser-mimicking
Rust library wraps BoringSSL.

*What works.* Cloudflare's `boring` (BoringSSL — the library Chrome itself uses) reproduces
Chrome's hello **exactly**. Prototype `webtunnel/examples/boring_hello.rs` plus the self-test
`webtunnel/tests/fingerprint.rs` produce JA4 `t13d1515h1_8daaf6152771_f04195365787`, byte-equal
in cipher set, extension set, groups, sigalgs and ALPN to Chromium 152 opening a WebSocket,
stable across connections (GREASE varies, JA4 excludes it). The recipe: TLS 1.2–1.3,
`set_grease_enabled`, `set_permute_extensions`, Chrome's explicit cipher list,
`X25519MLKEM768:X25519:P-256:P-384`, ALPN `http/1.1`, OCSP + SCT + brotli cert-compression +
GREASE-ECH.

*The ALPN worry was unfounded.* An earlier note feared a Chrome copy would advertise `h2` and a
bridge's nginx would take it, breaking the Upgrade. But Chrome's **WebSocket** hello — which is
what we imitate — offers only `http/1.1`, exactly what WebTunnel needs. No deviation from
Chrome is required. (Firefox offers `h2` even for WebSockets, which is why we mimic Chrome.)

*Honest limits.* JA4 match is necessary, not sufficient — sophisticated censors also look at
keyshare sizes, ALPS and active probing. Chrome's fingerprint drifts (it intermittently sends
the new `trust_anchors` draft extension → a 16-extension variant; we match the common 15). Some
party owns keeping the profile current forever; the `fingerprint` self-test is the tripwire.

*Cost accepted (see the top-of-file status and the commit).* BoringSSL is C built with cmake —
the class of dependency the core avoided by choosing rustls for Android. It is isolated in
`webtunnel-client` behind the **optional** `chrome-proto` feature: without the flag it is absent
from the dependency graph, so the ordinary build, CI and pre-commit hook stay pure-Rust. The Tor
path keeps rustls/ring regardless. **Gate:** the Android/F-Droid cross-compile of BoringSSL must
be proven separately before this feature becomes load-bearing on a device (part of step 4).

*Done (2026-09-20).* `tls.rs` now has two back ends behind one `connect_tls`/`TlsStream`
interface, chosen by `chrome-proto`: the pure-Rust rustls path (default, unchanged) and a
BoringSSL path. Under `chrome-proto`, `connect()` itself emits the Chrome hello, and all three
certificate modes are reimplemented on boring: `cert=` via a custom verify callback over the
chain hash, and CA / `cert-domain` via `SslVerifyMode::PEER` against a bundled-Mozilla-roots
store (`webpki-root-certs`, deterministic and Android-safe) with the verify-host decoupled from
the SNI. The self-test drives the real `connect()`. Verified: the fingerprint test (JA4 equals
the captured Chrome), the interop suite under `--features chrome-proto` (pin accept, wrong-pin
and self-signed reject, round-trips), a real bridge over the boring CA path, the untouched rustls
suite, and a full end-to-end run — real arti bootstrapped Tor (~22 s, working circuit) through
the boring Chrome-fingerprinted client to a live bridge
(`cargo run --example webtunnel_bootstrap -p nightdrop --features tor -F webtunnel-client/chrome-proto`).

*Residual limits (unchanged):* JA4 match is necessary, not sufficient (keyshare sizes, ALPS,
active probing remain), and Chrome's profile drifts — the `fingerprint` test is the tripwire and
someone must refresh the profile when it fails.

**End-to-end viability proven on desktop (2026-09-19).** Before building step 3 into the app, a
harness (`core/examples/webtunnel_bootstrap.rs`, `--features tor`) ran the exact architecture
step 3 will use: a real `arti` client with the `webtunnel` transport configured *unmanaged*
(`TransportConfigBuilder::proxy_addr` → our `SocksServer`), pointed at two live bridges from
bridges.torproject.org. Both bootstrapped Tor in ~20–22 s **through the WebTunnel client** and
opened a working circuit (`HTTP/1.1 200 OK` from example.com over Tor). So the whole chain —
arti → unmanaged SOCKS → our TLS+upgrade → real bridge → Tor — works; step 3 is wiring, not
invention.

Two findings from that run:
- *The `addr` in a WebTunnel bridge line is a `2001:db8:` placeholder by design.* The distributor
  never publishes the real IP (that would just be blockable); the real endpoint is the `url=`
  host, which the client resolves. Our design handles this for free: arti passes the placeholder
  `addr` as the SOCKS CONNECT target and the SOCKS layer discards it, dialing `url=` instead. So
  **for WebTunnel, ignore `addr`; resolve `url`** — the client already does.
- *IPv6.* The distributor may hand out IPv6-only-looking lines, but the `url=` host has an A
  record, so resolution reaches it over IPv4. No IPv6 egress is required (this box has none).

**Step 3 — into the core (DONE 2026-09-20, behind the `webtunnel` feature).** At Tor startup
`core/src/transport/tor.rs` now creates the runtime first, `spawn_webtunnel_proxy` binds
webtunnel-client's SOCKS listener on `127.0.0.1:0` with a fresh per-run secret and spawns it for
the client's lifetime, `register_webtunnel_transport` adds the unmanaged `webtunnel` transport at
that address, and `apply_bridges` appends `listener-secret=<secret>` to each `webtunnel` bridge
line so arti forwards it and the listener authorises. Proven end-to-end: the harness, updated to
this exact secret-authorised wiring, bootstraps real Tor in ~22 s through it.

The feature is **off by default and implies `tor`**, and pulls `webtunnel-client/chrome-proto`
(BoringSSL) — so the shipped build is unchanged and cross-compiles as before. A build *without*
the feature can't run these bridges, so `check_bridge_line` rejects a `webtunnel` line in the
editor and `apply_bridges` skips one at load (rather than failing the whole config build and
taking Tor down). Unit-tested both ways.

Two follow-ups this leaves: it adds a clearnet DNS lookup of the bridge's `url=` host (the same
name the SNI already reveals), which `DEPENDENCIES.md`'s "the only egress is Tor" statement must
describe; and the Dart bridge editor already accepts webtunnel lines via `check_bridge_line`, but
its help text should point users at where to get them.

**Step 4 — Android and F-Droid.**

*BoringSSL Android cross-compile — DONE / gate cleared (2026-09-20).* `webtunnel-client` with
`chrome-proto` (BoringSSL) cross-compiles for **all three F-Droid ABIs** — arm64-v8a,
armeabi-v7a, x86_64 — with NDK 28.2; the produced BoringSSL objects are genuine
ARM/ARM64/x86-64 ELF. This was the one unproven blocker for shipping WebTunnel. The snag was
`boring-sys` building BoringSSL's test-only `third_party/benchmark`, whose regex-backend
detection runs a target binary (impossible when cross-compiling); the fix is
`webtunnel/android/boringssl-toolchain.cmake`, a wrapper that forces `BUILD_TESTING OFF` and
includes the NDK toolchain, selected via `CMAKE_TOOLCHAIN_FILE` (which `boring-sys` defers to).
Full recipe + per-ABI settings (armv7 needs `CC_*` set explicitly, since `ring`'s cc-rs guesses
a clang name the NDK doesn't ship): `webtunnel/android/README.md`.

*cargokit wiring — DONE + validated by a real APK (2026-09-20).* cargokit's `builder.dart`
adds `--features webtunnel` and exports the toolchain + per-ABI `ND_ANDROID_ABI` only when
`NIGHTDROP_WEBTUNNEL=1` (off by default). A `NIGHTDROP_WEBTUNNEL=1 fvm flutter build apk --debug`
built a WebTunnel-capable APK: `boring-sys` produced `libcrypto.a`/`libssl.a` for the Android
ABIs, and the arm64 `libnightdrop.so` links both BoringSSL (`X25519MLKEM768KeyShare`) and the
WebTunnel Rust (`webtunnel/src/socks.rs`, `listener-secret=`). cargokit built arm64-v8a, x86,
x86_64 here; armv7 was proven separately.

*Remaining:* install the APK on a device and run the "block Tor → app falls back to WebTunnel"
test against a real bridge from bridges.torproject.org; and confirm the F-Droid build stays
reproducible with BoringSSL added (the toolchain forces only tests off, so libcrypto/libssl are
unchanged and deterministic) before ever enabling the feature in a shipped build.

**Step 4 — Android and F-Droid.** Cross-compile, test on a device against a real bridge from
bridges.torproject.org, and confirm the F-Droid build stays reproducible.

## 6. First on-device run (2026-09-20)

The WebTunnel APK ran on hardware for the first time. Tor bootstrapped, the onion descriptor
published to 8/8 HSDirs, and `spawn_webtunnel_proxy` bound its loopback SOCKS listener — so the
step-3 wiring is live on Android. Three defects surfaced that no desktop harness could have
caught, all three because a phone restarts the core and a one-shot harness never does.

### 6.1 `libc++_shared.so` — a load failure that looked like a hang

`boring-sys` linked against the NDK's *shared* STL, so `libnightdrop.so` carried a `DT_NEEDED`
on `libc++_shared.so`, which cargokit does not bundle. `dlopen` failed.

It presented as the app sitting on the splash screen forever rather than crashing, because
`RustNightdropCore.start()` makes two FFI calls — `setDiagnostics` and `_closeCore()`'s
`rust.unsubscribe()` — **before** its `try`/`finally`. The load threw there, so `_booting` was
never cleared and `_Root` kept rendering `_Splash`. Any future library-load failure will do the
same: those two awaits belong inside the `try`, so the existing `_LoadErrorScreen` can show it.

Fixed by linking libc++ statically (`ANDROID_STL c++_static` plus `BORING_BSSL_RUST_CPPLIB` and
`-lc++abi` for the ABI symbols the static libc++ leaves undefined). Verified in the shipped lib:
`DT_NEEDED` is now only `liblog`/`libdl`/`libm`/`libc`.

### 6.2 The listener secret cannot be per-run

`spawn_webtunnel_proxy` minted a fresh random secret on every core start, and `apply_bridges`
appended it to the bridge line as `listener-secret=`. But **arti persists a bridge's
pluggable-transport settings, that secret included, into its own `state/guards.json`** and dials
us with the stored copy on the next start. So the first reconnect left arti presenting a stale
secret, `Access::Secret` refused it, and the bridge was marked down — permanently, since the
rejection is indistinguishable from a dead bridge:

```
"settings": [["url", "…"], ["ver", "0.0.5"], ["listener-secret", "10bf149e0eec272e6c1f7c0ea91830e1"]]
"unlisted_since": "2026-09-21T01:05:23.944410050Z"   ← the first in-app reconnect
```

The secret is now persisted at `<state_dir>/arti-state/webtunnel-listener-secret` (0600) and
reused, so it stays in step with whatever arti stored. It only gates *local* access to a loopback
proxy and never leaves the device, so a stable secret costs nothing. Regression test:
`listener_secret_is_stable_across_restarts`.

Recovering a device already in this state means deleting `guards.json` — the stale secret is
baked into the persisted guard entry and nothing else clears it.

**Do not read `guards.json`'s timestamps as history.** The entry examined here said
`added_at: 2026-09-10` on a package first installed on the 20th, which looked like state seeded
from somewhere untraced. It is not: arti sets `added_at` to
`randomize_time(now, lifetime_unconfirmed / 10)` (`tor-guardmgr` `guard.rs:290`), and
`lifetime_unconfirmed` defaults to 120 days — so the recorded time is up to **12 days earlier than
the guard was really added**, rounded to 10 seconds. `confirmed_at` is fuzzed the same way
(`guard.rs:727`). It is deliberate anti-fingerprinting: the file must not reveal when a client
started using Tor. Every other field in it is reliable; these two are noise by design.

Verified on the S25, 2026-09-20, with the fix in place. The secret on disk and the one arti
persisted are now the same value, the guard entry is healthy (`unlisted_since: null`), and
`default guards: 0` — the bridge is the *only* entry point, so nothing here could have quietly
fallen back to direct Tor:

```
Guard set loaded. n_guards=1 n_confirmed=1
bridgedesc: download succeeded for "webtunnel [2001:db8:…]:443 … listener-secret=ddc6c0dd…"
chanmgr::factory: Attempting to open a new channel to [… via webtunnel …]
guardmgr::guard: We have found that guard [… via webtunnel …] is usable.
publish::reactor: descriptor uploaded successfully to 8/8 HSDirs
```

That last sequence is from a **restart**, which is the case that used to fail: a fresh listener on
a new port, reusing the persisted secret, bootstrapping and republishing the onion. Before the
fix the first restart marked the bridge down permanently.

### 6.3 The bridge editor was locked behind the censorship it defeats

`BridgesScreen` was reachable only from `home_screen.dart`, which renders only when
`core.identity != null`. Creating an identity calls `TorClient::create_bootstrapped()` with a
120s timeout. So for the users this feature exists for — a first run where Tor is blocked —
identity creation could never succeed, and the bridge editor could never be opened. The whole
censorship path was unreachable by exactly the people it was built for.

`OnboardingScreen` now carries a "Tor blocked? Set up a bridge" link. `readBridges`/`writeBridges`
go straight to the Tor state directory and need no core instance, so the editor works with no
identity. The "reconnect now?" prompt is suppressed there: there is no connection yet, and the
core built by identity creation a moment later reads the new bridges anyway.

`bridges.txt` living outside the encrypted state — so it survives identity loss — is the same
property seen from the other side, and is deliberate: bridges must outlive the identity, because
without them there is no way to get one.

### 6.4 Under an actual Tor block (2026-09-20)

Tested on the S25 against the MikroTik, with the phone's `forward` chain cut to DNS plus a single
HTTPS host — `185.67.127.183:443`, the WebTunnel bridge — and everything else dropped. Mobile data
was disabled first: Android silently fails over to LTE when Wi-Fi loses internet, which would have
produced a pass that meant nothing.

| | bridge configured | bridge cleared (control) |
|---|---|---|
| bootstrap | **100 %** | stalls at 85 % |
| circuits | guard usable via webtunnel | `All tunnel attempts failed due to timeout` |
| onion descriptor | **uploaded to 8/8 HSDirs** | never published |

The control is what makes the first column mean anything: under the identical firewall, direct Tor
could not build a single circuit. The 85 % it does reach is not partial connectivity — it is the
cached consensus from the previous run being read off disk, so the directory is "usable" while
every channel to every relay times out. Confirmed at the router as well: with the bridge cleared,
the accept rule's packet counter stayed flat and only the drop rule advanced.

So everything Tor did in the passing case went through ordinary HTTPS to one web host. That is the
censorship claim, demonstrated rather than argued.

What this still does not cover is **DPI**. The firewall blocks by address, as a censor blocking the
public relay list does; it does not fingerprint traffic. The Chrome-exact TLS fingerprint (§5,
`chrome-proto`) is the answer to that, and it is proven on desktop only — it has never been
exercised against a real DPI censor.

### 6.5 The DPI question (2026-09-20)

§6.4 proves the bridge survives a censor that blocks by *address*. A censor that blocks by how
traffic *looks* is a different problem, and the honest position is that it is only partly tested.

**What the wire actually carries.** A passive observer sees one TCP connection to `:443` of an
ordinary web host, carrying TLS. Everything else — the HTTP Upgrade, the WebSocket framing, Tor
itself — is inside that TLS. So the only thing a DPI box can fingerprint is the TLS handshake,
and specifically the ClientHello.

**The ClientHello now verifies on the device, not just on a laptop.**
`webtunnel/tests/fingerprint.rs` asserts that `connect()` still produces the Chrome JA4 captured
from Chromium 152 (`t13d1515h1_8daaf6152771_f04195365787`). Run by `cargo test` that proves it for
the x86-64 host build, which is not what ships: on Android the BoringSSL is a different build, for
a different architecture, produced by a different toolchain. Since the fingerprint *is* the
defence, "it matches on my laptop" was not the claim worth making.

`webtunnel/android/run-fingerprint-on-device.sh` cross-compiles that test to arm64 and runs it on
the phone. It needs no network — the test connects to a listener it binds itself — so it works on
any connected device, and it is cheap enough to re-run whenever BoringSSL or the profile moves.
Both tests pass on the S25.

**The `User-Agent` is not a tell, and is deliberate.** `upgrade_request` sends
`Go-http-client/1.1`, which is not what a browser sends and looks wrong beside a Chrome TLS
fingerprint. It is correct twice over: that header travels *inside* TLS, so no DPI box on the path
ever sees it; and it reproduces Go's `http.Request.Write` byte for byte, so a bridge operator
cannot tell this client from lyrebird's. The anonymity set is other WebTunnel users, not browsers
— and matching Chrome's TLS while WebTunnel's reference client also uses a Chrome profile is the
same reasoning applied one layer down.

**What is still untested, and how it could be.** Nothing here has met an actual DPI censor. In
descending order of what they would buy:

1. *Run from a censored network.* The only real test. Needs a host inside such a network; there is
   no substitute and no way to simulate the part that matters — a censor's classifier, which is
   not public.
2. *Classify a capture with an open-source DPI engine* (nDPI, Zeek, Suricata) and check the flow is
   reported as ordinary TLS rather than Tor or "unknown/obfuscated". This is the strongest thing
   available locally. None of those tools are installed on this machine and `tcpdump` needs root,
   so it was not run.
3. *Active probing.* A censor who suspects a bridge connects to it and sees what it serves. That is
   the WebTunnel server's behaviour, not this client's — though worth noting that a plain GET to
   the bridge's own URL returned `502`, which is not a convincing decoy site. It is the operator's
   configuration, and a reason not to rely on any single bridge.

JA4 is also not the only fingerprint. JA4S, HTTP/2 settings fingerprints, packet timing and flow
shape are all available to a determined censor, and none of them are addressed here.
