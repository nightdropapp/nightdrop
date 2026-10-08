import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:dbus/dbus.dart';
import 'package:flutter/services.dart';

/// The Linux tray icon: a StatusNotifierItem on the session bus, the protocol KDE, GNOME with the
/// AppIndicator extension, Cinnamon, XFCE and most others read.
///
/// Spoken directly over D-Bus rather than through a tray plugin, because those link
/// `libayatana-appindicator3`, which many systems do not have — and a missing library would stop
/// the AppImage from starting at all (MAINTENANCE.md §11.7). Here a desktop without a tray just
/// reports [available] false and the app keeps its plain close-to-quit behaviour.
///
/// The icon is sent as pixels: an AppImage's icon is not installed into the system icon theme, so
/// a theme name would show nothing. There is no menu; clicking the icon (either button) opens the
/// window, and the app's own menu has Quit.
class LinuxTray {
  LinuxTray._(this._client, this._item);

  static const _watcherName = 'org.kde.StatusNotifierWatcher';
  static final _watcherPath = DBusObjectPath('/StatusNotifierWatcher');

  final DBusClient _client;
  final _TrayItem _item;
  StreamSubscription<DBusNameOwnerChangedEvent>? _watcherRestarts;

  /// Whether this session shows tray icons at all: a watcher is running and a host (the panel
  /// that draws the icons) has registered with it. Stock GNOME has neither.
  static Future<bool> available() async {
    if (!Platform.isLinux) return false;
    final client = DBusClient.session();
    try {
      return await _hasHost(client);
    } catch (_) {
      return false;
    } finally {
      await client.close();
    }
  }

  static Future<bool> _hasHost(DBusClient client) async {
    if (!await client.nameHasOwner(_watcherName)) return false;
    final watcher = DBusRemoteObject(client, name: _watcherName, path: _watcherPath);
    final host = await watcher.getProperty(
        'org.kde.StatusNotifierWatcher', 'IsStatusNotifierHostRegistered');
    return host is DBusBoolean && host.value;
  }

  /// Show the icon; [onActivate] runs when it is clicked. Null when there is no tray.
  static Future<LinuxTray?> show({required void Function() onActivate}) async {
    if (!Platform.isLinux) return null;
    final client = DBusClient.session();
    try {
      if (!await _hasHost(client)) {
        await client.close();
        return null;
      }
      final item = _TrayItem(onActivate, await _iconPixmaps());
      await client.registerObject(item);
      final name = 'org.kde.StatusNotifierItem-$pid-1';
      await client.requestName(name);
      final tray = LinuxTray._(client, item);
      await tray._register(name);
      // A panel restart (plasmashell, gnome-shell) brings a new watcher that has never heard of
      // us; register again so the icon comes back.
      tray._watcherRestarts = client.nameOwnerChanged
          .where((e) => e.name == _watcherName && e.newOwner != null)
          .listen((_) => unawaited(tray._register(name)));
      return tray;
    } catch (_) {
      await client.close();
      return null;
    }
  }

  Future<void> _register(String name) async {
    try {
      await _client.callMethod(
        destination: _watcherName,
        path: _watcherPath,
        interface: 'org.kde.StatusNotifierWatcher',
        name: 'RegisterStatusNotifierItem',
        values: [DBusString(name)],
      );
    } catch (_) {
      // No watcher right now; the restart listener registers when one appears.
    }
  }

  /// Remove the icon.
  Future<void> close() async {
    await _watcherRestarts?.cancel();
    await _client.unregisterObject(_item);
    await _client.close();
  }

  /// The app icon as `a(iiay)`: ARGB32, network byte order, at the sizes panels commonly draw.
  static Future<List<(int, Uint8List)>> _iconPixmaps() async {
    final data = await rootBundle.load('assets/icons/icon-512.png');
    final out = <(int, Uint8List)>[];
    for (final size in const [22, 32, 48, 64]) {
      final codec = await ui.instantiateImageCodec(data.buffer.asUint8List(),
          targetWidth: size, targetHeight: size);
      final image = (await codec.getNextFrame()).image;
      final rgba = (await image.toByteData(format: ui.ImageByteFormat.rawRgba))!;
      out.add((size, rgbaToArgb(rgba.buffer.asUint8List())));
      image.dispose();
    }
    return out;
  }

  /// RGBA bytes to the ARGB order StatusNotifierItem expects.
  static Uint8List rgbaToArgb(Uint8List rgba) {
    final argb = Uint8List(rgba.length);
    for (var i = 0; i + 3 < rgba.length; i += 4) {
      argb[i] = rgba[i + 3];
      argb[i + 1] = rgba[i];
      argb[i + 2] = rgba[i + 1];
      argb[i + 3] = rgba[i + 2];
    }
    return argb;
  }
}

class _TrayItem extends DBusObject {
  _TrayItem(this._onActivate, this._icons)
      : super(DBusObjectPath('/StatusNotifierItem'));

  static const _iface = 'org.kde.StatusNotifierItem';
  final void Function() _onActivate;
  final List<(int, Uint8List)> _icons;

  DBusValue get _pixmaps => DBusArray(
        DBusSignature('(iiay)'),
        _icons.map((icon) => DBusStruct([
              DBusInt32(icon.$1),
              DBusInt32(icon.$1),
              DBusArray.byte(icon.$2),
            ])),
      );

  Map<String, DBusValue> get _properties => {
        'Category': const DBusString('Communications'),
        'Id': const DBusString('app.nightdrop'),
        'Title': const DBusString('Night Drop'),
        'Status': const DBusString('Active'),
        'WindowId': const DBusInt32(0),
        'IconName': const DBusString(''),
        'IconPixmap': _pixmaps,
        'OverlayIconName': const DBusString(''),
        'OverlayIconPixmap': DBusArray(DBusSignature('(iiay)')),
        'AttentionIconName': const DBusString(''),
        'AttentionIconPixmap': DBusArray(DBusSignature('(iiay)')),
        'AttentionMovieName': const DBusString(''),
        'ToolTip': DBusStruct([
          const DBusString(''),
          DBusArray(DBusSignature('(iiay)')),
          const DBusString('Night Drop'),
          const DBusString('Running in the background. Click to open.'),
        ]),
        'ItemIsMenu': const DBusBoolean(false),
        'Menu': DBusObjectPath('/NO_DBUSMENU'),
      };

  @override
  Future<DBusMethodResponse> handleMethodCall(DBusMethodCall methodCall) async {
    if (methodCall.interface != _iface) {
      return DBusMethodErrorResponse.unknownInterface();
    }
    switch (methodCall.name) {
      // No menu to offer, so every click opens the window: a right-click that did nothing would
      // look broken.
      case 'Activate':
      case 'SecondaryActivate':
      case 'ContextMenu':
        _onActivate();
        return DBusMethodSuccessResponse();
      case 'Scroll':
        return DBusMethodSuccessResponse();
    }
    return DBusMethodErrorResponse.unknownMethod();
  }

  @override
  Future<DBusMethodResponse> getProperty(String interface, String name) async {
    final value = interface == _iface ? _properties[name] : null;
    return value == null
        ? DBusMethodErrorResponse.unknownProperty()
        : DBusGetPropertyResponse(value);
  }

  @override
  Future<DBusMethodResponse> getAllProperties(String interface) async =>
      DBusGetAllPropertiesResponse(interface == _iface ? _properties : {});
}
