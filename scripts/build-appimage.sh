#!/bin/bash
#
# Build the Night Drop Linux desktop app as a single-file **AppImage** — one executable users
# download, `chmod +x`, and run directly (no install, no extraction), the desktop equivalent of the
# Android APK. Output: website/applications/linux/Night_Drop-x86_64.AppImage.
#
# The AppImage carries the whole Flutter bundle, including the Rust security core (libnightdrop.so).
# It relies on the host having GTK3 (present on essentially every desktop Linux) — GTK is not
# bundled, to keep the file small; everything Night-Drop-specific is inside.
#
# Requirements:
#   * Flutter SDK (FLUTTER_HOME, default ~/flutter) and the Rust toolchain.
#   * appimagetool on PATH (or ~/.local/bin). Get it once:
#       curl -fL -o ~/.local/bin/appimagetool \
#         https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
#       chmod +x ~/.local/bin/appimagetool
#
# Usage:
#   scripts/build-appimage.sh              # build release bundle, then package the AppImage
#   scripts/build-appimage.sh --no-build   # package using the existing build/ bundle
#   scripts/build-appimage.sh --diag       # bake in opt-in diagnostics (protocol outcomes only)

set -euo pipefail

PROJECT_ROOT="${PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
FLUTTER_HOME="${FLUTTER_HOME:-$HOME/flutter}"
APP_DIR="$PROJECT_ROOT/app"
APP_ID="${NIGHTDROP_APP_ID:-app.nightdrop}"
BUNDLE_SRC="$APP_DIR/build/linux/x64/release/bundle"
OUT_DIR="$PROJECT_ROOT/website/applications/linux"
OUT="$OUT_DIR/Night_Drop-x86_64.AppImage"
ICON_SRC="$APP_DIR/linux/packaging/$APP_ID.png"

c()  { printf '\033[0;34mℹ\033[0m %s\n' "$*"; }
ok() { printf '\033[0;32m✓\033[0m %s\n' "$*"; }
err(){ printf '\033[0;31m✗\033[0m %s\n' "$*" >&2; }

DO_BUILD=1; DIAG=0
for a in "$@"; do case "$a" in
  --no-build) DO_BUILD=0 ;;
  --diag) DIAG=1 ;;
  -h|--help) awk 'NR>1 && !/^#/{exit} NR>1{sub(/^# ?/,""); print}' "$0"; exit 0 ;;
  *) err "unknown option: $a"; exit 1 ;;
esac; done

# appimagetool: prefer PATH, then ~/.local/bin. Run under FUSE if available, else self-extract.
AITOOL="$(command -v appimagetool || echo "$HOME/.local/bin/appimagetool")"
[ -x "$AITOOL" ] || { err "appimagetool not found (see the header for the one-line install)"; exit 1; }
AITOOL_RUN=("$AITOOL")
command -v fusermount >/dev/null 2>&1 || command -v fusermount3 >/dev/null 2>&1 \
  || AITOOL_RUN=("$AITOOL" --appimage-extract-and-run)

if [ "$DO_BUILD" = 1 ]; then
  [ -x "$FLUTTER_HOME/bin/flutter" ] || { err "Flutter not found at $FLUTTER_HOME/bin/flutter (set FLUTTER_HOME)"; exit 1; }
  # Same production wiring as the desktop installer: embedded Tor, and the baked-in relay onion so
  # store-and-forward works out of the box.
  DEFINES=(--dart-define=NIGHTDROP_TOR=1)
  if [ -f "$PROJECT_ROOT/relay-state/onion" ]; then
    DEFINES+=("--dart-define=NIGHTDROP_RELAY=$(cat "$PROJECT_ROOT/relay-state/onion")")
    ok "relay baked in: $(cat "$PROJECT_ROOT/relay-state/onion")"
  else
    c "relay not found (relay-state/onion) — P2P only, no store-and-forward"
  fi
  [ "$DIAG" = 1 ] && DEFINES+=(--dart-define=NIGHTDROP_DIAG=1) && ok "diagnostics ON (protocol outcomes only)"
  c "Building Linux release bundle…"
  ( cd "$APP_DIR" && export PATH="$FLUTTER_HOME/bin:$PATH" && flutter build linux --release "${DEFINES[@]}" )
fi

[ -x "$BUNDLE_SRC/night_drop" ] || { err "bundle not found at $BUNDLE_SRC (run without --no-build)"; exit 1; }
# The Rust security core MUST be in the bundle — an incremental Flutter build can silently drop it,
# leaving an app that crashes at FFI init. Fail loudly (same guard as the desktop installer).
[ -f "$BUNDLE_SRC/lib/libnightdrop.so" ] || {
  err "libnightdrop.so missing from the bundle — the Rust core wasn't bundled. Do a CLEAN build:"
  err "  (cd app && flutter clean) && scripts/build-appimage.sh"
  exit 1
}

# Assemble the AppDir. Flutter resolves its data/ and lib/ relative to the executable, so keeping
# night_drop + lib/ + data/ together under usr/bin/ is all it needs.
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
APPDIR="$WORK/NightDrop.AppDir"
mkdir -p "$APPDIR/usr/bin"
cp -a "$BUNDLE_SRC/." "$APPDIR/usr/bin/"

# XDG_DATA_DIRS must include the AppDir's share/ so GTK's icon theme lookup finds the bundled
# hicolor icons — the runner calls gtk_window_set_icon_name(APPLICATION_ID), which resolves by
# theme name, not by path. Without this the window/taskbar icon falls back to a generic one even
# though the AppImage file itself shows the logo (that comes from .DirIcon).
cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export XDG_DATA_DIRS="$HERE/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
exec "$HERE/usr/bin/night_drop" "$@"
EOF
chmod +x "$APPDIR/AppRun"

# The .desktop basename MUST equal the Wayland app_id (application-id / g_set_prgname) so
# compositors bind the window to this entry; StartupWMClass does the same under X11. Kept in
# sync with the entry written by install-desktop-app.sh.
cat > "$APPDIR/$APP_ID.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Night Drop
GenericName=Private Messenger
Comment=Private, anonymous, end-to-end encrypted 1:1 messenger over Tor
Exec=night_drop
Icon=$APP_ID
Terminal=false
StartupNotify=true
StartupWMClass=$APP_ID
Categories=Network;InstantMessaging;Chat;Security;
Keywords=chat;messenger;tor;private;encrypted;anonymous;messaging;
EOF

[ -f "$ICON_SRC" ] || { err "icon not found at $ICON_SRC"; exit 1; }
# Top-level icon: appimagetool requires it next to the .desktop and turns it into .DirIcon,
# which is what file managers and desktop-integration tools show for the AppImage file.
cp "$ICON_SRC" "$APPDIR/$APP_ID.png"
# Themed copies at standard hicolor sizes, resolved at runtime via XDG_DATA_DIRS above.
CONVERT="$(command -v magick || command -v convert || true)"
[ -n "$CONVERT" ] || c "ImageMagick not found — bundling the 512px master at every icon size"
for s in 16 32 48 64 128 256 512; do
  dst="$APPDIR/usr/share/icons/hicolor/${s}x${s}/apps/$APP_ID.png"; mkdir -p "$(dirname "$dst")"
  if [ -n "$CONVERT" ]; then "$CONVERT" "$ICON_SRC" -resize "${s}x${s}" "$dst"; else cp "$ICON_SRC" "$dst"; fi
done
ok "icons → AppDir usr/share/icons/hicolor/*/apps/$APP_ID.png (16–512) + .DirIcon"

mkdir -p "$OUT_DIR"
c "Packaging AppImage…"
ARCH=x86_64 "${AITOOL_RUN[@]}" "$APPDIR" "$OUT" >/dev/null 2>&1 \
  || ARCH=x86_64 "${AITOOL_RUN[@]}" "$APPDIR" "$OUT"   # re-run verbosely on failure
chmod +x "$OUT"

ok "single-file build → $OUT ($(du -h "$OUT" | cut -f1))"
c "Users: download it, then  chmod +x Night_Drop-x86_64.AppImage && ./Night_Drop-x86_64.AppImage"
