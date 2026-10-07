#!/bin/bash
#
# Launch an AppImage on a bare Ubuntu 22.04 (Containerfile.test) and check it opens a window — the
# test the AppImage catalog runs (MAINTENANCE.md §11.7). Run it on every release AppImage before
# publishing. Uses a throwaway HOME inside the container: no identity is created or touched.
#
# Usage: scripts/linux-build/test-appimage.sh path/to/Night_Drop-x86_64.AppImage [screenshot.xwd]
#   Exit 0 = alive after 25 s with a "Night Drop" window and no unhandled Dart exception.
#   The optional second argument saves a screenshot (convert with `magick shot.xwd shot.png`).

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
APPIMAGE="$(realpath "${1:?usage: $0 AppImage [screenshot.xwd]}")"
SHOT="${2:-}"
IMAGE=nightdrop-appimage-test:22.04

podman image exists "$IMAGE" || podman build -t "$IMAGE" -f "$HERE/Containerfile.test" "$HERE"

OUT="$(mktemp -d)"; trap 'rm -rf "$OUT"' EXIT
# Labelling off so the container can read the AppImage wherever it lives in the home directory.
podman run --rm --security-opt label=disable \
  -v "$APPIMAGE:/app.AppImage:ro" -v "$OUT:/out" "$IMAGE" bash -c '
    export HOME=/tmp/h; mkdir -p "$HOME"
    Xvfb :99 -screen 0 1280x800x24 >/dev/null 2>&1 & sleep 2; export DISPLAY=:99
    cp /app.AppImage /tmp/a.AppImage && chmod +x /tmp/a.AppImage
    /tmp/a.AppImage --appimage-extract-and-run > /out/app.log 2>&1 & APP=$!
    sleep 25
    kill -0 $APP 2>/dev/null && echo alive > /out/status || echo "died: $(wait $APP; echo $?)" > /out/status
    xwininfo -root -tree > /out/windows
    xwd -root -silent > /out/shot.xwd'

[ -n "$SHOT" ] && cp "$OUT/shot.xwd" "$SHOT"
status="$(cat "$OUT/status")"
window="$(grep -c '"Night Drop"' "$OUT/windows" || true)"
unhandled="$(grep -c 'Unhandled Exception' "$OUT/app.log" || true)"
echo "process: $status | Night Drop windows: $window | unhandled Dart exceptions: $unhandled"
if [ "$status" = alive ] && [ "$window" -ge 1 ] && [ "$unhandled" = 0 ]; then
  echo "PASS"
else
  echo "FAIL — app log:"; cat "$OUT/app.log"; exit 1
fi
