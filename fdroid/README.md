# F-Droid metadata

`app.nightdrop.yml` is a **byte-identical copy** of `metadata/app.nightdrop.yml` in
[fdroiddata](https://gitlab.com/fdroid/fdroiddata) (MR
[!43625](https://gitlab.com/fdroid/fdroiddata/-/merge_requests/43625)). Keep it that way — no
local-only comments — so syncing is a plain `diff`. Explanations aimed at F-Droid maintainers
belong in the recipe's `MaintainerNotes:`; everything else belongs here.

The store listing (title, summary, description, changelogs) is **not** in the recipe — F-Droid
pulls it from `fastlane/metadata/android/en-US/`. Reproducibility evidence lives in
`docs/reproducible-builds.md`.

`mr-description.md` mirrors the MR description. It is fdroiddata's **App inclusion** template
and nothing else: the boilerplate above "Please remove above lines!" is deleted, the `Closes
rfp#…` lines are dropped (no RFP issue exists), and the task boxes are ticked. Do **not** put an
app summary or description in it — not in the description, not in the `.yml`. F-Droid pulls both
from the fastlane folder, and the template says so explicitly. Anything else (a reply to review
feedback, a pipeline link) belongs in an MR *comment*, not the description.

## Recipe shape

It follows [`templates/build-flutter.yml`](https://gitlab.com/fdroid/fdroiddata/-/blob/master/templates/build-flutter.yml),
srclib variant (we don't vendor Flutter as a submodule):

- **Flutter** comes from the `flutter@stable` srclib. The version is pinned upstream in
  `app/.fvmrc` (the standard fvm file, also consumed by this repo's CI) and *extracted* by the
  recipe, so a Flutter bump needs no fdroiddata MR and CI cannot drift from what we ship.
- **The build runs at a fixed path** (`/build/nightdrop`), per the template's "use the same build
  path as upstream" step. This is load-bearing. Measured across two build paths, 9 entries
  differ: `libapp.so` (the Flutter AOT snapshot), `libdartjni.so` and `libflutter_zxing.so`, on
  all three ABIs — `libnightdrop.so` does not, because the remap flags cover our crate and
  nothing else. Verified by building 0.1.8 at `/home/vagrant/somewhere/else` and still matching
  the published APK (`./fdroid/build-locally.sh` honours `WS_PATH=` for exactly this test).
- **Rust** comes from Debian's `rustup` package (`apt-get install -y rustup`), which puts the
  `cargo`/`rustc` proxies in `/usr/bin`. Never install it from `sh.rustup.rs` — F-Droid does not
  accept fetching a toolchain installer over the network.
- **`commit:`** is always the full 40-char hash. Tags and branches are rejected.
- **`PUB_CACHE`** is relocated into the tree so the source scanner sees the Dart packages, and
  `app/.pub-cache` is `scandelete`d. `scandelete` deletes only the individual files the scanner
  flags (gradle wrappers, `archive`'s test fixtures, `example/` maven repos) — not the
  directory — so the build still has its packages.
- **Remap flags go in `CARGO_ENCODED_RUSTFLAGS`, not `RUSTFLAGS`.** cargokit's Android
  environment sets the encoded variable itself (an `-L` libgcc workaround), and that overrides
  plain `RUSTFLAGS` entirely in cargo — a local buildserver run with `RUSTFLAGS` produced a core
  still carrying 779 `/home/vagrant/.cargo/registry` paths. cargokit preserves and appends to the
  encoded variable, so the remap survives there; `\037` separates encoded flags.
- **cargokit is patched** in `prebuild`. It runs `rustup run stable cargo build`, which
  overrides `rust-toolchain.toml`, so without the patch the core is built with whatever
  `stable` happens to be that day. Note our `sed` differs from the one every other cargokit
  recipe uses: `s/'stable'/'<version>'/` only rewrites the `?? 'stable'` fallback, and because
  we ship `core/cargokit.yaml` the toolchain comes from the parsed options instead — so the
  whole `_toolchain` expression has to be replaced. Re-check this after any cargokit update.
- Non-Android platform dirs are `rm`'d. `rm`/`scandelete` paths are relative to the **repo
  root**, not `subdir`.
- **`binary:`** declares the published APK so F-Droid verifies reproducibility — see below.

## Reproducible builds (`binary:`)

`AllowedAPKSigningKeys` alone does not earn "reproducible" status; F-Droid also needs `binary:`,
the URL of the published APK, to rebuild and compare. 1398 of the 1414 fdroiddata recipes that
pin signing keys declare it. **It must be in place before the app is merged** — per the
inclusion template, an app published under F-Droid's key cannot switch to developer-signed
later without breaking upgrades for everyone who installed it.

v0.1.6 could not have it: that APK was built without the remap flags (826 `/home/shawn/…` paths
in the shipped `.so`), so no rebuild could ever byte-match it. **v0.1.7 is the first release
built by `build-locally.sh`**, i.e. in F-Droid's own container at F-Droid's own build path, and
it verifies:

```
Successfully built app.nightdrop:8 from 210daa37c0538ece325ac96d8091b8c086755edf
...successfully verified
compared built binary to supplied reference binary successfully
```

### Cutting a release that stays verifiable

1. Bump `app/pubspec.yaml`, then **run `make config`** — it regenerates `app_version.dart`, and
   skipping it ships a build whose About screen shows the previous version.
2. Add a changelog for **every** versionCode the recipe builds — with the per-ABI split that is
   `changelogs/1012.txt`, `2012.txt`, `4012.txt`, not the pubspec's base. F-Droid resolves
   `changelogs/<versionCode>.txt` and reads it from the *tagged commit*, so a missing file means
   the release ships with no "What's New" and cannot be fixed afterwards.
   `check-metadata.sh` fails if any is missing. Commit, tag, push.
3. Update the recipe's `commit:`/`versionName`/`versionCode`/`CurrentVersion*`, then build the
   release artifact with `SKIP_BINARY=1 ./fdroid/build-locally.sh --fresh` (the flag is needed
   because `binary:` makes fdroidserver download an APK that does not exist yet).

   **`--fresh` is not optional on a release**, and this bites every time: the cached volume holds a
   clone of this repo from the *previous* build, `fetchsrclibs` runs with `refresh=False`, and the
   commit you just pushed is not in it. The failure is
   `VCSException: Git checkout of '<sha>' failed … fatal: unable to read tree`, which reads like a
   bad hash rather than a stale cache. A clean volume also matches what CI does, so a green run here
   means more.
4. Sign it — **with `--alignment-preserved`**:

```sh
apksigner sign --ks "$storeFile" --ks-key-alias "$keyAlias" \
  --v1-signing-enabled false --v2-signing-enabled true \
  --v3-signing-enabled false --v4-signing-enabled false \
  --alignment-preserved \
  --in ~/.cache/fdroid-local/apk/app.nightdrop_<code>.apk --out NightDrop.apk
```

   Without that flag apksigner re-aligns entries, shifting every local header a few bytes. The
   contents still compare equal (fdroidserver's `diff -r` passes) but the copied v2 signature
   fails its CHUNKED_SHA512 check, so a genuinely reproducible build reports as **not**
   reproducible. This cost one failed verification round.
5. Publish, then re-run `./fdroid/build-locally.sh` (no `SKIP_BINARY`) and confirm
   "compared built binary to supplied reference binary successfully" for every block.
6. **Update MR !43625.** linsui asked for this explicitly on 2026-07-29: while the MR is queued
   for testing, any new release must be reflected in it, or they will test a version that is no
   longer current. Copy the recipe to the fork's `add-nightdrop` branch and push — this needs a
   GitLab token, which is not kept on this box.

Field order (top level and inside a build entry) must match `yaml_app_field_order` /
`build_flags` in fdroidserver's `metadata.py`, or `rewritemeta` fails CI.

## Peer recipes

15 apps in fdroiddata build with **cargokit**, our exact toolchain — closest are
`dev.khoj.pitaka.fdroid` (Rust 1.96.0, NDK r28c, near-identical shape),
`org.localsend.localsend_app` (`subdir: app` with the same `rm`/`scandelete` prefixing, and the
only other user of `CARGO_ENCODED_RUSTFLAGS`), `com.secluso.mobile`, and
`business.braid.polycule` (also vodozemac). Re-derive the list any time with:

```sh
curl -sL "https://gitlab.com/fdroid/fdroiddata/-/archive/master/fdroiddata-master.tar.gz?path=metadata" | tar xz
grep -l cargokit fdroiddata-master-metadata/metadata/*.yml
```

We match that cohort on every structural axis (Debian rustup, Flutter srclib, cargokit patch,
`--enforce-lockfile`, relocated `PUB_CACHE`, `rm`/`scandelete`, NDK r28c). Two deliberate
departures, both justified above: `CARGO_ENCODED_RUSTFLAGS` + remap (only LocalSend also does
this, and it is required for the flags to take effect at all), and no `binary:` while
`AllowedAPKSigningKeys` is set (`com.kjxbyz.picguard` is the same).

Worth knowing when editing: **no metadata file in fdroiddata contains a `#` comment** — 0 of
6360. Rationale goes in `MaintainerNotes` (used by 11%) or here.

## Run the actual F-Droid build locally

`./fdroid/build-locally.sh` runs the build in the same container image and with the same command
fdroiddata's CI uses to gate merge requests, so a green run here means the MR's `fdroid build`
job should pass. It is **not** the Vagrant buildserver VM (`makebuildserver`) — that needs
Vagrant + libvirt/VirtualBox and ~200 GB; fdroiddata's own pipeline runs
`fdroid build --on-server` inside `registry.gitlab.com/fdroid/fdroidserver:buildserver-trixie`,
which is what this reproduces. Requirements: podman (or docker, swap the command) and ~40 GB.

```sh
./fdroid/build-locally.sh setup     # provision only — fast plumbing check
./fdroid/build-locally.sh           # full build, ~12 min warm / ~25 min cold
./fdroid/build-locally.sh --fresh   # nuke the cached volumes first
```

The APK lands in `~/.cache/fdroid-local/apk/`, logs next to it. Two podman volumes cache the
Flutter srclib, gradle, pub, cargo and the NDK between runs. Notes for anyone touching the
script, both learned the hard way:

- **Mount volumes `:z`.** With SELinux, podman gives a named volume a per-container MCS
  category, so the *next* container gets "Permission denied" on what the previous one wrote —
  including a `chown -R` failing as root.
- **Install the SDK/NDK as `vagrant`, not root.** fdroidserver's own NDK auto-install runs as
  `vagrant` and silently fails to download; installing as root leaves a tree the build user
  cannot read.

## Validate before pushing to the MR

Run **`./fdroid/check-metadata.sh`** (add `--write` to apply). It reproduces the CI job exactly
and is the only check that counts.

⚠️ **Do not validate with a pip-installed fdroidserver.** The `fdroid rewritemeta` job runs on
`debian:trixie-slim` and takes its *code* from the fdroidserver master tarball but its
*dependencies* from the Debian package — and line wrapping is decided by **ruamel.yaml**, so
Debian's ruamel version defines the canonical form. A pip install (release *or* master) wraps at
a different width and will bless a file that CI then rejects. This cost two failed pipelines:
the PyPI release unwrapped a long line CI wanted folded, and pip-master folded four lines CI
wanted flat.

## Why there is no `binary:` yet

`AllowedAPKSigningKeys` alone does not get "reproducible" status; F-Droid also needs `binary:`
(the URL of our published APK) to rebuild and compare. 1398 of the 1414 fdroiddata recipes that
pin signing keys do declare it, so expect to be asked.

We can't yet: the published `v0.1.6` `NightDrop.apk` was built **without** the remap
`RUSTFLAGS` the recipe uses — its `libnightdrop.so` still contains ~826 `/home/shawn/…` paths —
so an F-Droid rebuild cannot byte-match it and verification would fail. Adding `binary:` only
makes sense from the first release whose APK is built with the *same* flags the recipe sets:

```sh
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export RUSTFLAGS="--remap-path-prefix=$PWD=. --remap-path-prefix=$CARGO_HOME=/cargo"
```

Then add, in the build entry (after `output:`):
`binary: https://github.com/nightdropapp/nightdrop/releases/download/v%v/NightDrop.apk`
(`%v` = versionName, `%c` = versionCode). The published APK already matches the recipe in every
other respect: `app.nightdrop`, versionCode 7, universal (arm64-v8a/armeabi-v7a/x86_64), and its
signing cert SHA-256 equals `AllowedAPKSigningKeys`.

Field order (top level and inside a build entry) must match `yaml_app_field_order` /
`build_flags` in fdroidserver's `metadata.py`, or `rewritemeta` fails CI.

## Peer recipes

15 apps in fdroiddata build with **cargokit**, our exact toolchain — closest are
`dev.khoj.pitaka.fdroid` (Rust 1.96.0, NDK r28c, near-identical shape),
`org.localsend.localsend_app` (`subdir: app` with the same `rm`/`scandelete` prefixing, and the
only other user of `CARGO_ENCODED_RUSTFLAGS`), `com.secluso.mobile`, and
`business.braid.polycule` (also vodozemac). Re-derive the list any time with:

```sh
curl -sL "https://gitlab.com/fdroid/fdroiddata/-/archive/master/fdroiddata-master.tar.gz?path=metadata" | tar xz
grep -l cargokit fdroiddata-master-metadata/metadata/*.yml
```

We match that cohort on every structural axis (Debian rustup, Flutter srclib, cargokit patch,
`--enforce-lockfile`, relocated `PUB_CACHE`, `rm`/`scandelete`, NDK r28c). Two deliberate
departures, both justified above: `CARGO_ENCODED_RUSTFLAGS` + remap (only LocalSend also does
this, and it is required for the flags to take effect at all), and no `binary:` while
`AllowedAPKSigningKeys` is set (`com.kjxbyz.picguard` is the same).

Worth knowing when editing: **no metadata file in fdroiddata contains a `#` comment** — 0 of
6360. Rationale goes in `MaintainerNotes` (used by 11%) or here.

## Run the actual F-Droid build locally

`./fdroid/build-locally.sh` runs the build in the same container image and with the same command
fdroiddata's CI uses to gate merge requests, so a green run here means the MR's `fdroid build`
job should pass. It is **not** the Vagrant buildserver VM (`makebuildserver`) — that needs
Vagrant + libvirt/VirtualBox and ~200 GB; fdroiddata's own pipeline runs
`fdroid build --on-server` inside `registry.gitlab.com/fdroid/fdroidserver:buildserver-trixie`,
which is what this reproduces. Requirements: podman (or docker, swap the command) and ~40 GB.

```sh
./fdroid/build-locally.sh setup     # provision only — fast plumbing check
./fdroid/build-locally.sh           # full build, ~12 min warm / ~25 min cold
./fdroid/build-locally.sh --fresh   # nuke the cached volumes first
```

The APK lands in `~/.cache/fdroid-local/apk/`, logs next to it. Two podman volumes cache the
Flutter srclib, gradle, pub, cargo and the NDK between runs. Notes for anyone touching the
script, both learned the hard way:

- **Mount volumes `:z`.** With SELinux, podman gives a named volume a per-container MCS
  category, so the *next* container gets "Permission denied" on what the previous one wrote —
  including a `chown -R` failing as root.
- **Install the SDK/NDK as `vagrant`, not root.** fdroidserver's own NDK auto-install runs as
  `vagrant` and silently fails to download; installing as root leaves a tree the build user
  cannot read.

## Validate before pushing to the MR

⚠️ **Install fdroidserver from master, not from PyPI.** fdroiddata CI installs it from the master
tarball, and master disagrees with the released version about line wrapping: the release
*unwraps* long build commands, master *folds* them. Validating against the PyPI release produced
a file that passed locally and then failed CI's `fdroid rewritemeta` on formatting alone.

`rewritemeta` canonicalises formatting, so run it and expect an empty diff:

```sh
python3 -m venv /tmp/fdv
/tmp/fdv/bin/pip install https://gitlab.com/fdroid/fdroidserver/-/archive/master/fdroidserver-master.tar.gz
mkdir -p /tmp/fdtest/metadata && cd /tmp/fdtest
cp <repo>/fdroid/app.nightdrop.yml metadata/
printf 'repo_url: https://example.com/fdroid/repo\nrepo_name: t\nrepo_description: t\n' > config.yml
/tmp/fdv/bin/fdroid rewritemeta app.nightdrop && diff -u <repo>/fdroid/app.nightdrop.yml metadata/app.nightdrop.yml
/tmp/fdv/bin/fdroid lint app.nightdrop
```

`lint` will falsely report `Categories 'Internet'/'Security' is not valid` — that only means the
throwaway config has no `config/categories.yml`; both are real fdroiddata categories. Any other
lint output is a genuine finding.

## Bumping a release

1. Tag upstream, then put the tag's **full commit hash** in `commit:`.
2. Update `versionName`/`versionCode` and `CurrentVersion`/`CurrentVersionCode` (the code comes
   from `app/pubspec.yaml`'s `version: X.Y.Z+N`).
3. If Flutter, the NDK, or Rust moved, update the pin **and** re-verify reproducibility, then
   update `MaintainerNotes` and `docs/reproducible-builds.md`.
4. Copy the file into the fdroiddata MR unchanged.
