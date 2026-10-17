# Run a build step inside a bubblewrap sandbox (sourced, not run). MAINTENANCE.md §8.
#
#   nd_sandbox <command> [args...]
#
# A build runs third-party code as you: Gradle plugins, Cargo build scripts, pub package hooks, the
# NDK's tools. On this machine, that code could otherwise
#   * write website/, which IS the live onion site (served from the checkout, §12): a planted
#     binary or pgp.txt would be published the moment it is written;
#   * read the relay's and the website's onion keys, the GPG/SSH keys, tokens, the key stick;
#   * plant something that runs later (a git hook, ~/.bashrc, a systemd user unit, a script here).
# Inside the sandbox all of that is out of reach:
#   * the whole home directory is read-only, except the caches and SDKs a build writes to and the
#     checkout itself;
#   * in the checkout, website/, relay-state*/, .git/, scripts/, .github/ and the Makefile are
#     read-only, and gitlab_token.txt reads as empty;
#   * hidden (an empty directory in their place): ~/.gnupg, ~/.ssh, ~/.config/gh, the relay's Tor
#     keys (relay-state*/arti-*), the onion website's and stats job's state, the desktop app's own
#     data (~/.local/share/app.nightdrop*), ~/.config/nightdrop-signing, the PGP key folder, the
#     key stick ($NIGHTDROP_KEY_DIR and the removable-drive mounts), and /run/user/$UID — the
#     session bus, systemd's user manager (systemd-run would escape any sandbox), the agents'
#     sockets, the display;
#   * the release keystore and key.properties read as empty: no build signs with the release key;
#     the scripts sign the finished APKs afterwards, outside the sandbox (lib/keys.sh);
#   * adb's key reads as empty and no USB device is visible, so nothing can reach a phone.
# Environment variables whose names say KEY, TOKEN, SECRET, PASSWORD or CREDENTIAL are dropped.
# It gets a private /tmp, and a Gradle/Kotlin daemon of its own that ends with the build: an
# unsandboxed daemon left running from an earlier build would otherwise be handed this build's work.
# It also starts a new session, so nothing inside can push keystrokes into your terminal (TIOCSTI).
# The internet stays (Gradle, pub and Cargo download), but in a network namespace of its own: no
# service listening on this machine (adb, databases, the VM's SSH, the onion site) is reachable.
# Set ND_NO_SANDBOX=1 to run without it.

nd_sandbox() {
  if [ -n "${ND_NO_SANDBOX:-}" ]; then
    echo "warning: ND_NO_SANDBOX set — running the build step unsandboxed" >&2
    "$@"
    return
  fi
  if ! command -v bwrap >/dev/null 2>&1; then
    echo "error: bubblewrap (bwrap) not found — sudo dnf install bubblewrap, or ND_NO_SANDBOX=1" >&2
    return 1
  fi
  local root="${PROJECT_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
  local uid
  uid="$(id -u)"
  # A private /tmp: no sockets or files shared with what runs outside (the X11 socket, other
  # tools' temp files). Mounted before the binds below, so a checkout under /tmp stays visible.
  local -a a=(--dev-bind / / --ro-bind "$HOME" "$HOME" --tmpfs /tmp)
  local d
  # Writable: what a Flutter/Gradle/Cargo build writes to.
  for d in "$root" "${FLUTTER_HOME:-}" "$HOME/.pub-cache" "$HOME/.gradle" "$HOME/.cargo" \
           "$HOME/.rustup" "$HOME/.cache" "$HOME/.android" "$HOME/.dart-tool" "$HOME/.dartServer" \
           "$HOME/.flutter-devtools" "$HOME/.config/flutter" "$HOME/.config/dart" "$HOME/.kotlin" \
           "$HOME/.m2" "${ANDROID_HOME:-}" "${ND_SANDBOX_RW:-}"; do
    [ -n "$d" ] && [ -d "$d" ] && a+=(--bind "$d" "$d")
  done
  # Flutter keeps two state files directly in $HOME.
  for d in "$HOME/.flutter" "$HOME/.flutter_tool_state"; do
    [ -f "$d" ] && a+=(--bind "$d" "$d")
  done
  # Gradle and Kotlin hand a build to an already-running daemon of theirs. One started outside the
  # sandbox would run this build's code unsandboxed, so the sandbox sees an empty daemon registry
  # and starts its own. Its own PID namespace ends that daemon, and everything else the build
# started, when the build ends.
  mkdir -p "$HOME/.local/share/kotlin/daemon" 2>/dev/null || true
  for d in "$HOME/.gradle/daemon" "$HOME/.local/share/kotlin/daemon"; do
    [ -n "$d" ] && [ -d "$d" ] && a+=(--tmpfs "$d")
  done
  # Read-only inside the checkout.
  for d in "$root/website" "$root/.git" "$root/scripts" "$root/.github" "$root/Makefile" \
           "$root"/relay-state*; do
    [ -e "$d" ] && a+=(--ro-bind "$d" "$d")
  done
  # Hidden. A hidden file reads as empty: /dev/null in its place (bound as a device, because
  # bwrap's other binds forbid device nodes and the read would fail instead; writes go nowhere).
  for d in "$HOME/.gnupg" "$HOME/.ssh" "$HOME/.config/gh" "$HOME/.local/share/nightdrop-stats" \
           "$HOME/.local/share/nightdrop-website-onion" "$HOME/rust-projects/pgp-keygen" \
           "$HOME/.config/nightdrop-signing" "$HOME"/.local/share/app.nightdrop* \
           "$root"/relay-state*/arti-state "$root"/relay-state*/arti-cache \
           "${NIGHTDROP_KEY_DIR:-}" "/run/media/$USER" "/media/$USER" "/run/user/$uid"; do
    [ -n "$d" ] && [ -d "$d" ] && a+=(--tmpfs "$d")
  done
  # Nor any secret in the environment (API keys from ~/.bashrc and the like).
  local v
  for v in $(compgen -e); do
    case "${v^^}" in *KEY*|*TOKEN*|*SECRET*|*PASSWORD*|*PASSWD*|*CREDENTIAL*) a+=(--unsetenv "$v") ;; esac
  done
  [ -f "$root/gitlab_token.txt" ] && a+=(--dev-bind /dev/null "$root/gitlab_token.txt")
  # adb's key too: with it, and the USB devices, build code could start an adb server of its own
  # and drive any phone that trusts this machine. USB devices are hidden below.
  for d in "$root/app/android/key.properties" "$HOME/.android/nightdrop-release.jks" \
           "$HOME/.android/adbkey" "$HOME/.android/adb_known_hosts.pb"; do
    [ -f "$d" ] && a+=(--dev-bind /dev/null "$d")
  done
  [ -d /dev/bus/usb ] && a+=(--tmpfs /dev/bus/usb)
  # Its own network namespace (pasta, the user-mode network podman uses): the internet as usual,
  # but none of this machine's loopback or LAN services. Sharing the host's network let build code
  # drive the adb server on 127.0.0.1:5037 — no authentication — and through it any phone that is
  # plugged in or paired. DNS still works through systemd-resolved's socket.
  if ! command -v pasta >/dev/null 2>&1; then
    echo "error: pasta (passt) not found — sudo dnf install passt, or ND_NO_SANDBOX=1" >&2
    return 1
  fi
  # bwrap by absolute path: pasta runs confined (SELinux pasta_t) and, given a bare name, searches
  # $PATH — including ~/.local/bin, which the policy denies, raising an AVC alert on every build.
  pasta --config-net --quiet -T none -U none -t none -u none --no-map-gw -D none -- \
  "$(command -v bwrap)" "${a[@]}" --unshare-user --uid "$uid" --gid "$(id -g)" \
    --unshare-pid --proc /proc --die-with-parent --new-session \
    --unsetenv DBUS_SESSION_BUS_ADDRESS --unsetenv SSH_AUTH_SOCK --unsetenv GPG_AGENT_INFO \
    --unsetenv DISPLAY --unsetenv WAYLAND_DISPLAY --unsetenv XAUTHORITY \
    --chdir "$PWD" -- "$@"
}
