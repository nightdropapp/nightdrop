# Night Drop: building and running for development

Day-to-day dev builds on Linux and Android. Releases are a different, ordered procedure:
`MAINTENANCE.md` §11. Windows has its own page: [`docs/building-windows.md`](docs/building-windows.md).
Toolchain prerequisites: `MAINTENANCE.md` §2.

Before anything else, two rules that each destroyed or nearly published something once:

- **`scripts/install-android-app.sh --release` publishes** the APK to the live onion site. For a
  test build, use the default (debug) mode or build by hand.
- **Never install a debug build over a release install of `app.nightdrop`**, and never
  `adb uninstall` a real install: both destroy that device's identity. Put test builds beside it
  under their own app id (below).

---

## Desktop (Linux)

```sh
make app-run                              # demo core: an in-process peer, for UI work
scripts/install-desktop-app.sh --run      # real build: Tor + relay baked in, installed and launched
```

`install-desktop-app.sh` reads the relay address from `relay-state/onion`, passes
`--dart-define=NIGHTDROP_TOR=1 --dart-define=NIGHTDROP_RELAY=…`, builds the Rust core and the
Flutter release bundle, and installs a `.desktop` entry, icons and `~/.local/bin/nightdrop`
(`--no-build`, `--uninstall`). On desktop the same two settings can instead come from environment
variables at run time, for a hot-reload loop against the real network:

```sh
cd app && flutter run -d linux \
  --dart-define=NIGHTDROP_TOR=1 --dart-define="NIGHTDROP_RELAY=$(cat ../relay-state/onion)"
```

Without `NIGHTDROP_RELAY` the app is P2P-only: QR pairing works, short codes and offline delivery
do not. First Tor bootstrap takes ~30–60 s; the onion is reachable after ~1–3 min.

## Android

Connect the phone (USB debugging, or wireless debugging: discover the current address with
`adb mdns services | grep _adb-tls-connect` — it changes whenever wireless debugging is toggled),
then:

```sh
# A test build BESIDE the real install (its own data, its own identity):
NIGHTDROP_APP_ID=app.nightdrop.test NIGHTDROP_APP_NAME="ND Test" scripts/install-android-app.sh

scripts/install-android-app.sh --build-only     # APK only: app/build/app/outputs/flutter-apk/
scripts/install-android-app.sh --install-only   # install the last build
scripts/install-android-app.sh --list-devices
scripts/install-android-app.sh --diag           # opt-in field diagnostics (ARCHITECTURE.md §6)
```

The script builds only the ABI of the connected device unless `--universal` is given, bakes the
relay in from `relay-state/onion`, installs and launches. Logs: `adb logcat | grep nd-` on a
`--diag` build. More device-testing notes, including why a black screenshot is not a blank
screen: `MAINTENANCE.md` §13.

## A dev relay

The production relay runs as the `nightdrop-relay` user service on the maintainer's machine, from
`relay-state/` (`MAINTENANCE.md` §14). For a relay of your own while developing:

```sh
NIGHTDROP_RELAY_TUI=1 make relay-run      # state in relay-state-dev/ — never relay-state/
cat relay-state-dev/onion                 # its address, to build an app against
```

A second process on `relay-state/` would publish the production onion from two places at once.
Running a relay for real (service, private relays, the signed directory): `relay/README.md`,
`RELAYS.md`.

## Tests

```sh
make core-test && make clippy && make app-test
```

The full loop, including the integration test and the Tor test targets nothing builds
automatically: `MAINTENANCE.md` §3.

## Troubleshooting

- **`flutter: command not found` / the wrong Flutter.** The scripts look for the SDK `app/.fvmrc`
  pins (`FLUTTER_HOME`, else `~/flutter-<version>`, `~/fvm/versions/<version>`, `~/flutter`) and
  refuse any other version; install it in one of those places or set `FLUTTER_HOME`
  (`MAINTENANCE.md` §8).
- **`libnightdrop.so` not found** on a bare `flutter run`: `make core-build` first.
- **CMake "Permission denied" on `/usr/local`**, or Android "No Android SDK found":
  `MAINTENANCE.md` §9.
- **No relay in the build** (short codes fail, "P2P only" in the script output): `relay-state/onion`
  is missing — in a worktree, copy just that file in (`MAINTENANCE.md` §8).
- **The app cannot reach anyone after deleting its Tor state.** `~/.local/share/<app-id>/arti-state/`
  holds the device's onion key. Deleting it gives the device a new address its contacts never
  learn — don't, unless the identity is disposable.
- **Install fails with a signature or version conflict.** You are installing a debug build over a
  release one, or a lower versionCode over a higher one. Install under a test app id instead of
  uninstalling the real one.
