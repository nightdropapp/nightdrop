import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/core/network_watch.dart';

void main() {
  test('a change of the device addresses is noticed once, the first look is not a change', () async {
    var addrs = {'192.168.88.20'};
    final w = NetworkWatch(addresses: () async => addrs);
    var changes = 0;
    w.addListener(() => changes++);

    await w.check(); // first look: records only
    await w.check(); // same network
    expect(changes, 0);
    expect(w.changedAt, isNull);

    addrs = {'10.71.4.9'}; // Wi-Fi off, mobile data on
    await w.check();
    expect(changes, 1);
    expect(w.changedAt, isNotNull);

    await w.check(); // settled on the new network
    expect(changes, 1);
    w.dispose();
  });

  test('a failed look changes nothing', () async {
    final w = NetworkWatch(addresses: () async => throw Exception('no network list'));
    var changes = 0;
    w.addListener(() => changes++);
    await w.check();
    await w.check();
    expect(changes, 0);
    w.dispose();
  });
}
