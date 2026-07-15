#!/bin/bash
# Run the relay with the dev TUI dashboard (ARCHITECTURE.md §11.9). Its .onion stays
# stable across restarts via the persisted relay-state/ dir (NIGHTDROP_RELAY_STATE).
set -e
cd "$(dirname "$0")"
NIGHTDROP_RELAY_TUI=1 cargo run -p nightdrop_relay
