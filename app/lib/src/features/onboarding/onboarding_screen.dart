import 'dart:async';

import 'package:flutter/material.dart';

import '../../../l10n/app_localizations.dart';
import '../../app.dart';
import '../../core/backup_files.dart';
import '../../core/nightdrop_core.dart';

/// First-run screen. No sign-up — just generate an anonymous, device-held identity.
class OnboardingScreen extends StatefulWidget {
  const OnboardingScreen({super.key});

  @override
  State<OnboardingScreen> createState() => _OnboardingScreenState();
}

/// Staged status lines shown while the (slow) Tor bootstrap runs, so the user can see that
/// something is happening rather than staring at a bare spinner.
List<String> _bootstrapStages(AppLocalizations l10n) => [
      l10n.stageGeneratingIdentity,
      l10n.stageConnectingTor,
      l10n.stageBuildingCircuit,
      l10n.stagePublishingOnion,
      l10n.stageAlmostReady,
    ];

class _OnboardingScreenState extends State<OnboardingScreen> {
  bool _busy = false;
  String _status = '';
  Timer? _statusTimer;

  @override
  void dispose() {
    _statusTimer?.cancel();
    super.dispose();
  }

  /// Cycle through [_bootstrapStages] while a long operation runs.
  void _startProgress() {
    final stages = _bootstrapStages(AppLocalizations.of(context)!);
    var i = 0;
    setState(() => _status = stages.first);
    _statusTimer?.cancel();
    _statusTimer = Timer.periodic(const Duration(seconds: 6), (_) {
      if (i < stages.length - 1) {
        i++;
        if (mounted) setState(() => _status = stages[i]);
      }
    });
  }

  void _stopProgress() {
    _statusTimer?.cancel();
    _statusTimer = null;
    if (mounted) setState(() => _status = '');
  }

  Future<void> _create() async {
    setState(() => _busy = true);
    _startProgress();
    try {
      await GhostScope.of(context).createIdentity();
      // _Root rebuilds to HomeScreen once an identity exists.
    } catch (e) {
      if (!mounted) return;
      _stopProgress();
      setState(() => _busy = false);
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(identitySetupError(e))),
      );
    }
  }

  /// Restore an existing identity + chats from a password-encrypted backup (§7, TODO #5):
  /// pick the file, enter the recovery password, decrypt and load it.
  Future<void> _restore() async {
    final path = await BackupFiles.choosePickPath();
    if (path == null || !mounted) return;

    final l10n = AppLocalizations.of(context)!;
    final controller = TextEditingController();
    final password = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        scrollable: true,
        title: Text(l10n.restoreFromBackup),
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
            child: Text(l10n.restore),
          ),
        ],
      ),
    );
    if (password == null || password.isEmpty || !mounted) return;

    setState(() => _busy = true);
    _startProgress();
    try {
      await GhostScope.of(context).importBackup(path, password);
      // _Root rebuilds to HomeScreen once the restored identity exists.
    } catch (e) {
      if (!mounted) return;
      _stopProgress();
      setState(() => _busy = false);
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text(l10n.couldNotRestoreBackup(e.toString())),
        ),
      );
    }
  }

  /// Restore from an opt-in server backup (§7c / #9): enter the recovery password; the core
  /// fetches the encrypted blob from the relay over Tor and rebuilds the identity.
  Future<void> _restoreFromServer() async {
    final l10n = AppLocalizations.of(context)!;
    final controller = TextEditingController();
    final password = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        scrollable: true,
        title: Text(l10n.restoreFromServerBackup),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              l10n.enterServerRecoveryPassword,
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
            child: Text(l10n.restore),
          ),
        ],
      ),
    );
    if (password == null || password.isEmpty || !mounted) return;

    setState(() => _busy = true);
    _startProgress();
    try {
      await GhostScope.of(context).importServerBackup(password);
      // _Root rebuilds to HomeScreen once the restored identity exists.
    } catch (e) {
      if (!mounted) return;
      _stopProgress();
      setState(() => _busy = false);
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text(l10n.couldNotRestoreServer(e.toString())),
        ),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final l10n = AppLocalizations.of(context)!;
    return Scaffold(
      body: Center(
        child: Padding(
          padding: const EdgeInsets.all(32),
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              const Text('👻', style: TextStyle(fontSize: 72)),
              const SizedBox(height: 16),
              Text(l10n.appTitle, style: theme.textTheme.headlineMedium),
              const SizedBox(height: 8),
              Text(
                l10n.onboardingTagline,
                textAlign: TextAlign.center,
                style: theme.textTheme.bodyMedium
                    ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
              ),
              const SizedBox(height: 40),
              FilledButton(
                onPressed: _busy ? null : _create,
                child: _busy
                    ? const SizedBox(
                        height: 18,
                        width: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : Text(l10n.createMyIdentity),
              ),
              if (_busy && _status.isNotEmpty) ...[
                const SizedBox(height: 20),
                Row(
                  mainAxisAlignment: MainAxisAlignment.center,
                  children: [
                    const SizedBox(
                      height: 14,
                      width: 14,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    ),
                    const SizedBox(width: 10),
                    Flexible(
                      child: Text(
                        _status,
                        textAlign: TextAlign.center,
                        style: theme.textTheme.bodySmall
                            ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
                      ),
                    ),
                  ],
                ),
              ],
              const SizedBox(height: 8),
              TextButton(
                onPressed: _busy ? null : _restore,
                child: Text(l10n.restoreFromBackupFile),
              ),
              TextButton(
                onPressed: _busy ? null : _restoreFromServer,
                child: Text(l10n.restoreFromServerBackup),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
