import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/desktop/desktop_shell.dart';
import 'package:night_drop/src/desktop/linux_tray.dart';

void main() {
  group('settings', () {
    late Directory dir;
    setUp(() {
      dir = Directory.systemTemp.createTempSync('nd-desktop-');
      DesktopShell.settingsDir = () async => dir;
    });
    tearDown(() => dir.deleteSync(recursive: true));

    test('default to off, and round-trip through the file', () async {
      expect((await DesktopShell.load()).closeToTray, isFalse);
      await DesktopShell.save(const DesktopSettings(closeToTray: true, startAtLogin: true));
      final back = await DesktopShell.load();
      expect(back.closeToTray, isTrue);
      expect(back.startAtLogin, isTrue);
    });

    test('a damaged file reads as off rather than failing the launch', () async {
      File('${dir.path}/desktop_settings.json').writeAsStringSync('{not json');
      final s = await DesktopShell.load();
      expect(s.closeToTray, isFalse);
      expect(s.startAtLogin, isFalse);
    });
  });

  group('Linux autostart entry', () {
    test('starts the AppImage file, not its temporary mount', () {
      expect(
        Autostart.executable(
            env: {'APPIMAGE': '/home/a/Night_Drop-x86_64.AppImage'},
            resolved: '/tmp/.mount_Night/night_drop'),
        '/home/a/Night_Drop-x86_64.AppImage',
      );
      expect(Autostart.executable(env: {}, resolved: '/opt/nd/night_drop'), '/opt/nd/night_drop');
    });

    test('honours XDG_CONFIG_HOME', () {
      expect(Autostart.linuxEntryPath('app.nightdrop', env: {'HOME': '/home/a'}),
          '/home/a/.config/autostart/app.nightdrop.desktop');
      expect(
          Autostart.linuxEntryPath('app.nightdrop.test',
              env: {'HOME': '/home/a', 'XDG_CONFIG_HOME': '/cfg'}),
          '/cfg/autostart/app.nightdrop.test.desktop',
          reason: 'a test build has its own entry');
    });

    test('starts hidden only when asked', () {
      expect(Autostart.linuxEntry('/a/b', hidden: true), contains('Exec="/a/b" --hidden\n'));
      expect(Autostart.linuxEntry('/a/b', hidden: false), contains('Exec="/a/b"\n'));
    });

    // Desktop Entry spec: inside quotes `"` ` $ \ take a backslash, and the file's own string
    // escaping then doubles every backslash.
    test('escapes the path twice, as the spec requires', () {
      final entry = Autostart.linuxEntry(r'/home/my "x"/$a`b\c', hidden: false);
      expect(entry, contains(r'Exec="/home/my \\"x\\"/\\$a\\`b\\\\c"' '\n'));
      expect(Autostart.linuxEntry('/home/my files/nd', hidden: false),
          contains('Exec="/home/my files/nd"\n'), reason: 'a space needs only the quotes');
    });
  });

  test('tray pixels go out as ARGB', () {
    final rgba = Uint8List.fromList([1, 2, 3, 4, 10, 20, 30, 40]);
    expect(LinuxTray.rgbaToArgb(rgba), [4, 1, 2, 3, 40, 10, 20, 30]);
  });
}
