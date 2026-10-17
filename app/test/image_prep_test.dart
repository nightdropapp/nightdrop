import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:image/image.dart' as img;
import 'package:night_drop/src/core/image_prep.dart';

/// A camera-like JPEG: noisy pixels, make/model, a GPS position and an orientation tag.
Uint8List cameraJpeg(
    {int width = 3000, int height = 2000, int orientation = 1}) {
  final im = img.Image(width: width, height: height);
  for (final p in im) {
    p
      ..r = (p.x * 7 + p.y * 13) % 256
      ..g = (p.x * p.y) % 256
      ..b = (p.x ^ p.y) % 256;
  }
  im.exif.imageIfd['Make'] = img.IfdValueAscii('SAMSUNG');
  im.exif.imageIfd['Model'] = img.IfdValueAscii('SM-S931W');
  im.exif.imageIfd['Orientation'] = img.IfdValueShort(orientation);
  im.exif.gpsIfd['GPSLatitudeRef'] = img.IfdValueAscii('N');
  im.exif.gpsIfd['GPSLatitude'] = img.IfdValueRational(4525, 100);
  return img.encodeJpg(im, quality: 95);
}

bool containsBytes(List<int> hay, List<int> needle) {
  for (var i = 0; i + needle.length <= hay.length; i++) {
    var j = 0;
    while (j < needle.length && hay[i + j] == needle[j]) {
      j++;
    }
    if (j == needle.length) return true;
  }
  return false;
}

void main() {
  test('a camera photo loses its EXIF, is downscaled and stays a JPEG', () {
    final out = cleanJpeg(cameraJpeg())!;
    expect(out[0] == 0xFF && out[1] == 0xD8, isTrue);
    final back = img.decodeJpg(out)!;
    expect([back.width, back.height], [1600, 1067]);
    expect(back.exif.isEmpty, isTrue, reason: 'no EXIF block at all');
    for (final gone in ['SAMSUNG', 'SM-S931W', 'Exif']) {
      expect(containsBytes(out, gone.codeUnits), isFalse, reason: gone);
    }
    // The latitude rational 4525/100, big-endian, as it sits in the source's GPS block.
    expect(containsBytes(out, [0, 0, 0x11, 0xAD, 0, 0, 0, 0x64]), isFalse);
  });

  test('the orientation tag is applied to the pixels before it is dropped', () {
    // 6 = "rotate 90° clockwise to display": a 300x200 sensor image shows as 200x300.
    final out = cleanJpeg(cameraJpeg(width: 300, height: 200, orientation: 6))!;
    final back = img.decodeJpg(out)!;
    expect([back.width, back.height], [200, 300]);
  });

  test('a PNG screenshot becomes a JPEG, transparency on white', () {
    final png = img.Image(width: 40, height: 20, numChannels: 4)
      ..clear(img.ColorRgba8(0, 0, 0, 0));
    img.fillRect(png,
        x1: 0, y1: 0, x2: 19, y2: 19, color: img.ColorRgba8(255, 0, 0, 255));
    final out = cleanJpeg(img.encodePng(png))!;
    final back = img.decodeJpg(out)!;
    expect([back.width, back.height], [40, 20]);
    final clear = back.getPixel(30, 10);
    expect([clear.r, clear.g, clear.b].every((c) => c > 245), isTrue,
        reason: 'transparent became white, got $clear');
    final red = back.getPixel(5, 10);
    expect(red.r > 200 && red.g < 60, isTrue,
        reason: 'opaque red kept, got $red');
  });

  test('other formats the Dart decoder reads become JPEGs too', () {
    final src = img.Image(width: 64, height: 48)
      ..clear(img.ColorRgb8(10, 120, 200));
    final gif = img.Image(width: 64, height: 48, numChannels: 3)
      ..clear(img.ColorRgb8(1, 2, 3));
    gif.addFrame(
        img.Image(width: 64, height: 48)..clear(img.ColorRgb8(200, 0, 0)));
    for (final encoded in [
      img.encodeGif(gif),
      img.encodeBmp(src),
      img.encodeTiff(src),
      img.encodeTga(src),
    ]) {
      final out = cleanJpeg(encoded)!;
      expect(img.decodeJpg(out), isNotNull);
    }
  });

  test('bytes no decoder reads are refused, never passed through', () {
    expect(cleanJpeg(Uint8List.fromList('not an image'.codeUnits)), isNull);
    expect(cleanJpeg(Uint8List(0)), isNull);
  });

  testWidgets('the platform fallback refuses what it cannot read as well',
      (tester) async {
    final out = await tester.runAsync(() => prepareImageForSending(
        Uint8List.fromList('neither can read this'.codeUnits)));
    expect(out, isNull);
  });

  testWidgets('a readable picture comes back through the full path',
      (tester) async {
    final out =
        await tester.runAsync(() => prepareImageForSending(cameraJpeg()));
    expect(img.decodeJpg(out!)!.exif.isEmpty, isTrue);
  });
}
