import 'dart:io';
import 'dart:typed_data';

import 'package:file_picker/file_picker.dart';

/// Helpers for choosing where an encrypted backup is written or read from (§7).
///
/// Both saving and loading go through the OS file picker so the backup lives somewhere the
/// user can actually reach — on mobile that means the system Storage Access Framework
/// (Documents/Downloads/Drive/…), reachable from the Files app, not an app-private folder.
class BackupFiles {
  static bool get _isDesktop =>
      Platform.isLinux || Platform.isWindows || Platform.isMacOS;

  static String _defaultFileName() {
    final stamp = DateTime.now()
        .toIso8601String()
        .replaceAll(':', '-')
        .split('.')
        .first;
    return 'nightdrop-backup-$stamp.bin';
  }

  static String? _desktopDir() {
    if (!_isDesktop) return null;
    final home =
        Platform.environment['HOME'] ?? Platform.environment['USERPROFILE'] ?? '';
    if (home.isEmpty) return null;
    final desktop = Directory('$home${Platform.pathSeparator}Desktop');
    return desktop.existsSync() ? desktop.path : home;
  }

  /// Save the encrypted backup [bytes] to a user-chosen location; returns where it landed
  /// (or null if cancelled). file_picker ≥ 12 writes the bytes itself on every platform —
  /// a Save dialog on desktop (defaulting to the Desktop), the SAF picker on mobile (so
  /// the file lands somewhere reachable from the phone's Files app).
  static Future<String?> saveBackup(List<int> bytes) {
    return FilePicker.saveFile(
      dialogTitle: 'Save your encrypted backup',
      fileName: _defaultFileName(),
      initialDirectory: _desktopDir(),
      type: FileType.any,
      bytes: Uint8List.fromList(bytes),
    );
  }

  /// Ask the user to pick a backup file to import and return its absolute path (or null).
  static Future<String?> choosePickPath() async {
    final result = await FilePicker.pickFiles(
      dialogTitle: 'Choose a backup file',
      type: FileType.any,
      initialDirectory: _desktopDir(),
    );
    return result?.files.single.path;
  }
}
