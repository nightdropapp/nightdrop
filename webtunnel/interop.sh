#!/usr/bin/env bash
# Build the Tor Project's WebTunnel server at a pinned commit and run the interop tests
# against it (tests/interop.rs, the #[ignore]d ones). Needs Go and network access for the
# first clone; later runs reuse target/webtunnel-go.
#
#   webtunnel/interop.sh            # build if needed, run all tests including interop
#
# Bump WEBTUNNEL_COMMIT deliberately, and read the upstream diff when you do: the point of
# these tests is compatibility with what real bridges run.
set -euo pipefail

WEBTUNNEL_REPO=https://gitlab.torproject.org/tpo/anti-censorship/pluggable-transports/webtunnel.git
WEBTUNNEL_COMMIT=11334a222c1beeb45759d436b7aef963a10600b2   # v0.0.7, 2026-09-15

root=$(cd "$(dirname "$0")/.." && pwd)
src=$root/target/webtunnel-go
bin=$src/webtunnel-server-$WEBTUNNEL_COMMIT

if [[ ! -x $bin ]]; then
    command -v go >/dev/null || { echo "interop.sh: Go is required to build the reference server" >&2; exit 1; }
    if [[ ! -d $src/.git ]]; then
        git clone --quiet "$WEBTUNNEL_REPO" "$src"
    fi
    git -C "$src" fetch --quiet origin
    git -C "$src" -c advice.detachedHead=false checkout --quiet "$WEBTUNNEL_COMMIT"
    (cd "$src" && go build -trimpath -o "$bin" ./main/server)
fi

export WEBTUNNEL_SERVER_BIN=$bin
cd "$root"
cargo test -p nightdrop-webtunnel -- --include-ignored "$@"
