#!/usr/bin/env bash
# Validate fdroid/app.nightdrop.yml the way fdroiddata CI validates it, and rewrite it into
# canonical form. Run this before pushing to the MR branch — a formatting-only mismatch fails
# the pipeline just as hard as a broken build.
#
#   ./fdroid/check-metadata.sh          # report only; non-zero exit if not canonical
#   ./fdroid/check-metadata.sh --write  # overwrite the recipe with the canonical form
#
# Why a container: the `fdroid rewritemeta` job runs on debian:trixie-slim and installs
# fdroidserver from BOTH the Debian package (which supplies the dependencies) and the master
# tarball (which supplies the code). The line wrapping is decided by **ruamel.yaml**, so it is
# Debian's ruamel version that determines the canonical form — a pip-installed fdroidserver, of
# any version, wraps differently and will happily bless a file that CI then rejects.
set -euo pipefail

APPID=app.nightdrop
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RECIPE="$REPO_ROOT/fdroid/$APPID.yml"
WRITE=0
[ "${1:-}" = "--write" ] && WRITE=1

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$RECIPE" "$WORK/in.yml"

podman run --rm -v "$WORK:/out:z" debian:trixie-slim bash -c '
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get -qq update >/dev/null
apt-get -qq install -y --no-install-recommends fdroidserver apksigner curl ca-certificates >/dev/null 2>&1
mkdir -p /w/fdroidserver /w/metadata && cd /w
curl -sL https://gitlab.com/fdroid/fdroidserver/-/archive/master/fdroidserver-master.tar.gz \
    | tar -xz --strip-components=1 -C /w/fdroidserver
export PATH="/w/fdroidserver:$PATH" PYTHONPATH="/w/fdroidserver:/w/fdroidserver/examples"
printf "repo_url: https://f-droid.org/repo\nrepo_name: t\nrepo_description: t\n" > config.yml
chmod 600 config.yml
cp /out/in.yml metadata/'"$APPID"'.yml
fdroid rewritemeta '"$APPID"' >/dev/null 2>&1
cp metadata/'"$APPID"'.yml /out/out.yml
# Categories are only resolvable against fdroiddata config/, so that complaint is noise here.
fdroid lint '"$APPID"' 2>&1 | grep -v "Categories .* is not valid" > /out/lint.txt || true
' >/dev/null

lint=$(grep -v '^20[0-9][0-9]-' "$WORK/lint.txt" 2>/dev/null || true)
if [ -n "$lint" ]; then echo "lint findings:"; echo "$lint"; fi

if diff -q "$RECIPE" "$WORK/out.yml" >/dev/null; then
    echo "rewritemeta: canonical${lint:+ (but see lint above)}"
    [ -z "$lint" ]
else
    echo "rewritemeta: NOT canonical — CI will reject this. Diff (yours -> canonical):"
    diff -u "$RECIPE" "$WORK/out.yml" || true
    if [ "$WRITE" = 1 ]; then
        cp "$WORK/out.yml" "$RECIPE"
        echo "written: $RECIPE"
    else
        echo "re-run with --write to apply."
        exit 1
    fi
fi
