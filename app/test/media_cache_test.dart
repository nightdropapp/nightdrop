import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/core/media_cache.dart';

void main() {
  setUp(() async => MediaCache.wipe());

  test('a burned or unsent attachment leaves the cache at the next refresh', () {
    // Self-audit 2026-10-13, round 6: decrypted bytes stayed in memory until logout.
    MediaCache.bytes['A'] = Future.value(Uint8List(1));
    MediaCache.bytes['thumb:T'] = Future.value(Uint8List(1));
    MediaCache.bytes['B'] = Future.value(Uint8List(1));
    MediaCache.retainChat('chat1',
        MediaCache.keysOf([(mediaId: 'A', thumbId: 'T'), (mediaId: 'B', thumbId: '')]));
    expect(MediaCache.bytes.keys, containsAll(['A', 'thumb:T', 'B']));
    // A burned: its message became a tombstone, with no media id any more.
    MediaCache.retainChat('chat1', MediaCache.keysOf([(mediaId: 'B', thumbId: '')]));
    expect(MediaCache.bytes.keys, ['B']);
  });

  test('another chat refreshing never evicts this one, and a deleted chat drops everything', () {
    MediaCache.bytes['A'] = Future.value(Uint8List(1));
    MediaCache.bytes['X'] = Future.value(Uint8List(1));
    MediaCache.retainChat('chat1', {'A'});
    MediaCache.retainChat('chat2', {'X'});
    MediaCache.retainChat('chat2', {});
    expect(MediaCache.bytes.keys, ['A']);
    MediaCache.forgetChat('chat1');
    expect(MediaCache.bytes, isEmpty);
  });
}
