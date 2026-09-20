import 'dart:io' show Directory, File;

import 'package:flutter/foundation.dart' show visibleForTesting;
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';

/// Moves a finished file somewhere the user can actually find it.
///
/// The update download used to land in `getApplicationSupportDirectory()` — app-private, so
/// installing it meant hunting through a file manager, if it could be reached at all.
///
/// The tempting cheap fix, `getExternalStorageDirectory()`, does not solve that on any current
/// phone: it returns a path under `/Android/data/`, and since **Android 11** the system Files app
/// and SAF-based file managers refuse to navigate there. It is kept below only as the pre-Android-10
/// fallback, where that lockdown does not exist and the public folder would cost
/// `WRITE_EXTERNAL_STORAGE` — a permission this app deliberately strips from its manifest.
///
/// So on API 29+ this goes through `MediaStore.Downloads` (see `Downloads.kt`), which reaches the
/// real Downloads folder and needs **no permission at all**.
///
/// [publish] takes an already-complete, already-verified file. It is not a download sink: the bytes
/// are hashed against what the onion site published *before* anything reaches a user-visible path,
/// so a file the user can see is always one that passed verification.
class PublicDownloads {
  PublicDownloads._();

  static const _channel = MethodChannel('app.nightdrop/downloads');

  /// The pre-API-29 fallback location, injectable so it can be tested. path_provider answers this
  /// one from the host platform rather than a mockable channel — on a Linux test host it throws
  /// `UnsupportedError` before any channel is consulted — so without a seam here the fallback
  /// branch could only ever be exercised on a real phone.
  @visibleForTesting
  static Future<Directory?> Function() externalDirectory =
      getExternalStorageDirectory;

  /// Publishes [source] under [displayName], deleting the original on success. Returns a location
  /// to show the user, or the source's own path if it could not be moved anywhere better —
  /// never null, because the file exists either way and the user is entitled to be told where.
  ///
  /// There is no `Platform.isAndroid` gate: on any other platform the channel is simply absent and
  /// the fallbacks below already handle absence. Checking twice would only add a way for the two
  /// answers to disagree.
  static Future<String> publish(
    File source, {
    required String displayName,
    String mimeType = 'application/octet-stream',
  }) async {
    try {
      final where = await _channel.invokeMethod<String>('publish', {
        'srcPath': source.path,
        'displayName': displayName,
        'mimeType': mimeType,
      });
      if (where != null) {
        // MediaStore holds its own copy now. Leaving ours behind would double a ~45MB file on a
        // device whose storage pressure is the reason this is worth being careful about.
        await source.delete().catchError((_) => source);
        return where;
      }
    } catch (_) {
      // No channel (test harness), or MediaStore refused. Fall through — a findable-ish file beats
      // failing a download that already succeeded.
    }

    // Pre-API-29 only, or MediaStore unavailable. Browsable on those releases.
    try {
      final dir = await externalDirectory();
      if (dir != null) {
        await dir.create(recursive: true);
        final dest = File('${dir.path}/$displayName');
        await source.rename(dest.path);
        return dest.path;
      }
    } catch (_) {
      // Same reasoning: the file is downloaded and verified. Report where it is.
    }
    return source.path;
  }

  /// Where a download should be assembled before it is verified: app-private, unreadable by other
  /// apps, and never somewhere a file manager will offer it to the user mid-write.
  static Future<Directory> staging() => getApplicationSupportDirectory();
}
