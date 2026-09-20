import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

/// Who installed this copy of the app.
///
/// Exists for one decision: whether to run the **automatic** update check. F-Droid is the update
/// channel for anyone who installed from it, so a second updater asking our onion site once a day
/// is duplicative — and "the app checks for update automatically" is an item on F-Droid's own
/// review checklist. It stays on for sideloads, for the GitHub download and for the desktop
/// AppImage, which have no channel at all and are the reason the check was written
/// (`ARCHITECTURE.md`).
///
/// The **manual** "Update app" item is never gated by this. A user who deliberately asks must get
/// a real answer wherever they got the app.
class InstallSource {
  InstallSource._();

  static const _channel = MethodChannel('app.nightdrop/screenshots');

  /// F-Droid's client and its "Basic" variant. An install from either means F-Droid updates us.
  static const _fdroidInstallers = {'org.fdroid.fdroid', 'org.fdroid.basic'};

  static bool? _isFdroid;

  /// Whether F-Droid installed this copy.
  ///
  /// False whenever we cannot tell — no channel, an older embedding, a platform that has no
  /// concept of an installer, or Android declining to say. That default is deliberate: guessing
  /// "F-Droid" would silently take the only update signal away from a sideloader, while guessing
  /// wrong the other way merely leaves a redundant check running for someone F-Droid also updates.
  /// Cached, because it cannot change without a reinstall.
  /// Deliberately no `Platform.isAndroid` short-circuit. It would be redundant — no other platform
  /// hosts this channel, so they land in the `catch` and get the same `false` — and it made the
  /// only interesting logic here unreachable from a test, which runs on a Linux host.
  static Future<bool> isFdroid() async {
    if (kIsWeb) return false;
    if (_isFdroid != null) return _isFdroid!;
    try {
      final installer = await _channel.invokeMethod<String>('installerPackage');
      _isFdroid = _fdroidInstallers.contains(installer);
    } catch (_) {
      _isFdroid = false;
    }
    return _isFdroid!;
  }

  @visibleForTesting
  static void resetForTest() => _isFdroid = null;
}
