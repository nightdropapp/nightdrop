## Required

* [x] The app complies with the [inclusion criteria](https://f-droid.org/docs/Inclusion_Policy)
* [x] The original app author has been notified (and does not oppose the inclusion) — I am the app author.
* [x] All related [fdroiddata](https://gitlab.com/fdroid/fdroiddata/issues) and [RFP issues](https://gitlab.com/fdroid/rfp/issues) have been referenced in this merge request — there is no prior RFP or fdroiddata issue for this app.
* [x] Builds with `fdroid build` and all pipelines pass
* [x] There is an issue tracker and contact info of the author so that we can report bugs and contact the author.

## Strongly Recommended

* [x] The upstream app source code repo contains the app metadata _(summary/description/images/changelog/etc)_ in a [Fastlane](https://gitlab.com/snippets/1895688) or [Triple-T](https://gitlab.com/snippets/1901490) folder structure
* [x] Releases are tagged and auto update is enabled

## Suggested

* [ ] External repos are added as git submodules instead of srclibs — Flutter is provisioned with the `flutter@stable` srclib, following the srclib variant of `templates/build-flutter.yml`; it is not vendored as a submodule upstream. The Flutter version is pinned in `app/.fvmrc` and extracted by the recipe, so bumping it needs no change here either.
* [x] Enable [Reproducible Builds](https://f-droid.org/docs/Reproducible_Builds)
* [x] Multiple apks for native code — built per ABI (armeabi-v7a / arm64-v8a / x86_64), 35–46 MB each instead of one 113 MB universal APK.

/label ~"New App"
