import 'dart:async';

import 'package:flutter/material.dart';

import '../../../l10n/app_localizations.dart';
import '../bridges/bridges_screen.dart';
import '../../app.dart';
import '../../core/background_delivery.dart';
import '../../core/backup_errors.dart';
import '../../core/backup_files.dart';
import '../../core/nightdrop_core.dart';
import 'setup_screen.dart';

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

  /// Run the setup step ([SetupScreen]), apply what was chosen, then create the identity.
  ///
  /// Background delivery and cover traffic are both off by default and always will be — a
  /// messenger that quietly runs a service, or sends dummy traffic, is not what this app is for.
  /// But leaving the choices buried in a menu meant most people never made them, and for
  /// background delivery the result was an app that looks broken: Android suspends the process
  /// whenever it is off screen, so messages simply do not arrive until it is opened. Asking once,
  /// here, is the difference between an informed "no" and never knowing.
  ///
  /// Decided **before** [NightdropCore.createIdentity], and applied before it too: creating the
  /// identity notifies listeners and `_Root` swaps this screen for the home screen. Nothing here
  /// needs an identity — both are preferences, cover traffic is a process-wide flag the core reads
  /// once it runs, and the foreground service is only started by the lifecycle handler, which
  /// already waits for `hasIdentity`.
  Future<void> _create() async {
    final core = NightdropScope.of(context);
    final choices = await Navigator.of(context).push<SetupChoices>(
      MaterialPageRoute(
        builder: (_) => SetupScreen(
          backgroundDelivery: BackgroundDelivery.supported &&
              _backgroundAlreadyOn,
        ),
      ),
    );
    if (choices == null || !mounted) return;
    // Without the notification permission Android will not let the service run, so a "yes" that
    // cannot be honoured is recorded as the "no" it effectively is, rather than leaving the user
    // believing they are covered.
    if (choices.backgroundDelivery && await BackgroundDelivery.ensurePermission()) {
      await BackgroundDelivery.setEnabled(true);
    }
    if (choices.coverTraffic) await core.setCoverTraffic(true);
    if (!mounted) return;
    setState(() => _busy = true);
    _startProgress();
    try {
      await core.createIdentity(relays: choices.relays);
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

  /// Background delivery left on from an earlier install, so the setup switch starts where the
  /// user already is. Read once at start.
  bool _backgroundAlreadyOn = false;

  @override
  void initState() {
    super.initState();
    if (BackgroundDelivery.supported) {
      BackgroundDelivery.isEnabled().then((on) {
        if (mounted) setState(() => _backgroundAlreadyOn = on);
      });
    }
  }

  /// Restore an existing identity + chats from a password-encrypted backup (§7):
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
              enableIMEPersonalizedLearning: false,
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
              enableIMEPersonalizedLearning: false,
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
              // The bridge editor must be reachable BEFORE an identity exists. Creating one
              // bootstraps Tor (`create_bootstrapped`, 120s timeout), so where Tor is blocked
              // identity creation cannot succeed — and every other route to this screen is behind
              // HomeScreen, which only renders once an identity exists. Without this link the
              // censorship feature is locked behind the censorship it exists to defeat.
              TextButton(
                onPressed: _busy
                    ? null
                    : () => Navigator.of(context).push(MaterialPageRoute<void>(
                          builder: (_) => const BridgesScreen(),
                        )),
                child: Text(l10n.onboardingTorBlocked),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
