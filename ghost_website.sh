#!/bin/bash
# Serve the static website for a local preview. Loopback only — don't expose the dev
# server to the LAN.
set -e
cd "$(dirname "$0")"
python3 -m http.server --bind 127.0.0.1 --directory website 8000
