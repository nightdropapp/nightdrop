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
  if (current is! String || !context.mounted) return;
  await _armDuress(context, core, current);
}

/// Ask for a wipe code and arm it, given an already-verified normal secret.
///
/// Every slow step (each is a deliberately expensive key derivation, seconds on a phone) runs while
/// its dialog is still open and showing a spinner, and the dialog only closes when the step is done.
/// Done between dialogs instead, the screen went blank for seconds after each tap, which looked as
/// though the flow had been abandoned (phone, 2026-10-09).
Future<void> _armDuress(BuildContext context, NightdropCore core, String current) async {
  final l10n = AppLocalizations.of(context)!;
  final messenger = ScaffoldMessenger.of(context);
  // A result replaces whatever is showing ("App lock is on." from the step before), instead of
  // queueing behind it and appearing seconds after the dialog has gone.
  void say(String text) => messenger
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(text)));
  // The navigator, not the caller's context, shows the dialogs: the screen that started this can be
  // rebuilt away while a slow step runs, and with `context` the warning was once skipped without a
  // sound (phone, 2026-10-08).
  final navigator = Navigator.of(context);

  // The new code is checked inside its own dialog: one that also opens the normal slot could never
  // reach the duress one, and that is the one refusal arming has. So the warning below is only ever
  // shown for a code that can be set.
  final duress = await _askNewDuress(navigator.context, core);
  if (duress == null || !navigator.mounted) return;

  // Warn first, arm after. The user can still back out here — Cancel, or the back button — and
  // nothing has changed: an existing wipe code stays as it was. Arming first and disarming on cancel
  // could not do that for a replacement, whose old code is gone once the new one is written.
  final armed = await showDialog<bool>(
    context: navigator.context,
    builder: (context) => _BusyDialog(
      title: Text(l10n.duressWarnTitle),
      content: SingleChildScrollView(child: Text(l10n.duressWarnBody)),
      cancelLabel: l10n.cancel,
      actionLabel: l10n.duressUnderstand,
      // The core self-checks the slot it writes and rolls back if the check fails.
      action: () async {
        try {
          await core.setDuressSecret(current, duress);
          return const _Done(true);
        } catch (_) {
          return const _Done(false);
        }
      },
    ),
  );
  if (armed == null) {
    say(l10n.duressCancelled);
  } else {
    say(armed ? l10n.duressDone : l10n.duressSame);
  }
}

/// Disarm. The removal runs inside the secret dialog, after the secret checks out.
Future<void> _removeDuress(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final messenger = ScaffoldMessenger.of(context);
  final removed = await _askVerifiedSecret(
    context,
    core,
    then: (current) async {
      try {
        await core.clearDuressSecret(current);
        return true;
      } catch (_) {
        return false;
      }
    },
  );
  if (removed == null) return;
  messenger
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(
        content: Text(removed == true ? l10n.duressCleared : l10n.appLockWrongSecret)));
}

/// Ask for the current secret and **check it before returning**, so a wrong one is rejected on the
/// spot instead of surfacing several screens later. The check runs with the dialog still open: a
/// wrong secret shows its error in place, and nothing closes until the secret is right. With [then],
/// that next step runs inside the same dialog too, and its result is returned instead of the secret.
/// Null if the user cancels.
Future<Object?> _askVerifiedSecret(
  BuildContext context,
  NightdropCore core, {
  Future<Object?> Function(String secret)? then,
}) async {
  final l10n = AppLocalizations.of(context)!;
  final controller = TextEditingController();
  return showDialog<Object?>(
    context: context,
    builder: (context) => _Owns(
      disposer: controller.dispose,
      child: _BusyDialog(
        content: (error, submit) => TextField(
          enableIMEPersonalizedLearning: false,
          controller: controller,
          autofocus: true,
          obscureText: true,
          autocorrect: false,
          enableSuggestions: false,
          decoration: InputDecoration(
              labelText: l10n.duressCurrentSecret, errorText: error, errorMaxLines: 3),
          onSubmitted: (_) => submit(),
        ),
        cancelLabel: l10n.cancel,
        actionLabel: l10n.confirm,
        action: () async {
          final secret = controller.text;
          if (secret.isEmpty) return const _Retry(null);
          if (!await core.verifyStoreSecret(secret)) {
            // Generic, and identical for "wrong secret" and "that's the wipe code" — this screen
            // never confirms to anyone which of the two they typed.
            controller.clear();
            return _Retry(l10n.appLockWrongSecret);
          }
          return _Done(then == null ? secret : await then(secret));
        },
      ),
    ),
  );
}

/// Ask twice for a new wipe code. Minimum length follows the same rule as the normal secret: a
/// digits-only code is a PIN (6), anything else is a passphrase (12).
Future<String?> _askNewDuress(BuildContext context, NightdropCore core) async {
  final l10n = AppLocalizations.of(context)!;
  final first = TextEditingController();
  final second = TextEditingController();
  // The keyboard covers the lower half of a dialog this tall, hiding the confirm field entirely.
  // The content scrolls, but a user can't scroll to a field they can't see is there — so the first
  // field's action key moves focus to the second, and the framework scrolls it into view.
  final secondFocus = FocusNode();
  String? error;
  var busy = false;
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
        Future<void> submit() async {
          if (busy) return;
          final a = first.text, b = second.text;
          final digitsOnly = RegExp(r'^\d+$').hasMatch(a);
          String? problem;
          if (a.length < (digitsOnly ? 6 : 12)) {
            problem = digitsOnly ? l10n.appLockTooShortPin : l10n.appLockTooShortPassphrase;
          } else if (a != b) {
            problem = l10n.appLockMismatch;
          } else {
            // Slow (key derivation), so the dialog stays up with a spinner while it runs.
            setState(() => busy = true);
            final opensLock = await core.verifyStoreSecret(a);
            if (!context.mounted) return;
            if (opensLock) problem = l10n.duressSame;
          }
          setState(() {
            busy = false;
            error = problem;
          });
          if (problem == null) {
            Navigator.of(context).pop(a);
          } else {
            // The error sits under the confirm field, which the keyboard hides. Focusing it scrolls
            // it into view; without this, a press on the button with the second field empty did
            // nothing anyone could see (phone, 2026-10-08).
            secondFocus.requestFocus();
          }
        }

        return PopScope(
          canPop: !busy,
          child: AlertDialog(
          title: Text(l10n.duressTitle),
          content: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(l10n.duressBody),
                const SizedBox(height: 16),
                TextField(
                  enableIMEPersonalizedLearning: false,
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
                  enableIMEPersonalizedLearning: false,
                  controller: second,
                  focusNode: secondFocus,
                  obscureText: true,
                  autocorrect: false,
                  enableSuggestions: false,
                  textInputAction: TextInputAction.done,
                  decoration: InputDecoration(
                    labelText: l10n.duressConfirm,
                    errorText: error,
                    // Errors here are whole sentences; one truncated line cut them mid-word.
                    errorMaxLines: 3,
                  ),
                  onSubmitted: (_) => submit(),
                ),
              ],
            ),
          ),
          actions: [
            TextButton(
              onPressed: busy ? null : () => Navigator.of(context).pop(),
              child: Text(l10n.cancel),
            ),
            FilledButton(
              onPressed: busy ? null : submit,
              child: busy ? const _Spinner() : Text(l10n.duressSet),
            ),
          ],
          ),
        );
        },
      ),
    ),
  );
  return result;
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

  // Last gate, because this is the irreversible part: there is no recovery path by design. The lock
  // is written while this dialog is still up (a key derivation, seconds on a phone).
  final enabled = await showDialog<bool>(
    context: context,
    builder: (context) => _BusyDialog(
      title: Text(l10n.appLockEnable),
      content: Text(l10n.appLockWarnNoRecovery),
      cancelLabel: l10n.cancel,
      actionLabel: l10n.appLockIUnderstand,
      action: () async {
        await core.enableStoreLock(secret);
        return const _Done(true);
      },
    ),
  );
  if (enabled != true || !context.mounted) return;

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
  final messenger = ScaffoldMessenger.of(context);
  final controller = TextEditingController();
  // The removal runs with the dialog open: a wrong secret shows its error in place (the core proves
  // knowledge before removing anything), and the dialog closes only once the lock is gone.
  final disabled = await showDialog<bool>(
    context: context,
    builder: (context) => _Owns(
      disposer: controller.dispose,
      child: _BusyDialog(
        title: Text(l10n.appLockDisable),
        content: (error, submit) => TextField(
          enableIMEPersonalizedLearning: false,
          controller: controller,
          autofocus: true,
          obscureText: true,
          autocorrect: false,
          enableSuggestions: false,
          decoration:
              InputDecoration(
              labelText: l10n.appLockCurrentSecret, errorText: error, errorMaxLines: 3),
          onSubmitted: (_) => submit(),
        ),
        cancelLabel: l10n.cancel,
        actionLabel: l10n.appLockDisable,
        action: () async {
          if (controller.text.isEmpty) return const _Retry(null);
          try {
            await core.disableStoreLock(controller.text);
            return const _Done(true);
          } catch (_) {
            controller.clear();
            return _Retry(l10n.appLockWrongSecret);
          }
        },
      ),
    ),
  );
  if (disabled == true) {
    messenger.showSnackBar(SnackBar(content: Text(l10n.appLockDisabled)));
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
                  enableIMEPersonalizedLearning: false,
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
                  enableIMEPersonalizedLearning: false,
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
                    // Errors here are whole sentences; one truncated line cut them mid-word.
                    errorMaxLines: 3,
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

/// What a [_BusyDialog] action produced: close with [value], or stay open showing [error].
sealed class _Outcome {
  const _Outcome();
}

class _Done extends _Outcome {
  const _Done(this.value);
  final Object? value;
}

class _Retry extends _Outcome {
  const _Retry(this.error);
  final String? error;
}

/// A dialog whose confirm button runs a slow [action] **while the dialog stays open**: a spinner in
/// the button, both buttons disabled, back blocked. The action either finishes it ([_Done], which
/// pops with its value) or keeps it open with an error shown in place ([_Retry]). Cancel pops null.
///
/// Exists because the app lock's steps are deliberately slow key derivations, and running them
/// after the dialog had closed left the screen blank for seconds between dialogs.
class _BusyDialog extends StatefulWidget {
  const _BusyDialog({
    this.title,
    required this.content,
    required this.cancelLabel,
    required this.actionLabel,
    required this.action,
  });

  final Widget? title;

  /// A widget, or a `Widget Function(String? error, VoidCallback submit)` for a field that shows the
  /// latest [_Retry] error and submits from the keyboard's action key.
  final Object content;
  final String cancelLabel;
  final String actionLabel;
  final Future<_Outcome> Function() action;

  @override
  State<_BusyDialog> createState() => _BusyDialogState();
}

class _BusyDialogState extends State<_BusyDialog> {
  bool _busy = false;
  String? _error;

  Future<void> _run() async {
    if (_busy) return;
    setState(() => _busy = true);
    final outcome = await widget.action();
    if (!mounted) return;
    switch (outcome) {
      case _Done(:final value):
        Navigator.of(context).pop(value);
      case _Retry(:final error):
        setState(() {
          _busy = false;
          _error = error;
        });
    }
  }

  @override
  Widget build(BuildContext context) {
    final content = widget.content;
    final body = content is Widget Function(String?, VoidCallback)
        ? content(_error, _run)
        : content as Widget;
    return PopScope(
      canPop: !_busy,
      child: AlertDialog(
        title: widget.title,
        content: body,
        actions: [
          TextButton(
            onPressed: _busy ? null : () => Navigator.of(context).pop(),
            child: Text(widget.cancelLabel),
          ),
          FilledButton(
            onPressed: _busy ? null : _run,
            child: _busy ? const _Spinner() : Text(widget.actionLabel),
          ),
        ],
      ),
    );
  }
}

/// The small spinner shown in a button while its action runs.
class _Spinner extends StatelessWidget {
  const _Spinner();

  @override
  Widget build(BuildContext context) =>
      const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2));
}
