#!/bin/bash
#
# Build the Night Drop Linux desktop bundle (Tor + relay baked in) and install it as a
# proper desktop app: a stable copy of the bundle, a .desktop launcher, and themed icons.
# After this the app appears in the application menu and launches from the taskbar WITH
# its icon under both X11 and Wayland.
#
# Why it shows the icon on both:
#   • The .desktop basename (app.nightdrop) == the app's Wayland app_id
#     (GApplication application-id / g_set_prgname in linux/runner/my_application.cc),
#     so Wayland compositors (GNOME, KWin) bind the window to this entry.
#   • StartupWMClass=app.nightdrop matches the X11 WM_CLASS for the same binding.
#   • Icon=app.nightdrop resolves in the hicolor theme (installed below), which also
#     satisfies gtk_window_set_icon_name(APPLICATION_ID) in the runner.
#
# Usage:
#   scripts/install-desktop-app.sh              # build release bundle, then install entry + icons
#   scripts/install-desktop-app.sh --no-build   # install using the existing build/ bundle
#   scripts/install-desktop-app.sh --run        # ...and launch it afterwards
#   scripts/install-desktop-app.sh --diag       # build with opt-in pairing/transport diagnostics
#                                               # (protocol outcomes only, never keys or codes)
#   scripts/install-desktop-app.sh --uninstall  # remove the installed app, entry, and icons

set -euo pipefail

# This script lives in scripts/; the repo root is its parent.
PROJECT_ROOT="${PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
FLUTTER_HOME="${FLUTTER_HOME:-$HOME/flutter}"
APP_DIR="$PROJECT_ROOT/app"
APP_ID="${NIGHTDROP_APP_ID:-app.nightdrop}"       # Wayland app_id / .desktop basename / icon name
APP_NAME="${NIGHTDROP_APP_NAME:-Night Drop}"      # launcher display name
# The Linux CMake reads NIGHTDROP_APP_ID for the desktop application id; export both so a
# rename is picked up at build time (mirrors install-android-app.sh).
export NIGHTDROP_APP_ID="$APP_ID" NIGHTDROP_APP_NAME="$APP_NAME"
BUNDLE_SRC="$APP_DIR/build/linux/x64/release/bundle"
PKG_DIR="$APP_DIR/linux/packaging"            # holds the icon master ($APP_ID.png)

# Per-user install locations (no sudo). XDG-correct.
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
INSTALL_DIR="$HOME/.local/lib/nightdrop"           # the bundle (binary + lib/ + data/)
APPS_DIR="$DATA_HOME/applications"
ICONS_DIR="$DATA_HOME/icons/hicolor"
DESKTOP_FILE="$APPS_DIR/$APP_ID.desktop"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
info(){ echo -e "${BLUE}ℹ${NC} $1"; }
ok(){ echo -e "${GREEN}✓${NC} $1"; }
warn(){ echo -e "${YELLOW}⚠${NC} $1"; }
err(){ echo -e "${RED}✗${NC} $1" >&2; }

DO_BUILD=1; DO_RUN=0; ACTION=install; DIAG=0
for a in "$@"; do case "$a" in
  --no-build) DO_BUILD=0 ;;
  --run) DO_RUN=1 ;;
  --diag) DIAG=1 ;;
  --uninstall) ACTION=uninstall ;;
  # Print the header block, stopping at the first non-comment line — a fixed line range silently
  # starts printing shell code the moment the header grows.
  -h|--help) awk 'NR>1 && !/^#/{exit} NR>1{sub(/^# ?/,""); print}' "$0"; exit 0 ;;
  *) err "unknown option: $a"; exit 1 ;;
esac; done

refresh_caches() {
  # Best-effort; the app still works if these tools are missing, icons just take longer
  # to appear.
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$ICONS_DIR" 2>/dev/null || true
  command -v update-desktop-database >/dev/null && update-desktop-database -q "$APPS_DIR" 2>/dev/null || true
}

uninstall() {
  rm -rf "$INSTALL_DIR"
  rm -f "$DESKTOP_FILE" "$HOME/.local/bin/nightdrop"
  for s in 16 32 48 64 128 256 512; do rm -f "$ICONS_DIR/${s}x${s}/apps/$APP_ID.png"; done
  refresh_caches
  ok "Uninstalled Night Drop desktop entry, icons, and bundle."
  exit 0
}
[ "$ACTION" = uninstall ] && uninstall

# 1. Build the release bundle (Tor + relay baked in) unless told to reuse it.
#    NIGHTDROP_TOR enables the embedded Tor transport; NIGHTDROP_RELAY (the relay's .onion, from
#    relay-state/onion) enables rendezvous short codes + offline store-and-forward. Both are
#    baked in as compile-time --dart-define values, so the bundle needs no runtime env.
if [ "$DO_BUILD" -eq 1 ]; then
  [ -x "$FLUTTER_HOME/bin/flutter" ] || { err "Flutter not found at $FLUTTER_HOME/bin/flutter (set FLUTTER_HOME)"; exit 1; }
  DART_DEFINES=(--dart-define=NIGHTDROP_TOR=1)
  if [ "$DIAG" = 1 ]; then
    DART_DEFINES+=(--dart-define=NIGHTDROP_DIAG=1)
    ok "diagnostics ON — protocol outcomes to stderr (no keys/codes/addresses)"
  fi
  if [ -f "$PROJECT_ROOT/relay-state/onion" ]; then
    RELAY_ADDR="$(cat "$PROJECT_ROOT/relay-state/onion")"
    DART_DEFINES+=("--dart-define=NIGHTDROP_RELAY=$RELAY_ADDR")
    ok "relay baked in for store-and-forward: $RELAY_ADDR"
  else
    info "relay not found (relay-state/onion) — P2P only, no store-and-forward"
  fi
  info "Building release bundle…"
  ( cd "$APP_DIR" && export PATH="$FLUTTER_HOME/bin:$PATH" && flutter build linux --release "${DART_DEFINES[@]}" )
fi
[ -x "$BUNDLE_SRC/night_drop" ] || { err "bundle not found at $BUNDLE_SRC (run without --no-build)"; exit 1; }

# Guard: the Rust security core MUST be bundled. After a native-lib rename an incremental
# Flutter build can silently drop libnightdrop.so from the bundle (seen during the 2026-07
# nightdrop_core->nightdrop rename), leaving an app that crashes at FFI init. Fail loudly.
if [ ! -f "$BUNDLE_SRC/lib/libnightdrop.so" ]; then
  err "libnightdrop.so is missing from the bundle ($BUNDLE_SRC/lib/) — the Rust core wasn't bundled."
  err "Do a CLEAN build:  (cd app && flutter clean) && ./install-desktop-app.sh"
  exit 1
fi
ok "core lib present: libnightdrop.so ($(du -h "$BUNDLE_SRC/lib/libnightdrop.so" | cut -f1))"

# 2. Install the bundle to a stable location (keeps night_drop next to its lib/ + data/,
#    which its $ORIGIN/lib RPATH and asset loader require).
info "Installing bundle → $INSTALL_DIR"
rm -rf "$INSTALL_DIR"; mkdir -p "$INSTALL_DIR"
cp -a "$BUNDLE_SRC/." "$INSTALL_DIR/"
ok "bundle installed"

# 3. Install themed icons at standard hicolor sizes (downscaled from the 512 master).
MASTER="$PKG_DIR/$APP_ID.png"
[ -f "$MASTER" ] || { err "master icon missing: $MASTER"; exit 1; }
CONVERT="$(command -v magick || command -v convert || true)"
for s in 16 32 48 64 128 256 512; do
  dst="$ICONS_DIR/${s}x${s}/apps/$APP_ID.png"; mkdir -p "$(dirname "$dst")"
  if [ -n "$CONVERT" ]; then "$CONVERT" "$MASTER" -resize "${s}x${s}" "$dst"; else cp "$MASTER" "$dst"; fi
done
ok "icons → $ICONS_DIR/*/apps/$APP_ID.png (16–512)"

# 4a. Drop a `nightdrop` launcher on PATH so the app can be started by name from a terminal.
mkdir -p "$HOME/.local/bin"
cat > "$HOME/.local/bin/nightdrop" <<EOF
#!/bin/sh
exec "$INSTALL_DIR/night_drop" "\$@"
EOF
chmod 755 "$HOME/.local/bin/nightdrop"
ok "launcher → ~/.local/bin/nightdrop"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) warn "~/.local/bin is not on PATH — add it so 'nightdrop' works from a terminal";; esac

# 4b. Generate the .desktop menu entry (self-contained — no separate template file to
#     accidentally launch). Exec is pinned to the absolute installed binary so the
#     app-menu launch is PATH-independent. The file's basename MUST equal the app's
#     Wayland app_id (application-id / g_set_prgname) so compositors bind the window to
#     this entry; StartupWMClass does the same for X11; Icon is a hicolor theme name
#     that resolves under both sessions (and matches gtk_window_set_icon_name).
mkdir -p "$APPS_DIR"
cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Version=1.5
Name=$APP_NAME
GenericName=Private Messenger
Comment=Anonymous, end-to-end encrypted 1:1 chat over Tor
Exec=$INSTALL_DIR/night_drop
Icon=$APP_ID
Terminal=false
StartupNotify=true
StartupWMClass=$APP_ID
Categories=Network;InstantMessaging;Chat;Security;
Keywords=chat;messenger;tor;private;encrypted;anonymous;messaging;
EOF
chmod 644 "$DESKTOP_FILE"
ok "desktop entry → $DESKTOP_FILE"

# 5. Refresh caches and validate.
refresh_caches
if command -v desktop-file-validate >/dev/null; then
  if desktop-file-validate "$DESKTOP_FILE"; then ok "desktop-file-validate: clean"; else warn "desktop-file-validate reported issues (above)"; fi
fi

echo
ok "Night Drop installed. Find it in your app launcher (may take a few seconds to appear)."
info "Session: ${XDG_SESSION_TYPE:-unknown} — icon binds via app_id/StartupWMClass=$APP_ID."

if [ "$DO_RUN" -eq 1 ]; then
  info "Launching…"
  if command -v gtk-launch >/dev/null; then gtk-launch "$APP_ID"; else nohup "$INSTALL_DIR/night_drop" >/tmp/nightdrop-desktop.log 2>&1 & fi
fi
exit 0
