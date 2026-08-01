import 'dart:io' show Platform;

import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/services.dart';

/// Tells us when the user screenshots the app, so the open chat can log it and tell the peer (#1).
///
/// Backed by Android 14's `Activity.ScreenCaptureCallback` (see `MainActivity.kt`). What it reports
/// is *that* a capture happened — never the image.
///
/// **This is not a protection, and the UI must not present it as one.** It is blind to:
///
///  * every Android below 14, and every desktop platform — hence [canDetect];
///  * a photo of the screen taken with another camera, which no API can ever see;
///  * screen *recording*, which the platform callback does not cover.
///
/// So a peer who sees no screenshot notice has learned nothing. The value here is honesty when a
/// capture *is* visible, not a guarantee that captures are visible.
class ScreenshotDetector {
  ScreenshotDetector._();

  static const _channel = MethodChannel('app.nightdrop/screenshots');

  static bool get supported => !kIsWeb && Platform.isAndroid;

  /// Whether this device can report screenshots at all (Android 14+). Cached after the first call:
  /// the answer is a platform constant.
  static bool? _canDetect;

  static Future<bool> canDetect() async {
    if (!supported) return false;
    if (_canDetect != null) return _canDetect!;
    try {
      _canDetect = await _channel.invokeMethod<bool>('canDetect') ?? false;
    } catch (_) {
      _canDetect = false; // no channel (test harness, older embedding) — just stays off
    }
    return _canDetect!;
  }

  static void Function()? _onScreenshot;

  /// Register `handler` for screenshots taken while the app is in the foreground. Calling this
  /// again replaces the previous handler — only one chat is ever open at a time, and a stale
  /// handler would report a screenshot against the wrong chat.
  static void listen(void Function() handler) {
    _onScreenshot = handler;
    // Registered on every platform, not just Android: a handler on a channel that nothing ever
    // calls costs nothing, and the alternative — a platform branch here — meant the wiring could
    // only be exercised on a device. [canDetect] is what gates any user-facing claim.
    _channel.setMethodCallHandler((call) async {
      if (call.method == 'screenshot') _onScreenshot?.call();
      return null;
    });
  }

  /// Stop reporting. Called when a chat closes so a screenshot taken on the chat *list* isn't
  /// attributed to the conversation the user was last looking at.
  static void stop() {
    _onScreenshot = null;
  }
}
