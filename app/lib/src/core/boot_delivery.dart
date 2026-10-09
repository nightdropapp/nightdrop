import 'package:flutter/widgets.dart';

import 'app_process.dart';
import 'background_delivery.dart';
import 'nightdrop_core.dart';
import 'notifications.dart';

/// Background delivery after a reboot or an app update (Android, ARCHITECTURE.md §11.8).
///
/// `BootReceiver` (Kotlin) answers `BOOT_COMPLETED` and `MY_PACKAGE_REPLACED` only when background
/// delivery is on. It creates the same cached engine `MainActivity` attaches to and runs `main`
/// with [arg], and [run] then does what opening the app would: start the core and keep it running
/// behind the foreground service. When the user does open the app, the screen attaches to this
/// engine and its core ([NightdropCore.startOnce]); nothing is started twice.
///
/// SimpleX restarts its service the same way; Briar, whose database needs a password, only posts a
/// reminder to sign in. Night Drop does whichever applies: without an app lock the key is in the
/// platform keystore and the core starts; with one, the key is not in memory after a restart, so
/// it posts a reminder and ends the process instead of running a service that can receive nothing.
class BootDelivery {
  BootDelivery._();

  /// The `main` argument `BootReceiver` passes. Keep in step with `BootReceiver.kt`.
  static const String arg = '--boot';

  static Future<void> run(NightdropCore core) async {
    if (!BackgroundDelivery.supported) return;
    if (!await BackgroundDelivery.isEnabled()) {
      // The receiver checks this too; a preference turned off in between ends here.
      await _endUnlessOnScreen();
      return;
    }
    // First, before anything slow: Android lets an app start a foreground service from the
    // background only for a short while after `BOOT_COMPLETED`, and restoring the core
    // bootstraps Tor, which can take a minute.
    await BackgroundDelivery.start();
    await NotificationService.init();
    await core.startOnce();

    if (core.identity != null) {
      if (!_onScreen) {
        core.setLifecycle(false); // background cadence, as when the screen is closed
        await BackgroundDelivery.onLifecycle(foreground: false, hasIdentity: true);
      }
      return;
    }
    // Opened meanwhile: the screen shows the lock screen (or the error) itself, and the lifecycle
    // handler owns the service from here.
    if (_onScreen) return;
    await BackgroundDelivery.stop(force: true);
    if (core.needsUnlock) {
      await NotificationService.show(
        'Night Drop is locked',
        'After a restart your app lock has to be opened once. Open Night Drop to receive messages.',
      );
    } else if (core.loadError) {
      await NotificationService.show(
        'Night Drop could not start',
        'Open Night Drop to see what happened. No messages are received until then.',
      );
    }
    await _endUnlessOnScreen();
  }

  /// Whether the user opened the app while this ran: then the screen owns the process.
  static bool get _onScreen =>
      WidgetsBinding.instance.lifecycleState == AppLifecycleState.resumed;

  /// End a process nobody is looking at and nothing is running in. A headless engine holds memory
  /// for nothing, and the next open starts cleanly.
  static Future<void> _endUnlessOnScreen() async {
    if (!_onScreen) await AppProcess.exit();
  }
}
