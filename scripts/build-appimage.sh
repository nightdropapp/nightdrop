#!/bin/bash
#
# Build the Night Drop Linux desktop app as a single-file **AppImage** — one executable users
# download, `chmod +x`, and run directly (no install, no extraction), the desktop equivalent of the
# Android APK. Output: website/applications/linux/Night_Drop-x86_64.AppImage.
#
# The AppImage carries the whole Flutter bundle, including the Rust security core (libnightdrop.so).
# It relies on the host having glibc and the standard desktop libraries (GTK3, libsecret, …) — those
# are not bundled, to keep the file small; everything Night-Drop-specific is inside.
#
# PORTABILITY: the bundle is built in an Ubuntu 22.04 container (scripts/linux-build/Containerfile)
# so every native piece needs at most glibc 2.35 and runs on any desktop distro from that era on.
# Built on the dev box itself it needed glibc 2.39 and the box's own FFmpeg, and would not start on
# Ubuntu 22.04 (MAINTENANCE.md §11.7). A portability check runs before packaging either way.
#
# Requirements:
#   * Flutter SDK at the version app/.fvmrc pins (FLUTTER_HOME, default ~/flutter) and the Rust
#     toolchain; both are mounted into the container, never baked into the image.
#   * podman (the container build; skip with --host).
#   * appimagetool on PATH (or ~/.local/bin). Get it once:
#       curl -fL -o ~/.local/bin/appimagetool \
#         https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
#       chmod +x ~/.local/bin/appimagetool
#
# Usage:
#   scripts/build-appimage.sh              # build in the 22.04 container, package, PUBLISH (below)
#   scripts/build-appimage.sh --out FILE   # write the AppImage to FILE instead; publishes nothing
#   scripts/build-appimage.sh --host       # build on this machine (NOT portable; dev only)
#   scripts/build-appimage.sh --no-build   # package using the existing build/ bundle
#   scripts/build-appimage.sh --diag       # bake in opt-in diagnostics (protocol outcomes only)
#
# Without --out the AppImage lands in website/applications/linux/, which the onion site serves
# live: running this IS a deploy (MAINTENANCE.md §12).

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

DO_BUILD=1; DIAG=0; IN_CONTAINER=1; CUSTOM_OUT=""
while [ $# -gt 0 ]; do case "$1" in
  --no-build) DO_BUILD=0 ;;
  --diag) DIAG=1 ;;
  --host) IN_CONTAINER=0 ;;
  --out) shift; CUSTOM_OUT="${1:?--out needs a file path}" ;;
  -h|--help) awk 'NR>1 && !/^#/{exit} NR>1{sub(/^# ?/,""); print}' "$0"; exit 0 ;;
  *) err "unknown option: $1"; exit 1 ;;
esac; shift; done
[ -n "$CUSTOM_OUT" ] && OUT="$(realpath -m "$CUSTOM_OUT")" && OUT_DIR="$(dirname "$OUT")"

# The newest glibc any shipped binary may require: Ubuntu 22.04's. Raising it drops every distro
# older than the new value, so it is a decision, not a side effect of whatever the build box runs.
MAX_GLIBC=2.35
BUILD_IMAGE=nightdrop-linux-build:22.04

# appimagetool: prefer PATH, then ~/.local/bin. Run under FUSE if available, else self-extract.
AITOOL="$(command -v appimagetool || echo "$HOME/.local/bin/appimagetool")"
[ -x "$AITOOL" ] || { err "appimagetool not found (see the header for the one-line install)"; exit 1; }
AITOOL_RUN=("$AITOOL")
command -v fusermount >/dev/null 2>&1 || command -v fusermount3 >/dev/null 2>&1 \
  || AITOOL_RUN=("$AITOOL" --appimage-extract-and-run)

if [ "$DO_BUILD" = 1 ]; then
  [ -x "$FLUTTER_HOME/bin/flutter" ] || { err "Flutter not found at $FLUTTER_HOME/bin/flutter (set FLUTTER_HOME)"; exit 1; }
  # The pinned SDK, not whatever FLUTTER_HOME happens to be: a 3.44.6 build once shipped instead of
  # 3.47.6 because the default pointed at the old SDK (MAINTENANCE.md §8).
  WANT="$(grep -o '"flutter": *"[^"]*"' "$APP_DIR/.fvmrc" | grep -o '[0-9][0-9.]*')"
  HAVE="$(grep -o '"frameworkVersion": *"[^"]*"' "$FLUTTER_HOME/bin/cache/flutter.version.json" 2>/dev/null | grep -o '[0-9][0-9.]*' || true)"
  [ "$WANT" = "$HAVE" ] || { err "Flutter at $FLUTTER_HOME is ${HAVE:-unknown}, app/.fvmrc pins $WANT — set FLUTTER_HOME"; exit 1; }
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
  # A clean build dir, whichever side builds: cargokit and CMake reuse objects whose inputs did not
  # change, so a host-compiled object would carry this machine's glibc into the "portable" result,
  # and the container's CMake cache names compilers this machine does not have.
  rm -rf "$APP_DIR/build/linux"
  if [ "$IN_CONTAINER" = 1 ]; then
    command -v podman >/dev/null || { err "podman not found (or pass --host for a non-portable build)"; exit 1; }
    podman image exists "$BUILD_IMAGE" || {
      c "Building the $BUILD_IMAGE image (once)…"
      podman build -t "$BUILD_IMAGE" -f "$PROJECT_ROOT/scripts/linux-build/Containerfile" \
        "$PROJECT_ROOT/scripts/linux-build"
    }
    RUSTUP="${RUSTUP_HOME:-$HOME/.rustup}"; CARGO="${CARGO_HOME:-$HOME/.cargo}"; PUB="${PUB_CACHE:-$HOME/.pub-cache}"
    c "Building Linux release bundle in $BUILD_IMAGE (glibc ≤ $MAX_GLIBC)…"
    # Same paths inside as outside (package_config.json and CMake caches hold absolute paths), the
    # host's uid (keep-id), and no SELinux relabelling of the host's toolchain dirs.
    podman run --rm --userns=keep-id --security-opt label=disable \
      -v "$PROJECT_ROOT:$PROJECT_ROOT" -v "$FLUTTER_HOME:$FLUTTER_HOME" \
      -v "$CARGO:$CARGO" -v "$RUSTUP:$RUSTUP" -v "$PUB:$PUB" \
      -e HOME=/tmp/home -e PUB_CACHE="$PUB" -e CARGO_HOME="$CARGO" -e RUSTUP_HOME="$RUSTUP" \
      -e PATH="$FLUTTER_HOME/bin:$CARGO/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
      -w "$APP_DIR" "$BUILD_IMAGE" \
      bash -c 'mkdir -p "$HOME" && flutter config --no-analytics >/dev/null 2>&1; flutter build linux --release "$@"' \
      bash "${DEFINES[@]}"
  else
    c "Building Linux release bundle on this machine (NOT portable — dev only)…"
    ( cd "$APP_DIR" && export PATH="$FLUTTER_HOME/bin:$PATH" && flutter build linux --release "${DEFINES[@]}" )
  fi
fi

[ -x "$BUNDLE_SRC/night_drop" ] || { err "bundle not found at $BUNDLE_SRC (run without --no-build)"; exit 1; }
# The Rust security core MUST be in the bundle — an incremental Flutter build can silently drop it,
# leaving an app that crashes at FFI init. Fail loudly (same guard as the desktop installer).
[ -f "$BUNDLE_SRC/lib/libnightdrop.so" ] || {
  err "libnightdrop.so missing from the bundle — the Rust core wasn't bundled. Do a CLEAN build:"
  err "  (cd app && flutter clean) && scripts/build-appimage.sh"
  exit 1
}

# Portability gate: what the bundle needs from the system it lands on. Every binary must make do
# with glibc ≤ MAX_GLIBC, and every library it links must be in the bundle or one a desktop always
# has. Both failures the AppImage catalog caught (FFmpeg 8 linked from this machine, glibc 2.39)
# would have stopped here. Applies to --host builds too: they fail it, which is the point.
SYSTEM_LIBS='^(ld-linux-x86-64\.so\.2|lib(c|m|dl|pthread|rt|gcc_s|stdc\+\+)\.so\.[0-9]+|lib(gtk|gdk)-3\.so\.0|libgdk_pixbuf-2\.0\.so\.0|lib(glib|gobject|gio|gmodule)-2\.0\.so\.0|libpango(cairo)?-1\.0\.so\.0|libcairo(-gobject)?\.so\.2|libatk-1\.0\.so\.0|libharfbuzz\.so\.0|libepoxy\.so\.0|libfontconfig\.so\.1|libsecret-1\.so\.0|libz\.so\.1)$'
bad=0
for f in "$BUNDLE_SRC/night_drop" "$BUNDLE_SRC"/lib/*.so; do
  g="$(objdump -T "$f" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -1 || true)"
  if [ -n "$g" ] && [ "$(printf '%s\n%s\n' "$g" "$MAX_GLIBC" | sort -V | tail -1)" != "$MAX_GLIBC" ]; then
    err "$(basename "$f") needs glibc $g (> $MAX_GLIBC)"; bad=1
  fi
  for n in $(readelf -d "$f" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do
    [ -e "$BUNDLE_SRC/lib/$n" ] && continue
    echo "$n" | grep -Eq "$SYSTEM_LIBS" || { err "$(basename "$f") links $n, which is neither bundled nor a standard desktop library"; bad=1; }
  done
done
[ "$bad" = 0 ] || { err "the bundle is not portable — build in the container (drop --host)"; exit 1; }
ok "portable: glibc ≤ $MAX_GLIBC, links only bundled and standard desktop libraries"

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

# The AppImage lands directly in website/applications/, which the onion service serves live — so
# refresh the signed manifest now rather than leaving SHA256SUMS describing the previous build.
# Not for --out: nothing was published.
if [ -z "$CUSTOM_OUT" ] && [ -x "$PROJECT_ROOT/scripts/deploy-website.sh" ]; then
    "$PROJECT_ROOT/scripts/deploy-website.sh" || c "manifest refresh failed — run scripts/deploy-website.sh by hand"
fi
c "Users: download it, then  chmod +x Night_Drop-x86_64.AppImage && ./Night_Drop-x86_64.AppImage"
