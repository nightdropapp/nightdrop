#!/bin/bash
# Android App Installer for Night Drop
# Builds APK and installs on connected device via adb
# Handles adb setup, Flutter build, and device installation

set -e

# Color codes
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration (customizable via environment variables)
FLUTTER_HOME="${FLUTTER_HOME:-$HOME/flutter}"
# This script lives in scripts/; the repo root is its parent.
PROJECT_ROOT="${PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
ADB="${ADB:-}"
# The single device serial install/launch target (resolved by select_device). Seed it from
# --device or the standard ANDROID_SERIAL env var so an explicit choice always wins.
TARGET_SERIAL="${ANDROID_SERIAL:-}"
# Android SDK. Honor the standard env vars first, then fall back to the layout
# Android Studio uses on Linux (~/Android/Sdk); override with ANDROID_SDK if yours differs.
ANDROID_SDK="${ANDROID_SDK:-${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}}"

# App identity — override these to rename the app before a full release, e.g.:
#   GHOST_APP_ID=org.example.chat GHOST_APP_NAME="My Chat" ./install-android-app.sh
# NOTE: a different GHOST_APP_ID is a different app to Android — it installs alongside
# the old one instead of upgrading it (uninstall the old id manually).
GHOST_APP_ID="${GHOST_APP_ID:-app.nightdrop}"
GHOST_APP_NAME="${GHOST_APP_NAME:-Night Drop}"
# Gradle picks these up as project properties (build.gradle.kts reads appId/appName).
export ORG_GRADLE_PROJECT_appId="$GHOST_APP_ID"
export ORG_GRADLE_PROJECT_appName="$GHOST_APP_NAME"

# Functions
log_info() {
    echo -e "${BLUE}ℹ${NC} $1"
}

log_success() {
    echo -e "${GREEN}✓${NC} $1"
}

log_error() {
    echo -e "${RED}✗${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}⚠${NC} $1"
}

print_header() {
    echo ""
    echo "╔════════════════════════════════════════════════════════════════╗"
    echo "║ $1" $(printf "%*s" $((62 - ${#1})) "")
    echo "╚════════════════════════════════════════════════════════════════╝"
    echo ""
}

find_adb() {
    # Check if adb is in PATH
    if command -v adb &>/dev/null; then
        echo "$(command -v adb)"
        return 0
    fi

    # Common Android SDK locations. Linux is case-sensitive, so cover the capitalization
    # variants different installers use (Android Studio: ~/Android/sdk; some setups:
    # ~/Android/Sdk or ~/android-sdk). ANDROID_HOME/ANDROID_SDK_ROOT are honored first.
    local common_paths=(
        "$ANDROID_SDK/platform-tools/adb"
        "${ANDROID_HOME:-}/platform-tools/adb"
        "${ANDROID_SDK_ROOT:-}/platform-tools/adb"
        "$HOME/Android/sdk/platform-tools/adb"
        "$HOME/Android/Sdk/platform-tools/adb"
        "$HOME/android-sdk/platform-tools/adb"
        "/opt/android-sdk/platform-tools/adb"
        "/usr/local/android-sdk/platform-tools/adb"
    )

    for path in "${common_paths[@]}"; do
        if [ -f "$path" ] && [ -x "$path" ]; then
            echo "$path"
            return 0
        fi
    done

    return 1
}

setup_adb() {
    print_header "Setting Up ADB"

    # Try to find adb
    if adb_path=$(find_adb); then
        ADB="$adb_path"
        log_success "Found adb at: $ADB"
        return 0
    fi

    log_error "adb not found"
    echo ""
    echo "Please install Android SDK or set ADB environment variable:"
    echo ""
    echo "  export ADB=/path/to/android-sdk/platform-tools/adb"
    echo ""
    echo "Common install paths:"
    echo "  - ~/Android/sdk/platform-tools/adb   (Android Studio default)"
    echo "  - ~/Android/Sdk/platform-tools/adb"
    echo "  - ~/android-sdk/platform-tools/adb"
    echo ""
    echo "Or download from: https://developer.android.com/tools/releases/platform-tools"
    echo ""
    return 1
}

check_flutter() {
    print_header "Checking Flutter"

    if [ ! -d "$FLUTTER_HOME" ]; then
        log_error "Flutter not found at: $FLUTTER_HOME"
        echo ""
        echo "Set FLUTTER_HOME environment variable:"
        echo "  export FLUTTER_HOME=/path/to/flutter"
        echo ""
        return 1
    fi

    FLUTTER="$FLUTTER_HOME/bin/flutter"
    if [ ! -x "$FLUTTER" ]; then
        log_error "flutter executable not found at: $FLUTTER"
        return 1
    fi

    log_success "Flutter found at: $FLUTTER"

    # Check Flutter version
    flutter_version=$("$FLUTTER" --version 2>/dev/null | head -1)
    log_info "$flutter_version"

    return 0
}

check_project() {
    print_header "Checking Project"

    if [ ! -d "$PROJECT_ROOT" ]; then
        log_error "Project not found at: $PROJECT_ROOT"
        return 1
    fi

    log_success "Project root: $PROJECT_ROOT"

    # Check for app directory
    if [ ! -d "$PROJECT_ROOT/app" ]; then
        log_error "app/ directory not found"
        return 1
    fi

    log_success "app/ directory found"

    # Check for pubspec.yaml
    if [ ! -f "$PROJECT_ROOT/app/pubspec.yaml" ]; then
        log_error "pubspec.yaml not found in app/"
        return 1
    fi

    log_success "pubspec.yaml found"

    return 0
}

check_device() {
    print_header "Checking Connected Device"

    # List devices. `|| true`: grep exits 1 when no devices are attached, which would
    # kill the whole script under `set -e` before the helpful error below prints.
    devices=$("$ADB" devices 2>/dev/null | tail -n +2 | grep -v '^$' || true)

    if [ -z "$devices" ]; then
        log_error "No Android devices found"
        echo ""
        echo "Connect an Android device via USB or adb wireless:"
        echo ""
        echo "  USB: Connect via USB cable"
        echo "  WiFi: $0 --wireless 192.168.X.X 5555"
        echo ""
        return 1
    fi

    log_success "Device(s) found:"
    echo "$devices" | while read -r device status; do
        [ -z "$device" ] && continue
        if [ "$status" = "device" ]; then
            log_success "  $device (connected)"
        else
            log_warn "  $device ($status)"
        fi
    done

    # Check for at least one connected device
    connected=$("$ADB" devices 2>/dev/null | grep -c "device$" || true)
    if [ "$connected" -eq 0 ]; then
        log_error "No devices in 'device' state"
        return 1
    fi

    log_success "$connected device(s) ready for install"
    return 0
}

# Resolve exactly ONE device serial to install onto, into TARGET_SERIAL. Handles the common
# "adb: more than one device/emulator" case caused by wireless debugging, which registers a
# single phone once per transport (a manual IP:PORT connection PLUS one or more mDNS
# `_adb-tls-connect` entries). Logic:
#   1. An explicit choice (--device / ANDROID_SERIAL) always wins, if it's connected.
#   2. One connected device → use it.
#   3. Several entries that are the SAME physical phone (same model:+device:) → auto-pick one
#      (preferring a stable IP:PORT entry over an ephemeral mDNS name).
#   4. Genuinely different devices → stop and ask the user to pick.
# `adb devices` is TAB-separated (serial<TAB>state), and an mDNS serial can itself contain a
# space, so we split on tab — never on general whitespace — when reading serials.
select_device() {
    # 1. Explicit target from --device / ANDROID_SERIAL.
    if [ -n "$TARGET_SERIAL" ]; then
        if "$ADB" devices | awk -F'\t' '$2=="device"{print $1}' | grep -qxF "$TARGET_SERIAL"; then
            log_success "Target device: $TARGET_SERIAL (explicit)"
            return 0
        fi
        log_error "Requested device '$TARGET_SERIAL' is not connected/ready. Connected:"
        "$ADB" devices -l | tail -n +2 | grep -v '^$' || true
        return 1
    fi

    # 2. All serials currently in the 'device' state.
    local serials=()
    while IFS= read -r s; do [ -n "$s" ] && serials+=("$s"); done \
        < <("$ADB" devices | awk -F'\t' '$2=="device"{print $1}')
    if [ "${#serials[@]}" -eq 0 ]; then
        log_error "No devices in 'device' state"
        return 1
    fi
    if [ "${#serials[@]}" -eq 1 ]; then
        TARGET_SERIAL="${serials[0]}"
        log_success "Target device: $TARGET_SERIAL"
        return 0
    fi

    # 3. Multiple entries — same physical phone? Fingerprint by model:+device: (order-independent).
    local fingerprints=()
    while IFS= read -r fp; do [ -n "$fp" ] && fingerprints+=("$fp"); done < <(
        "$ADB" devices -l | tail -n +2 | awk 'NF && $2=="device"{
            m=""; d="";
            for (i=1; i<=NF; i++) { if ($i ~ /^model:/) m=$i; if ($i ~ /^device:/) d=$i }
            print m"|"d
        }' | sort -u
    )
    if [ "${#fingerprints[@]}" -le 1 ]; then
        # One phone over several transports: prefer a stable IP:PORT serial, else take the first.
        local pick=""
        for s in "${serials[@]}"; do
            if [[ "$s" =~ ^[0-9].*:[0-9]+$ ]]; then pick="$s"; break; fi
        done
        [ -z "$pick" ] && pick="${serials[0]}"
        TARGET_SERIAL="$pick"
        log_warn "One device seen over ${#serials[@]} transports (wireless debugging) — using: $TARGET_SERIAL"
        return 0
    fi

    # 4. Genuinely different devices — the user must choose.
    log_error "Multiple different devices connected — choose one:"
    "$ADB" devices -l | tail -n +2 | grep -v '^$' || true
    echo ""
    echo "  Re-run with:  $0 --device <serial>     (or set ANDROID_SERIAL=<serial>)"
    return 1
}

# Make sure the Flutter/Gradle build can find the Android SDK. adb detection is
# separate from this — `flutter build apk` needs ANDROID_HOME/ANDROID_SDK_ROOT set,
# so export them (deriving the SDK root from a found adb if the default is missing).
export_android_sdk() {
    local sdk="$ANDROID_SDK"
    if [ ! -d "$sdk" ]; then
        # Fall back to the SDK root implied by whichever adb we located.
        local adb_path
        if adb_path=$(find_adb); then
            sdk="$(cd "$(dirname "$adb_path")/.." && pwd)"
        fi
    fi
    if [ -d "$sdk" ]; then
        export ANDROID_HOME="$sdk"
        export ANDROID_SDK_ROOT="$sdk"
        log_success "Android SDK: $sdk"
    else
        log_warn "Android SDK dir not found; flutter build may fail (set ANDROID_SDK)"
    fi
}

build_apk() {
    print_header "Building Android APK ($BUILD_MODE, Tor mode)"

    cd "$PROJECT_ROOT/app"

    export_android_sdk

    log_info "Running: flutter pub get"
    "$FLUTTER" pub get

    log_warn "First build may take several minutes (Rust cross-compile)"

    # Tor mode + relay are compile-time --dart-define values: Android apps can't read
    # runtime env vars, so the relay's .onion must be baked into the APK.
    local defines=(--dart-define=GHOST_TOR=1)
    if [ -f "$PROJECT_ROOT/relay-state/onion" ]; then
        RELAY_ADDR=$(cat "$PROJECT_ROOT/relay-state/onion")
        defines+=("--dart-define=GHOST_RELAY=$RELAY_ADDR")
        log_success "Relay baked in for store-and-forward: $RELAY_ADDR"
    else
        log_warn "Relay not found (relay-state/onion) - P2P only, no store-and-forward"
    fi
    if [ "$BUILD_MODE" == "release" ] && [ ! -f "$PROJECT_ROOT/app/android/key.properties" ]; then
        log_warn "No android/key.properties: release APK will be DEBUG-signed (local use only)"
    fi

    if "$FLUTTER" build apk "--$BUILD_MODE" "${defines[@]}" 2>&1; then
        APK_PATH="$PROJECT_ROOT/app/build/app/outputs/flutter-apk/app-$BUILD_MODE.apk"
        if [ -f "$APK_PATH" ]; then
            log_success "APK built: $APK_PATH"
            # Guard: the Rust security core MUST be in the APK's jniLibs. A native-lib rename
            # or a stale build can silently drop it, yielding an app that crashes at FFI init.
            if command -v unzip >/dev/null && ! unzip -l "$APK_PATH" 2>/dev/null | grep -qE 'lib/[^/]+/libnightdrop\.so'; then
                log_error "libnightdrop.so is missing from the APK — the Rust core wasn't bundled."
                log_error "Do a CLEAN build:  (cd app && flutter clean) && $0${TARGET_SERIAL:+ --device $TARGET_SERIAL}"
                return 1
            fi
            log_success "core lib present in APK: libnightdrop.so"
            return 0
        fi
    fi

    log_error "APK build failed"
    return 1
}

install_apk() {
    print_header "Installing APK on Device"

    if [ ! -f "$APK_PATH" ]; then
        log_error "APK not found at: $APK_PATH"
        return 1
    fi

    log_info "Installing app${TARGET_SERIAL:+ on $TARGET_SERIAL}..."
    # -s targets the single resolved device so multiple adb transports (wireless debugging)
    # don't trigger "more than one device/emulator". Array keeps quoting correct (incl. an
    # empty target, or a serial containing a space).
    local -a tgt=()
    [ -n "$TARGET_SERIAL" ] && tgt=(-s "$TARGET_SERIAL")
    if "$ADB" "${tgt[@]}" install -r "$APK_PATH"; then
        log_success "App installed successfully"
        return 0
    else
        log_error "Installation failed"
        return 1
    fi
}

launch_app() {
    print_header "Launching App"

    log_info "Launching Night Drop..."
    local -a tgt=()
    [ -n "$TARGET_SERIAL" ] && tgt=(-s "$TARGET_SERIAL")
    if "$ADB" "${tgt[@]}" shell monkey -p "$GHOST_APP_ID" -c android.intent.category.LAUNCHER 1 2>/dev/null; then
        log_success "App launched"
        return 0
    else
        log_warn "Could not auto-launch app"
        log_info "Launch manually: Open Night Drop on your device"
        return 0
    fi
}

show_help() {
    cat << 'EOF'
╔════════════════════════════════════════════════════════════════╗
║ Android App Installer for Night Drop
╚════════════════════════════════════════════════════════════════╝

USAGE
  install-android-app.sh [OPTIONS]

OPTIONS
  (none)              Build APK (debug, Tor + relay baked in) and install on device
  --release           Build/install the RELEASE APK instead of debug (combinable with
                      the options below). Needs android/key.properties for real
                      signing; without it the release APK is debug-signed.
  --wireless IP PORT  Connect via adb wireless first, then build/install
  --device SERIAL     Install to this exact device serial (from --list-devices). Needed only
                      when several DIFFERENT devices are attached; a single phone showing up
                      over multiple transports (wireless debugging) is auto-resolved.
  --build-only        Build APK without installing
  --install-only      Install existing APK (skip build)
  --list-devices      Show connected devices
  --help              Show this help message

ENVIRONMENT VARIABLES
  FLUTTER_HOME        Flutter SDK path (default: ~/flutter)
  PROJECT_ROOT        Night Drop root (default: this script's directory)
  ADB                 adb command path (auto-detected if not set)
  ANDROID_SERIAL      Device serial to install to (same as --device; standard adb env var)
  ANDROID_SDK         Android SDK path (default: ~/Android/sdk)
  GHOST_APP_ID        Android applicationId (default: app.nightdrop).
                      Change to rename the app identity before a full release.
                      A new id installs ALONGSIDE the old app, not over it.
  GHOST_APP_NAME      Launcher display name (default: Night Drop)

EXAMPLES
  # Build and install
  ./install-android-app.sh

  # Release build, then install
  ./install-android-app.sh --release

  # Connect wireless first
  ./install-android-app.sh --wireless 192.168.88.26 33603

  # Force a specific device (only needed with multiple DIFFERENT devices attached)
  ./install-android-app.sh --device 10.0.0.49:37193

  # Build only (skip install)
  ./install-android-app.sh --build-only

  # List connected devices
  ./install-android-app.sh --list-devices

QUICK START
  1. Connect device via USB
  2. Run: ./install-android-app.sh
  3. Wait for build and install
  4. App opens automatically on device

EOF
}

main() {
    # Pre-scan for position-independent options (--release, --device SERIAL); other args keep
    # their meaning and order (e.g. --wireless IP PORT).
    BUILD_MODE="debug"
    local args=()
    while [ $# -gt 0 ]; do
        case "$1" in
            --release) BUILD_MODE="release" ;;
            --device)
                if [ -z "${2:-}" ]; then
                    log_error "--device needs a serial (see: $0 --list-devices)"
                    exit 1
                fi
                TARGET_SERIAL="$2"
                shift
                ;;
            *) args+=("$1") ;;
        esac
        shift
    done
    set -- "${args[@]}"

    # Parse arguments
    case "${1:-}" in
        --help)
            show_help
            exit 0
            ;;
        --list-devices)
            setup_adb || exit 1
            print_header "Connected Devices"
            "$ADB" devices -l
            exit 0
            ;;
        --wireless)
            if [ -z "$2" ] || [ -z "$3" ]; then
                log_error "Usage: $0 --wireless IP PORT"
                exit 1
            fi
            setup_adb || exit 1
            print_header "Connecting via ADB Wireless"
            log_info "Connecting to $2:$3"
            "$ADB" connect "$2:$3"
            sleep 2
            ;;
        --build-only)
            check_flutter || exit 1
            check_project || exit 1
            build_apk || exit 1
            exit 0
            ;;
        --install-only)
            setup_adb || exit 1
            check_device || exit 1
            select_device || exit 1
            APK_PATH="$PROJECT_ROOT/app/build/app/outputs/flutter-apk/app-$BUILD_MODE.apk"
            install_apk || exit 1
            exit 0
            ;;
    esac

    # Full workflow
    print_header "Android App Installer"
    log_info "Building and installing Night Drop"
    echo ""

    setup_adb || exit 1
    check_flutter || exit 1
    check_project || exit 1
    check_device || exit 1
    select_device || exit 1
    build_apk || exit 1
    install_apk || exit 1
    launch_app

    print_header "Complete! ✓"
    log_success "Night Drop installed and running on your device"
    echo ""
}

# Run main
main "$@"
