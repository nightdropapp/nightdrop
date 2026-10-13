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

  /// The cache keys each chat's history referenced at its last refresh.
  static final Map<String, Set<String>> _keysByChat = {};

  /// The cache keys a message list references: media ids and `thumb:`-prefixed thumbnail ids.
  static Set<String> keysOf(Iterable<({String mediaId, String thumbId})> media) => {
        for (final m in media) ...[
          if (m.mediaId.isNotEmpty) m.mediaId,
          if (m.thumbId.isNotEmpty) 'thumb:${m.thumbId}',
        ],
      };

  /// Called on every refresh of a chat's history: evict what the chat referenced last time and no
  /// longer does — a burned, unsent or expired attachment. Its decrypted bytes stayed in memory
  /// until logout, so a burned photo outlived its burn for the whole session (self-audit
  /// 2026-10-13, round 6). The core deletes the files; this drops the copies held here.
  static void retainChat(String chatId, Set<String> live) {
    final previous = _keysByChat[chatId] ?? const <String>{};
    for (final key in previous.difference(live)) {
      bytes.remove(key);
      files.remove(key);
    }
    _keysByChat[chatId] = live;
  }

  /// A deleted chat: drop everything it referenced.
  static void forgetChat(String chatId) => retainChat(chatId, const <String>{});

  /// Drop every cached decrypt and delete the plaintext temp files written by
  /// `mediaToFile` (the Rust core names them `nightdrop-media-<id>.<ext>` in the OS temp
  /// dir). Best-effort: a file the OS already reclaimed, or one held open by an external
  /// player, is skipped silently.
  static Future<void> wipe() async {
    bytes.clear();
    files.clear();
    _keysByChat.clear();
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
