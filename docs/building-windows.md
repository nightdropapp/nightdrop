# Building Night Drop on Windows

Night Drop builds and runs on Windows 11 x64 from the same tree as Linux and Android. As of
2026-09-30 it has been built and tested in a Windows 11 25H2 VM: a Tor identity created over
WebTunnel bridges, paired with an Android phone, and messages sent and received in both directions.
`scripts/build-windows-installer.ps1` turns a release build into an installer (below), published
from 0.1.26 as `NightDropSetup.exe`. It is not code-signed.

Flutter cannot cross-compile a Windows desktop app, so the build has to run **on Windows**.

## Toolchain

Everything below installs with `winget` from an Administrator PowerShell. Versions must match the
rest of the project: Rust is pinned in `rust-toolchain.toml`, Flutter in `app/.fvmrc`.

| Tool | Why | Install |
|---|---|---|
| Visual Studio 2022 Build Tools, C++ workload **plus ATL** | MSVC compiler and linker; plugins including `flutter_secure_storage_windows` and `fc_native_video_thumbnail` include ATL headers and fail with `C1083: Cannot open include file: 'atlbase.h'` without it | see below |
| Rust (MSVC target) | the core | `winget install Rustlang.Rustup`, then `rustup toolchain install <version>-x86_64-pc-windows-msvc` with the version from `rust-toolchain.toml` |
| Flutter | the app | `git clone -b <version> https://github.com/flutter/flutter.git C:\src\flutter`, add `C:\src\flutter\bin` to `PATH`, `flutter config --enable-windows-desktop` |
| Git | Flutter and the tree | `winget install Git.Git` |
| CMake | BoringSSL, the Flutter runner | `winget install Kitware.CMake` |
| NASM | BoringSSL's assembly (WebTunnel) | `winget install NASM.NASM`; it installs to `C:\Program Files\NASM`, which is **not** added to `PATH` |
| LLVM | `boring-sys` runs bindgen, which needs `libclang.dll` | `winget install LLVM.LLVM`, then set `LIBCLANG_PATH=C:\Program Files\LLVM\bin` |

Visual Studio Build Tools with both components in one go (the tested setup installed the workload
first and added ATL afterwards with `setup.exe modify --add Microsoft.VisualStudio.Component.VC.ATL`;
the combined form below uses the same component IDs):

```powershell
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.ATL --includeRecommended"
```

## Long paths

Turn on Windows long-path support before the first build (Administrator PowerShell):

```powershell
Set-ItemProperty HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem -Name LongPathsEnabled -Value 1
git config --global core.longpaths true
```

Without it the build fails inside BoringSSL's CMake step with `MSB4018 ... The item metadata
"%(FullPath)" cannot be applied to the path`. Cargokit builds the core under
`app\build\windows\x64\plugins\nightdrop\cargokit_build\...`, and BoringSSL's CMake scratch
directories nest deep enough below that to pass 260 characters. Cloning to a short path such as
`C:\src\nightdrop` helps too.

## Build

The production wiring is set at compile time, exactly as for the Linux AppImage
(`scripts/build-appimage.sh`). **Without `NIGHTDROP_TOR=1` you get the in-process demo core**, which
never touches the network; nothing on screen says so.

```powershell
$env:Path += ";C:\Program Files\NASM"
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
cd C:\src\nightdrop\app
flutter build windows --release `
  --dart-define=NIGHTDROP_TOR=1 `
  "--dart-define=NIGHTDROP_RELAY=<relay onion address>"
```

Add `--dart-define=NIGHTDROP_DIAG=1` for a diagnostic build. On Windows the diagnostic lines go to
stderr, which a GUI app launched normally does not have; launch it from a console with `2> file`.

The app is `app\build\windows\x64\runner\Release\night_drop.exe`, and it needs the whole
`Release` folder next to it (`nightdrop.dll` is the Rust core, 22–23 MB). User data lives in
`%APPDATA%\Night Drop\Night Drop`.

A first build takes around 10 minutes on 4 cores; most of it is arti and BoringSSL.

## Installer

```powershell
winget install JRSoftware.InnoSetup      # once
powershell -ExecutionPolicy Bypass -File scripts\build-windows-installer.ps1
```

It builds the release app with the production wiring (Tor on; the relay address is read from
`fdroid/app.nightdrop.yml`, so it always matches the published Android builds; no diagnostics),
stages the bundle with the Visual C++ runtime, and compiles `app/windows/installer/night_drop.iss`
into `app\build\windows\installer\NightDropSetup.exe` (about 20 MB). The name carries no version on
purpose: the website links `/releases/latest/download/NightDropSetup.exe`.

- **Per-user, no administrator rights:** installs to `%LOCALAPPDATA%\Programs\Night Drop`, with a
  Start-menu entry, an optional desktop icon and an uninstaller. Upgrades install over the old
  version in place (same `AppId`, which must never change).
- **The C++ runtime is bundled app-local** (`msvcp140.dll`, `vcruntime140.dll`,
  `vcruntime140_1.dll`) — exactly what the runner, plugins and core import (`dumpbin /dependents`);
  the Universal CRT ships with Windows 10+. The installed app loads these copies, not the system's.
- **Uninstalling keeps your identity and chats** (`%APPDATA%\Night Drop`), so a reinstall or upgrade
  picks them up. Use the app's wipe, or delete that folder, to remove them.
- **No licence click-through:** the AGPL needs no acceptance to run the program, so the installer
  does not ask for one; `LICENSE.txt` is installed next to the app.
- **Not code-signed.** Without a Windows code-signing certificate, SmartScreen warns about an
  unknown publisher. Publish it with a GPG signature and a `SHA256SUMS` entry like the other
  binaries.

Tested 2026-09-30 in the Windows 11 VM: silent and interactive install, launch (connected over
WebTunnel bridges), uninstall (program, shortcut and uninstall entry gone, identity kept),
reinstall (same identity restored), and an in-place upgrade.

## Known limits on Windows

- **No background delivery.** Android keeps a foreground service running; Windows has no equivalent
  wired up, so messages arrive while the app is open and wait on the relay (24 h) otherwise.
- **Tor over bridges needs a patched dependency.** `saturating-time` loops forever on Windows'
  100 ns clock, which hung every bridge bootstrap at one core, 100% CPU (arti#2726). The tree
  carries a fix in `third_party/saturating-time/` (on 0.5.0; see its `NIGHTDROP-PATCH.md`). Do not
  drop it on a release note's word: 0.5.0 was said to contain the fix and does not — read the
  upstream source first.
- **Bridge bootstraps can take longer than the app's 120 s limit** on slow volunteer bridges. This
  is not Windows-specific: the same bridges measured 9–88 s on Linux and 10–73 s on Windows.
- **Not reproducible or code-signed yet**, unlike the Android APKs (which F-Droid rebuilds and
  verifies). Verify the installer against `SHA256SUMS` and its `.asc` signature.

## Testing in a VM

A virt-manager/libvirt Windows 11 guest works. Two things that look like app bugs are not:

- **A white window** after launching while the guest's display is asleep: the renderer comes up
  without a surface. Relaunch with the display awake, or turn display sleep off in the guest
  (`powercfg /change monitor-timeout-ac 0`).
- **A GUI started over SSH is invisible**: it runs in the non-interactive session. Start it through a
  scheduled task with an interactive principal instead.
