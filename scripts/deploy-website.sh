#!/usr/bin/env bash
# Publish binaries to the locally-served Tor website and refresh its verification manifest.
#
# The onion service (nightdrop-onion.service) serves website/ live from disk, so copying a file
# into website/applications/ publishes it immediately — there is no deploy step to forget, and no
# restart. That cuts both ways: the site silently drifted to 0.1.6 while releases were at 0.1.10,
# and a careless copy would publish an unsigned build just as instantly.
#
# So this refuses to publish anything it cannot vouch for:
#   * APKs must be signed by the release key (SHA-256 below), never a debug key.
#   * SHA256SUMS is regenerated over everything actually present, and GPG-signed, along with each
#     binary — otherwise the site would serve files that fail the instructions in SECURITY.md.
#
# Usage:
#   scripts/deploy-website.sh                      # re-sign whatever is already deployed
#   scripts/deploy-website.sh FILE...              # publish these, then re-sign everything
set -euo pipefail

PROJECT_ROOT="${PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
WEB="$PROJECT_ROOT/website/applications"
GPG_ID="${NIGHTDROP_GPG_ID:-security@nightdrop.app}"
# Release signing cert, from `apksigner verify --print-certs`. Matches AllowedAPKSigningKeys in
# fdroid/app.nightdrop.yml — an APK signed by anything else must not reach the download page.
RELEASE_CERT_SHA256="d08b8e6408aff4f43556c05b819fef25afdd6912be7a678b0552468a4582c0ac"

c()  { printf '\033[0;34mℹ\033[0m %s\n' "$*"; }
ok() { printf '\033[0;32m✓\033[0m %s\n' "$*"; }
err(){ printf '\033[0;31m✗\033[0m %s\n' "$*" >&2; }

find_apksigner() {
    for p in "${ANDROID_SDK:-$HOME/android-sdk}"/build-tools/*/apksigner \
             "${ANDROID_HOME:-}"/build-tools/*/apksigner; do
        [ -x "$p" ] && { echo "$p"; return 0; }
    done
    return 1
}

# An APK reaches the site only if the release key signed it. This is the check that stops a debug
# build — which install-android-app.sh produces by default — becoming the public download.
verify_apk() {
    local apk=$1 signer
    signer=$(find_apksigner) || { err "apksigner not found; cannot verify $(basename "$apk")"; return 1; }
    local got
    got=$("$signer" verify --print-certs "$apk" 2>/dev/null |
              grep -m1 'certificate SHA-256 digest' | awk '{print $NF}')
    if [ "$got" != "$RELEASE_CERT_SHA256" ]; then
        err "$(basename "$apk") is not signed by the release key"
        err "  got:      ${got:-<unsigned or unreadable>}"
        err "  expected: $RELEASE_CERT_SHA256"
        return 1
    fi
    return 0
}

mkdir -p "$WEB/android" "$WEB/linux" "$WEB/signatures"

for f in "$@"; do
    [ -f "$f" ] || { err "no such file: $f"; exit 1; }
    case "$f" in
        *.apk)
            verify_apk "$f" || { err "refusing to publish it"; exit 1; }
            cp -f "$f" "$WEB/android/$(basename "$f")"
            ok "published android/$(basename "$f")"
            ;;
        *.AppImage)
            cp -f "$f" "$WEB/linux/$(basename "$f")"
            ok "published linux/$(basename "$f")"
            ;;
        *) err "don't know where to put $(basename "$f")"; exit 1 ;;
    esac
done

# Re-sign everything present, not just what was copied: a stale SHA256SUMS is worse than none,
# because SECURITY.md tells people to trust it.
cd "$WEB"
mapfile -t files < <(cd "$WEB" && ls android/*.apk linux/*.AppImage 2>/dev/null || true)
[ ${#files[@]} -gt 0 ] || { c "nothing deployed yet — nothing to sign"; exit 0; }

# Paths in SHA256SUMS are bare filenames so `sha256sum -c` works from the download directory,
# which is how a user who fetched the files will run it.
: > signatures/SHA256SUMS
for rel in "${files[@]}"; do
    (cd "$(dirname "$rel")" && sha256sum "$(basename "$rel")") >> signatures/SHA256SUMS
    gpg --batch --yes --pinentry-mode loopback --local-user "$GPG_ID" \
        --detach-sign --armor -o "signatures/$(basename "$rel").asc" "$rel"
done
gpg --batch --yes --pinentry-mode loopback --local-user "$GPG_ID" \
    --detach-sign --armor -o signatures/SHA256SUMS.asc signatures/SHA256SUMS
gpg --batch --yes --armor --export "$GPG_ID" > signatures/nightdrop-signing-key.asc

# The Android download button points at this folder rather than at one APK, so the visitor can
# choose. Left to itself the server would answer with a bare `python3 -m http.server` listing —
# four near-identical filenames with no way to tell which one to take — so write a real index.
# Generated here, from the files actually present, because a hand-written page would drift the
# moment a release lands (which is how the site ended up offering 0.1.6).
write_android_index() {
    local apksigner aapt
    apksigner=$(find_apksigner 2>/dev/null || true)
    aapt=${apksigner%/apksigner}/aapt2
    local rows="" apk name label ver size hash
    # Deliberate order: the safe choice first, then smaller/narrower ones.
    for apk in NightDrop.apk NightDrop-arm64-v8a.apk NightDrop-armeabi-v7a.apk NightDrop-x86_64.apk; do
        [ -f "android/$apk" ] || continue
        case "$apk" in
            NightDrop.apk)              label="Universal — runs on any phone. Take this if unsure." ;;
            *-arm64-v8a.apk)            label="arm64-v8a — almost every phone made since 2015." ;;
            *-armeabi-v7a.apk)          label="armeabi-v7a — older 32-bit phones." ;;
            *-x86_64.apk)               label="x86_64 — emulators, a few Intel tablets and Chromebooks." ;;
        esac
        ver=""
        [ -x "$aapt" ] && ver=$("$aapt" dump badging "android/$apk" 2>/dev/null |
            grep -oE "versionName='[^']+'" | cut -d"'" -f2)
        size=$(du -h "android/$apk" | cut -f1)
        hash=$(awk -v f="$apk" '$2==f {print $1}' signatures/SHA256SUMS)
        rows+="        <li><a class=\"btn ghost\" download href=\"$apk\">$apk</a>
          <span class=\"muted\">${ver:+v$ver · }$size</span>
          <p>$label</p>
          <code class=\"hash\">$hash</code></li>
"
    done
    cat > android/index.html <<HTML
<!doctype html>
<!-- GENERATED by scripts/deploy-website.sh from the files in this folder — do not edit by hand. -->
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Night Drop for Android — choose a build</title>
  <link rel="stylesheet" href="/styles.css" />
</head>
<body>
  <main>
    <section class="section">
      <h2>Night Drop for Android</h2>
      <p class="lead">Pick the build that matches your phone.</p>
      <ul class="downloads-list">
$rows      </ul>
      <p class="fineprint">
        Every build is the same app, signed by the same key — they differ only in which processor
        they carry code for. Installing a per-processor build after the universal one requires
        uninstalling first, because Android reads the change as a downgrade.
      </p>
      <p class="fineprint">
        The hash under each file is its SHA-256. Check it before installing:
        <code>sha256sum -c SHA256SUMS</code> using
        <a href="/applications/signatures/SHA256SUMS">SHA256SUMS</a>, whose
        <a href="/applications/signatures/SHA256SUMS.asc">signature</a> you can verify with our
        <a href="/applications/signatures/nightdrop-signing-key.asc">signing key</a>.
      </p>
      <p class="fineprint"><a href="/#download">← back to downloads</a></p>
    </section>
  </main>
</body>
</html>
HTML
    ok "wrote android/index.html chooser"
}
write_android_index

# The site offers several APKs; if a partial deploy leaves them at different versions, visitors
# get 0.1.13 for one CPU and 0.1.12 for another. Warn rather than block — the operator may be
# mid-way through a release — but never let it pass silently, which is how it drifted to 0.1.6.
apksigner=$(find_apksigner 2>/dev/null || true)
aapt=${apksigner%/apksigner}/aapt2
if [ -x "$aapt" ]; then
    versions=$(for a in android/*.apk; do
        [ -f "$a" ] || continue
        v=$("$aapt" dump badging "$a" 2>/dev/null | grep -oE "versionName='[^']+'" | cut -d"'" -f2)
        printf '%s\t%s\n' "${v:-?}" "$(basename "$a")"
    done)
    distinct=$(cut -f1 <<<"$versions" | sort -u | grep -c .)
    if [ "$distinct" -gt 1 ]; then
        err "the deployed APKs are not all the same version:"
        sed 's/^/      /' <<<"$versions" >&2
        err "visitors would get different versions depending on their CPU"
    else
        ok "all android downloads are $(cut -f1 <<<"$versions" | head -1)"
    fi
fi

ok "signed manifest over ${#files[@]} file(s)"
sed 's/^/    /' signatures/SHA256SUMS
c "served live from disk by nightdrop-onion.service — no restart needed"
