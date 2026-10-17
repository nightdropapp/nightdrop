// What a contact actually receives when real photos and videos are sent: run ON A PHONE, through
// the app's own preparation (image_prep.dart) and the real core's send and receive, between two
// cores paired over the phone's own network (self-audit 2026-10-17, P1/P2).
//
// The inputs are real camera files, so they are pushed in while the test waits, and the received
// files are pulled out before it ends. Under a TEST app id only — never the default one:
//
//   ORG_GRADLE_PROJECT_appId=app.nightdrop.mediatest \
//     flutter test integration_test/media_privacy_test.dart -d <device>
//   # while it prints "waiting for input": for each file,
//   adb exec-in run-as app.nightdrop.mediatest sh -c 'cat > files/media-in/NAME' < NAME
//   adb shell run-as app.nightdrop.mediatest touch files/media-in/GO
//   # when it prints "results ready": pull files/media-out/*, then
//   adb shell run-as app.nightdrop.mediatest touch files/media-out/COLLECTED
//
// Then check the pulled files (exiftool / magick identify -verbose / ffprobe): no GPS, make,
// model, software, time zone or recording time; pictures upright; videos play.

import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'package:flutter/foundation.dart';
import 'package:fc_native_video_thumbnail/fc_native_video_thumbnail.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:night_drop/src/core/image_prep.dart';
import 'package:night_drop/src/rust/api.dart';
import 'package:night_drop/src/rust/frb_generated.dart';
import 'package:path_provider/path_provider.dart';

Future<void> waitFor(File marker, String what) async {
  for (var i = 0; i < 600 && !marker.existsSync(); i++) {
    if (i % 15 == 0) debugPrint('[media-test] waiting for $what (${marker.path})');
    await Future<void>.delayed(const Duration(seconds: 1));
  }
  expect(marker.existsSync(), isTrue, reason: 'timed out waiting for $what');
}

String key() =>
    base64.encode(List<int>.generate(32, (_) => Random.secure().nextInt(256)));

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('real media reaches a contact without metadata', (tester) async {
    await tester.runAsync(() async {
      await RustLib.init();
      final root = await getApplicationSupportDirectory();
      final inDir = Directory('${root.path}/media-in')..createSync(recursive: true);
      final outDir = Directory('${root.path}/media-out')..createSync(recursive: true);
      final work = Directory('${root.path}/media-work');
      if (work.existsSync()) work.deleteSync(recursive: true);
      work.createSync();

      await waitFor(File('${inDir.path}/GO'), 'input');

      // Two cores on this phone, paired over its own network.
      final a = await NightdropCore.newLan(
          port: 47101, persistPath: '${work.path}/a.bin', persistKey: key());
      final b = await NightdropCore.newLan(
          port: 47102, persistPath: '${work.path}/b.bin', persistKey: key());
      final invite = await a.createInvite();
      final bSideA = (await b.connectViaQr(payload: invite.qrPayload)).id;
      List<Contact> req = [];
      for (var i = 0; i < 60 && req.isEmpty; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 500));
        req = await a.incomingRequests();
      }
      expect(req, isNotEmpty, reason: 'the pairing request never arrived');
      final aSideB = req.first.id;
      await a.authorize(contactId: aSideB, accept: true);
      await Future<void>.delayed(const Duration(seconds: 2));

      final report = StringBuffer();
      final inputs = inDir.listSync().whereType<File>().where((f) => !f.path.endsWith('/GO'));
      for (final f in inputs) {
        final name = f.uri.pathSegments.last;
        final isVideo = name.endsWith('.mp4');
        var bytes = await f.readAsBytes();
        var thumb = <int>[];
        if (isVideo) {
          thumb = await FcNativeVideoThumbnail().saveThumbnailToBytes(
                  srcFile: f.path, width: 320, height: 320, format: 'jpeg', quality: 60) ??
              const [];
        } else {
          final clean = await prepareImageForSending(bytes);
          if (clean == null) {
            report.writeln('$name: REFUSED by the app (unreadable)');
            continue;
          }
          bytes = clean;
        }
        final before = (await b.messages(contactId: bSideA)).length;
        try {
          await a.sendMedia(
              contactId: aSideB,
              data: bytes,
              mime: isVideo ? 'video/mp4' : 'image/jpeg',
              kind: isVideo ? 'video' : 'image',
              thumb: thumb);
        } catch (e) {
          report.writeln('$name: REFUSED by the core: $e');
          continue;
        }
        ChatMessage? got;
        for (var i = 0; i < 120 && got == null; i++) {
          await Future<void>.delayed(const Duration(milliseconds: 500));
          final msgs = await b.messages(contactId: bSideA);
          if (msgs.length > before && msgs.last.mediaId.isNotEmpty) got = msgs.last;
        }
        if (got == null) {
          report.writeln('$name: NOT RECEIVED');
          continue;
        }
        if (isVideo) {
          final path = await b.mediaToFile(mediaId: got.mediaId, ext: '');
          File(path).copySync('${outDir.path}/$name');
          if (got.thumbId.isNotEmpty) {
            File('${outDir.path}/$name.thumb.jpg')
                .writeAsBytesSync(await b.mediaBytes(mediaId: got.thumbId));
          }
        } else {
          final drawn = await b.mediaBytes(mediaId: got.mediaId);
          File('${outDir.path}/$name.received.jpg').writeAsBytesSync(Uint8List.fromList(drawn));
        }
        report.writeln('$name: received ${got.kind} ${got.mime} ${got.mediaSize} bytes'
            '${got.thumbId.isNotEmpty ? ' + preview' : ''}');
      }
      File('${outDir.path}/report.txt').writeAsStringSync(report.toString());
      debugPrint('[media-test] results ready:\n$report');
      await waitFor(File('${outDir.path}/COLLECTED'), 'the results to be pulled');
      await a.shutdown();
      await b.shutdown();
    });
  }, timeout: const Timeout(Duration(minutes: 25)));
}
