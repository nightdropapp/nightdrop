import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/core/backup_errors.dart';

void main() {
  group('isBackupDecryptFailure', () {
    test('a wrong password / damaged file is a decrypt failure', () {
      // The marker the Rust core emits from `storage::open`.
      expect(
        isBackupDecryptFailure(
            Exception('decrypt failed: wrong key or corrupt store')),
        isTrue,
      );
    });

    test('still matches inside an anyhow cause chain across the bridge', () {
      expect(
        isBackupDecryptFailure(Exception(
          'AnyhowException(open backup\n\nCaused by:\n'
          '    0: decrypt failed: wrong key or corrupt store)',
        )),
        isTrue,
      );
    });

    test("Tor's state-lock failure is NOT a password problem", () {
      // The real report behind #2: this was shown to users as "check the password and file",
      // sending them after a password mistake that never happened.
      expect(
        isBackupDecryptFailure(Exception(
          'AnyhowException(launch onion service\n\nCaused by:\n'
          '    0: tor: local resource (port, lockfile, etc.) already in use\n'
          '    1: Unable to launch onion service\n'
          '    2: Unable to access on-disk state\n'
          '    3: State already locked while acquiring lock on instance "hss"/"nightdrop")',
        )),
        isFalse,
      );
    });

    test('an unrelated failure is not attributed to the password', () {
      expect(isBackupDecryptFailure(Exception('No such file or directory')),
          isFalse);
      expect(
        isBackupDecryptFailure(
            Exception("Tor didn't connect within 120s — check your connection")),
        isFalse,
      );
    });
  });
}
