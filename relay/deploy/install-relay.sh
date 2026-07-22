#!/bin/bash
#
# Install the Night Drop relay as a supervised systemd service.
#
# Builds the release binary and installs it plus a hardened unit, then enables + starts
# it. The relay publishes its OWN Tor onion (no inbound ports / TLS / domain needed) and
# holds only opaque, end-to-end-encrypted blobs — no keys, no logs. See ../README.md.
#
# If relay-state/relay-list.json exists (from `nightdrop-relay sign-directory`), it is copied
# into the state dir so the relay serves the operator-signed relay directory (§3.1 — rotate
# the relay set without an app update).
#
# Two modes:
#   (default) system-wide   — needs sudo. Binary → /usr/local/bin/nightdrop-relay, unit →
#                             /etc/systemd/system/nightdrop-relay.service, state (stable onion)
#                             → /var/lib/nightdrop-relay. Best for a VPS / dedicated box.
#   --user [STATE_DIR]      — no sudo. Binary → ~/.local/bin, unit → the systemd *user*
#                             manager, linger enabled so it runs without a login and across
#                             reboots. STATE_DIR defaults to the repo's relay-state/ if
#                             present (preserving the onion baked into existing app builds),
#                             else ~/.local/state/nightdrop-relay.
#
# Usage:
#   relay/deploy/install-relay.sh                 # system-wide (sudo)
#   relay/deploy/install-relay.sh --user          # per-user, keep repo relay-state onion
#   relay/deploy/install-relay.sh --user /path    # per-user, explicit state dir
#   relay/deploy/install-relay.sh --status        # show service state + onion
#   relay/deploy/install-relay.sh --uninstall [--user]

set -euo pipefail

# Repo root is two levels up from this script (relay/deploy/).
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
UNIT_SRC="$REPO_ROOT/relay/deploy/nightdrop-relay.service"
CARGO="${CARGO:-cargo}"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
info()  { echo -e "${BLUE}ℹ${NC} $1"; }
ok()    { echo -e "${GREEN}✓${NC} $1"; }
warn()  { echo -e "${YELLOW}⚠${NC} $1"; }
err()   { echo -e "${RED}✗${NC} $1" >&2; }
header(){ echo; echo "── $1 ──"; }

MODE="system"
STATE_DIR=""
ACTION="install"

while [ $# -gt 0 ]; do
  case "$1" in
    --user)      MODE="user" ;;
    --status)    ACTION="status" ;;
    --uninstall) ACTION="uninstall" ;;
    -h|--help)   sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    --*)         err "unknown option: $1"; exit 1 ;;
    *)           STATE_DIR="$1" ;;   # positional: explicit state dir (implies --user use)
  esac
  shift
done

build_binary() {
  header "Build (release)"
  info "cargo build --release -p nightdrop_relay"
  ( cd "$REPO_ROOT" && "$CARGO" build --release -p nightdrop_relay )
  BIN="$REPO_ROOT/target/release/nightdrop-relay"
  [ -x "$BIN" ] || { err "binary not found at $BIN"; exit 1; }
  ok "built $BIN"
}

# Install the operator-signed relay directory (relay-list.json) into the state dir so the relay
# serves it (§3.1 — rotate the relay set without an app update). Sourced from the repo's
# relay-state/ where `nightdrop-relay sign-directory` writes it. No-op if none has been signed.
# Args: <dest-state-dir> [sudo]
install_directory() {
  local dest="$1" priv="${2:-}"
  local src="$REPO_ROOT/relay-state/relay-list.json"
  if [ ! -f "$src" ]; then
    info "no relay-list.json — skipping signed directory (run 'nightdrop-relay sign-directory' to enable rotation)"
    return
  fi
  if [ "$src" -ef "$dest/relay-list.json" ]; then
    ok "signed directory already in state dir ($dest/relay-list.json)"
    return
  fi
  if [ "$priv" = "sudo" ]; then
    sudo install -Dm644 "$src" "$dest/relay-list.json"
  else
    install -Dm644 "$src" "$dest/relay-list.json"
  fi
  ok "signed directory → $dest/relay-list.json"
}

# Resolve the default per-user state dir: prefer the repo's relay-state/ so the onion
# baked into existing desktop/Android builds keeps working; otherwise XDG state home.
resolve_user_state_dir() {
  if [ -n "$STATE_DIR" ]; then :;
  elif [ -f "$REPO_ROOT/relay-state/onion" ] || [ -d "$REPO_ROOT/relay-state/arti-state" ]; then
    STATE_DIR="$REPO_ROOT/relay-state"
  else
    STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/nightdrop-relay"
  fi
}

install_system() {
  command -v sudo >/dev/null || { err "sudo required for system install (or use --user)"; exit 1; }
  build_binary
  header "Install (system-wide)"
  sudo install -Dm755 "$BIN" /usr/local/bin/nightdrop-relay
  ok "binary → /usr/local/bin/nightdrop-relay"
  sudo install -Dm644 "$UNIT_SRC" /etc/systemd/system/nightdrop-relay.service
  sudo install -Dm644 "$REPO_ROOT/relay/README.md" /usr/share/doc/nightdrop-relay/README.md 2>/dev/null || true
  ok "unit → /etc/systemd/system/nightdrop-relay.service"
  install_directory /var/lib/nightdrop-relay sudo
  sudo systemctl daemon-reload
  sudo systemctl enable nightdrop-relay.service
  # restart (not just start) so a reinstall picks up the new binary / signed directory.
  sudo systemctl restart nightdrop-relay.service
  ok "enabled + (re)started (state: /var/lib/nightdrop-relay)"
  sleep 3
  status_system
}

install_user() {
  resolve_user_state_dir
  mkdir -p "$STATE_DIR"
  build_binary
  header "Install (per-user)"
  install -Dm755 "$BIN" "$HOME/.local/bin/nightdrop-relay"
  ok "binary → ~/.local/bin/nightdrop-relay"

  local unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
  mkdir -p "$unit_dir"
  # Derive the user unit from the committed system unit: same hardening, but a per-user
  # ExecStart/state (systemd user units can't use DynamicUser/StateDirectory the same way,
  # so pin an explicit NIGHTDROP_RELAY_STATE and ReadWritePaths, and target the user manager).
  {
    echo "# Generated by relay/deploy/install-relay.sh --user. Do not edit; re-run the installer."
    echo "# Per-user variant of relay/deploy/nightdrop-relay.service (canonical, system-wide)."
    echo "[Unit]"
    echo "Description=Night Drop relay (rendezvous mailbox + 24h store-and-forward, own Tor onion)"
    echo "After=network-online.target"
    echo "Wants=network-online.target"
    echo
    echo "[Service]"
    echo "Type=simple"
    echo "ExecStart=%h/.local/bin/nightdrop-relay"
    echo "Restart=always"
    echo "RestartSec=5"
    echo "# Backstop to the in-relay reachability watchdog: cycle at most weekly so any slow"
    echo "# degradation still refreshes hands-off (see the canonical unit for the rationale)."
    echo "RuntimeMaxSec=1w"
    echo "Environment=NIGHTDROP_RELAY_STATE=$STATE_DIR"
    echo "WorkingDirectory=$STATE_DIR"
    echo "# Hardening (user-manager subset of the system unit)."
    echo "NoNewPrivileges=yes"
    echo "ProtectSystem=strict"
    echo "ProtectHome=read-only"
    echo "ReadWritePaths=$STATE_DIR"
    echo "PrivateTmp=yes"
    echo "ProtectControlGroups=yes"
    echo "ProtectKernelModules=yes"
    echo "ProtectKernelTunables=yes"
    echo "RestrictSUIDSGID=yes"
    echo "LockPersonality=yes"
    echo "RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX"
    echo
    echo "[Install]"
    echo "WantedBy=default.target"
  } > "$unit_dir/nightdrop-relay.service"
  ok "unit → $unit_dir/nightdrop-relay.service"
  ok "state (stable onion) → $STATE_DIR"
  install_directory "$STATE_DIR"

  # Keep the relay up without an active login session / across reboot.
  if loginctl enable-linger "$USER" 2>/dev/null; then
    ok "linger enabled (survives logout + reboot)"
  else
    warn "could not enable linger (needs polkit/root) — relay runs only while you're logged in"
  fi

  systemctl --user daemon-reload
  systemctl --user enable nightdrop-relay.service
  # restart (not just start) so a reinstall picks up the new binary / signed directory.
  systemctl --user restart nightdrop-relay.service
  ok "enabled + (re)started"
  sleep 3
  status_user
}

status_system() {
  header "Status (system)"
  systemctl --no-pager --full status nightdrop-relay.service 2>/dev/null | sed -n '1,6p' || true
  local onion=/var/lib/nightdrop-relay/onion
  [ -r "$onion" ] && ok "onion: $(cat "$onion")" || warn "onion not published yet (first bootstrap can take a minute)"
  [ -r /var/lib/nightdrop-relay/relay-list.json ] && ok "serving signed directory (relay-list.json)" || info "no signed directory served"
}

status_user() {
  header "Status (user)"
  systemctl --user --no-pager --full status nightdrop-relay.service 2>/dev/null | sed -n '1,6p' || true
  resolve_user_state_dir
  [ -r "$STATE_DIR/onion" ] && ok "onion: $(cat "$STATE_DIR/onion")" || warn "onion not published yet (first bootstrap can take a minute)"
  [ -r "$STATE_DIR/relay-list.json" ] && ok "serving signed directory (relay-list.json)" || info "no signed directory served"
}

uninstall() {
  if [ "$MODE" = "user" ]; then
    systemctl --user disable --now nightdrop-relay.service 2>/dev/null || true
    rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/nightdrop-relay.service"
    systemctl --user daemon-reload 2>/dev/null || true
    rm -f "$HOME/.local/bin/nightdrop-relay"
    ok "removed per-user service + binary (state dir left intact — delete it to drop the onion)"
  else
    sudo systemctl disable --now nightdrop-relay.service 2>/dev/null || true
    sudo rm -f /etc/systemd/system/nightdrop-relay.service /usr/local/bin/nightdrop-relay
    sudo systemctl daemon-reload 2>/dev/null || true
    ok "removed system service + binary (/var/lib/nightdrop-relay left intact — delete it to drop the onion)"
  fi
}

case "$ACTION" in
  status)    [ "$MODE" = user ] && status_user || status_system ;;
  uninstall) uninstall ;;
  install)   [ "$MODE" = user ] && install_user || install_system ;;
esac
