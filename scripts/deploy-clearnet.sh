#!/usr/bin/env bash
# Deploy the clear-web site (https://nightdrop.app) to THIS machine's nginx web root.
#
# The onion mirror needs no deploy (it serves website/ live from disk); this copies the same
# directory to the clear-web root, minus the binaries. See docs/hosting.md.
#
# It regenerates config.js, stages SECURITY.md (security.txt's Policy: URL), and rsyncs with
# --delete. Two exclusions are deliberate: applications/ (binaries are onion + GitHub only;
# config.js points clear-web visitors at GitHub Releases) and README.md (developer docs).
# nginx serves static files, so no reload is needed.
#
# Usage: scripts/deploy-clearnet.sh [WEB_ROOT]   (default /var/www/nightdrop; must be writable)
set -euo pipefail
ROOT="${1:-/var/www/nightdrop}"
REPO="${PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
WEB="$REPO/website"

[ -f "$WEB/index.html" ] || { echo "error: no website at $WEB" >&2; exit 1; }
[ -d "$ROOT" ] && [ -w "$ROOT" ] || {
  echo "error: $ROOT missing or not writable (one-time setup: docs/hosting.md)" >&2; exit 1; }

echo "regenerating website/config.js from config/app_config.json ..."
make -C "$REPO" config >/dev/null

if [ -f "$REPO/SECURITY.md" ]; then
  cp -f "$REPO/SECURITY.md" "$WEB/SECURITY.md"
  echo "staged SECURITY.md (security.txt's Policy: URL)"
else
  echo "warning: $REPO/SECURITY.md missing — /SECURITY.md will 404" >&2
fi

# No -X/-A: files take the web root's SELinux label (httpd_sys_content_t), which nginx needs.
rsync -rlt --delete --chmod=D755,F644 \
  --exclude '.git' --exclude '*.swp' --exclude '.DS_Store' \
  --exclude 'applications' --exclude 'README.md' \
  "$WEB/" "$ROOT/"
echo "deployed $WEB/ -> $ROOT/ ($(find "$ROOT" -type f | wc -l) files, $(du -sh "$ROOT" | cut -f1))"
