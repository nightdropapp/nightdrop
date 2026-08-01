# Tor bridges (censorship circumvention)

Night Drop reaches the network over Tor. Where the **public** Tor relays are IP-blocked
(some ISPs and national firewalls do this), you can route through **bridges** — unlisted
entry relays that aren't on the public list — so the client can still bootstrap. This is
the censorship-resistance path referenced in `ARCHITECTURE.md` §6.

## Vanilla (direct) bridges

An address + fingerprint, no pluggable transport. These help against a plain block of the
public relay IPs and need nothing extra on the device. (For where bridge IPs themselves are
DPI-blocked, see **obfs4 / Snowflake** below.)

The core reads bridge lines from a file named **`bridges.txt`** in the app's Tor state
directory (the same directory that contains `arti-state/`):

- **Linux desktop:** the app support directory, typically
  `~/.local/share/<app-id>/bridges.txt` (alongside `arti-state/`).
- **Android:** the state directory is app-private, so there is no file to drop. Use the in-app
  editor instead — **menu → Tor bridges** — which validates each line with the same parser this
  file describes and writes it there for you (`docs/design/android-bridges.md`). Pluggable
  transports remain desktop-only: they need a client binary Android has no way to provide yet.

Format — one bridge per line; blank lines and `#` comments are ignored; an optional
leading `Bridge` keyword (torrc style) is tolerated:

```
# Example vanilla bridge lines (get REAL ones — see below)
38.229.33.83:80 0BAC39417268B96B9F514E7F63FA6FBA1A788955
Bridge 38.229.33.84:443 0BAC39417268B96B9F514E7F63FA6FBA1A788955
```

A malformed line is skipped (with a note on stderr), never fatal. Restart the app after
editing `bridges.txt`.

## Where to get bridge lines

Bridges are distributed out-of-band so a censor can't just block them all:

- <https://bridges.torproject.org/> (choose "vanilla"/without transport for the above)
- Email `bridges@torproject.org` from a Gmail/Riseup address with `get transport none`
  in the body.

## obfs4 / Snowflake (pluggable transports)

Where a censor blocks even bridge IPs by deep-packet-inspecting for the Tor protocol, a
**pluggable transport (PT)** disguises the traffic itself. Night Drop supports managed PTs
via arti's `pt-client` feature: you supply the PT client binary and a bridge line that
names the transport, and arti launches the binary on demand.

Two pieces are needed:

1. **A bridge line naming the transport**, in `bridges.txt` (same file as vanilla bridges):

   ```
   obfs4 38.229.33.83:80 0BAC39417268B96B9F514E7F63FA6FBA1A788955 cert=… iat-mode=0
   snowflake 192.0.2.3:80 2B280B23E1107BB62ABFC40DDCC8824814F80A72 fingerprint=…
   ```

   Get real ones from <https://bridges.torproject.org/> (choose obfs4), or email
   `bridges@torproject.org` with `get transport obfs4`. Snowflake uses a single well-known
   line (see the Tor Browser `torrc-defaults`).

2. **A `transports.txt`** (in the Tor state dir, alongside `bridges.txt`) mapping each
   transport to its client binary — one per line; `#` comments and blank lines ignored; an
   optional leading `Transport` keyword tolerated:

   ```
   # <protocols>       <path-to-client-binary>
   obfs4               /usr/bin/lyrebird
   snowflake           /usr/bin/snowflake-client
   obfs4,meek_lite     /usr/bin/lyrebird        # one binary can provide several
   ```

   `<protocols>` is one transport name or several comma-separated. The rest of the line is
   the binary path (may contain spaces). Install the binaries from your distro
   (`obfs4proxy`/`lyrebird`, `snowflake-client`) or Tor's builds. A malformed line is
   skipped (noted on stderr), never fatal; arti only spawns a binary when a bridge actually
   needs it, so a stale entry can't stall bootstrap on its own. Restart the app after
   editing.

   To generate this file from the PT clients already installed on a desktop, run
   `scripts/setup-pluggable-transports.sh` — it detects `lyrebird`/`obfs4proxy` and
   `snowflake-client` on `PATH` and writes a ready `transports.txt` (it never installs or
   runs anything). Copy the result into the Tor state dir.

## Follow-ups (not yet implemented)

- **Bundling PT binaries.** Today you install `lyrebird`/`snowflake-client` yourself (the
  `scripts/setup-pluggable-transports.sh` helper then wires `transports.txt` from what's on
  `PATH`). Shipping the binaries *inside* the app so no separate install is needed —
  especially on mobile, where they'd be packaged as native assets and unpacked to the
  app-private state dir — is a larger, platform-specific effort still to do.
- **Android UI.** Because the Android state dir is app-private, bridge/PT entry there needs
  an in-app settings screen that writes `bridges.txt` + `transports.txt` (or passes them
  through the FFI). Desktop works via the files today.
