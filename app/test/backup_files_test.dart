import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/core/backup_files.dart';

void main() {
  test(
      'a saved backup is described by its path, or the readable end of a content URI',
      () {
    expect(
        BackupFiles.describeSaved(
            Uri.file('/home/me/Desktop/nightdrop-backup.ndbak')),
        '/home/me/Desktop/nightdrop-backup.ndbak');
    expect(
        BackupFiles.describeSaved(Uri.parse(
            'content://com.android.externalstorage.documents/document/primary%3ADownload%2Fnightdrop-backup.ndbak')),
        'primary:Download/nightdrop-backup.ndbak');
    // Nothing readable after the authority: show the whole URI rather than nothing.
    expect(BackupFiles.describeSaved(Uri(scheme: 'content', host: 'provider')), 'content://provider');
  });
}
