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

# Desktop builds (the AppImage and the Windows installer), keyed by CPU architecture in their own
# sections. Never under "android": "x86_64" there is the Android x86_64 APK, and that is exactly
# the entry a PC used to find, so Linux and Windows users were handed an APK.
#
# Unlike the APKs, these files are copied in by deploy-website.sh without a version check, so one
# can lag behind the release (an AppImage is rebuilt separately, the installer comes from the VM).
# Offering a stale one under the new version is the forever-prompt described above, so each is
# included only if the version embedded in the file is this release.
desktop_entry() { # file url embedded-version
  local file=$1 url=$2 got=$3
  if [ "$got" != "$ver" ]; then
    echo "gen-update-manifest: NOT offering $url: the file is ${got:-unreadable}, the release is $ver" >&2
    return 1
  fi
  printf '"x86_64":{"url":"%s","sha256":"%s"}' "$url" "$(sha256sum "$file" | cut -d' ' -f1)"
}

appimage="website/applications/linux/Night_Drop-x86_64.AppImage"
linux=""
if [ -f "$appimage" ]; then
  # The version Flutter compiled in ("0.1.26+412"), read from libapp.so inside the AppImage. The
  # AppImage extracts that one file itself; nothing of the app runs.
  tmp="$(mktemp -d)"
  (cd "$tmp" && "$OLDPWD/$appimage" --appimage-extract 'usr/bin/lib/libapp.so' >/dev/null 2>&1) || true
  got="$(strings -a "$tmp/squashfs-root/usr/bin/lib/libapp.so" 2>/dev/null \
    | grep -oE '^[0-9]+\.[0-9]+\.[0-9]+\+[0-9]+$' | head -1)"
  rm -rf "$tmp"
  linux="$(desktop_entry "$appimage" "/applications/linux/Night_Drop-x86_64.AppImage" "${got%%+*}")" || linux=""
fi

installer="website/applications/windows/NightDropSetup.exe"
windows=""
if [ -f "$installer" ]; then
  # Inno Setup stamps AppVersion into the PE version resource, which is stored uncompressed.
  got="$(strings -el "$installer" | grep -A1 -x 'ProductVersion' | tail -1 | tr -d '[:space:]')"
  windows="$(desktop_entry "$installer" "/applications/windows/NightDropSetup.exe" "$got")" || windows=""
fi

# Apps from 0.2 refuse a manifest past its expiry, or without one (core/src/update.rs): a site host
# that serves an old signed manifest can no longer freeze update notices for ever. Re-sign before it
# lapses, release or not (MAINTENANCE.md §11.5). NIGHTDROP_MANIFEST_DAYS overrides the 120 days.
expires=$(( $(date +%s) + ${NIGHTDROP_MANIFEST_DAYS:-120} * 86400 ))
json="{\"latest\":\"$ver\",\"expires\":$expires"
[ -n "$entries" ] && json="$json,\"android\":{$entries}"
[ -n "$linux" ] && json="$json,\"linux\":{$linux}"
[ -n "$windows" ] && json="$json,\"windows\":{$windows}"
json="$json}"

# Sign it (core/src/update.rs, UPDATE_PUBKEY). Apps from the signing release on trust ONLY the
# signed payload; older ones read the same fields in the clear and ignore "signed". Without a valid
# signature a new app sees no update at all, so a missing key is a hard stop, never an unsigned
# file. The key never goes on a command line (`ps`), and is read from outside website/.
#
# Where the key is found, first match wins (MAINTENANCE.md §11.5):
#   1. NIGHTDROP_UPDATE_KEY — only for a key restored from the paper backup, when no copy is reachable;
#   2. ~/.config/nightdrop-signing/update-signing-key — a copy on this machine, if one is kept;
#   3. update-signing-key at the top of any mounted removable drive (/run/media/$USER/*, /media/$USER/*)
#      — the offline USB stick: plug it in, run this, unmount it.
# Two drives holding different keys is refused rather than guessed between.
find_update_key() {
  if [ -n "${NIGHTDROP_UPDATE_KEY:-}" ]; then
    echo "$NIGHTDROP_UPDATE_KEY"
    return
  fi
  local home_key="$HOME/.config/nightdrop-signing/update-signing-key"
  if [ -r "$home_key" ]; then
    echo "$home_key"
    return
  fi
  local found=() f
  for f in /run/media/"$USER"/*/update-signing-key /media/"$USER"/*/update-signing-key; do
    [ -r "$f" ] && found+=("$f")
  done
  if [ "${#found[@]}" -gt 1 ]; then
    for f in "${found[@]:1}"; do
      if ! cmp -s "${found[0]}" "$f"; then
        echo "gen-update-manifest: different update keys on ${found[*]} — unplug all but the real one" >&2
        return 1
      fi
    done
  fi
  if [ "${#found[@]}" -gt 0 ]; then
    echo "${found[0]}"
  fi
  return 0 # nothing found is reported by the caller, with what to do about it
}
key="$(find_update_key)" || exit 1
if [ -z "$key" ] || [ ! -r "$key" ]; then
  echo "gen-update-manifest: no update signing key found — NOT writing update.json." >&2
  echo "  Plug in the offline key USB (it holds update-signing-key) and run this again." >&2
  echo "  Only if no copy of the file is left: restore it from the paper backup to a file and run" >&2
  echo "  NIGHTDROP_UPDATE_KEY=<that file> make update-manifest   (MAINTENANCE.md §11.5)." >&2
  exit 1
fi
echo "gen-update-manifest: signing with $key" >&2
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
printf '%s' "$json" > "$tmp"
sig="$(cargo run -q --release -p nightdrop_relay -- sign-update "$key" "$tmp")"
payload="$(base64 -w0 < "$tmp")"
[ -n "$sig" ] && [ -n "$payload" ] || { echo "gen-update-manifest: signing failed" >&2; exit 1; }
# A rotation of the signing key (core/src/keyroll.rs), once one has been made, rides along in every
# manifest from then on, so an app that missed it learns the new key on its next check.
rotation=""
if [ -f deploy/update-rotation.json ]; then
  rotation=",\"rotation\":$(tr -d '\n' < deploy/update-rotation.json)"
fi
printf '%s,"signed":{"payload":"%s","sig":"%s"}%s}\n' "${json%\}}" "$payload" "$sig" "$rotation" > website/update.json
echo "gen-update-manifest: signed; it expires $(date -d "@$expires" +%F) — re-sign before then (MAINTENANCE.md §11.5)"
