import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';

/// Notices when the device changes networks (Wi-Fi to mobile data, a new Wi-Fi), so the app can say
/// that Tor is reconnecting. Nothing on screen used to: the onion address does not change with the
/// network, so the "publishing" banner rightly stays away, but for a minute or two after a switch
/// nothing reaches the relay or a contact, and a short-code join in that window just timed out
/// (0.2 device pass, 2026-10-17).
///
/// Detected by watching the device's own addresses — no platform channel, the same on every OS.
class NetworkWatch extends ChangeNotifier {
  NetworkWatch({this.interval = const Duration(seconds: 10), Future<Set<String>> Function()? addresses})
      : _addresses = addresses ?? _deviceAddresses;

  final Duration interval;
  final Future<Set<String>> Function() _addresses;
  Set<String>? _last;
  Timer? _timer;

  /// When the network last changed; null if it has not since the watch started.
  DateTime? changedAt;

  void start() {
    _timer ??= Timer.periodic(interval, (_) => check());
    check();
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  /// Compare the device's addresses with the last look. The first look only records them.
  @visibleForTesting
  Future<void> check() async {
    final Set<String> now;
    try {
      now = await _addresses();
    } catch (_) {
      return;
    }
    final last = _last;
    _last = now;
    if (last != null && !setEquals(last, now)) {
      changedAt = DateTime.now();
      notifyListeners();
    }
  }

  static Future<Set<String>> _deviceAddresses() async {
    final ifaces = await NetworkInterface.list(includeLoopback: false, includeLinkLocal: false);
    return {for (final i in ifaces) for (final a in i.addresses) a.address};
  }
}
