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

# F-Droid caps the fastlane whatsNew (changelogs/<versionCode>.txt) at 500 characters; over that
# the Code Quality report raises a Minor. Checked here because it is read from the *tagged*
# commit, so it cannot be fixed after a release without cutting another one.
# F-Droid reads the changelog as changelogs/<versionCode>.txt, matched against each build's
# versionCode (and CurrentVersionCode for the app-level entry) - see insert_localized_app_metadata
# in fdroidserver/update.py. With per-ABI splitting those are versionCode * 10 + ABI (4021/4022/
# 4023), not the pubspec's base, so a missing file means the release silently ships with no
# "What's New" at all.
missing=0
for vc in $(sed -n -E 's/^ *versionCode: ([0-9]+)$/\1/p' "$RECIPE"); do
    [ -f "$REPO_ROOT/fastlane/metadata/android/en-US/changelogs/$vc.txt" ] \
        || { echo "no changelog for versionCode $vc (expected changelogs/$vc.txt)"; missing=1; }
done
[ "$missing" = 0 ] || echo "^ add before tagging, F-Droid reads these from the built commit"

overlong=0
for f in "$REPO_ROOT"/fastlane/metadata/android/en-US/changelogs/*.txt; do
    n=$(python3 -c "import sys;print(len(open(sys.argv[1],encoding='utf-8').read().strip()))" "$f")
    if [ "$n" -ge 500 ]; then echo "changelog too long: $(basename "$f") is $n chars (limit 500)"; overlong=1
    elif [ "$n" -ge 450 ]; then echo "changelog close to limit: $(basename "$f") is $n chars"; fi
done
[ "$overlong" = 0 ] || echo "^ trim before tagging a release"

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
# Categories are only resolvable against fdroiddata config/, so that complaint is noise here —
# they are checked separately against the real list below, which is NOT noise: a bad category
# fails both `fdroid lint` and schema validation in CI.
fdroid lint '"$APPID"' 2>&1 | grep -v "Categories .* is not valid" > /out/lint.txt || true
' >/dev/null

# Categories, against fdroiddata's own config/categories.yml. Dropping the lint complaint above
# without checking this anywhere is how `Messager` (for `Messaging`) reached CI and failed two
# jobs — a typo in a one-line review suggestion, invisible to every local check we had.
# Network-dependent, so a fetch failure warns rather than fails.
cats=$(sed -n '/^Categories:/,/^[A-Za-z]/p' "$RECIPE" | sed -n -E 's/^  - (.+)$/\1/p')
valid=$(curl -fsS --max-time 20 \
    "https://gitlab.com/fdroid/fdroiddata/-/raw/master/config/categories.yml" 2>/dev/null \
    | sed -n -E 's/^([A-Za-z][^:]*):$/\1/p')
if [ -z "$valid" ]; then
    echo "note: could not fetch fdroiddata config/categories.yml — categories unchecked"
elif [ -n "$cats" ]; then
    while IFS= read -r c; do
        [ -n "$c" ] || continue
        if printf '%s\n' "$valid" | grep -qxF "$c"; then
            echo "category OK: $c"
        else
            echo "INVALID CATEGORY: '$c' is not in fdroiddata config/categories.yml"
            echo "  closest: $(printf '%s\n' "$valid" | grep -iF "${c:0:5}" | paste -sd', ' -)"
            badcat=1
        fi
    done <<< "$cats"
fi

lint=$(grep -v '^20[0-9][0-9]-' "$WORK/lint.txt" 2>/dev/null || true)
if [ -n "$lint" ]; then echo "lint findings:"; echo "$lint"; fi

if [ -n "${badcat:-}" ]; then
    echo "^ fix the category before pushing: CI fails both 'fdroid lint' and 'schema validation'"
fi

if diff -q "$RECIPE" "$WORK/out.yml" >/dev/null; then
    echo "rewritemeta: canonical${lint:+ (but see lint above)}"
    [ -z "$lint" ] && [ -z "${badcat:-}" ]
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
