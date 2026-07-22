#!/usr/bin/env bash
# Deploy the Night Drop static website to the VPS. Idempotent — run it whenever the site
# changes. It rsyncs website/ to the server's web root and reloads nginx. Everything in
# website/ is public (marketing pages, the clearsigned security.txt, the PUBLIC pgp key).
#
# It does NOT move the onion key or set up nginx/tor/mail — those are one-time steps, see
# docs/hosting.md and deploy/{nginx-nightdrop.conf,onion-torrc}.
#
# Usage:
#   scripts/deploy-vps.sh user@host [REMOTE_ROOT]
#     REMOTE_ROOT   web root on the server (default: /var/www/nightdrop)
#
# Prereqs on the server: nginx configured (deploy/nginx-nightdrop.conf), and your SSH user
# able to write REMOTE_ROOT (or use a deploy user that owns it) and to `sudo systemctl reload nginx`.

set -euo pipefail

TARGET="${1:-}"
REMOTE_ROOT="${2:-/var/www/nightdrop}"
if [ -z "$TARGET" ]; then
  echo "usage: $0 user@host [REMOTE_ROOT]" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"
WEB="$REPO/website"
[ -f "$WEB/index.html" ] || { echo "error: no website at $WEB" >&2; exit 1; }

# Make sure config.js (donation addresses / copy / links) is regenerated from the shared config.
if command -v make >/dev/null 2>&1 && [ -f "$REPO/Makefile" ]; then
  echo "regenerating website/config.js from config/app_config.json ..."
  make -C "$REPO" config >/dev/null 2>&1 || echo "  (make config skipped/failed; deploying current config.js)"
fi

echo "deploying $WEB/ -> $TARGET:$REMOTE_ROOT/"
# --delete keeps the server a mirror of website/. Never copies VCS/editor cruft.
rsync -az --delete \
  --exclude '.git' --exclude '*.swp' --exclude '.DS_Store' \
  "$WEB/" "$TARGET:$REMOTE_ROOT/"

echo "reloading nginx on $TARGET ..."
# shellcheck disable=SC2029  # we intend the command to expand locally into the remote shell.
ssh "$TARGET" 'sudo nginx -t && sudo systemctl reload nginx' \
  || echo "  note: couldn't reload nginx remotely — reload it yourself: sudo systemctl reload nginx"

echo "done. Verify:  curl -I https://nightdrop.app/  and  the .onion in Tor Browser."
