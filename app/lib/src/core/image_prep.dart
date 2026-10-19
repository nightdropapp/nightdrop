import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:image/image.dart' as img;

/// Longest side of a sent photo. Small enough to move over Tor quickly.
const int kMaxImageSide = 1600;

/// JPEG quality for a photo that was already a JPEG (a camera picture): its detail is photographic,
/// and 82 is indistinguishable at this size.
const int kPhotoQuality = 82;

/// JPEG quality for anything else, mostly screenshots: sharp edges and text blur at photo quality.
const int kGraphicQuality = 90;

/// Turn a picture the user chose into what Night Drop sends: a JPEG of at most [kMaxImageSide],
/// right way up, transparency on white, carrying **no metadata** — no EXIF (GPS position, camera
/// make and model, time taken), no XMP, no colour profile, no comment. The recipient's app draws
/// only plain JPEGs (`core/src/node/media_kind.rs`), and the core strips any metadata segment again
/// before sending, so this is the first of two layers, not the only one.
///
/// Decoded by its content, whatever its file name says. What the Dart decoder cannot read (HEIC
/// from a phone camera set to "high efficiency", for one) goes through the platform's decoder.
/// `null` if neither can read it: the caller refuses to send it rather than send the original.
Future<Uint8List?> prepareImageForSending(Uint8List input) async {
  final viaDart = await compute(cleanJpeg, input);
  if (viaDart != null) return viaDart;
  final rgba = await _platformDecode(input);
  if (rgba == null) return null;
  return compute(_cleanRgba, rgba);
}

/// [prepareImageForSending] for what the Dart decoder reads. Runs in a background isolate.
Uint8List? cleanJpeg(Uint8List input) {
  img.Image? decoded;
  try {
    decoded = img.decodeImage(input);
  } catch (_) {
    decoded = null;
  }
  if (decoded == null) return null;
  final wasJpeg = input.length > 2 && input[0] == 0xFF && input[1] == 0xD8;
  // The camera's "rotate me" tag goes with the rest of the EXIF, so apply it to the pixels first.
  return _encode(
      img.bakeOrientation(decoded), wasJpeg ? kPhotoQuality : kGraphicQuality);
}

/// A decoded picture as straight RGBA, from the platform decoder.
class _Rgba {
  _Rgba(this.width, this.height, this.pixels);
  final int width;
  final int height;
  final ByteBuffer pixels;
}

Uint8List _cleanRgba(_Rgba rgba) => _encode(
      img.Image.fromBytes(
          width: rgba.width,
          height: rgba.height,
          bytes: rgba.pixels,
          numChannels: 4,
          order: img.ChannelOrder.rgba),
      kGraphicQuality,
    );

/// Downscale, flatten onto white and encode as a JPEG. Drawing onto a fresh image is what drops
/// the metadata: the new image has none, so the encoder writes none.
Uint8List _encode(img.Image src, int quality) {
  final first = src.frames.isEmpty
      ? src
      : src.frames.first; // an animation sends its first frame
  final scaled = (first.width > kMaxImageSide || first.height > kMaxImageSide)
      ? img.copyResize(
          first,
          width: first.width >= first.height ? kMaxImageSide : null,
          height: first.height > first.width ? kMaxImageSide : null,
          interpolation: img.Interpolation.average,
        )
      : first;
  // Blended by hand: normalised channels read the same for 8- and 16-bit, palette and alpha-less
  // sources (alpha 1), where the library's channel conversion replaced alpha before compositing.
  final out =
      img.Image(width: scaled.width, height: scaled.height, numChannels: 3);
  for (final p in scaled) {
    final a = p.aNormalized.toDouble();
    int over(num c) => (255 * (c * a + (1 - a))).round().clamp(0, 255);
    out.setPixelRgb(p.x, p.y, over(p.rNormalized), over(p.gNormalized),
        over(p.bNormalized));
  }
  return img.encodeJpg(out, quality: quality);
}

/// Decode with the engine's platform decoder (Android: HEIF/HEIC, AVIF on newer releases), at most
/// [kMaxImageSide] on the longest side. The engine applies the orientation tag itself. `null` if
/// it cannot read the bytes either.
Future<_Rgba?> _platformDecode(Uint8List input) async {
  try {
    final buffer = await ui.ImmutableBuffer.fromUint8List(input);
    final codec =
        await ui.instantiateImageCodecWithSize(buffer, getTargetSize: (w, h) {
      if (w <= kMaxImageSide && h <= kMaxImageSide) {
        return ui.TargetImageSize(width: w, height: h);
      }
      return w >= h
          ? const ui.TargetImageSize(width: kMaxImageSide)
          : const ui.TargetImageSize(height: kMaxImageSide);
    });
    final frame = await codec.getNextFrame();
    final image = frame.image;
    final data =
        await image.toByteData(format: ui.ImageByteFormat.rawStraightRgba);
    final rgba =
        data == null ? null : _Rgba(image.width, image.height, data.buffer);
    image.dispose();
    codec.dispose();
    return rgba;
  } catch (_) {
    return null;
  }
}
