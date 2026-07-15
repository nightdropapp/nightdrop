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

  static Future<void> show(String title, String body) async {
    await init();
    if (!_inited) return;
    try {
      const details = NotificationDetails(
        android: AndroidNotificationDetails(
          'ghost_messages',
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
