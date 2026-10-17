#!/usr/bin/env bash
# Copy this machine's release signing secrets to the offline key stick, check every copy, and print
# (never run) the commands that remove them from this machine (MAINTENANCE.md §11.5).
#
#   scripts/keys-to-stick.sh [STICK_DIR]
#
# STICK_DIR defaults to the mounted drive that already holds update-signing-key (the key stick).
# Copied, each only if present here:
#   ~/.android/nightdrop-release.jks + app/android/key.properties -> <stick>/android/
#       (storeFile rewritten to the copy beside it)
#   the release GPG key ($NIGHTDROP_GPG_ID, default security@nightdrop.app) -> <stick>/gnupg/,
#       a keyring of its own; gpg may ask for the key's passphrase to export it
#   ~/rust-projects/pgp-keygen/private-key.asc -> <stick>/pgp/
#   gitlab_token.txt -> <stick>/
# After this, the signing scripts use the stick whenever a key is missing here (scripts/lib/keys.sh,
# app/android/app/build.gradle.kts), so deleting the copies here makes signing need the stick.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GPG_ID="${NIGHTDROP_GPG_ID:-security@nightdrop.app}"
ok()  { printf '\033[0;32m✓\033[0m %s\n' "$*"; }
err() { printf '\033[0;31m✗\033[0m %s\n' "$*" >&2; }

stick="${1:-}"
if [ -z "$stick" ]; then
  mapfile -t found < <(ls -d /run/media/"$USER"/*/update-signing-key /media/"$USER"/*/update-signing-key 2>/dev/null || true)
  [ "${#found[@]}" -eq 1 ] || { err "plug in the key stick (exactly one drive holding update-signing-key), or name it"; exit 1; }
  stick="$(dirname "${found[0]}")"
fi
[ -d "$stick" ] && [ -w "$stick" ] || { err "$stick is not a writable directory"; exit 1; }
echo "Key stick: $stick"
remove=()

copy() { # copy <src> <dest>: copy, flush, compare byte for byte
  mkdir -p "$(dirname "$2")"
  cp "$1" "$2"
  sync
  cmp -s "$1" "$2" || { err "copy of $1 does not match — the stick may be failing"; exit 1; }
  ok "$1 -> $2 (verified)"
  remove+=("$1")
}

# Android release keystore and its properties.
props="$ROOT/app/android/key.properties"
if [ -f "$props" ]; then
  store="$(sed -n 's/^storeFile=//p' "$props")"
  case "$store" in /*) ;; *) store="$ROOT/app/android/app/$store" ;; esac
  copy "$store" "$stick/android/$(basename "$store")"
  mkdir -p "$stick/android"
  sed "s#^storeFile=.*#storeFile=$(basename "$store")#" "$props" > "$stick/android/key.properties"
  sync
  diff <(grep -v '^storeFile=' "$props") <(grep -v '^storeFile=' "$stick/android/key.properties") >/dev/null \
    || { err "key.properties copy differs"; exit 1; }
  ok "$props -> $stick/android/key.properties (storeFile points beside it; verified)"
  remove+=("$props")
fi

# Release GPG key, into a keyring of its own on the stick.
if gpg --batch --list-secret-keys "$GPG_ID" >/dev/null 2>&1; then
  home="$stick/gnupg"
  mkdir -p "$home"
  printf 'no-permission-warning\n' > "$home/gpg.conf" # FAT cannot hold the 0700 gpg wants
  gpg --armor --export-secret-keys "$GPG_ID" | gpg --batch --no-permission-warning --homedir "$home" --import 2>/dev/null
  want="$(gpg --batch --with-colons --list-secret-keys "$GPG_ID" | awk -F: '/^(sec|ssb)/{print $5}' | sort)"
  have="$(gpg --batch --no-permission-warning --homedir "$home" --with-colons --list-secret-keys "$GPG_ID" | awk -F: '/^(sec|ssb)/{print $5}' | sort)"
  [ -n "$have" ] && [ "$want" = "$have" ] || { err "the stick's keyring does not hold the same secret keys"; exit 1; }
  ok "GPG secret keys for $GPG_ID -> $home ($(echo "$have" | wc -l) keys; verified)"
  gpg_on_pc=1
fi

[ -f "$HOME/rust-projects/pgp-keygen/private-key.asc" ] \
  && copy "$HOME/rust-projects/pgp-keygen/private-key.asc" "$stick/pgp/private-key.asc"
[ -f "$ROOT/gitlab_token.txt" ] && copy "$ROOT/gitlab_token.txt" "$stick/gitlab_token.txt"

echo
echo "Everything above is on the stick. To make signing need the stick, remove the copies here:"
for f in "${remove[@]}"; do echo "  shred -u '$f'"; done
[ -n "${gpg_on_pc:-}" ] && echo "  gpg --delete-secret-keys $GPG_ID   # this machine's keyring only"
echo "Then test with the stick plugged in: scripts/build-appimage.sh --out /tmp/t.AppImage (signs)."
echo "A second stick with the same contents is worth having: this one has needed a disk repair."
