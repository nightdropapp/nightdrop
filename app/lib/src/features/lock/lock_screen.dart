import 'dart:async';

import 'package:flutter/material.dart';
import '../../../l10n/app_localizations.dart';

import '../../app.dart';
import '../../core/nightdrop_core.dart';

/// Asked for the secret that unwraps the at-rest key, before any chat data is reachable.
///
/// See `docs/design/app-lock.md`. Two things this screen must not do:
///
///  * **Say why an attempt failed.** A wrong secret and an unreadable lock file both come back as
///    the same generic message, so the screen never confirms to someone holding the phone that
///    they guessed the right *kind* of secret.
///  * **Offer a way past it.** There is no recovery: the key exists only inside the lock file. The
///    only alternative is wiping and starting over, which is deliberately not a button here.
class LockScreen extends StatefulWidget {
  const LockScreen({super.key});

  @override
  State<LockScreen> createState() => _LockScreenState();
}

class _LockScreenState extends State<LockScreen> {
  final _controller = TextEditingController();
  bool _busy = false;
  bool _failed = false;
  int _attempts = 0;

  /// Seconds until the core accepts another attempt. After five wrong ones in a row it imposes a
  /// growing wait, kept on disk so closing the app does not reset it (`storage/lock.rs`).
  int _wait = 0;
  Timer? _ticker;
  bool _askedWait = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (!_askedWait) {
      _askedWait = true;
      _refreshWait(NightdropScope.of(context)); // a wait may be running from before a restart
    }
  }

  Future<void> _refreshWait(NightdropCore core) async {
    final secs = await core.unlockWaitSecs();
    if (!mounted) return;
    _ticker?.cancel();
    setState(() => _wait = secs);
    if (secs <= 0) return;
    _ticker = Timer.periodic(const Duration(seconds: 1), (t) {
      if (!mounted) return t.cancel();
      if (_wait <= 1) {
        t.cancel();
        _refreshWait(core); // ask again rather than trust the local count
      } else {
        setState(() => _wait--);
      }
    });
  }

  String _waitText(int secs) => secs < 60 ? '$secs s' : '${(secs / 60).ceil()} min';

  @override
  void dispose() {
    _ticker?.cancel();
    _controller.dispose();
    super.dispose();
  }

  Future<void> _submit(NightdropCore core) async {
    final secret = _controller.text;
    if (secret.isEmpty || _busy || _wait > 0) return;
    setState(() {
      _busy = true;
      _failed = false;
    });
    // A deliberate pause after a failure, growing to 5s. This only slows someone typing at the
    // phone — it does nothing against an attacker working on a copy of the lock file, so it is not
    // presented to the user as protection (see the design note on PIN entropy).
    final ok = await core.unlockStore(secret);
    if (!mounted) return;
    if (ok) {
      _controller.clear();
      return; // the app swaps this screen out; nothing else to do
    }
    _attempts++;
    setState(() {
      _failed = true;
      _controller.clear();
    });
    // The core counts this failure on disk and may now impose a wait (shown instead of the
    // generic failure). Below that, a short pause after a typo.
    await _refreshWait(core);
    if (_wait == 0) {
      await Future<void>.delayed(Duration(milliseconds: (500 * _attempts).clamp(500, 5000)));
    }
    if (mounted) setState(() => _busy = false);
  }

  @override
  Widget build(BuildContext context) {
    final core = NightdropScope.of(context);
    final theme = Theme.of(context);
    final l10n = AppLocalizations.of(context)!;
    return Scaffold(
      body: SafeArea(
        child: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 420),
            child: SingleChildScrollView(
              padding: const EdgeInsets.all(28),
              child: Column(
                mainAxisAlignment: MainAxisAlignment.center,
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Icon(Icons.lock_outline, size: 56, color: theme.colorScheme.primary),
                  const SizedBox(height: 24),
                  Text(l10n.lockedTitle,
                      textAlign: TextAlign.center, style: theme.textTheme.headlineSmall),
                  const SizedBox(height: 12),
                  Text(
                    l10n.lockedBody,
                    textAlign: TextAlign.center,
                    style: theme.textTheme.bodyMedium
                        ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
                  ),
                  const SizedBox(height: 28),
                  TextField(
                    enableIMEPersonalizedLearning: false,
                    controller: _controller,
                    autofocus: true,
                    obscureText: true,
                    enabled: !_busy,
                    // Not `keyboardType: number`: the same field takes a PIN or a passphrase, and
                    // forcing a numeric keypad would make a passphrase unenterable.
                    keyboardType: TextInputType.text,
                    autocorrect: false,
                    enableSuggestions: false,
                    // Keep the secret out of the keyboard's learned words and clipboard history.
                    smartDashesType: SmartDashesType.disabled,
                    smartQuotesType: SmartQuotesType.disabled,
                    textInputAction: TextInputAction.go,
                    onSubmitted: (_) => _submit(core),
                    decoration: InputDecoration(
                      labelText: l10n.lockedField,
                      border: const OutlineInputBorder(),
                      errorText: _wait > 0
                          ? l10n.lockedWait(_waitText(_wait))
                          : (_failed ? l10n.lockedFailed : null),
                    ),
                  ),
                  const SizedBox(height: 20),
                  FilledButton(
                    onPressed: _busy || _wait > 0 ? null : () => _submit(core),
                    child: _busy
                        ? const SizedBox(
                            height: 18, width: 18, child: CircularProgressIndicator(strokeWidth: 2))
                        : Text(l10n.lockedUnlock),
                  ),
                  const SizedBox(height: 20),
                  Text(
                    l10n.lockedNoRecovery,
                    textAlign: TextAlign.center,
                    style: theme.textTheme.bodySmall
                        ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
