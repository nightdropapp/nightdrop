import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/core/public_downloads.dart';
import 'package:path_provider/path_provider.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  const downloads = MethodChannel('app.nightdrop/downloads');
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;

  late Directory tmp;
  late File source;

  setUp(() {
    tmp = Directory.systemTemp.createTempSync('nd-downloads-test');
    source = File('${tmp.path}/staged.apk')..writeAsStringSync('a build');
  });

  tearDown(() {
    messenger.setMockMethodCallHandler(downloads, null);
    PublicDownloads.externalDirectory = getExternalStorageDirectory;
    if (tmp.existsSync()) tmp.deleteSync(recursive: true);
  });

  test('a published build lands in Downloads and leaves no second copy', () async {
    String? asked;
    messenger.setMockMethodCallHandler(downloads, (call) async {
      if (call.method != 'publish') return null;
      asked = call.arguments['displayName'] as String;
      return 'Downloads/${call.arguments['displayName']}';
    });

    final where = await PublicDownloads.publish(source,
        displayName: 'NightDrop-0.1.18.apk');

    expect(where, 'Downloads/NightDrop-0.1.18.apk');
    expect(asked, 'NightDrop-0.1.18.apk');
    // MediaStore holds its own copy. Keeping ours would double ~45MB on a device whose storage
    // pressure is the whole reason for being careful here.
    expect(source.existsSync(), isFalse,
        reason: 'the staged copy must not survive a successful publish');
  });

  test('below API 29 it falls back to external storage, still moving the file', () async {
    // The channel answering null is how `Downloads.kt` reports "this release has no MediaStore
    // Downloads collection" — the public folder there would cost WRITE_EXTERNAL_STORAGE, which
    // this app strips from its manifest.
    messenger.setMockMethodCallHandler(downloads, (call) async => null);
    final external = Directory('${tmp.path}/external')..createSync();
    PublicDownloads.externalDirectory = () async => external;

    final where = await PublicDownloads.publish(source,
        displayName: 'NightDrop-0.1.18.apk');

    expect(where, '${external.path}/NightDrop-0.1.18.apk');
    expect(File(where).readAsStringSync(), 'a build');
    expect(source.existsSync(), isFalse);
  });

  test('a verified build is never lost when nothing can publish it', () async {
    // Both routes gone: no platform channel at all (desktop, or the test harness), and no external
    // storage. The download already succeeded and was already hash-verified, so throwing it away —
    // or returning null and telling the user it failed — would discard minutes of Tor transfer for
    // a problem that is not the download's.
    messenger.setMockMethodCallHandler(
        downloads, (call) async => throw MissingPluginException('no channel'));
    PublicDownloads.externalDirectory = () async => null;

    final where = await PublicDownloads.publish(source,
        displayName: 'NightDrop-0.1.18.apk');

    expect(where, source.path);
    expect(source.readAsStringSync(), 'a build',
        reason: 'the file must still be where the user was told it is');
  });
}
