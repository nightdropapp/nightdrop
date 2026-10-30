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
///  * every Android below 14, and every desktop platform — hence [canDetect] (iOS reports
///    screenshots, and recording/mirroring through `UIScreen.isCaptured`; see AppDelegate.swift);
///  * a photo of the screen taken with another camera, which no API can ever see;
///  * screen *recording* below Android 15 (15+ reports it: `WindowManager`'s recording callback).
///
/// So a peer who sees no screenshot notice has learned nothing. The value here is honesty when a
/// capture *is* visible, not a guarantee that captures are visible.
class ScreenshotDetector {
  ScreenshotDetector._();

  static const _channel = MethodChannel('app.nightdrop/screenshots');

  /// Android 14+ and iOS report screenshots; Android 15+ and iOS report screen recording.
  static bool get supported =>
      supportedForTest ?? (!kIsWeb && (Platform.isAndroid || Platform.isIOS));

  /// Lets a test stand in for a phone.
  @visibleForTesting
  static bool? supportedForTest;

  /// What "Block screenshots" can do on this platform; see [CaptureBlock].
  static CaptureBlock get blockKind {
    if (blockKindForTest != null) return blockKindForTest!;
    if (canBlockForTest != null) return canBlockForTest! ? CaptureBlock.full : CaptureBlock.none;
    if (kIsWeb) return CaptureBlock.none;
    if (Platform.isAndroid || Platform.isWindows) return CaptureBlock.full;
    if (Platform.isIOS) return CaptureBlock.recordingOnly;
    if (Platform.isMacOS) return CaptureBlock.partial;
    return CaptureBlock.none; // Linux: X11 and Wayland give an app no way to opt out
  }

  /// Whether "Block screenshots" is offered and a contact's block is held here at all.
  static bool get canBlockCapture => blockKind != CaptureBlock.none;

  /// Lets a test stand in for an Android phone ([CaptureBlock.full]) or a Linux desktop.
  @visibleForTesting
  static bool? canBlockForTest;

  /// Lets a test stand in for a specific platform.
  @visibleForTesting
  static CaptureBlock? blockKindForTest;

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
    // Ask as well: the platform's event can have come before anything here listened (a recording
    // already running when the app came forward), and it is not sent again.
    if (supported) {
      _channel.invokeMethod<bool>('isRecording').then((on) {
        if (on == true) recordingChanged(true);
      }, onError: (_) {});
    }
  }

  /// Stop reporting. Called when a chat closes so a screenshot taken on the chat *list* isn't
  /// attributed to the conversation the user was last looking at.
  static void stop() {
    _onScreenshot = null;
    _onRecording = null;
  }

  /// Whether "Block screenshots" is on (on unless the user turned it off; false where it is not
  /// offered).
  static Future<bool> blockCapture() async {
    if (!canBlockCapture) return false;
    try {
      return await _channel.invokeMethod<bool>('getBlockCapture') ?? true;
    } catch (_) {
      return false;
    }
  }

  /// Block captures while a chat whose contact asked for it is open, whatever our own setting
  /// (where [canBlockCapture]; elsewhere there is nothing to hold). Released when it closes.
  static Future<void> holdBlock(bool hold) async {
    if (!canBlockCapture) return;
    try {
      await _channel.invokeMethod<void>('holdBlock', hold);
    } catch (_) {}
  }

  /// Turn "Block screenshots" on or off; it applies at once and to every later start.
  static Future<void> setBlockCapture(bool on) async {
    if (!canBlockCapture) return;
    await _channel.invokeMethod<void>('setBlockCapture', on);
  }
}

/// How far "Block screenshots" reaches on a platform. Each native side holds the setting itself, so
/// it applies before any Dart runs: `MainActivity.kt`, `windows/runner/flutter_window.cpp`,
/// `macos/Runner/MainFlutterWindow.swift`, `ios/Runner/AppDelegate.swift`.
enum CaptureBlock {
  /// Nothing can be blocked (Linux: neither X11 nor Wayland lets a window opt out of capture).
  none,

  /// Screenshots, recording, casting and streaming are all blocked: Android's `FLAG_SECURE`, and
  /// Windows' `WDA_EXCLUDEFROMCAPTURE`, which leaves the window out of every capture.
  full,

  /// iOS: no screenshot block exists, so screenshots are only reported; while the screen is
  /// recorded, mirrored or AirPlayed the app covers its own contents.
  recordingOnly,

  /// macOS: the window asks to be left out (`NSWindow.sharingType = .none`); older capture APIs
  /// honour it, but ScreenCaptureKit on recent macOS may not. A request, not a guarantee.
  partial,
}
