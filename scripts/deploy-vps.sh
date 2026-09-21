#!/usr/bin/env bash
# Deploy the Night Drop static website to the VPS. Idempotent — run it whenever the site
# changes. It rsyncs website/ to the server's web root and reloads nginx. Everything in
# website/ is public (marketing pages, the clearsigned security.txt, the PUBLIC pgp key).
#
# It does NOT install nginx, obtain the TLS certificate, or move the onion key — those are
# one-time steps: see docs/hosting.md and deploy/{nginx-nightdrop.conf,onion-torrc}.
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

# /.well-known/security.txt advertises `Policy: https://nightdrop.app/SECURITY.md`, so that path
# has to resolve. It would 404: the policy lives at the repo root and the web root is website/.
# Copied at deploy time rather than committed as a second copy — a checked-in duplicate goes stale
# the moment the real one is edited, and a security policy that quietly disagrees with the one in
# the repo is worse than a missing one.
#
# This writes into website/, which the onion service serves live from disk, so the onion picks the
# policy up at the same moment. That is intended; it is the same file either way.
if [ -f "$REPO/SECURITY.md" ]; then
  cp -f "$REPO/SECURITY.md" "$WEB/SECURITY.md"
  echo "staged SECURITY.md into the web root (security.txt's Policy: URL)"
else
  echo "warning: $REPO/SECURITY.md missing — https://nightdrop.app/SECURITY.md will 404" >&2
fi

echo "deploying $WEB/ -> $TARGET:$REMOTE_ROOT/"
# --delete keeps the server a mirror of website/. Never copies VCS/editor cruft.
#
# applications/ is excluded on purpose. It is ~260 MB of APKs and AppImages that only the ONION
# mirror ever links to: index.html switches to the same-origin /applications/ paths only when
# location.hostname ends in .onion, and on clearnet config.js points at GitHub Releases instead.
# Shipping it would push a quarter-gigabyte over the wire on every deploy to serve files nothing
# on this host links to, and would put release binaries somewhere that is not the signed download
# path users are told to verify. README.md is developer documentation, and robots.txt lets every
# crawler in.
rsync -az --delete \
  --exclude '.git' --exclude '*.swp' --exclude '.DS_Store' \
  --exclude 'applications' --exclude 'README.md' \
  "$WEB/" "$TARGET:$REMOTE_ROOT/"

echo "reloading nginx on $TARGET ..."
# shellcheck disable=SC2029  # we intend the command to expand locally into the remote shell.
ssh "$TARGET" 'sudo nginx -t && sudo systemctl reload nginx' \
  || echo "  note: couldn't reload nginx remotely — reload it yourself: sudo systemctl reload nginx"

echo "done. Verify:  curl -I https://nightdrop.app/  and  the .onion in Tor Browser."
