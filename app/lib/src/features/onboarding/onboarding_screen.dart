import 'dart:async';

import 'package:flutter/material.dart';

import '../../../l10n/app_localizations.dart';
import '../../app.dart';
import '../../core/background_delivery.dart';
import '../../core/backup_errors.dart';
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

  /// Offer background delivery as part of setup, explaining what it costs and what it does not do.
  ///
  /// It is off by default and always will be — a messenger that quietly runs a service is not what
  /// this app is for. But leaving the choice buried in a menu meant most people never made it, and
  /// the result is an app that looks broken: Android suspends the process whenever it is off
  /// screen, so messages simply do not arrive until you open it, and the sender sees them sitting
  /// undelivered. Asking once, here, is the difference between an informed "no" and never knowing.
  ///
  /// Asked **before** [NightdropCore.createIdentity], deliberately: creating the identity notifies
  /// listeners, `_Root` swaps this screen for the home screen, and a dialog awaited after that
  /// would be holding a context that is being torn out from under it. Nothing here needs an
  /// identity to exist — the flag is a preference, and the foreground service is only actually
  /// started by the lifecycle handler, which already waits for `hasIdentity`.
  ///
  /// Android-only: iOS cannot keep a process alive this way, so there is nothing to offer.
  Future<void> _offerBackgroundDelivery() async {
    if (!BackgroundDelivery.supported || await BackgroundDelivery.isEnabled()) return;
    if (!mounted) return;
    final l10n = AppLocalizations.of(context)!;
    final wants = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        scrollable: true,
        title: Text(l10n.onboardingBackgroundTitle),
        content: Text(l10n.onboardingBackgroundBody),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(l10n.onboardingBackgroundSkip),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: Text(l10n.onboardingBackgroundEnable),
          ),
        ],
      ),
    );
    if (wants != true) return;
    // Without the notification permission Android will not let the service run, so a "yes" that
    // cannot be honoured is recorded as the "no" it effectively is, rather than leaving the user
    // believing they are covered.
    if (!await BackgroundDelivery.ensurePermission()) return;
    await BackgroundDelivery.setEnabled(true);
  }

  Future<void> _create() async {
    await _offerBackgroundDelivery();
    if (!mounted) return;
    setState(() => _busy = true);
    _startProgress();
    try {
      await NightdropScope.of(context).createIdentity();
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
      await NightdropScope.of(context).importBackup(path, password);
      // _Root rebuilds to HomeScreen once the restored identity exists.
    } catch (e) {
      if (!mounted) return;
      _stopProgress();
      setState(() => _busy = false);
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          // Only say "check the password" when the backup actually failed to decrypt.
          content: Text(isBackupDecryptFailure(e)
              ? l10n.couldNotRestoreBackup(e.toString())
              : l10n.couldNotRestoreBackupFailed(e.toString())),
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
      await NightdropScope.of(context).importServerBackup(password);
      // _Root rebuilds to HomeScreen once the restored identity exists.
    } catch (e) {
      if (!mounted) return;
      _stopProgress();
      setState(() => _busy = false);
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          // Only say "check the password" when the backup actually failed to decrypt.
          content: Text(isBackupDecryptFailure(e)
              ? l10n.couldNotRestoreServer(e.toString())
              : l10n.couldNotRestoreServerFailed(e.toString())),
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
