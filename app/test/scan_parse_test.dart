import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/features/pairing/scan_screen.dart';

void main() {
  test('accepts a valid nightdrop pairing payload', () {
    expect(
      isNightdropInvite('nightdrop://pair?addr=abc.onion&ik=KEY&otk=OTK'),
      isTrue,
    );
  });

  test('rejects non-invite QR contents', () {
    expect(isNightdropInvite('https://example.com'), isFalse);
    expect(isNightdropInvite('nightdrop://pair?addr=abc'), isFalse); // no ik
    expect(isNightdropInvite('not a uri at all %%%'), isFalse);
    expect(isNightdropInvite('nightdrop://other?ik=KEY'), isFalse);
    expect(isNightdropInvite(''), isFalse);
  });

  test('recognizes a short code QR (slot-secret-words)', () {
    expect(looksLikeShortCode('ab3xz9-cobalt-river-ember-quartz'), isTrue);
    expect(looksLikeShortCode('7-cedar-lantern-river'), isTrue);
    expect(isScannablePairing('ab3xz9-cobalt-river-ember'), isTrue);
    // Not short codes: too few parts, wrong charset, URLs.
    expect(looksLikeShortCode('justoneword'), isFalse);
    expect(looksLikeShortCode('only-two'), isFalse);
    expect(looksLikeShortCode('Has-Caps-Here'), isFalse);
    expect(looksLikeShortCode('https://example.com'), isFalse);
  });

  // Regression: the real invite payload carries base64 key material. Both standard base64
  // (+, /, =) and URL-safe base64 (-, _) must be accepted, as must a realistic 56-char v3
  // .onion address and the optional otk param — these are the shapes that a live invite
  // actually produces, and the filter only gates on the presence of `ik`, never its bytes.
  group('real-world invite payloads', () {
    const onion =
        'nightdrop://pair?addr=abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion';

    test('standard base64 key material (+ / =)', () {
      final p = '$onion&ik=aGVsbG8+d29ybGQ/Zm9v=&otk=YS9iK2M9ZA==';
      expect(isNightdropInvite(p), isTrue);
      expect(isScannablePairing(p), isTrue);
    });

    test('URL-safe base64 key material (- _)', () {
      final p = '$onion&ik=aGVsbG8-d29ybGRfZm9v&otk=YS1iX2NkZQ';
      expect(isNightdropInvite(p), isTrue);
      expect(isScannablePairing(p), isTrue);
    });

    test('uppercase/mixed-case key material still parses', () {
      final p = '$onion&ik=AbCdEfGh1234&otk=ZzYyXx';
      expect(isNightdropInvite(p), isTrue);
    });

    test('ik param present but empty is still structurally an invite', () {
      // The value is validated in the Rust core (parse_invite); the scanner filter only
      // needs the key present so it hands the payload on rather than ignoring the QR.
      expect(isNightdropInvite('$onion&ik='), isTrue);
    });
  });

  test('isScannablePairing accepts both invite URIs and short codes, rejects junk', () {
    expect(isScannablePairing('nightdrop://pair?addr=x.onion&ik=K'), isTrue);
    expect(isScannablePairing('4-cedar-lantern-river'), isTrue);
    expect(isScannablePairing('https://example.com/qr'), isFalse);
    expect(isScannablePairing('hello world'), isFalse);
    expect(isScannablePairing(''), isFalse);
  });
}
