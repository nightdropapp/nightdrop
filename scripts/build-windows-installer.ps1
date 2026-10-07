# Build the Night Drop Windows installer (NightDropSetup.exe). Runs on Windows; see
# docs/building-windows.md for the toolchain. Also needs Inno Setup 6 (winget install JRSoftware.InnoSetup).
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows-installer.ps1
#
# The app is built with the production wiring: Tor on, and the same relay the published Android
# builds carry, read from the F-Droid recipe so the two cannot drift apart. No diagnostics.
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$App = Join-Path $Root 'app'

# Version from pubspec (x.y.z of "x.y.z+code").
$verLine = (Select-String -Path (Join-Path $App 'pubspec.yaml') -Pattern '^version:\s*(\S+)').Matches[0].Groups[1].Value
$Version = $verLine.Split('+')[0]

# Relay onion from the F-Droid recipe's --dart-define=NIGHTDROP_RELAY=...
$relayMatch = Select-String -Path (Join-Path $Root 'fdroid\app.nightdrop.yml') -Pattern 'NIGHTDROP_RELAY=([a-z2-7]{56}\.onion)' | Select-Object -First 1
if (-not $relayMatch) { throw 'no NIGHTDROP_RELAY in fdroid/app.nightdrop.yml' }
$Relay = $relayMatch.Matches[0].Groups[1].Value

$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 not found (winget install JRSoftware.InnoSetup)' }

# The Rust core's BoringSSL build needs NASM and libclang (docs/building-windows.md).
$env:Path += ';C:\Program Files\NASM'
if (-not $env:LIBCLANG_PATH) { $env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin' }

# The pinned Flutter (app/.fvmrc), not whatever `flutter` PATH finds first: the build VM's default
# is an older SDK, and a release built on it would ship without that SDK's fixes (MAINTENANCE.md §8).
$want = ((Get-Content -Raw (Join-Path $App '.fvmrc')) | ConvertFrom-Json).flutter
$sdk = Split-Path -Parent (Split-Path -Parent (Get-Command flutter).Source)
$have = ((Get-Content -Raw (Join-Path $sdk 'bin\cache\flutter.version.json')) | ConvertFrom-Json).frameworkVersion
if ($have -ne $want) { throw "flutter on PATH ($sdk) is $have, app/.fvmrc pins $want - put that SDK's bin first on PATH" }

Write-Host "==> Night Drop $Version, relay $Relay, Flutter $have"
Push-Location $App
try {
    & flutter build windows --release --dart-define=NIGHTDROP_TOR=1 "--dart-define=NIGHTDROP_RELAY=$Relay"
    if ($LASTEXITCODE -ne 0) { throw "flutter build failed ($LASTEXITCODE)" }
} finally { Pop-Location }
$Release = Join-Path $App 'build\windows\x64\runner\Release'

# Stage the bundle plus the Visual C++ runtime, app-local: a clean Windows may not have it, and
# both the Flutter runner and the Rust core (built /MD) link against it. Microsoft permits
# redistributing these files alongside the application.
$Stage = Join-Path $App 'build\windows\installer\bundle'
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force $Stage | Out-Null
Copy-Item -Recurse -Path (Join-Path $Release '*') -Destination $Stage
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
$crt = Get-ChildItem -Directory (Join-Path $vs 'VC\Redist\MSVC') | Where-Object { $_.Name -match '^\d' } |
    Sort-Object Name -Descending | Select-Object -First 1
$crtDir = Get-ChildItem -Directory (Join-Path $crt.FullName 'x64') | Where-Object { $_.Name -like 'Microsoft.VC*.CRT' } | Select-Object -First 1
# Exactly what the runner, plugins and core import (dumpbin /dependents, 0.1.26); the Universal CRT
# (api-ms-win-crt-*, ucrtbase) ships with Windows 10 and later.
foreach ($dll in 'msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll') {
    Copy-Item (Join-Path $crtDir.FullName $dll) $Stage
}

$Out = Join-Path $App 'build\windows\installer'
New-Item -ItemType Directory -Force $Out | Out-Null
& $iscc /Q "/DAppVersion=$Version" "/DBundleDir=$Stage" "/DOutputDir=$Out" (Join-Path $App 'windows\installer\night_drop.iss')
if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
$setup = Join-Path $Out "NightDropSetup.exe"
Write-Host "==> $setup, Night Drop $Version ($([math]::Round((Get-Item $setup).Length / 1MB, 1)) MB)"
