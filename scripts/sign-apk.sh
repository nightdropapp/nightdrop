#!/usr/bin/env bash
# Sign an APK with the release key, the way every published APK is signed (fdroid/README.md step 4,
# MAINTENANCE.md §11.3 step 5): v2 only, alignment preserved, certificate checked afterwards.
#
#   scripts/sign-apk.sh IN.apk OUT.apk
#
# The key is app/android/key.properties, else the key stick's (lib/keys.sh). IN is left as it is.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/keys.sh
source "$ROOT/scripts/lib/keys.sh"

[ $# -eq 2 ] || { echo "usage: $0 IN.apk OUT.apk" >&2; exit 2; }
in="$1" out="$2"
[ -f "$in" ] || { echo "no such APK: $in" >&2; exit 1; }
[ "$(realpath -m "$in")" != "$(realpath -m "$out")" ] || { echo "OUT must differ from IN" >&2; exit 2; }
props="$(nd_android_key_properties "${PROJECT_ROOT:-$ROOT}")"
[ -n "$props" ] || { echo "no release key: app/android/key.properties, or plug in the key stick" >&2; exit 1; }

cp -f "$in" "$out"
if ! nd_sign_apk_release "$props" "$out"; then
  rm -f "$out"
  exit 1
fi
echo "signed: $out (release certificate checked)"
