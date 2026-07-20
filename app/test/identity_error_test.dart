import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/core/nightdrop_core.dart';

// The Rust bridge surfaces core failures as an `AnyhowException` whose `toString()` is
// `AnyhowException(<message>)`; the tests pass strings shaped the same way (a String's
// toString() is itself), which is exactly what cleanCoreError/identitySetupError see.
void main() {
  group('identitySetupError', () {
    test('maps the onion-service launch failure to actionable guidance', () {
      // What the core surfaces when a second desktop instance can't acquire the shared
      // arti onion-service lock (the `.context("launch onion service")` in transport::tor).
      final msg = identitySetupError('AnyhowException(launch onion service)');
      expect(msg, contains('already be running'));
      expect(msg.toLowerCase(), contains('close the other window'));
      // The raw arti/lock context is not shown to the user.
      expect(msg, isNot(contains('onion service')));
    });

    test('passes other failures through with the setup prefix', () {
      final msg = identitySetupError('AnyhowException(bootstrap Tor: no network)');
      expect(msg, startsWith('Could not set up your identity:'));
      expect(msg, contains('bootstrap Tor: no network'));
    });

    test('matches the context case-insensitively', () {
      final msg = identitySetupError('AnyhowException(Launch Onion Service: fs lock held)');
      expect(msg, contains('already be running'));
    });
  });

  group('cleanCoreError', () {
    test('strips the AnyhowException wrapper', () {
      expect(cleanCoreError('AnyhowException(unknown contact)'), 'unknown contact');
    });
  });
}
