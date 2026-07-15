#!/bin/bash
#
# Sign / rotate the signed relay directory (§3.1) in one step.
#
# Reads the operator's private signing key, auto-bumps the version, signs a new relay-list.json
# listing the relays you pass, writes it to the state dir, and (optionally) restarts the local
# relay so it serves the new list immediately.
#
# The signed list is a FULL REPLACEMENT: pass EVERY relay you want clients to use — omitting one
# removes it on the clients' next poll. Distribute the resulting relay-list.json to EVERY relay's
# state dir (not just one), so clients that can only reach a surviving relay still get updates.
#
# Usage:
#   relay/deploy/sign-directory.sh <relay.onion> [<relay.onion> ...]     # sign a new list
#   relay/deploy/sign-directory.sh --restart <relay.onion> ...           # + restart local relay
#   relay/deploy/sign-directory.sh --version N <relay.onion> ...          # force an explicit version
#   relay/deploy/sign-directory.sh --state DIR <relay.onion> ...          # explicit state dir
#
# The private key is read from <state>/directory-signing-key (created by
# `nightdrop-relay gen-directory-key`). Keep it secret and backed up — losing it means you can no
# longer sign updates, which defeats the whole point of the directory.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CARGO="${CARGO:-cargo}"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
info()  { echo -e "${BLUE}ℹ${NC} $1"; }
ok()    { echo -e "${GREEN}✓${NC} $1"; }
warn()  { echo -e "${YELLOW}⚠${NC} $1"; }
err()   { echo -e "${RED}✗${NC} $1" >&2; }

# Default state dir: $NIGHTDROP_RELAY_STATE, else the repo's relay-state/ (matches the relay + the
# other deploy scripts).
STATE_DIR="${NIGHTDROP_RELAY_STATE:-$REPO_ROOT/relay-state}"
FORCE_VERSION=""
RESTART=0
RELAYS=()

while [ $# -gt 0 ]; do
  case "$1" in
    --restart)  RESTART=1 ;;
    --version)  shift; FORCE_VERSION="${1:-}" ;;
    --state)    shift; STATE_DIR="${1:-}" ;;
    -h|--help)  sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    --*)        err "unknown option: $1"; exit 1 ;;
    *)          RELAYS+=("$1") ;;
  esac
  shift
done

[ "${#RELAYS[@]}" -ge 1 ] || { err "give at least one <relay.onion> to include in the list"; exit 1; }

KEYFILE="$STATE_DIR/directory-signing-key"
LISTFILE="$STATE_DIR/relay-list.json"

[ -f "$KEYFILE" ] || {
  err "no signing key at $KEYFILE"
  err "generate one first:  nightdrop-relay gen-directory-key   (then bake its public key into the app)"
  exit 1
}

# Shape-check each relay address so a typo can't ship a dead directory. v3 onions are 56 chars of
# base32 + '.onion'.
for r in "${RELAYS[@]}"; do
  if ! [[ "$r" =~ ^[a-z2-7]{56}\.onion$ ]]; then
    err "does not look like a v3 onion: $r"
    exit 1
  fi
done

# Resolve the signing binary (prefer an existing release build; build if absent).
BIN="$REPO_ROOT/target/release/nightdrop-relay"
if [ ! -x "$BIN" ]; then
  info "building nightdrop-relay (release)…"
  ( cd "$REPO_ROOT" && "$CARGO" build --release -p nightdrop_relay )
fi

# Determine the next version: --version wins; else current relay-list.json version + 1; else 1.
if [ -n "$FORCE_VERSION" ]; then
  VERSION="$FORCE_VERSION"
elif [ -f "$LISTFILE" ]; then
  CUR="$(python3 - "$LISTFILE" <<'PY'
import base64, json, sys
try:
    d = json.load(open(sys.argv[1]))
    print(json.loads(base64.b64decode(d["payload"]))["version"])
except Exception:
    print(0)
PY
)"
  VERSION=$((CUR + 1))
else
  VERSION=1
fi

info "signing directory v$VERSION with ${#RELAYS[@]} relay(s):"
for r in "${RELAYS[@]}"; do echo "    $r"; done

PRIV="$(cat "$KEYFILE")"
"$BIN" sign-directory "$PRIV" "$VERSION" "${RELAYS[@]}" > "$LISTFILE"
ok "wrote $LISTFILE (version $VERSION)"

echo
warn "This list is a FULL REPLACEMENT and lives on ONE relay so far ($STATE_DIR)."
warn "Copy it to EVERY relay's state dir so clients on any surviving relay get the update:"
for r in "${RELAYS[@]}"; do echo "    scp $LISTFILE  <host-for-$r>:<state>/relay-list.json"; done

if [ "$RESTART" = 1 ]; then
  echo
  info "restarting the local relay so it serves v$VERSION…"
  if systemctl --user restart nightdrop-relay.service 2>/dev/null; then
    ok "restarted (user service)"
  elif command -v sudo >/dev/null && sudo systemctl restart nightdrop-relay.service 2>/dev/null; then
    ok "restarted (system service)"
  else
    warn "could not restart a nightdrop-relay service — restart your relay manually to serve the new list"
  fi
fi
