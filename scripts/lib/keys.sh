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

# Print property $2 of the Java properties file $1, read the way Gradle reads key.properties
# (java.util.Properties: `\` escapes, \uXXXX, line continuations, `=`/`:`/space separators), so
# a password with a backslash in it means the same thing here as in build.gradle.kts.
nd_prop() {
  python3 -I - "$1" "$2" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="latin-1").read()
lines, buf = [], None
for raw in text.splitlines():
    line = raw.lstrip(" \t\f") if buf is None else raw.lstrip(" \t\f")
    if buf is None and (not line or line[0] in "#!"):
        continue
    trail = len(line) - len(line.rstrip("\\"))
    cont = trail % 2 == 1
    piece = line[:-1] if cont else line
    buf = piece if buf is None else buf + piece
    if not cont:
        lines.append(buf); buf = None
if buf is not None:
    lines.append(buf)
def unescape(s):
    out, i = [], 0
    while i < len(s):
        c = s[i]
        if c == "\\" and i + 1 < len(s):
            n = s[i + 1]
            if n == "u" and re.fullmatch(r"[0-9a-fA-F]{4}", s[i + 2:i + 6]):
                out.append(chr(int(s[i + 2:i + 6], 16))); i += 6; continue
            out.append({"t": "\t", "n": "\n", "r": "\r", "f": "\f"}.get(n, n)); i += 2; continue
        if c == "\\":
            i += 1; continue
        out.append(c); i += 1
    return "".join(out)
props = {}
for l in lines:
    i, n = 0, len(l)
    while i < n and l[i] not in "=: \t\f":
        i += 2 if l[i] == "\\" else 1
    key = l[:i]
    while i < n and l[i] in " \t\f":
        i += 1
    if i < n and l[i] in "=:":
        i += 1
    while i < n and l[i] in " \t\f":
        i += 1
    props[unescape(key)] = unescape(l[i:])
v = props.get(sys.argv[2])
if v is not None:
    sys.stdout.write(v)
PY
}

# The release signing certificate, from `apksigner verify --print-certs`. Matches
# AllowedAPKSigningKeys in fdroid/app.nightdrop.yml: an APK signed by anything else must not ship.
ND_RELEASE_CERT_SHA256="d08b8e6408aff4f43556c05b819fef25afdd6912be7a678b0552468a4582c0ac"

nd_find_apksigner() {
  local p
  for p in "${ANDROID_SDK:-$HOME/android-sdk}"/build-tools/*/apksigner \
           "${ANDROID_HOME:-}"/build-tools/*/apksigner; do
    [ -x "$p" ] && { echo "$p"; return 0; }
  done
  return 1
}

# Sign APK $2 in place with the release key described by key.properties $1, the way the F-Droid
# builds are signed (fdroid/README.md): v2 only, alignment preserved. Signing happens here, outside
# the build, so no build step (Gradle plugins and the like run sandboxed, lib/sandbox.sh) ever
# reads the keystore. Checks the result carries the release certificate. Passwords pass to
# apksigner through its environment, never on a command line.
nd_sign_apk_release() {
  local props="$1" apk="$2" signer store alias got
  signer="$(nd_find_apksigner)" || { echo "apksigner not found" >&2; return 1; }
  store="$(nd_prop "$props" storeFile)"
  # Relative to the properties file's folder, as build.gradle.kts reads it.
  case "$store" in /*) ;; *) store="$(dirname "$props")/$store" ;; esac
  alias="$(nd_prop "$props" keyAlias)"
  [ -f "$store" ] && [ -n "$alias" ] || { echo "key.properties $props: no keystore or alias" >&2; return 1; }
  ND_KS_PASS="$(nd_prop "$props" storePassword)" \
  ND_KEY_PASS="$(nd_prop "$props" keyPassword)" \
    "$signer" sign --ks "$store" --ks-key-alias "$alias" \
      --ks-pass env:ND_KS_PASS --key-pass env:ND_KEY_PASS \
      --v1-signing-enabled false --v2-signing-enabled true \
      --v3-signing-enabled false --v4-signing-enabled false \
      --alignment-preserved --in "$apk" --out "$apk.signed" || { rm -f "$apk.signed"; return 1; }
  got="$("$signer" verify --print-certs "$apk.signed" 2>/dev/null | grep -m1 'certificate SHA-256 digest' | awk '{print $NF}')"
  if [ "$got" != "$ND_RELEASE_CERT_SHA256" ]; then
    rm -f "$apk.signed"
    echo "signed $apk, but not with the release certificate (got ${got:-nothing})" >&2
    return 1
  fi
  mv -f "$apk.signed" "$apk"
}
