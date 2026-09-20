# Design draft — Bridges and pluggable transports on Android

**Status:** 🟢 in-app **bridge** configuration implemented (2026-08-01), not yet exercised on a
device. PT binaries are a separate, larger piece (§3) and are **not** included. 🟡 An in-process
**WebTunnel** client (§5) is being built in `webtunnel/` instead: step 1 of 4 done (2026-09-19).
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

**Step 4 — Android and F-Droid.** Cross-compile (the open BoringSSL/cmake gate), test on a device
against a real bridge from bridges.torproject.org, and confirm the F-Droid build stays
reproducible.

**Step 4 — Android and F-Droid.** Cross-compile, test on a device against a real bridge from
bridges.torproject.org, and confirm the F-Droid build stays reproducible.

