import 'dart:async';
import 'dart:io' as io;

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

/// Copying a secret (an invite link carries its single-use pass) without leaving it lying around:
/// on Android it is marked sensitive, so the system's clipboard preview hides it and keyboards that
/// honour the flag (Gboard, among others) keep it out of their clipboard history; on iOS it stays
/// on this device (no Universal Clipboard) and expires; and everywhere it is cleared after
/// [clearAfter], if it is still what the clipboard holds — something copied since is never touched.
/// Where there is a system share sheet (Android, iOS), [share] avoids the clipboard altogether.
class SensitiveClipboard {
  SensitiveClipboard._();

  static const _channel = MethodChannel('app.nightdrop/process');
  static const clearAfter = Duration(seconds: 60);

  /// Whether [share] can open a system share sheet here (Android, iOS).
  static bool get canShare => isMobile();

  /// Hand [text] to the system share sheet, so it goes straight to the app the user picks and
  /// never through the clipboard (whose history some keyboards keep, and some phones sync to other
  /// devices). Android and iOS; elsewhere, [copy].
  static Future<void> share(String text) =>
      _channel.invokeMethod<void>('shareText', text);

  /// Injectable for tests.
  @visibleForTesting
  static bool Function() isMobile =
      () => !kIsWeb && (io.Platform.isAndroid || io.Platform.isIOS);

  static Timer? _timer;

  static Future<void> copy(String text) async {
    var marked = false;
    if (isMobile()) {
      try {
        await _channel.invokeMethod<void>('copySensitive', text);
        marked = true;
      } catch (_) {
        // Older app shell or a test: the ordinary copy below still works.
      }
    }
    if (!marked) await Clipboard.setData(ClipboardData(text: text));
    _timer?.cancel();
    _timer = Timer(clearAfter, () => clearIfStill(text));
  }

  /// Clear the clipboard if it still holds [text]. On Android a backgrounded app cannot read the
  /// clipboard; then nothing is cleared (Android 13+ clears it by itself after an hour).
  @visibleForTesting
  static Future<void> clearIfStill(String text) async {
    try {
      final now = await Clipboard.getData(Clipboard.kTextPlain);
      if (now?.text == text) await Clipboard.setData(const ClipboardData(text: ''));
    } catch (_) {}
  }
}
