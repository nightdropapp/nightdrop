# Design draft — Bridges and pluggable transports on Android

**Status:** 🟢 in-app **bridge** configuration implemented (2026-08-01), not yet exercised on a
device. PT binaries are a separate, larger piece (§3) and are **not** included.
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
