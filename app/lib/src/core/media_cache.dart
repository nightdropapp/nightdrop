import 'dart:io';

import 'package:flutter/foundation.dart';

/// Process-wide caches of **decrypted** media, shared by the chat UI (fast rebuilds,
/// pre-warmed video opens) and wiped by [NightdropCore.logout] so no plaintext outlives the
/// identity it belongs to.
class MediaCache {
  MediaCache._(); // static-only

  /// Decrypted attachment bytes per media id (media is immutable once stored, so entries
  /// never go stale). Keyed by `mediaId` or `'thumb:<thumbId>'`.
  static final Map<String, Future<Uint8List>> bytes = {};

  /// Decrypted temp-file paths per media id (videos opened in the system player).
  static final Map<String, Future<String>> files = {};

  /// Drop every cached decrypt and delete the plaintext temp files written by
  /// `mediaToFile` (the Rust core names them `nightdrop-media-<id>.<ext>` in the OS temp
  /// dir). Best-effort: a file the OS already reclaimed, or one held open by an external
  /// player, is skipped silently.
  static Future<void> wipe() async {
    bytes.clear();
    files.clear();
    try {
      await for (final entry in Directory.systemTemp.list()) {
        if (entry is File &&
            entry.uri.pathSegments.last.startsWith('nightdrop-media-')) {
          try {
            await entry.delete();
          } catch (_) {}
        }
      }
    } catch (_) {}
  }
}
