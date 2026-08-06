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
void nightdropBackgroundCallback() {
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
          channelId: 'nightdrop_background',
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

  // Last known lifecycle state, kept so a hold taken or released at any moment can work out what
  // the service should be doing without waiting for the next lifecycle callback.
  static bool _foreground = true;
  static bool _hasIdentity = false;

  /// Outstanding [holdDuring] calls. The service must run while any of them do, **regardless of
  /// the background-delivery opt-in** — a long job the user explicitly started is not the same
  /// question as whether they want passive message delivery.
  static int _holds = 0;
  static String _holdText = 'Working';

  /// React to an app-lifecycle change: start the service when backgrounding (if opted in and an
  /// identity exists), stop it when returning to the foreground (the UI polls directly then) —
  /// unless a hold is keeping it up.
  static Future<void> onLifecycle({
    required bool foreground,
    required bool hasIdentity,
  }) async {
    _foreground = foreground;
    _hasIdentity = hasIdentity;
    await _reconcile();
  }

  /// Keep the process at foreground priority for as long as [job] runs.
  ///
  /// For work that takes minutes and must not be frozen halfway — the update download is the
  /// case this exists for. Without it, Doze and App Standby are free to freeze the process
  /// mid-transfer, and the user comes back to a download that silently stopped.
  ///
  /// Two things make this more than a call to [start]. It ignores the background-delivery opt-in,
  /// because a user who declined passive delivery has not thereby declined to finish a download
  /// they just asked for. And it survives [onLifecycle], which would otherwise stop the service
  /// the moment they returned to the app to watch the progress.
  ///
  /// **Call this while the app is still in the foreground.** Android 12+ forbids starting a
  /// foreground service from the background, so taking the hold lazily after the user has already
  /// left would be refused — exactly when it is needed.
  ///
  /// Best-effort by design: if the service cannot start (permission declined, plugin missing),
  /// [job] still runs. A download that would have succeeded must not fail because the
  /// notification did not.
  static Future<T> holdDuring<T>(
    Future<T> Function() job, {
    required String notificationText,
  }) async {
    if (!supported) return job();
    _holds++;
    _holdText = notificationText;
    await _reconcile();
    try {
      return await job();
    } finally {
      _holds--;
      await _reconcile();
    }
  }

  /// Whether the service should be running, given everything that has a say.
  ///
  /// Pulled out as a pure function because the interesting case is not obvious and is easy to
  /// regress: returning to the foreground stops the service, so without the `holds` term a user
  /// who reopened the app to watch a download would have killed the very thing protecting it.
  @visibleForTesting
  static bool shouldRun({
    required bool foreground,
    required bool hasIdentity,
    required bool optedIn,
    required int holds,
  }) =>
      holds > 0 || (!foreground && hasIdentity && optedIn);

  /// Bring the service into line with what the current state calls for. The single place that
  /// decides, so a hold and a lifecycle change cannot each act on half the picture.
  static Future<void> _reconcile() async {
    if (!supported) return;
    final run = shouldRun(
      foreground: _foreground,
      hasIdentity: _hasIdentity,
      optedIn: await isEnabled(),
      holds: _holds,
    );
    if (run) {
      await start(text: _holds > 0 ? _holdText : 'Watching for messages');
    } else {
      await stop();
    }
  }

  /// Start the foreground service, or update its notification if it is already running
  /// (idempotent either way).
  static Future<void> start({String text = 'Watching for messages'}) async {
    if (!supported) return;
    try {
      if (await FlutterForegroundTask.isRunningService) {
        // Already up, possibly saying the wrong thing: a persistent notification reading
        // "Watching for messages" through a ten-minute download tells the user nothing about why
        // their phone is busy.
        await FlutterForegroundTask.updateService(
          notificationTitle: 'Night Drop',
          notificationText: text,
        );
        return;
      }
      await FlutterForegroundTask.startService(
        serviceId: 424242,
        notificationTitle: 'Night Drop',
        notificationText: text,
        callback: nightdropBackgroundCallback,
      );
    } catch (_) {}
  }

  /// Stop the foreground service (idempotent). Refuses while a [holdDuring] job is outstanding,
  /// so an unrelated caller cannot cut a download off at the knees.
  static Future<void> stop() async {
    if (!supported || _holds > 0) return;
    try {
      if (await FlutterForegroundTask.isRunningService) {
        await FlutterForegroundTask.stopService();
      }
    } catch (_) {}
  }
}
