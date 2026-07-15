# Night Drop: Build, Compile & Deploy Guide

Complete guide to build and deploy Night Drop on Desktop (Linux) and Mobile (Android) with relay store-and-forward messaging.

---

## 🎯 Quick Start

### Desktop (Tor Mode + Relay)
```bash
cd ~/ghost-chat
./run-ghost-tor.sh
```

### Android (Tor Mode + Relay)
```bash
cd ~/ghost-chat
./install-android-app.sh
```

---

## 🖥️ Desktop Application (Linux)

### Prerequisites

**Tools Required:**
```bash
flutter --version          # Flutter SDK 3.0+
rustc --version            # Rust toolchain
clang --version            # C compiler
cmake --version            # Build system
ninja --version            # Build parallelizer
pkg-config --version       # Package config
```

**Installation (Ubuntu/Debian):**
```bash
sudo apt-get update
sudo apt-get install -y clang cmake ninja-build pkg-config libgtk-3-dev
```

### Build Modes

#### 1. Tor Mode + Relay Fallback (RECOMMENDED)
```bash
cd ~/ghost-chat
./run-ghost-tor.sh
```

**Features:**
- ✅ Full P2P via Tor (.onion addresses)
- ✅ Relay fallback for offline delivery (24h store-and-forward)
- ✅ Complete anonymity
- ✅ Works anywhere (LTE, WiFi, VPN, etc.)
- ✅ Encrypted end-to-end (Signal Double Ratchet)

**First Run:** 30-60 seconds (Tor bootstrap)
**Subsequent Runs:** 5-10 seconds (cached state)

**What It Does:**
1. Auto-detects relay address from `relay-state/onion`
2. Sets `GHOST_RELAY` environment variable
3. Builds Rust core (`libnightdrop.so`)
4. Launches Flutter app in Tor mode
5. App shows QR code for pairing

#### 2. Manual Build
```bash
# Get relay address
RELAY_ADDR=$(cat ~/ghost-chat/relay-state/onion)

# Set environment
export FLUTTER_HOME=~/flutter
export PROJECT_ROOT=~/ghost-chat
export GHOST_TOR=1
export GHOST_RELAY="$RELAY_ADDR"

# Build Rust core first
cd $PROJECT_ROOT
make core-build

# Run app
cd $PROJECT_ROOT/app
$FLUTTER_HOME/bin/flutter run -d linux --dart-define=GHOST_TOR=1
```

#### 3. Release Build (Optimized)
```bash
cd ~/ghost-chat
./run-ghost-tor.sh --build            # compiles the release bundle (Tor + relay baked in)
./run-ghost-tor.sh --build --launch   # ...and launches it
# Manual equivalent:
#   cd app && flutter build linux --release \
#     --dart-define=GHOST_TOR=1 --dart-define="GHOST_RELAY=$(cat ../relay-state/onion)"
```

**Output:** ~80-120 MB standalone binary

#### 4. Demo Mode (Local Testing)
```bash
cd ~/ghost-chat/app
~/flutter/bin/flutter run -d linux
```

**Note:** Both sender and receiver in same app, no relay, for UI testing only.

---

## 📱 Android Application (Phone)

### Prerequisites

**Tools Required:**
```bash
flutter --version              # Flutter SDK 3.0+
adb --version                  # Android Debug Bridge
```

**Find adb:**
```bash
which adb                      # If in PATH
# OR
~/android-sdk/platform-tools/adb --version
```

### Device Connection

#### USB Connection
1. Connect phone via USB cable
2. Enable USB Debugging: Settings → Developer Options → USB Debugging
3. Run installer (auto-detects device)

#### WiFi Connection
```bash
# Enable wireless debugging on phone
adb tcpip 5555

# Connect wirelessly
./adb-wireless.sh 192.168.88.26 33603

# Disconnect USB (stays connected via WiFi)
```

### Build & Install

#### Automatic (Recommended)
```bash
cd ~/ghost-chat
./install-android-app.sh
```

**What It Does:**
1. ✅ Validates adb and device
2. ✅ Detects relay address
3. ✅ Builds APK with relay configured (`--dart-define=GHOST_RELAY=...`)
4. ✅ Installs on connected device
5. ✅ Auto-launches Night Drop

**Build Time:** 3-5 min (first), 30-90 sec (subsequent)

#### Build-Only (Skip Install)
```bash
./install-android-app.sh --build-only
# APK output: app/build/app/outputs/flutter-apk/app-debug.apk
```

#### Install Existing APK
```bash
./install-android-app.sh --install-only
```

#### Manual Build with Relay
```bash
# Get relay address
RELAY_ADDR=$(cat ~/ghost-chat/relay-state/onion)

# Build with relay embedded (compile-time constant)
cd ~/ghost-chat/app
~/flutter/bin/flutter build apk --debug \
  --dart-define=GHOST_TOR=1 \
  --dart-define="GHOST_RELAY=$RELAY_ADDR"

# Install
adb install -r build/app/outputs/flutter-apk/app-debug.apk

# Launch
adb shell monkey -p app.nightdrop -c android.intent.category.LAUNCHER 1
```

#### List Connected Devices
```bash
./install-android-app.sh --list-devices
```

---

## 🔗 Relay Configuration

### What Is Relay?
- Opaque encrypted blob store (can't read plaintext)
- 24-hour default message TTL (time-to-live)
- No keys or identity data stored (full E2E)
- Location: `bzcqxuxwvtmrmvprsoscnronkjf5wknfuj5ozxiq5fr6qowvnkwrwwad.onion`

### How It Works

**Message Flow (Offline Delivery):**
```
Sender sends message
  ↓
[Try 1] Direct P2P via Tor
  ├─ Recipient online?
  │  └─ Success → Delivered instantly ✅
  │
  └─ Recipient offline?
     ↓
     [Try 2] Store on relay
       ├─ Relay configured?
       │  └─ Success → Message queued 24h ✅
       │
       └─ No relay?
          └─ Fail ❌

Recipient comes online
  ↓
  Polls relay regularly
  ↓
  Relay returns queued messages ✅
  ↓
  Messages displayed in chat ✅
```

### Platform Differences

#### Desktop: Runtime Configuration
```bash
# Set before running
export GHOST_RELAY=$(cat relay-state/onion)

# App reads: Platform.environment['GHOST_RELAY']
./run-ghost-tor.sh
```

#### Android: Build-Time Configuration
```bash
# Embedded in APK during build
--dart-define=GHOST_RELAY=$(cat relay-state/onion)

# App reads: String.fromEnvironment('GHOST_RELAY')
flutter build apk --debug --dart-define=GHOST_RELAY=...
```

**Why Different?** Android is sandboxed; runtime env vars inaccessible. Values must be baked in at build time.

### Verify Relay Configured

**Desktop:**
```bash
./run-ghost-tor.sh
# Look for: "✓ Relay configured for store-and-forward: ..."
```

**Android:**
```bash
./install-android-app.sh
# Look for: "Relay configured for store-and-forward: ..."
```

---

## 📋 Full Workflow: Build Both Platforms

### Step 1: Start Relay Server
```bash
cd ~/ghost-chat

# Start relay (if not already running)
./nightdrop_relay.sh

# Verify relay is up
sleep 2
cat relay-state/onion
# Should output: bzcqxuxwvtmrmvprsoscnronkjf5wknfuj5ozxiq5fr6qowvnkwrwwad.onion
```

### Step 2: Build & Run Desktop
```bash
# Terminal 1
cd ~/ghost-chat
./run-ghost-tor.sh

# Wait for app window to open
# Look for QR code display
# Desktop is now waiting to pair
```

### Step 3: Build & Deploy Android
```bash
# Terminal 2
cd ~/ghost-chat

# Connect phone (USB or WiFi)
./adb-wireless.sh 192.168.88.26 33603  # If WiFi

# Build, install, and launch
./install-android-app.sh

# Wait for app to launch on phone
# Phone is now ready to pair
```

### Step 4: Pair Devices
```
Desktop App:
  1. Shows QR code with .onion address

Phone App:
  1. New Chat → Scan QR Code
  2. Scan desktop's QR
  3. Confirm pairing on both devices
```

### Step 5: Test Messaging
```
Both devices online:
  Desktop → Send message → Phone ✅ (instant P2P)
  Phone → Send message → Desktop ✅ (instant P2P)

Phone offline:
  Desktop → Send message
  → Message stored on relay for 24h
  Phone online:
    → Retrieves message ✅
    → Appears in chat

Desktop offline:
  Phone → Send message
  → Message stored on relay for 24h
  Desktop online:
    → Retrieves message ✅
    → Appears in chat
```

---

## ⏱️ Build Times

| Task | Time | Notes |
|------|------|-------|
| Desktop (first) | 3-7 min | Tor bootstrap 30-60s |
| Desktop (rebuild) | 30-90 sec | Incremental |
| Desktop (hot reload) | <2 sec | Code changes only |
| Desktop (release) | 5-10 min | Fully optimized |
| Android (first) | 3-5 min | Gradle downloads deps |
| Android (rebuild) | 30-90 sec | Cached |
| Rust core | 1-2 min | Incremental |

---

## 🔧 Manual Commands

### Desktop Compilation Only
```bash
# Just build core
make core-build
# Output: target/debug/libnightdrop.so

# Clean build
flutter clean && cargo clean
make core-build

# Test everything
make core-test && make app-test
```

### Android Compilation Only
```bash
# Just build APK
./install-android-app.sh --build-only

# Rebuild (clear cache first)
cd app && flutter clean && flutter pub get
./install-android-app.sh --build-only

# Build without relay (P2P only)
cd app
~/flutter/bin/flutter build apk --debug \
  --dart-define=GHOST_TOR=1
```

### Environment Variables (Optional)

Set these if tools aren't in PATH:
```bash
export FLUTTER_HOME=~/flutter
export PROJECT_ROOT=~/ghost-chat
export ADB=~/android-sdk/platform-tools/adb

# Rebuild apps
./run-ghost-tor.sh
./install-android-app.sh
```

---

## 🧹 Cleanup & Reset

### Clear Desktop Tor State
```bash
# Removes .onion address, forces new one on next run
rm -rf ~/.local/share/app.nightdrop/arti-state/

# Run again - generates new .onion
./run-ghost-tor.sh
```

### Uninstall Android App
```bash
adb uninstall app.nightdrop
```

### Clear All Build Artifacts
```bash
cd ~/ghost-chat

# Flutter
cd app
flutter clean

# Rust
cd ..
cargo clean

# Rebuild from scratch
make core-build
./run-ghost-tor.sh
```

---

## 📂 Key Files

| File | Purpose |
|------|---------|
| `run-ghost-tor.sh` | Desktop launcher (auto-configures relay) |
| `install-android-app.sh` | Android builder (auto-configures relay) |
| `adb-wireless.sh` | WiFi phone connector |
| `nightdrop_relay.sh` | Relay server launcher |
| `Makefile` | Build commands (core-build, tests, etc.) |
| `relay-state/onion` | Relay .onion address |
| `app/lib/src/core/rust_nightdrop_core.dart` | Transport config (Tor, Demo, Networked) |
| `core/src/api.rs` | FFI API surface |

---

## 🐛 Troubleshooting

### Desktop Issues

**"Flutter command not found"**
```bash
export FLUTTER_HOME=~/flutter
export PATH=$FLUTTER_HOME/bin:$PATH
./run-ghost-tor.sh
```

**"libnightdrop.so not found"**
```bash
make core-build
./run-ghost-tor.sh
```

**"GTK libraries not found"**
```bash
sudo apt-get install libgtk-3-dev
flutter clean
./run-ghost-tor.sh
```

**"Relay configured for store-and-forward" not showing**
```bash
# Check relay is running
ps aux | grep relay

# Check relay state file
ls -la ~/ghost-chat/relay-state/onion

# Restart script
./run-ghost-tor.sh
```

### Android Issues

**"adb command not found"**
```bash
export ADB=~/android-sdk/platform-tools/adb
./install-android-app.sh
```

**"No Android devices found"**
```bash
# USB: Verify USB cable and USB Debugging enabled
adb devices -l

# WiFi: Run adb tcpip first
adb tcpip 5555
./adb-wireless.sh 192.168.X.X 5555
```

**"Build fails"**
```bash
cd ~/ghost-chat/app
flutter clean
flutter pub get
cd ..
./install-android-app.sh --build-only
```

**"Installation fails"**
```bash
# Uninstall old app
adb uninstall app.nightdrop

# Reinstall
./install-android-app.sh --install-only
```

---

## 🔐 Security & Privacy

### Messages
- ✅ End-to-end encrypted (Signal Double Ratchet)
- ✅ Relay cannot read plaintext
- ✅ Device-to-device encryption
- ✅ No server-side keys

### Identity
- ✅ Anonymous (no phone/email/username)
- ✅ Device-generated keypair
- ✅ Pairing via QR code or short code
- ✅ Authorization required before first message

### Transport
- ✅ Tor by default (P2P via .onion)
- ✅ Relay fallback (opaque blobs only)
- ✅ No IP addresses exposed (Tor)
- ✅ Works on any network

### Storage
- ✅ Local-first (messages stored on device)
- ✅ Relay storage optional (24h opt-in)
- ✅ Backup encrypted with user password
- ✅ Device-held keypair, never synced

---

## 📞 Common Tasks

| Task | Command | Time |
|------|---------|------|
| Start desktop | `./run-ghost-tor.sh` | 1-2 min |
| Start desktop (background) | `./run-ghost-tor.sh --detach` | 1-2 min |
| Build Android | `./install-android-app.sh` | 3-5 min |
| Build Android (no install) | `./install-android-app.sh --build-only` | 3-5 min |
| Connect phone WiFi | `./adb-wireless.sh 192.168.X.X` | 5 sec |
| Run tests | `make core-test && make app-test` | 5-10 min |
| Release build | `flutter build linux --release` | 5-10 min |
| Hot reload | Press 'r' in terminal | <1 sec |

---

## ✨ Tips for Future Builds

### Quick Development Cycle
```bash
# Terminal 1: Run desktop
./run-ghost-tor.sh

# Terminal 2: Edit code
nano app/lib/src/main.dart

# Back to Terminal 1: Press 'r' for hot reload
# Changes appear instantly!
```

### Rebuild Android with New Relay
```bash
# Old relay stopped? Start new one
./nightdrop_relay.sh

# Update Android APK with new relay
./install-android-app.sh --build-only

# Uninstall old app
adb uninstall app.nightdrop

# Install new APK
adb install -r app/build/app/outputs/flutter-apk/app-debug.apk
```

### Skip Desktop Tor Bootstrap
```bash
# First run: 30-60 seconds
./run-ghost-tor.sh

# Subsequent runs: 5-10 seconds (state cached)
# Just run again, faster now
./run-ghost-tor.sh
```

### Run Without Relay (P2P Only)
```bash
# Desktop (no relay config needed)
cd app
~/flutter/bin/flutter run -d linux --dart-define=GHOST_TOR=1

# Android (build without relay)
cd app
~/flutter/bin/flutter build apk --debug \
  --dart-define=GHOST_TOR=1
adb install -r build/app/outputs/flutter-apk/app-debug.apk
```

---

## 🚀 One-Command Deployment

```bash
#!/bin/bash
# Deploy both platforms in parallel

cd ~/ghost-chat

# Start desktop (background)
./run-ghost-tor.sh --detach &
DESKTOP_PID=$!

# Build and deploy Android
./install-android-app.sh

# Wait for desktop to start
sleep 5

echo "✅ Both platforms deployed!"
echo "Desktop PID: $DESKTOP_PID"
echo "Pair with QR code from desktop app"
```

---

## 📚 Related Documentation

- **ARCHITECTURE.md** — System design & threat model
- **CLAUDE.md** — Developer conventions & invariants

---

## 📝 Version Info

- **Night Drop:** Feature-complete, tested
- **Rust Core:** 30+ tests, clippy clean
- **Flutter App:** 8+ tests, analyze clean
- **Tor Transport:** Embedded arti, verified live
- **Relay:** 24h store-and-forward, verified working
- **E2E Crypto:** Signal Double Ratchet + SPAKE2 PAKE

---

## Quick Reference

```bash
# Everything in one place

# 1. Start relay (if needed)
./nightdrop_relay.sh

# 2. Build & run desktop
./run-ghost-tor.sh

# 3. Build & install Android  
./install-android-app.sh

# 4. Monitor logs
tail -f /tmp/ghost-chat-tor.log
tail -f /tmp/relay-*.log

# 5. See relay activity
ps aux | grep relay

# 6. Check relay state
cat relay-state/onion
```

Done! Everything you need to build and deploy both platforms with full relay support. 🚀
