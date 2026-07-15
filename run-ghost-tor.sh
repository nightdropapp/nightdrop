#!/bin/bash
#
# Night Drop Desktop App — Tor Mode Launcher / Builder
# Runs or compiles the Flutter desktop app with full Tor P2P support. Tor mode AND the
# relay address (read from relay-state/onion) are baked in as compile-time
# --dart-define values, so the produced binary needs no environment to run.
#
# Usage:
#   ./run-ghost-tor.sh              # Dev run (flutter run, interactive)
#   ./run-ghost-tor.sh --detach     # Dev run in the background
#   ./run-ghost-tor.sh --build      # Compile the RELEASE bundle (Tor + relay baked in)
#   ./run-ghost-tor.sh --build --launch   # Compile the release bundle, then launch it

set -e

# Configuration. PROJECT_ROOT defaults to this script's own directory so the script
# works from any checkout location; both are overridable via the environment.
FLUTTER_HOME="${FLUTTER_HOME:-$HOME/flutter}"
PROJECT_ROOT="${PROJECT_ROOT:-$(cd "$(dirname "$0")" && pwd)}"
APP_DIR="$PROJECT_ROOT/app"

# App identity — override to rename before a full release, mirroring install-android-app.sh:
#   GHOST_APP_ID=org.example.chat GHOST_APP_NAME="My Chat" ./run-ghost-tor.sh --build
# The Linux CMake reads GHOST_APP_ID for the desktop application id (falls back to
# app.nightdrop). The executable name stays "ghost_chat" (internal, set in linux/CMakeLists).
GHOST_APP_ID="${GHOST_APP_ID:-app.nightdrop}"
GHOST_APP_NAME="${GHOST_APP_NAME:-Night Drop}"
export GHOST_APP_ID GHOST_APP_NAME

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Functions
print_header() {
    echo -e "${BLUE}╔════════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${BLUE}║${NC} $1"
    echo -e "${BLUE}╚════════════════════════════════════════════════════════════════╝${NC}"
}

print_success() {
    echo -e "${GREEN}✓${NC} $1"
}

print_error() {
    echo -e "${RED}✗${NC} $1"
}

print_info() {
    echo -e "${YELLOW}ℹ${NC} $1"
}

# Compile-time defines for Tor mode. GHOST_TOR enables the embedded Tor transport;
# GHOST_RELAY (the relay's .onion, from relay-state/onion) enables rendezvous
# short codes + offline store-and-forward. Baked in with --dart-define so both dev
# runs and release bundles behave identically, with no runtime environment needed.
tor_defines() {
    DART_DEFINES=(--dart-define=GHOST_TOR=1)
    RELAY_ADDR=""
    if [ -f "$PROJECT_ROOT/relay-state/onion" ]; then
        RELAY_ADDR=$(cat "$PROJECT_ROOT/relay-state/onion")
        DART_DEFINES+=("--dart-define=GHOST_RELAY=$RELAY_ADDR")
        print_success "Relay baked in for store-and-forward: $RELAY_ADDR"
    else
        print_info "Relay not found (relay-state/onion) - P2P only, no store-and-forward"
    fi
}

check_requirements() {
    print_info "Checking requirements..."
    
    if [ ! -d "$FLUTTER_HOME" ]; then
        print_error "Flutter not found at $FLUTTER_HOME"
        echo "Set FLUTTER_HOME environment variable or install Flutter"
        exit 1
    fi
    
    if [ ! -d "$APP_DIR" ]; then
        print_error "Project not found at $APP_DIR"
        exit 1
    fi
    
    if ! "$FLUTTER_HOME/bin/flutter" --version &>/dev/null; then
        print_error "Flutter is not executable"
        exit 1
    fi
    
    print_success "Flutter found at $FLUTTER_HOME/bin/flutter"
    print_success "Project found at $APP_DIR"
}

build_and_run() {
    print_header "Night Drop — Tor Mode (P2P)"
    
    check_requirements
    
    print_info "Tor mode enables:"
    echo "  • Embedded Tor on your PC (generates .onion address)"
    echo "  • True P2P messaging with your phone"
    echo "  • Full end-to-end encryption (Signal Double Ratchet)"
    echo "  • Works over any network (LTE, WiFi, VPN, etc.)"
    echo ""
    
    print_info "Building and launching app..."
    echo ""
    
    cd "$APP_DIR"
    export PATH="$FLUTTER_HOME/bin:$PATH"
    tor_defines
    "$FLUTTER_HOME/bin/flutter" run -d linux "${DART_DEFINES[@]}"
}

# Compile the release bundle with Tor + relay baked in (the distributable desktop build).
build_release() {
    print_header "Night Drop — Release Build (Tor + Relay)"
    check_requirements
    cd "$APP_DIR"
    export PATH="$FLUTTER_HOME/bin:$PATH"
    tor_defines
    print_info "Building release bundle..."
    "$FLUTTER_HOME/bin/flutter" build linux --release "${DART_DEFINES[@]}"
    BUNDLE="$APP_DIR/build/linux/x64/release/bundle/ghost_chat"
    print_success "Built: $BUNDLE"
    if [ "$1" == "--launch" ]; then
        print_info "Launching..."
        nohup "$BUNDLE" > /tmp/nightdrop-desktop.log 2>&1 &
        print_success "Running (PID $!) — log: /tmp/nightdrop-desktop.log"
    else
        print_info "Launch it with: $BUNDLE"
    fi
}

detached_mode() {
    print_header "Night Drop — Tor Mode (Background)"
    
    check_requirements
    
    print_info "Starting app in background..."
    
    cd "$APP_DIR"
    export PATH="$FLUTTER_HOME/bin:$PATH"
    tor_defines

    # Run in background, redirect to log file
    nohup "$FLUTTER_HOME/bin/flutter" run -d linux "${DART_DEFINES[@]}" \
        > /tmp/ghost-chat-tor.log 2>&1 &

    # NOTE: this is the `flutter run` tool's PID; killing it detaches but may leave the
    # app window running — close the window itself to fully stop.
    PID=$!
    echo $PID > /tmp/ghost-chat-tor.pid
    
    print_success "App started in background (PID: $PID)"
    print_info "Log file: /tmp/ghost-chat-tor.log"
    print_info "To stop: kill $PID"
    echo ""
    print_info "Tailing logs (Ctrl+C to detach):"
    tail -f /tmp/ghost-chat-tor.log
}

# Main
case "$1" in
    --build)
        build_release "$2"
        ;;
    --detach)
        detached_mode
        ;;
    *)
        build_and_run
        ;;
esac
