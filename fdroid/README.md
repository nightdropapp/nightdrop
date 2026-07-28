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

- **Flutter** comes from the `flutter@stable` srclib, then `prebuild` checks out the exact
  revision we release with. Pinned by **full commit hash**, not the tag.
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
- **No `binary:`** — see below.

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

`rewritemeta` canonicalises formatting (including line wrapping — it now *unwraps* long build
commands rather than folding them), so run it and expect an empty diff:

```sh
python3 -m venv /tmp/fdv && /tmp/fdv/bin/pip install fdroidserver
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
