#!/usr/bin/env bash
# Generate website/update.json — what the in-app update check reads (core/src/update.rs).
#
# PUBLISH STEP ONLY — `make update-manifest`, never `make config`. This file is served live from
# disk by the onion site, and it describes builds that are actually downloadable. Regenerating it
# from a target every build depends on meant a version bump instantly announced a release that did
# not exist yet, offering the PREVIOUS release's APKs under the new number — hashes included, so
# the download verified and the user stayed on the old build, prompted forever. Run this after the
# new APKs are in website/applications/android/, and not before.
#
# The version comes from app/pubspec.yaml so it cannot drift from the
# released app, and each APK's sha256 is computed from the file actually being served, so the
# hash cannot drift from the bytes either. An APK that isn't present is simply omitted: the app
# then reports the update but offers no download, which is the right failure (tell the user
# something, promise nothing).
set -euo pipefail
cd "$(dirname "$0")/.."

ver="$(sed -n 's/^version: *//p' app/pubspec.yaml)"
ver="${ver%%+*}"
apkdir="website/applications/android"

entries=""
for abi in universal arm64-v8a armeabi-v7a x86_64; do
  if [ "$abi" = universal ]; then apk="$apkdir/NightDrop.apk"; else apk="$apkdir/NightDrop-$abi.apk"; fi
  [ -f "$apk" ] || continue
  sum="$(sha256sum "$apk" | cut -d' ' -f1)"
  [ -n "$entries" ] && entries="$entries,"
  name="NightDrop-$abi.apk"; [ "$abi" = universal ] && name="NightDrop.apk"
  entries="$entries\"$abi\":{\"url\":\"/applications/android/$name\",\"sha256\":\"$sum\"}"
done

if [ -n "$entries" ]; then
  printf '{"latest":"%s","android":{%s}}\n' "$ver" "$entries" > website/update.json
else
  printf '{"latest":"%s"}\n' "$ver" > website/update.json
fi
