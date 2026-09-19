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

**Step 2 — the TLS fingerprint (the hard part; not started).** rustls's ClientHello is
recognisable as rustls, and very little that browses the web sends it, so a censor could block
it for almost no collateral cost. That makes the transport *correct but not stealthy* until
this is solved, and **it must not ship to users before it is** — a recognisable handshake does
not just fail, it can mark the user as a circumventer. Constraints found while reading
lyrebird: its default is `hellorandomizednoalpn` — a *randomised* hello with **no ALPN**, not a
browser copy — and the no-ALPN part is load-bearing, because a bridge's nginx offered `h2`
would negotiate HTTP/2, where the Upgrade doesn't exist. A straight Chrome copy (BoringSSL via
the `boring` crate) therefore needs its ALPN changed, which is itself a deviation from Chrome.
Options to evaluate: BoringSSL with a Chrome-like hello minus `h2`; or randomisation in the
style of uTLS. Either needs a JA3/JA4 comparison against real traffic before it counts.

**Step 3 — into the core.** Spawn the SOCKS listener at Tor startup on `127.0.0.1:0` with a
fresh secret, add an unmanaged `webtunnel` transport pointing at it, append the secret to
WebTunnel bridge lines, and teach the bridge editor to accept them. This adds a clearnet DNS
lookup of the bridge's hostname (the same one the SNI already reveals), which
`DEPENDENCIES.md`'s "the only egress is Tor" statement must then describe.

**Step 4 — Android and F-Droid.** Cross-compile, test on a device against a real bridge from
bridges.torproject.org, and confirm the F-Droid build stays reproducible.

