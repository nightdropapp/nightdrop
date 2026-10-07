# Patched copy of fc_native_video_thumbnail 3.0.1

Upstream: <https://pub.dev/packages/fc_native_video_thumbnail>, BSD-3-Clause (`LICENSE`). Used as a
path dependency from `app/pubspec.yaml`.

**Why:** the plugin's Linux half links the build machine's FFmpeg (`libavformat`, `libavcodec`,
`libavutil`, `libswscale`) and `libjpeg` at load time, so the Linux app cannot even start on a
system without exactly that FFmpeg. Built on Fedora 44 it needed FFmpeg 8 (`libavformat.so.62`),
which the AppImage did not carry, and the AppImage catalog's test on Ubuntu 22.04 failed with
"error while loading shared libraries: libavformat.so.62" (2026-10-07). Bundling FFmpeg would add
dozens of codec libraries to make one optional preview frame.

**What changed:** the `linux:` platform entry is removed from `pubspec.yaml` and the `linux/`
directory deleted, so Flutter neither builds nor links it. On Linux the Dart call then fails with
`MissingPluginException`, which `_videoThumbnail` in `chat_screen.dart` already treats as "no
thumbnail" — the receiver sees the generic video tile. The `example/` app is dropped too. Android,
iOS/macOS and Windows code is byte-for-byte upstream.

**Updating:** copy the new release over this directory, delete `linux/` and `example/` and the
`linux:` platform entry again, and keep this file.
