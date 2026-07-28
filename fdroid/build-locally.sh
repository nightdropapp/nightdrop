#!/usr/bin/env bash
# Run the F-Droid build for Night Drop locally, in the same container image and with the same
# command fdroiddata's CI uses to gate merge requests — so a green run here means the MR's
# `fdroid build` job should pass too.
#
#   ./fdroid/build-locally.sh            # full build (long: NDK + arti + Flutter AOT)
#   ./fdroid/build-locally.sh setup      # provision only, then exit (fast plumbing check)
#   ./fdroid/build-locally.sh shell      # interactive shell in the buildserver
#   ./fdroid/build-locally.sh --fresh …  # discard the cached volume first
#
# This is NOT the Vagrant buildserver VM (`makebuildserver`); fdroiddata's own MR pipeline runs
# `fdroid build --on-server` inside this image, so this reproduces the gate that actually judges
# the MR. Reference: fdroiddata .gitlab-ci.yml, job "fdroid build".
set -euo pipefail

IMAGE=registry.gitlab.com/fdroid/fdroidserver:buildserver-trixie
VOLUME=fdroid-vagrant           # caches srclibs/gradle/pub/cargo between runs
SDK_VOLUME=fdroid-sdk           # caches /opt/android-sdk, incl. the ~1 GB NDK
# Both volumes are mounted :z — on SELinux systems podman gives a named volume a per-container
# MCS category, so without the shared label the *next* container is denied access to content the
# previous one wrote (shows up as "Permission denied" on the NDK, even for root).
APPID=app.nightdrop
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ARTIFACTS=${FDROID_LOCAL_ARTIFACTS:-$HOME/.cache/fdroid-local}

MODE=build
if [ "${1:-}" = "--fresh" ]; then
    echo "==> removing cached volume $VOLUME"
    podman volume rm -f "$VOLUME" >/dev/null 2>&1 || true
    podman volume rm -f "$SDK_VOLUME" >/dev/null 2>&1 || true
    shift
fi
[ $# -gt 0 ] && MODE=$1

# versionCode to build, read from the recipe so this can't drift.
VERCODE=$(sed -n -E 's/^ *versionCode: ([0-9]+)$/\1/p' "$REPO_ROOT/fdroid/$APPID.yml" | tail -1)
[ -n "$VERCODE" ] || { echo "could not read versionCode from the recipe" >&2; exit 1; }
# NDK release name (e.g. r28c), also from the recipe. Pre-installed as root during provisioning:
# fdroidserver's auto-install runs as `vagrant` and silently skips the download there.
NDKVER=$(sed -n -E 's/^ *ndk: *(.+)$/\1/p' "$REPO_ROOT/fdroid/$APPID.yml" | tail -1)
echo "==> $APPID:$VERCODE   mode=$MODE   image=$IMAGE"

mkdir -p "$ARTIFACTS/recipe"
# Staged through the artifacts dir rather than bind-mounted from the repo: on SELinux systems a
# direct mount needs a :z relabel, and relabelling files inside the git tree is rude.
cp "$REPO_ROOT/fdroid/$APPID.yml" "$ARTIFACTS/recipe/$APPID.yml"
# Chicken-and-egg: once the recipe declares binary:, fdroidserver downloads that APK to compare
# against and fails if it is missing. So the build that PRODUCES the release APK has to run with
# binary: stripped; re-run without SKIP_BINARY afterwards to get the reproducibility verdict.
if [ "${SKIP_BINARY:-0}" = 1 ]; then
    sed -i "/^ *binary: /d" "$ARTIFACTS/recipe/$APPID.yml"
    echo "==> binary: stripped for this run (pre-publication build)"
fi
podman volume create "$VOLUME" >/dev/null 2>&1 || true
podman volume create "$SDK_VOLUME" >/dev/null 2>&1 || true
podman image exists "$IMAGE" || podman pull "$IMAGE"

# The recipe builds from the *published* commit, so nothing local leaks into the build. Only the
# metadata is taken from the working tree — that is the thing under test.
podman run --rm -i \
    --volume "$VOLUME:/home/vagrant:z" \
    --volume "$SDK_VOLUME:/opt/android-sdk:z" \
    --volume "$ARTIFACTS:/mnt/out:z" \
    --env MODE="$MODE" --env APPID="$APPID" --env VERCODE="$VERCODE" --env NDKVER="$NDKVER" --env WS_PATH="${WS_PATH:-}" \
    "$IMAGE" bash -lc '
set -euxo pipefail
source /etc/profile.d/bsenv.sh
# Workspace is $home_vagrant itself, not a subdir: fdroiddata CI does `pushd $home_vagrant`
# before `fdroid build`, so the build dir is /home/vagrant/build/<appid>. Matching that path
# matters — the Rust core is path-remapped, but Flutter AOT/dex may embed absolute paths, and a
# published APK only byte-matches an F-Droid rebuild if both are built at the same path.
# (Do not use apostrophes in this block: the whole script is a single-quoted argument to bash -lc.)
WS=${WS_PATH:-$home_vagrant}

# --- provision, mirroring the CI job -------------------------------------------------
apt-get -q update
apt-get -qy install sudo   # `fdroid build --on-server` expects sudo and uninstalls it after
# The SDK must be owned by the build user. Installing it as root leaves a tree that `vagrant`
# cannot read (and, in a rootless-podman volume, that not even root can read afterwards), which
# fails later in fdroidserver init_ndk_paths().
chown -R vagrant "$ANDROID_HOME"
sdk="sudo -u vagrant env ANDROID_HOME=$ANDROID_HOME HOME=$home_vagrant sdkmanager"
$sdk "platform-tools" "build-tools;31.0.0" >/dev/null
if [ -n "$NDKVER" ] && [ ! -e "$ANDROID_HOME/.ndk-$NDKVER-installed" ]; then
    $sdk "ndk;$NDKVER"
    touch "$ANDROID_HOME/.ndk-$NDKVER-installed"
fi

if [ ! -x "$fdroidserver/fdroid" ]; then
    mkdir -p "$fdroidserver"
    curl --silent https://gitlab.com/fdroid/fdroidserver/-/archive/master/fdroidserver-master.tar.gz \
        | tar -xz --directory="$fdroidserver" --strip-components=1
fi
export PATH="$fdroidserver:$PATH"
export PYTHONPATH="$fdroidserver:$fdroidserver/examples"
export PYTHONUNBUFFERED=true
export GRADLE_USER_HOME=$home_vagrant/.gradle

# --- workspace: a minimal fdroiddata ---------------------------------------------------
mkdir -p "$WS"/{metadata,srclibs,build,tmp,logs,unsigned}
cp /mnt/out/recipe/$APPID.yml "$WS/metadata/$APPID.yml"
# srclib definition, same as fdroiddata/srclibs/flutter.yml
printf "RepoType: git\nRepo: https://github.com/flutter/flutter.git\n" > "$WS/srclibs/flutter.yml"
cat > "$WS/config.yml" <<EOF
repo_url: https://f-droid.org/repo
repo_name: local
repo_description: local build test
sdk_path: /opt/android-sdk
refresh_scanner: true
EOF
chown -R vagrant "$home_vagrant"
chmod 0600 "$WS/config.yml"

fdroid="sudo --preserve-env --user vagrant
    env PATH=$fdroidserver:$PATH
    env PYTHONPATH=$fdroidserver:$fdroidserver/examples
    env PYTHONUNBUFFERED=true env HOME=$home_vagrant
    env GRADLE_USER_HOME=$GRADLE_USER_HOME env ANDROID_HOME=$ANDROID_HOME
    fdroid"

cd "$WS"
if [ "$MODE" = shell ]; then exec bash; fi

$fdroid readmeta
$fdroid lint "$APPID" || true       # category names need fdroiddata config/, so never fatal here
$fdroid fetchsrclibs "$APPID:$VERCODE" --verbose
if [ "$MODE" = setup ]; then echo "SETUP OK"; exit 0; fi

# The exact command fdroiddata CI runs.
set +e
(unset CI; $fdroid build --verbose --test --refresh-scanner --on-server --no-tarball "$APPID:$VERCODE")
rc=$?
set -e

cp -a "$WS/logs/." /mnt/out/logs/ 2>/dev/null || { mkdir -p /mnt/out/logs; cp -a "$WS"/logs/. /mnt/out/logs/ 2>/dev/null || true; }
mkdir -p /mnt/out/apk
find "$WS/tmp" -maxdepth 1 -name "*.apk" -exec cp -a {} /mnt/out/apk/ \; 2>/dev/null || true
chmod -R a+rX /mnt/out || true
echo "BUILD EXIT: $rc"
exit $rc
'
echo "==> artifacts in $ARTIFACTS"
