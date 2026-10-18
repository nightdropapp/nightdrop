import 'dart:io' show Platform;

import 'package:flutter/foundation.dart' show kIsWeb, visibleForTesting;
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
///  * screen *recording* below Android 15 (15+ reports it: `WindowManager`'s recording callback).
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
  static void Function()? _onRecording;

  /// Whether the app's screen is being recorded right now (Android 15+ reports it).
  static bool get recording => _recording;
  static bool _recording = false;
  static int _recordingSession = 0;
  static final Set<String> _recordingReported = {};

  /// Whether the current recording still has to be reported for [contactId]: true once per
  /// recording and contact, so moving between chats does not repeat the notice.
  static bool claimRecordingReport(String contactId) =>
      _recording && _recordingReported.add('$_recordingSession:$contactId');

  /// The platform said whether the app's screen is being recorded. A start begins a new session.
  @visibleForTesting
  static void recordingChanged(bool on) {
    if (on && !_recording) {
      _recordingSession++;
      _recordingReported.clear();
      _recording = true;
      _onRecording?.call();
    } else if (!on) {
      _recording = false;
    }
  }

  /// Register `handler` for screenshots taken while the app is in the foreground, and
  /// `onRecording` for a screen recording that starts (or is already running) while it is. Calling
  /// this again replaces the previous handlers — only one chat is ever open at a time, and a stale
  /// handler would report a capture against the wrong chat.
  static void listen(void Function() handler, {void Function()? onRecording}) {
    _onScreenshot = handler;
    _onRecording = onRecording;
    // Registered on every platform, not just Android: a handler on a channel that nothing ever
    // calls costs nothing, and the alternative — a platform branch here — meant the wiring could
    // only be exercised on a device. [canDetect] is what gates any user-facing claim.
    _channel.setMethodCallHandler((call) async {
      if (call.method == 'screenshot') _onScreenshot?.call();
      if (call.method == 'recording') recordingChanged(call.arguments == true);
      return null;
    });
    if (_recording) onRecording?.call(); // opened while a recording runs
  }

  /// Stop reporting. Called when a chat closes so a screenshot taken on the chat *list* isn't
  /// attributed to the conversation the user was last looking at.
  static void stop() {
    _onScreenshot = null;
    _onRecording = null;
  }

  /// Whether the opt-in "Block screenshots" setting is on (Android only).
  static Future<bool> blockCapture() async {
    if (!supported) return false;
    try {
      return await _channel.invokeMethod<bool>('getBlockCapture') ?? false;
    } catch (_) {
      return false;
    }
  }

  /// Turn "Block screenshots" on or off; it applies at once and to every later start.
  static Future<void> setBlockCapture(bool on) async {
    if (!supported) return;
    await _channel.invokeMethod<void>('setBlockCapture', on);
  }
}
