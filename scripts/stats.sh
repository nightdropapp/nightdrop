#!/usr/bin/env bash
# Private project stats — for the maintainer only. Nothing here is exposed to visitors and nothing
# tracks individuals: GitHub's numbers are owner-only aggregates, and the optional website summary
# is computed from your own server log (over Tor every client is 127.0.0.1, so it's counts, not
# identities). Shows:
#   * GitHub repo traffic — views, unique visitors, clones, referrers (owner-only, 14-day window)
#   * GitHub Release download counts, per file
#   * (optional) website visits from a server access log
#
# Requires: gh (authenticated: `gh auth login`). Optional: goaccess for a full web report.
#
# Usage:
#   scripts/stats.sh                              # GitHub traffic + download counts
#   scripts/stats.sh /var/log/nginx/access.log   # ...plus a summary of that access log
#   NIGHTDROP_ACCESS_LOG=… scripts/stats.sh       # same via env var
set -euo pipefail

REPO="${NIGHTDROP_REPO:-nightdropapp/nightdrop}"
sec() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

command -v gh >/dev/null 2>&1 || { echo "error: gh (GitHub CLI) not found"; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "error: gh not authenticated — run: gh auth login"; exit 1; }

sec "GitHub repo traffic — $REPO (last 14 days, owner-only)"
gh api "repos/$REPO/traffic/views"  --jq '"  views:  \(.count)   unique visitors: \(.uniques)"' 2>/dev/null || echo "  (traffic needs push access to the repo)"
gh api "repos/$REPO/traffic/clones" --jq '"  clones: \(.count)   unique cloners:  \(.uniques)"' 2>/dev/null || true
gh api "repos/$REPO" --jq '"  stars:  \(.stargazers_count)   forks: \(.forks_count)   watchers: \(.subscribers_count)"' 2>/dev/null || true
echo "  referrers (how visitors reach the repo):"
gh api "repos/$REPO/traffic/popular/referrers" --jq '.[] | "    \(.referrer): \(.count) views (\(.uniques) uniq)"' 2>/dev/null || echo "    (none yet)"

sec "Release download counts"
gh api "repos/$REPO/releases" \
  --jq '.[] | "  \(.tag_name):", (.assets[] | "    \(.name): \(.download_count) downloads")' 2>/dev/null \
  || echo "  (no releases yet)"

LOG="${NIGHTDROP_ACCESS_LOG:-${1:-}}"
if [ -n "$LOG" ]; then
  if [ ! -f "$LOG" ]; then
    echo; echo "  (access log not found: $LOG)"; exit 0
  fi
  sec "Website visits — $LOG"
  total=$(wc -l < "$LOG" | tr -d ' ')
  addrs=$(awk '{print $1}' "$LOG" | sort -u | wc -l | tr -d ' ')
  # Page loads ≈ visits: requests for "/" or an .html doc (not the CSS/JS/images each visit also
  # pulls). Field 7 is the request path in both nginx/apache-combined and Python http.server logs.
  pages=$(awk '$7=="/" || $7 ~ /\.html($|\?)/' "$LOG" | wc -l | tr -d ' ')
  echo "  page loads (≈ visits): $pages"
  echo "  total requests: $total    distinct client addresses: $addrs"
  echo "  (behind Tor every client is 127.0.0.1, so 'distinct addresses' is 1 — counts, never identities)"
  if command -v goaccess >/dev/null 2>&1; then
    OUT="${NIGHTDROP_STATS_HTML:-/tmp/nightdrop-site-stats.html}"
    if goaccess "$LOG" -o "$OUT" --log-format=COMBINED --no-global-config >/dev/null 2>&1; then
      echo "  full private report → $OUT (open locally in a browser)"
    fi
  else
    echo "  (install goaccess for a full breakdown: sudo dnf install goaccess)"
  fi
fi
