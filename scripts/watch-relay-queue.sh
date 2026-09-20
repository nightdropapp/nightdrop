#!/usr/bin/env bash
# Watch the relay's mailbox queue — the only way to observe activity, because the relay itself
# keeps no logs (that is the point: "no server-side keys, no logs"). Reads relay-state/queue.json,
# which the relay rewrites on every post and take, and prints a line whenever a mailbox changes.
#
# Useful for testing cover traffic (#4): with it on, a device posts dummy mail to its OWN mailbox
# on a randomised 5-30min schedule, then drains it on its next poll. You should see a mailbox gain
# an entry and lose it again shortly after, with no message having been sent by anyone.
#
# What you cannot see here, by design: who posted, what it was, or whether it was real or cover.
# Each mailbox handle is a hash of an identity key — stable, but it names nobody.
#
#   scripts/watch-relay-queue.sh [interval_seconds]
set -euo pipefail
STATE="${NIGHTDROP_RELAY_STATE:-$(cd "$(dirname "$0")/.." && pwd)/relay-state}/queue.json"
INTERVAL=${1:-2}
echo "watching $STATE (every ${INTERVAL}s) — Ctrl-C to stop"
prev=""
while true; do
    now=$(python3 - "$STATE" <<'PY' 2>/dev/null || echo ""
import json,sys
try:
    q = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(0)
for h, entries in sorted(q.get("mailboxes", {}).items()):
    print(f"{h} {len(entries)}")
PY
)
    if [ "$now" != "$prev" ]; then
        printf '%s\n' "$(date +%H:%M:%S)"
        if [ -z "$now" ]; then
            echo "    (queue empty — everything drained)"
        else
            printf '    %s\n' "$now"
        fi
        prev="$now"
    fi
    sleep "$INTERVAL"
done
