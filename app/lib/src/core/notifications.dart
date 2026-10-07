import 'dart:io' show File, Platform;

import 'package:flutter_local_notifications/flutter_local_notifications.dart';

/// Thin wrapper over flutter_local_notifications for new-message/request alerts. All content
/// is generic ("New message") — no message text leaves the secure surfaces.
class NotificationService {
  static final FlutterLocalNotificationsPlugin _plugin =
      FlutterLocalNotificationsPlugin();
  static bool _inited = false;

  /// Initialize the plugin and request permission (Android 13+/iOS). Safe to call repeatedly;
  /// swallows platform/unsupported errors (e.g. in tests) so it never breaks the app.
  static Future<void> init() async {
    if (_inited) return;
    // Linux notifications go over the D-Bus session bus. Without one (a bare container, a headless
    // session) the plugin's signal listener fails *after* initialize() returns, as an unhandled
    // async error the catch below cannot see — seen in the AppImage's Ubuntu 22.04 test. There is
    // nothing to notify through, so don't start it.
    if (Platform.isLinux && !_hasSessionBus()) return;
    try {
      const android = AndroidInitializationSettings('@mipmap/ic_launcher');
      const linux = LinuxInitializationSettings(defaultActionName: 'Open');
      const darwin = DarwinInitializationSettings();
      await _plugin.initialize(
        settings: const InitializationSettings(
          android: android,
          linux: linux,
          iOS: darwin,
          macOS: darwin,
        ),
      );
      await _plugin
          .resolvePlatformSpecificImplementation<
              AndroidFlutterLocalNotificationsPlugin>()
          ?.requestNotificationsPermission();
      await _plugin
          .resolvePlatformSpecificImplementation<
              IOSFlutterLocalNotificationsPlugin>()
          ?.requestPermissions(alert: true, badge: true, sound: true);
      _inited = true;
    } catch (_) {
      // Notifications unavailable (unsupported platform / test harness) — ignore.
    }
  }

  /// Whether a D-Bus session bus is there to talk to: the address the session advertises, or the
  /// standard per-user socket that libdbus falls back to when none is set.
  static bool _hasSessionBus() {
    final address = Platform.environment['DBUS_SESSION_BUS_ADDRESS'];
    if (address != null && address.isNotEmpty) return true;
    final runtime = Platform.environment['XDG_RUNTIME_DIR'];
    return runtime != null && runtime.isNotEmpty && File('$runtime/bus').existsSync();
  }

  static Future<void> show(String title, String body) async {
    await init();
    if (!_inited) return;
    try {
      const details = NotificationDetails(
        android: AndroidNotificationDetails(
          'nightdrop_messages',
          'Messages',
          channelDescription: 'New messages and chat requests',
          importance: Importance.high,
          priority: Priority.high,
        ),
        linux: LinuxNotificationDetails(),
        iOS: DarwinNotificationDetails(),
        macOS: DarwinNotificationDetails(),
      );
      final id = DateTime.now().millisecondsSinceEpoch & 0x7fffffff;
      await _plugin.show(id: id, title: title, body: body, notificationDetails: details);
    } catch (_) {
      // best-effort
    }
  }
}
