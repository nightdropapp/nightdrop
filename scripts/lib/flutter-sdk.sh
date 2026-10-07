# shellcheck shell=bash
# Pick the Flutter SDK a build must use: the version app/.fvmrc pins, which is also what the F-Droid
# recipe builds with (MAINTENANCE.md §8). Sourced by the build scripts; sets FLUTTER_HOME or exits.
#
# A build on the wrong SDK is not a cosmetic problem: 0.1.28's universal APK first came out on 3.44.6
# without the libpng fix that 3.47.6 carried, because the scripts used ~/flutter. So the version is
# checked, never assumed:
#   1. FLUTTER_HOME, if set — and it must be the pinned version, or the build stops;
#   2. otherwise the first of ~/flutter-<version>, ~/fvm/versions/<version>, ~/flutter
#      that reports the pinned version.

# The frameworkVersion an SDK reports, or nothing if it is not a Flutter SDK.
flutter_sdk_version() {
  grep -o '"frameworkVersion": *"[^"]*"' "$1/bin/cache/flutter.version.json" 2>/dev/null \
    | grep -o '[0-9][0-9.]*' || true
}

# resolve_flutter_home <project root>: export FLUTTER_HOME as the pinned SDK, or exit 1 saying why.
resolve_flutter_home() {
  local root="$1" want sdk
  want="$(grep -o '"flutter": *"[^"]*"' "$root/app/.fvmrc" | grep -o '[0-9][0-9.]*')"
  [ -n "$want" ] || { echo "✗ no Flutter version pinned in $root/app/.fvmrc" >&2; exit 1; }
  if [ -n "${FLUTTER_HOME:-}" ]; then
    if [ "$(flutter_sdk_version "$FLUTTER_HOME")" != "$want" ]; then
      echo "✗ FLUTTER_HOME=$FLUTTER_HOME is Flutter $(flutter_sdk_version "$FLUTTER_HOME"), but app/.fvmrc pins $want" >&2
      echo "  unset FLUTTER_HOME to let the script find the pinned SDK, or point it at one" >&2
      exit 1
    fi
    export FLUTTER_HOME
    return
  fi
  for sdk in "$HOME/flutter-$want" "$HOME/fvm/versions/$want" "$HOME/flutter"; do
    if [ "$(flutter_sdk_version "$sdk")" = "$want" ]; then
      export FLUTTER_HOME="$sdk"
      return
    fi
  done
  echo "✗ Flutter $want (pinned in app/.fvmrc) not found in ~/flutter-$want, ~/fvm/versions/$want or ~/flutter" >&2
  echo "  install it there (git clone -b $want https://github.com/flutter/flutter.git ~/flutter-$want)" >&2
  echo "  or set FLUTTER_HOME to it" >&2
  exit 1
}
