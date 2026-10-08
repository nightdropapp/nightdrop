import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';

import 'linux_tray.dart';

/// The two desktop settings: keep running with the window closed, and start at login.
@immutable
class DesktopSettings {
  const DesktopSettings({this.closeToTray = false, this.startAtLogin = false});

  /// Closing the window hides it in the tray instead of ending the app.
  final bool closeToTray;

  /// Start with the session (in the tray, when [closeToTray] is on and there is one).
  final bool startAtLogin;

  DesktopSettings copyWith({bool? closeToTray, bool? startAtLogin}) => DesktopSettings(
        closeToTray: closeToTray ?? this.closeToTray,
        startAtLogin: startAtLogin ?? this.startAtLogin,
      );

  Map<String, Object> toJson() => {'closeToTray': closeToTray, 'startAtLogin': startAtLogin};

  static DesktopSettings fromJson(Object? json) => json is Map
      ? DesktopSettings(
          closeToTray: json['closeToTray'] == true,
          startAtLogin: json['startAtLogin'] == true,
        )
      : const DesktopSettings();
}

/// Keeping the app running with its window closed, on Linux and Windows (ARCHITECTURE.md §11.8).
///
/// Background delivery is Android-only; on desktop, closing the window used to end the process,
/// so nothing was received or sent until it was reopened. With [DesktopSettings.closeToTray] the
/// window hides in the tray and the core keeps running. Offered only where a tray exists —
/// Windows always, Linux when the session has a StatusNotifierItem host ([LinuxTray.available]) —
/// because a hidden window with no icon to bring it back would look like the app had vanished.
/// (Launching the app again also brings it back: both runners hand a second launch to the first.)
///
/// The runners own the window; this drives them over `app.nightdrop/window`
/// (`linux/runner/my_application.cc`, `windows/runner/flutter_window.cpp`).
class DesktopShell {
  DesktopShell._();

  static const _channel = MethodChannel('app.nightdrop/window');

  /// The command-line flag autostart passes: start in the tray, window hidden.
  static const String hiddenArg = '--hidden';

  static bool get supported => !kIsWeb && (Platform.isLinux || Platform.isWindows);

  static LinuxTray? _tray;
  static bool _keepsRunning = false;

  /// True while closing the window keeps the app running. The app lock leaves the key in memory
  /// then, as Android does with background delivery: the app cannot receive anything without it.
  static bool get keepsRunning => _keepsRunning;

  /// Where the settings live, injectable for tests.
  @visibleForTesting
  static Future<Directory> Function() settingsDir = getApplicationSupportDirectory;

  static Future<File> _settingsFile() async =>
      File('${(await settingsDir()).path}${Platform.pathSeparator}desktop_settings.json');

  static Future<DesktopSettings> load() async {
    try {
      final file = await _settingsFile();
      if (!await file.exists()) return const DesktopSettings();
      return DesktopSettings.fromJson(jsonDecode(await file.readAsString()));
    } catch (_) {
      return const DesktopSettings();
    }
  }

  static Future<void> save(DesktopSettings settings) async {
    final file = await _settingsFile();
    await file.parent.create(recursive: true);
    await file.writeAsString(jsonEncode(settings.toJson()));
  }

  /// Whether close-to-tray can be offered here.
  static Future<bool> trayAvailable() async {
    if (!supported) return false;
    if (Platform.isWindows) return true;
    return LinuxTray.available();
  }

  /// At startup: apply the saved settings. [args] are `main`'s.
  static Future<void> init(List<String> args) async {
    if (!supported) return;
    await apply(await load(), startedHidden: args.contains(hiddenArg));
  }

  /// Save [settings] and put them into effect now.
  static Future<void> update(DesktopSettings settings) async {
    await save(settings);
    await apply(settings);
  }

  /// Bring the window, the tray icon and the autostart entry in line with [settings].
  @visibleForTesting
  static Future<void> apply(DesktopSettings settings, {bool startedHidden = false}) async {
    var tray = settings.closeToTray && await trayAvailable();
    if (tray && Platform.isLinux) {
      _tray ??= await LinuxTray.show(onActivate: show);
      tray = _tray != null;
    }
    if (!tray && _tray != null) {
      await _tray!.close();
      _tray = null;
    }
    _keepsRunning = tray;
    await _invoke('setCloseToTray', tray);
    // Started into the tray, but there is no tray (turned off, or the panel is gone): show the
    // window rather than leave the app running invisibly.
    if (startedHidden && !tray) await show();
    try {
      await Autostart.set(settings.startAtLogin, hidden: tray);
    } catch (_) {
      // A read-only config dir or a refused registry write leaves autostart as it was; the rest of
      // the setting still applies.
    }
  }

  /// Show the window (from the tray, if it is there).
  static Future<void> show() => _invoke('show');

  static Future<void> _invoke(String method, [Object? args]) async {
    try {
      await _channel.invokeMethod<void>(method, args);
    } catch (_) {
      // No runner channel (tests, a runner from before this): nothing to drive.
    }
  }
}

/// Start at login: an XDG autostart entry on Linux, the per-user Run key on Windows. Both point at
/// the running executable — on Linux the AppImage file itself (`$APPIMAGE`), not the temporary
/// mount it runs from — and are rewritten on every start while on, so an update that lands under a
/// new file name is followed after its first run.
class Autostart {
  Autostart._();

  static const _runKey = r'HKCU\Software\Microsoft\Windows\CurrentVersion\Run';
  static const _runValue = 'Night Drop';

  /// Turn the entry on or off; [hidden] starts in the tray.
  static Future<void> set(bool on, {required bool hidden}) async {
    if (Platform.isLinux) return _linux(on, hidden: hidden);
    if (Platform.isWindows) return _windows(on, hidden: hidden);
  }

  /// The program autostart runs.
  @visibleForTesting
  static String executable({Map<String, String>? env, String? resolved}) {
    final appImage = (env ?? Platform.environment)['APPIMAGE'];
    if (appImage != null && appImage.isNotEmpty) return appImage;
    return resolved ?? Platform.resolvedExecutable;
  }

  /// `~/.config/autostart/<app id>.desktop`, honouring `XDG_CONFIG_HOME`. Named after the
  /// application id, so a test build (`NIGHTDROP_APP_ID`) never overwrites the real app's entry.
  @visibleForTesting
  static String linuxEntryPath(String appId, {Map<String, String>? env}) {
    final e = env ?? Platform.environment;
    final config = (e['XDG_CONFIG_HOME']?.isNotEmpty ?? false)
        ? e['XDG_CONFIG_HOME']!
        : '${e['HOME']}/.config';
    return '$config/autostart/$appId.desktop';
  }

  /// The application id this build runs under: on Linux, path_provider names the support
  /// directory after it (`~/.local/share/<id>`), which is the one place Dart can read it from.
  static Future<String> _appId() async {
    final dir = await DesktopShell.settingsDir();
    return dir.uri.pathSegments.lastWhere((s) => s.isNotEmpty, orElse: () => 'app.nightdrop');
  }

  /// The autostart entry's contents. The path is quoted and escaped as the Desktop Entry spec
  /// requires for `Exec`, so a home directory with a space (or worse) still starts.
  @visibleForTesting
  static String linuxEntry(String exe, {required bool hidden, String appId = 'app.nightdrop'}) {
    // Two layers: the Exec rules escape `"`, backtick, `$` and `\` with a backslash inside quotes,
    // then the file's string rules double every backslash — so `\` becomes four and `"` becomes
    // `\\"`.
    final quoted = exe.replaceAllMapped(RegExp(r'["`$\\]'),
        (m) => m[0] == r'\' ? r'\\\\' : '\\\\${m[0]}');
    return '[Desktop Entry]\n'
        'Type=Application\n'
        'Name=Night Drop\n'
        'Comment=Start Night Drop when you log in\n'
        'Exec="$quoted"${hidden ? ' ${DesktopShell.hiddenArg}' : ''}\n'
        'Icon=$appId\n'
        'Terminal=false\n'
        'X-GNOME-Autostart-enabled=true\n';
  }

  static Future<void> _linux(bool on, {required bool hidden}) async {
    final appId = await _appId();
    final file = File(linuxEntryPath(appId));
    if (!on) {
      if (await file.exists()) await file.delete();
      return;
    }
    await file.parent.create(recursive: true);
    await file.writeAsString(linuxEntry(executable(), hidden: hidden, appId: appId));
  }

  static Future<void> _windows(bool on, {required bool hidden}) async {
    if (!on) {
      // Exit code 1 when there was nothing to delete; that is the state wanted anyway.
      await Process.run('reg', ['delete', _runKey, '/v', _runValue, '/f']);
      return;
    }
    final command =
        '"${Platform.resolvedExecutable}"${hidden ? ' ${DesktopShell.hiddenArg}' : ''}';
    final result = await Process.run(
        'reg', ['add', _runKey, '/v', _runValue, '/t', 'REG_SZ', '/d', command, '/f']);
    if (result.exitCode != 0) {
      throw ProcessException('reg', const ['add'], '${result.stderr}', result.exitCode);
    }
  }
}
