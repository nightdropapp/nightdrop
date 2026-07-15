import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_foreground_task/flutter_foreground_task.dart';

/// Opt-in **Android foreground-service background delivery** (§11.8, TODO #13).
///
/// Wake-from-killed is deliberately out of scope (it would need FCM/APNs device tokens — an
/// anonymity leak). This instead keeps the app **process** alive with a persistent notification,
/// so the existing main-isolate poller keeps doing its Tor `peek` in the background and firing
/// local notifications (the "app alive" path in §11.8) — no push provider, no second Tor core.
///
/// The service task does no work of its own ([ForegroundTaskEventAction.nothing]); its only job
/// is to raise the process to foreground priority so Android doesn't kill it. iOS cannot do this
/// by design, so this is a no-op there. Everything is best-effort and guarded — a missing plugin
/// or permission must never crash the app.
///
/// Note: if the user *swipes the app away* (Activity destroyed) the Flutter engine may detach and
/// the poller pause even though the process lives; reliable delivery covers the backgrounded (not
/// force-closed) case. This needs on-device validation.
@pragma('vm:entry-point')
void ghostBackgroundCallback() {
  FlutterForegroundTask.setTaskHandler(_KeepAliveTaskHandler());
}

/// A do-nothing handler: the foreground service exists purely to keep the process alive.
class _KeepAliveTaskHandler extends TaskHandler {
  @override
  Future<void> onStart(DateTime timestamp, TaskStarter starter) async {}

  @override
  void onRepeatEvent(DateTime timestamp) {}

  @override
  Future<void> onDestroy(DateTime timestamp, bool isTimeout) async {}
}

class BackgroundDelivery {
  BackgroundDelivery._();

  static const String _kEnabledKey = 'bg_delivery_enabled';

  /// Only Android can host a foreground service; elsewhere every method is a no-op.
  static bool get supported => !kIsWeb && Platform.isAndroid;

  /// Configure the notification channel + task options. Call once at startup (after
  /// `WidgetsFlutterBinding.ensureInitialized`). Safe to call repeatedly.
  static void init() {
    if (!supported) return;
    try {
      FlutterForegroundTask.initCommunicationPort();
      FlutterForegroundTask.init(
        androidNotificationOptions: AndroidNotificationOptions(
          channelId: 'ghost_background',
          channelName: 'Background delivery',
          channelDescription:
              'Keeps Night Drop checking for messages over Tor while backgrounded.',
          channelImportance: NotificationChannelImportance.LOW,
          priority: NotificationPriority.LOW,
          onlyAlertOnce: true,
        ),
        iosNotificationOptions: const IOSNotificationOptions(),
        foregroundTaskOptions: ForegroundTaskOptions(
          // The service itself does nothing on a timer — the main isolate does the peeking;
          // we only need the process kept alive.
          eventAction: ForegroundTaskEventAction.nothing(),
          autoRunOnBoot: false,
          autoRunOnMyPackageReplaced: false,
          allowWakeLock: true,
          allowWifiLock: true,
        ),
      );
    } catch (_) {
      // Plugin unavailable (e.g. test harness) — background delivery just stays off.
    }
  }

  /// Whether the user has opted in. Persisted via the plugin's own key/value store (no extra
  /// dependency). Defaults to off.
  static Future<bool> isEnabled() async {
    if (!supported) return false;
    try {
      return await FlutterForegroundTask.getData<bool>(key: _kEnabledKey) ?? false;
    } catch (_) {
      return false;
    }
  }

  /// Turn the opt-in on/off. Turning it off also stops any running service.
  static Future<void> setEnabled(bool value) async {
    if (!supported) return;
    try {
      await FlutterForegroundTask.saveData(key: _kEnabledKey, value: value);
      if (!value) await stop();
    } catch (_) {}
  }

  /// Ensure the notification permission is granted (Android 13+) so the persistent notification
  /// can show. Returns whether it is granted.
  static Future<bool> ensurePermission() async {
    if (!supported) return false;
    try {
      final status = await FlutterForegroundTask.checkNotificationPermission();
      if (status == NotificationPermission.granted) return true;
      return (await FlutterForegroundTask.requestNotificationPermission()) ==
          NotificationPermission.granted;
    } catch (_) {
      return false;
    }
  }

  /// React to an app-lifecycle change: start the service when backgrounding (if opted in and an
  /// identity exists), stop it when returning to the foreground (the UI polls directly then).
  static Future<void> onLifecycle({
    required bool foreground,
    required bool hasIdentity,
  }) async {
    if (!supported) return;
    if (foreground) {
      await stop();
    } else if (hasIdentity && await isEnabled()) {
      await start();
    }
  }

  /// Start the foreground service (idempotent).
  static Future<void> start() async {
    if (!supported) return;
    try {
      if (await FlutterForegroundTask.isRunningService) return;
      await FlutterForegroundTask.startService(
        serviceId: 424242,
        notificationTitle: 'Night Drop',
        notificationText: 'Watching for messages',
        callback: ghostBackgroundCallback,
      );
    } catch (_) {}
  }

  /// Stop the foreground service (idempotent).
  static Future<void> stop() async {
    if (!supported) return;
    try {
      if (await FlutterForegroundTask.isRunningService) {
        await FlutterForegroundTask.stopService();
      }
    } catch (_) {}
  }
}
