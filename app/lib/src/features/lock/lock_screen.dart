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

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Future<void> _submit(NightdropCore core) async {
    final secret = _controller.text;
    if (secret.isEmpty || _busy) return;
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
    final delay = Duration(milliseconds: (500 * _attempts).clamp(500, 5000));
    setState(() {
      _failed = true;
      _controller.clear();
    });
    await Future<void>.delayed(delay);
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
                      errorText: _failed ? l10n.lockedFailed : null,
                    ),
                  ),
                  const SizedBox(height: 20),
                  FilledButton(
                    onPressed: _busy ? null : () => _submit(core),
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
