# Where the release signing keys are, for scripts that sign (sourced, not run). MAINTENANCE.md §11.5.
#
# Each key is looked for on this machine first; when it is not here, on the offline key stick:
# $NIGHTDROP_KEY_DIR if set, else the top of any mounted removable drive (/run/media/$USER/*,
# /media/$USER/*). So the copies on this machine can be deleted once the stick holds them
# (scripts/keys-to-stick.sh), and signing then works only while the stick is plugged in.

# The stick directories to look in, one per line.
nd_stick_dirs() {
  [ -n "${NIGHTDROP_KEY_DIR:-}" ] && echo "$NIGHTDROP_KEY_DIR"
  local d
  for d in /run/media/"$USER"/* /media/"$USER"/*; do
    [ -d "$d" ] && echo "$d"
  done
  return 0
}

# Make gpg find the secret key for $1 (an id or address): this machine's keyring if it has it,
# else <stick>/gnupg, exported as GNUPGHOME for every gpg call that follows. Returns 1 if neither has it.
nd_use_gpg_key() {
  local id="$1" d
  if gpg --batch --list-secret-keys "$id" >/dev/null 2>&1; then
    return 0
  fi
  while IFS= read -r d; do
    if [ -d "$d/gnupg" ] && gpg --batch --homedir "$d/gnupg" --no-permission-warning \
        --list-secret-keys "$id" >/dev/null 2>&1; then
      # (keys-to-stick.sh writes no-permission-warning into its gpg.conf: FAT has no 0700.)
      export GNUPGHOME="$d/gnupg"
      return 0
    fi
  done < <(nd_stick_dirs)
  return 1
}

# The Android key.properties to sign the real app with: app/android/key.properties, else
# <stick>/android/key.properties. Prints the path; prints nothing if neither exists.
# (app/android/app/build.gradle.kts looks in the same places, for the real app id only.)
nd_android_key_properties() {
  local root="$1" d
  if [ -f "$root/app/android/key.properties" ]; then
    echo "$root/app/android/key.properties"
    return 0
  fi
  while IFS= read -r d; do
    if [ -f "$d/android/key.properties" ]; then
      echo "$d/android/key.properties"
      return 0
    fi
  done < <(nd_stick_dirs)
  return 0
}
