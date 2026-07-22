import 'package:flutter/material.dart';

import '../../../l10n/app_localizations.dart';
import '../../core/backup_files.dart';
import '../../core/nightdrop_core.dart';

/// Shared backup UI flows (§7 / §11.5, #7–#8), used by both the home screen (whole-identity
/// backup) and the chat screen (single-chat scoped backup).

/// Ask which content matrix to back up (#7): Lite (no message history/media) or Full.
/// Returns null if cancelled.
Future<bool?> pickBackupMode(BuildContext context) {
  final l10n = AppLocalizations.of(context)!;
  return showModalBottomSheet<bool>(
    context: context,
    builder: (context) => SafeArea(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          ListTile(
            title: Text(l10n.backupWhatTitle),
            subtitle: Text(l10n.backupWhatSubtitle),
          ),
          ListTile(
            leading: const Icon(Icons.person_outline),
            title: Text(l10n.backupLiteTitle),
            subtitle: Text(l10n.backupLiteSubtitle),
            onTap: () => Navigator.pop(context, false),
          ),
          ListTile(
            leading: const Icon(Icons.inventory_2_outlined),
            title: Text(l10n.backupFullTitle),
            subtitle: Text(l10n.backupFullSubtitle),
            onTap: () => Navigator.pop(context, true),
          ),
        ],
      ),
    ),
  );
}

/// Show a one-time recovery [password] and require the user to **prove** they recorded it by typing
/// it back (§7 invariant — losing the password = losing the backup, so a bare "I saved it" tap is
/// too easy to click through and silently lose data). [intro] explains the context (file vs. server
/// backup); [footer] is an optional extra note (e.g. server-copy expiry). Barrier-locked; returns
/// true once confirmed, false if the user backs out. Shared by the file-backup and server-backup
/// flows so both enforce the same confirmation.
Future<bool?> acknowledgeRecoveryPassword(
  BuildContext context, {
  required String password,
  required String intro,
  String? footer,
}) {
  return showDialog<bool>(
    context: context,
    barrierDismissible: false,
    builder: (context) =>
        _PasswordAckDialog(password: password, intro: intro, footer: footer),
  );
}

/// Two-stage recovery-password acknowledgement: reveal, then confirm-by-retype. Kept stateful so
/// the user can flip back to re-read the password if they mistype, without regenerating it.
class _PasswordAckDialog extends StatefulWidget {
  const _PasswordAckDialog({
    required this.password,
    required this.intro,
    this.footer,
  });

  final String password;
  final String intro;
  final String? footer;

  @override
  State<_PasswordAckDialog> createState() => _PasswordAckDialogState();
}

class _PasswordAckDialogState extends State<_PasswordAckDialog> {
  bool _confirming = false;
  final _controller = TextEditingController();
  String? _error;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _verify() {
    final l10n = AppLocalizations.of(context)!;
    // Trim only outer whitespace; the password itself is generated without leading/trailing spaces.
    if (_controller.text.trim() == widget.password) {
      Navigator.pop(context, true);
    } else {
      setState(() => _error = l10n.passwordMismatch);
    }
  }

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context)!;
    if (!_confirming) {
      return AlertDialog(
        scrollable: true,
        title: Text(l10n.yourRecoveryPassword),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(widget.intro),
            const SizedBox(height: 16),
            SelectableText(
              widget.password,
              style: const TextStyle(fontFamily: 'monospace', fontSize: 18),
            ),
            if (widget.footer != null) ...[
              const SizedBox(height: 16),
              Text(widget.footer!, style: const TextStyle(fontSize: 12.5)),
            ],
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(l10n.cancel),
          ),
          FilledButton(
            onPressed: () => setState(() => _confirming = true),
            child: Text(l10n.writtenItDown),
          ),
        ],
      );
    }
    return AlertDialog(
      scrollable: true,
      title: Text(l10n.confirmRecoveryPassword),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(l10n.typeItBack),
          const SizedBox(height: 12),
          TextField(
            controller: _controller,
            autofocus: true,
            autocorrect: false,
            enableSuggestions: false,
            onChanged: (_) {
              if (_error != null) setState(() => _error = null);
            },
            onSubmitted: (_) => _verify(),
            decoration: InputDecoration(
              labelText: l10n.recoveryPassword,
              border: const OutlineInputBorder(),
              errorText: _error,
            ),
          ),
        ],
      ),
      actions: [
        TextButton(
          // Flip back to re-read the password rather than losing it to a mistype.
          onPressed: () => setState(() {
            _confirming = false;
            _error = null;
          }),
          child: Text(l10n.showItAgain),
        ),
        FilledButton(
          onPressed: _verify,
          child: Text(l10n.confirm),
        ),
      ],
    );
  }
}

/// Run [task] behind a blocking, non-dismissible loading dialog showing [message]. Used for the
/// slow, non-interactive backup steps — Argon2 key derivation (deliberately slow) and, for a
/// server backup, the Tor upload — so the user sees progress instead of a frozen screen. The
/// loader is always removed, even if [task] throws.
Future<T> runWithLoader<T>(
  BuildContext context,
  String message,
  Future<T> Function() task,
) async {
  final navigator = Navigator.of(context, rootNavigator: true);
  showDialog<void>(
    context: context,
    barrierDismissible: false,
    builder: (_) => PopScope(
      canPop: false,
      child: AlertDialog(
        content: Row(
          children: [
            const SizedBox(
              width: 22,
              height: 22,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
            const SizedBox(width: 20),
            Expanded(child: Text(message)),
          ],
        ),
      ),
    ),
  );
  try {
    return await task();
  } finally {
    navigator.pop(); // remove the loader (top route) whether task succeeded or threw.
  }
}

/// Prepare a backup, show + acknowledge its password, then hand the encrypted bytes to the OS
/// picker to save. When [contactId] is set this is a **single-chat scoped backup** (#8);
/// otherwise it backs up the whole identity (#7).
Future<void> createAndSaveBackup(
  BuildContext context,
  NightdropCore core, {
  String? contactId,
}) async {
  final l10n = AppLocalizations.of(context)!;
  final full = await pickBackupMode(context);
  if (full == null || !context.mounted) return;
  final String password;
  try {
    // Argon2 derivation makes this take a noticeable moment — show a loader.
    password = await runWithLoader(
      context,
      l10n.preparingBackup,
      () => contactId == null
          ? core.createBackup(full)
          : core.createChatBackup(contactId, full),
    );
  } catch (e) {
    if (!context.mounted) return;
    ScaffoldMessenger.of(context)
        .showSnackBar(SnackBar(content: Text(l10n.couldNotPrepareBackup(e.toString()))));
    return;
  }
  if (!context.mounted) return;
  final acknowledged = await acknowledgeRecoveryPassword(
    context,
    password: password,
    intro: l10n.backupIntroFile,
  );
  if (acknowledged != true || !context.mounted) return;

  try {
    final bytes = await core.backupBytes();
    final saved = await BackupFiles.saveBackup(bytes);
    if (!context.mounted) return;
    if (saved == null) return; // cancelled
    await core.recordBackupDone(); // stop the backup-reminder nudge
    if (!context.mounted) return;
    ScaffoldMessenger.of(context)
        .showSnackBar(SnackBar(content: Text(l10n.backupSavedTo(saved))));
  } catch (e) {
    if (!context.mounted) return;
    ScaffoldMessenger.of(context)
        .showSnackBar(SnackBar(content: Text(l10n.couldNotSaveBackup(e.toString()))));
  }
}

/// Import a single-chat scoped backup and **merge** it into the current identity (#8): pick the
/// file, enter its recovery password, add the chat (existing chats only gain missing history).
Future<void> mergeChatBackup(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final path = await BackupFiles.choosePickPath();
  if (path == null || !context.mounted) return;

  final controller = TextEditingController();
  final password = await showDialog<String>(
    context: context,
    builder: (context) => AlertDialog(
      scrollable: true,
      title: Text(l10n.mergeChatBackupTitle),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            l10n.enterRecoveryPasswordFor(path),
            style: Theme.of(context).textTheme.bodySmall,
          ),
          const SizedBox(height: 12),
          TextField(
            controller: controller,
            autofocus: true,
            decoration: InputDecoration(
              labelText: l10n.recoveryPassword,
              border: const OutlineInputBorder(),
            ),
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: Text(l10n.cancel),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(context, controller.text),
          child: Text(l10n.merge),
        ),
      ],
    ),
  );
  if (password == null || password.isEmpty || !context.mounted) return;

  try {
    final added = await runWithLoader(
      context,
      l10n.restoringBackup,
      () => core.mergeBackup(path, password),
    );
    if (!context.mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text(l10n.mergedChatBackup(added))),
    );
  } catch (e) {
    if (!context.mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text(l10n.couldNotMergeBackup(e.toString()))),
    );
  }
}
