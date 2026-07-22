#!/usr/bin/env bash
# Install the Night Drop onion website as a systemd *user* service so it starts on boot and
# restarts if it dies. Runs as your user (no root) — the onion key stays in your home dir.
#
# For the service to start at boot *without* you logging in, your user needs "linger"
# enabled (loginctl enable-linger). This script enables it if it isn't already; that step
# may need sudo, and the script will tell you the exact command if it can't do it for you.
#
# Usage:
#   scripts/install-onion-service.sh [PORT]     # PORT default 8787
#
# Manage afterwards:
#   systemctl --user status  nightdrop-onion
#   systemctl --user restart nightdrop-onion
#   systemctl --user disable --now nightdrop-onion     # stop + don't start on boot
#   journalctl --user -u nightdrop-onion -f            # logs (incl. the .onion address)

set -euo pipefail

PORT="${1:-8787}"
SVC="nightdrop-onion.service"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

command -v tor >/dev/null 2>&1 || {
  echo "error: the 'tor' daemon is not installed — run: sudo dnf install -y tor" >&2
  exit 1
}

mkdir -p "$UNIT_DIR"
cat > "$UNIT_DIR/$SVC" <<EOF
[Unit]
Description=Night Drop website served as a Tor onion service
Documentation=file://$SCRIPT_DIR/onion-website.sh

[Service]
# notify: the script sends readiness + the .onion address (systemd-notify), so the address
# shows up in `systemctl --user status` and the unit only reports "active" once it's serving.
Type=notify
# systemd-notify runs as a child (not the main PID), so accept notifications from the cgroup.
NotifyAccess=all
ExecStart=$SCRIPT_DIR/onion-website.sh $PORT
Restart=always
RestartSec=10
# Allow a slow first descriptor publish before the readiness notify (else startup times out).
TimeoutStartSec=120
TimeoutStopSec=20

[Install]
WantedBy=default.target
EOF
echo "wrote $UNIT_DIR/$SVC"

systemctl --user daemon-reload
systemctl --user enable --now "$SVC"
echo "enabled + started $SVC"

# Linger = the user manager runs at boot without a login session, so the service comes up on
# reboot. Enable it if needed.
linger="$(loginctl show-user "$USER" -p Linger --value 2>/dev/null || echo unknown)"
if [ "$linger" != "yes" ]; then
  echo "enabling linger (start on boot without login)..."
  if ! loginctl enable-linger "$USER" 2>/dev/null; then
    echo "  couldn't enable linger automatically. Run this once:"
    echo "    sudo loginctl enable-linger $USER"
  fi
else
  echo "linger already enabled — will start on reboot."
fi

echo
echo "The .onion address is logged by the service. See it with:"
echo "    journalctl --user -u $SVC -n 20 --no-pager | grep -i onion"
