import 'package:flutter/material.dart';
import '../../../l10n/app_localizations.dart';

import '../../core/background_delivery.dart';
import '../../core/nightdrop_core.dart';

/// Turn the app lock on or off. See `docs/design/app-lock.md`.
///
/// The PIN/passphrase choice is presented as a genuine security trade rather than a convenience
/// preference, because that is what it is: a short PIN cannot survive an attacker who copies the
/// lock file off the device, and no amount of key-derivation cost changes that. Users are allowed
/// to choose the weaker option — they just aren't misled about it.
Future<void> showAppLockSettings(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final locked = await core.isStoreLocked();
  if (!context.mounted) return;

  if (locked) {
    final off = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(l10n.appLockOnTitle),
        content: Text(l10n.appLockOnBody),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: Text(l10n.cancel),
          ),
          TextButton(
            onPressed: () => Navigator.of(context).pop(true),
            child: Text(l10n.appLockDisable),
          ),
        ],
      ),
    );
    if (off != true || !context.mounted) return;
    await _disable(context, core);
    return;
  }

  // Off → offer the two strengths, with the trade stated on each.
  final usePin = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(l10n.appLockOffTitle),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(l10n.appLockOffBody),
          const SizedBox(height: 16),
          _Choice(
            title: l10n.appLockChoosePin,
            body: l10n.appLockChoosePinBody,
            onTap: () => Navigator.of(context).pop(true),
          ),
          const SizedBox(height: 8),
          _Choice(
            title: l10n.appLockChoosePassphrase,
            body: l10n.appLockChoosePassphraseBody,
            onTap: () => Navigator.of(context).pop(false),
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(null),
          child: Text(l10n.cancel),
        ),
      ],
    ),
  );
  if (usePin == null || !context.mounted) return;
  await _enable(context, core, usePin: usePin);
}

/// The wipe-code screen, reached from its own entry in the home menu (#3).
///
/// State *is* shown here — "armed" or not — and the reasoning is worth keeping straight. The
/// on-disk deniability (`duress-wipe.md` §3) protects against someone who **images the device
/// without unlocking it**, and that is untouched: the flag lives sealed under the store key. This
/// screen is only reachable after a successful unlock, by which point an adversary has the messages
/// anyway. Hiding it there bought almost nothing and cost something real — a user who cannot see
/// whether their wipe code is armed can believe they have one when they don't, and discover it
/// while being coerced.
///
/// What stays out of the *menu row* is any hint of state: the row says "Wipe code" whether or not
/// one is set, so a glance at an unlocked phone gives nothing away.
Future<void> showDuressSettings(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  if (!await core.isStoreLocked()) {
    if (!context.mounted) return;
    // A wipe code wraps the same lock, so there has to be one first.
    await showDialog<void>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(l10n.duressTitle),
        content: Text(l10n.duressNeedsLock),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: Text(l10n.cancel),
          ),
        ],
      ),
    );
    return;
  }
  final armed = await core.isDuressArmed();
  if (!context.mounted) return;

  final action = await showDialog<_DuressAction>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(l10n.duressTitle),
      content: Text(armed ? l10n.duressOnBody : l10n.duressOffBody),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(l10n.cancel),
        ),
        // Offered only when there is something to remove.
        if (armed)
          TextButton(
            onPressed: () => Navigator.of(context).pop(_DuressAction.remove),
            child: Text(l10n.duressRemove),
          ),
        TextButton(
          onPressed: () => Navigator.of(context).pop(_DuressAction.set),
          child: Text(armed ? l10n.duressReplace : l10n.duressSet),
        ),
      ],
    ),
  );
  if (action == null || !context.mounted) return;
  switch (action) {
    case _DuressAction.set:
      await _setDuress(context, core);
    case _DuressAction.remove:
      await _removeDuress(context, core);
  }
}

enum _DuressAction { set, remove }

/// Arm or replace the wipe code (#3). The normal secret is checked **before** anything else is
/// asked: filling in a whole flow only to be told at the end that the first field was wrong is
/// both irritating and, here, misleading about what state the app is in.
///
/// The warning lands at arming and nowhere else — a standing reminder elsewhere would be the tell.
Future<void> _setDuress(BuildContext context, NightdropCore core) async {
  final current = await _askVerifiedSecret(context, core);
  if (current == null || !context.mounted) return;
  await _armDuress(context, core, current);
}

/// Ask for a wipe code and arm it, given an already-verified normal secret.
Future<void> _armDuress(BuildContext context, NightdropCore core, String current) async {
  final l10n = AppLocalizations.of(context)!;
  final duress = await _askNewDuress(context);
  if (duress == null || !context.mounted) return;

  final messenger = ScaffoldMessenger.of(context);
  // Arm first, so the warning is only shown for a code that actually took. The core self-checks
  // the slot it writes and rolls back if it fails, so reaching the dialog below means it works.
  try {
    await core.setDuressSecret(current, duress);
  } catch (_) {
    // The secret was verified above, so this is the other rejection: a code that would also open
    // the normal slot, and so could never reach the duress one.
    messenger.showSnackBar(SnackBar(content: Text(l10n.duressSame)));
    return;
  }
  if (!context.mounted) return;
  await showDialog<void>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(l10n.duressWarnTitle),
      content: SingleChildScrollView(child: Text(l10n.duressWarnBody)),
      actions: [
        FilledButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(l10n.duressUnderstand),
        ),
      ],
    ),
  );
  messenger.showSnackBar(SnackBar(content: Text(l10n.duressDone)));
}

/// Disarm.
Future<void> _removeDuress(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final current = await _askVerifiedSecret(context, core);
  if (current == null || !context.mounted) return;
  final messenger = ScaffoldMessenger.of(context);
  try {
    await core.clearDuressSecret(current);
    messenger.showSnackBar(SnackBar(content: Text(l10n.duressCleared)));
  } catch (_) {
    messenger.showSnackBar(SnackBar(content: Text(l10n.appLockWrongSecret)));
  }
}

/// Ask for the current secret and **check it before returning**, so a wrong one is rejected on the
/// spot instead of surfacing several screens later. Returns null if the user cancels or gives up.
Future<String?> _askVerifiedSecret(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  String? error;
  while (true) {
    if (!context.mounted) return null;
    // Each attempt gets a fresh dialog owning its own controller, so nothing here has to outlive
    // the dialog it belongs to — which is what made the previous shared-controller version tear
    // one down mid-animation.
    final secret = await _askCurrentSecret(
      context,
      label: l10n.duressCurrentSecret,
      error: error,
    );
    if (secret == null) return null;
    if (await core.verifyStoreSecret(secret)) return secret;
    // Generic, and identical for "wrong secret" and "that's the wipe code" — this screen never
    // confirms to anyone which of the two they typed.
    error = l10n.appLockWrongSecret;
  }
}

/// Ask twice for a new wipe code. Minimum length follows the same rule as the normal secret: a
/// digits-only code is a PIN (6), anything else is a passphrase (12).
Future<String?> _askNewDuress(BuildContext context) async {
  final l10n = AppLocalizations.of(context)!;
  final first = TextEditingController();
  final second = TextEditingController();
  // The keyboard covers the lower half of a dialog this tall, hiding the confirm field entirely.
  // The content scrolls, but a user can't scroll to a field they can't see is there — so the first
  // field's action key moves focus to the second, and the framework scrolls it into view.
  final secondFocus = FocusNode();
  String? error;
  final result = await showDialog<String>(
    context: context,
    builder: (context) => _Owns(
      disposer: () {
        first.dispose();
        second.dispose();
        secondFocus.dispose();
      },
      child: StatefulBuilder(
      builder: (context, setState) {
        void submit() {
          final a = first.text, b = second.text;
          final digitsOnly = RegExp(r'^\d+$').hasMatch(a);
          setState(() {
            if (a.length < (digitsOnly ? 6 : 12)) {
              error = digitsOnly ? l10n.appLockTooShortPin : l10n.appLockTooShortPassphrase;
            } else if (a != b) {
              error = l10n.appLockMismatch;
            } else {
              error = null;
            }
          });
          if (error == null) Navigator.of(context).pop(a);
        }

        return AlertDialog(
          title: Text(l10n.duressTitle),
          content: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(l10n.duressBody),
                const SizedBox(height: 16),
                TextField(
                  controller: first,
                  autofocus: true,
                  obscureText: true,
                  autocorrect: false,
                  enableSuggestions: false,
                  textInputAction: TextInputAction.next,
                  onSubmitted: (_) => secondFocus.requestFocus(),
                  decoration: InputDecoration(labelText: l10n.duressNew),
                ),
                const SizedBox(height: 12),
                TextField(
                  controller: second,
                  focusNode: secondFocus,
                  obscureText: true,
                  autocorrect: false,
                  enableSuggestions: false,
                  textInputAction: TextInputAction.done,
                  decoration: InputDecoration(
                    labelText: l10n.duressConfirm,
                    errorText: error,
                  ),
                  onSubmitted: (_) => submit(),
                ),
              ],
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.of(context).pop(),
              child: Text(l10n.cancel),
            ),
            FilledButton(onPressed: submit, child: Text(l10n.duressSet)),
          ],
        );
        },
      ),
    ),
  );
  return result;
}

/// One obscured field for the secret the user already has. `error` re-displays a previous failure.
/// The `controller` is owned by the caller, so a retry loop can reuse it (see
/// [_askVerifiedSecret]) instead of disposing one that a closing dialog still references.
Future<String?> _askCurrentSecret(
  BuildContext context, {
  required String label,
  String? error,
}) async {
  final l10n = AppLocalizations.of(context)!;
  // Created here and owned by the dialog: each attempt gets its own, disposed when that dialog's
  // element unmounts. A controller shared across attempts and disposed at the end of the loop is
  // torn down while the last dialog is still animating out.
  final controller = TextEditingController();
  final secret = await showDialog<String>(
    context: context,
    builder: (context) => _Owns(
      disposer: controller.dispose,
      child: AlertDialog(
        content: TextField(
        controller: controller,
        autofocus: true,
        obscureText: true,
        autocorrect: false,
        enableSuggestions: false,
        decoration: InputDecoration(labelText: label, errorText: error),
        onSubmitted: (v) => Navigator.of(context).pop(v),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(l10n.cancel),
        ),
          TextButton(
            onPressed: () => Navigator.of(context).pop(controller.text),
            child: Text(l10n.confirm),
          ),
        ],
      ),
    ),
  );
  return (secret == null || secret.isEmpty) ? null : secret;
}

class _Choice extends StatelessWidget {
  const _Choice({required this.title, required this.body, required this.onTap});

  final String title;
  final String body;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(10),
      child: Container(
        padding: const EdgeInsets.all(12),
        decoration: BoxDecoration(
          border: Border.all(color: theme.colorScheme.outlineVariant),
          borderRadius: BorderRadius.circular(10),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(title, style: theme.textTheme.titleSmall),
            const SizedBox(height: 4),
            Text(body,
                style: theme.textTheme.bodySmall
                    ?.copyWith(color: theme.colorScheme.onSurfaceVariant)),
          ],
        ),
      ),
    );
  }
}

Future<void> _enable(BuildContext context, NightdropCore core,
    {required bool usePin}) async {
  final l10n = AppLocalizations.of(context)!;
  // 6 digits for a PIN, 12 characters for a passphrase. The PIN minimum is not pretending to make
  // a PIN strong (see the design note); it just rules out 1234.
  final minLength = usePin ? 6 : 12;
  final secret = await _askNewSecret(context, usePin: usePin, minLength: minLength);
  if (secret == null || !context.mounted) return;

  // Last gate, because this is the irreversible part: there is no recovery path by design.
  final confirmed = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(l10n.appLockEnable),
      content: Text(l10n.appLockWarnNoRecovery),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(false),
          child: Text(l10n.cancel),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(true),
          child: Text(l10n.appLockIUnderstand),
        ),
      ],
    ),
  );
  if (confirmed != true || !context.mounted) return;

  await core.enableStoreLock(secret);
  if (!context.mounted) return;
  final messenger = ScaffoldMessenger.of(context);
  messenger.showSnackBar(SnackBar(content: Text(l10n.appLockEnabled)));
  // Background delivery keeps the key resident while locked; say so rather than letting the user
  // assume locking always forgets it.
  if (await BackgroundDelivery.isEnabled() && context.mounted) {
    messenger.showSnackBar(SnackBar(
      content: Text(l10n.appLockBgNote),
      duration: const Duration(seconds: 8),
    ));
  }

  // Offer the wipe code here, while the user is already thinking about the lock and has the secret
  // in mind — rather than leaving it to be discovered in a menu. Declining is a plain choice, not a
  // deferral, and it stays available later under "Wipe code".
  if (!context.mounted) return;
  final wantsDuress = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(l10n.duressTitle),
      content: Text(l10n.duressOfferBody),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(false),
          child: Text(l10n.duressSkip),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(true),
          child: Text(l10n.duressSet),
        ),
      ],
    ),
  );
  if (wantsDuress != true || !context.mounted) return;
  // The secret was just chosen and confirmed twice, so don't ask for it again — pass it straight
  // through to the arming step.
  await _armDuress(context, core, secret);
}

Future<void> _disable(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final controller = TextEditingController();
  final secret = await showDialog<String>(
    context: context,
    builder: (context) => _Owns(
      disposer: controller.dispose,
      child: AlertDialog(
        title: Text(l10n.appLockDisable),
      content: TextField(
        controller: controller,
        autofocus: true,
        obscureText: true,
        autocorrect: false,
        enableSuggestions: false,
        decoration: InputDecoration(labelText: l10n.appLockCurrentSecret),
        onSubmitted: (v) => Navigator.of(context).pop(v),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(l10n.cancel),
        ),
          TextButton(
            onPressed: () => Navigator.of(context).pop(controller.text),
            child: Text(l10n.appLockDisable),
          ),
        ],
      ),
    ),
  );
  if (secret == null || secret.isEmpty || !context.mounted) return;
  try {
    await core.disableStoreLock(secret);
    if (!context.mounted) return;
    ScaffoldMessenger.of(context)
        .showSnackBar(SnackBar(content: Text(l10n.appLockDisabled)));
  } catch (_) {
    // A wrong secret changes nothing (the core proves knowledge before removing the lock).
    if (!context.mounted) return;
    ScaffoldMessenger.of(context)
        .showSnackBar(SnackBar(content: Text(l10n.appLockWrongSecret)));
  }
}

/// Ask for a new secret twice, so a typo can't lock the user out of their own history.
Future<String?> _askNewSecret(BuildContext context,
    {required bool usePin, required int minLength}) async {
  final l10n = AppLocalizations.of(context)!;
  final first = TextEditingController();
  final second = TextEditingController();
  // Same keyboard problem as the wipe-code dialog: with the keyboard up, the confirm field can sit
  // below the fold. The action key moves focus to it, and the framework scrolls it into view.
  final secondFocus = FocusNode();
  String? error;
  final result = await showDialog<String>(
    context: context,
    builder: (context) => _Owns(
      disposer: () {
        first.dispose();
        second.dispose();
        secondFocus.dispose();
      },
      child: StatefulBuilder(
      builder: (context, setState) {
        void submit() {
          final a = first.text, b = second.text;
          setState(() {
            if (a.length < minLength) {
              error = usePin ? l10n.appLockTooShortPin : l10n.appLockTooShortPassphrase;
            } else if (a != b) {
              error = l10n.appLockMismatch;
            } else {
              error = null;
            }
          });
          if (error == null) Navigator.of(context).pop(a);
        }

        return AlertDialog(
          title: Text(usePin ? l10n.appLockChoosePin : l10n.appLockChoosePassphrase),
          content: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                TextField(
                  controller: first,
                  autofocus: true,
                  obscureText: true,
                  autocorrect: false,
                  enableSuggestions: false,
                  keyboardType: usePin ? TextInputType.number : TextInputType.text,
                  textInputAction: TextInputAction.next,
                  onSubmitted: (_) => secondFocus.requestFocus(),
                  decoration: InputDecoration(labelText: l10n.appLockNewSecret),
                ),
                const SizedBox(height: 12),
                TextField(
                  controller: second,
                  focusNode: secondFocus,
                  obscureText: true,
                  autocorrect: false,
                  enableSuggestions: false,
                  keyboardType: usePin ? TextInputType.number : TextInputType.text,
                  textInputAction: TextInputAction.done,
                  decoration: InputDecoration(
                    labelText: l10n.appLockConfirmSecret,
                    errorText: error,
                  ),
                  onSubmitted: (_) => submit(),
                ),
              ],
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.of(context).pop(),
              child: Text(l10n.cancel),
            ),
            FilledButton(onPressed: submit, child: Text(l10n.appLockEnable)),
          ],
        );
        },
      ),
    ),
  );
  return result;
}

/// Keeps dialog-owned disposables alive for as long as the dialog's element is.
///
/// `showDialog`'s future completes when the route is **popped**, not when it has finished
/// animating out — so disposing a controller or focus node on the line after `await` tears it down
/// while the dialog is still building, and the framework trips on the way out
/// ("A TextEditingController was used after being disposed"). Assertions are stripped from release
/// builds, so a shipped APK shows no red screen; it just half-performs whatever came next. That is
/// how enabling an app lock aborted between deleting the keystore copy and writing the lock file
/// (found on hardware 2026-08-08).
///
/// Owning them here moves disposal to unmount, which happens after the exit animation.
class _Owns extends StatefulWidget {
  const _Owns({required this.disposer, required this.child});

  final VoidCallback disposer;
  final Widget child;

  @override
  State<_Owns> createState() => _OwnsState();
}

class _OwnsState extends State<_Owns> {
  @override
  void dispose() {
    widget.disposer();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => widget.child;
}
