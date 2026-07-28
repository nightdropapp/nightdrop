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

* [ ] External repos are added as git submodules instead of srclibs — Flutter is provisioned with the `flutter@stable` srclib, following the srclib variant of `templates/build-flutter.yml`. It is not vendored as a submodule upstream.
* [x] Enable [Reproducible Builds](https://f-droid.org/docs/Reproducible_Builds)
* [ ] Multiple apks for native code — currently one universal APK (arm64-v8a / armeabi-v7a / x86_64).

/label ~"New App"
