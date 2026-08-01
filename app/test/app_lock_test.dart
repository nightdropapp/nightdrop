import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/features/lock/app_lock_settings.dart';

/// A core whose store is locked, as it is at launch when the user has set a PIN or passphrase:
/// the identity is unreadable until the secret arrives, so `identity` is null exactly as it is on
/// a fresh install. That collision is the hazard this test exists for.
class _LockedCore extends MockNightdropCore {
  final bool _locked = true;
  String? _key;

  @override
  Future<bool> isStoreLocked() async => _locked;

  @override
  bool get storeUnlocked => _key != null;

  @override
  bool get needsUnlock => _locked && _key == null;

  @override
  Future<bool> unlockStore(String secret) async {
    if (secret != 'the right passphrase') return false;
    _key = 'unwrapped';
    notifyListeners();
    return true;
  }
}

void main() {
  // The lock screen must win over onboarding. A locked store has no readable identity, which looks
  // identical to a fresh install from the outside — and dropping to onboarding there would offer to
  // create a new identity over data the user can still recover with their secret. Same class of bug
  // as the load-error path (see load_error_test.dart), different trigger.
  testWidgets('a locked store shows the lock screen, not onboarding', (tester) async {
    await tester.pumpWidget(NightdropApp(core: _LockedCore()));
    await tester.pump(); // let _Root run start() in its post-frame callback

    expect(find.text('Night Drop is locked'), findsOneWidget);
    expect(find.text('Create my identity'), findsNothing);
  });

  testWidgets('a wrong secret keeps the lock screen up and says nothing specific',
      (tester) async {
    await tester.pumpWidget(NightdropApp(core: _LockedCore()));
    await tester.pump();

    await tester.enterText(find.byType(TextField).first, 'not it');
    await tester.tap(find.text('Unlock'));
    await tester.pump();
    await tester.pump(const Duration(seconds: 1)); // clear the post-failure delay

    expect(find.text("That didn't unlock it. Try again."), findsOneWidget);
    // Still locked out; no route to onboarding opened up.
    expect(find.text('Create my identity'), findsNothing);
  });

  testWidgets('the right secret unlocks through to the app', (tester) async {
    await tester.pumpWidget(NightdropApp(core: _LockedCore()));
    await tester.pump();

    await tester.enterText(find.byType(TextField).first, 'the right passphrase');
    await tester.tap(find.text('Unlock'));
    await tester.pumpAndSettle();

    expect(find.text('Night Drop is locked'), findsNothing);
  });

  // The duress secret (#3) must be indistinguishable from a normal unlock at the screen: it
  // reports success and the lock screen goes away. What the person holding the phone sees is an
  // app that opened and happens to be empty — never an error, a warning, or a "wiped" message,
  // any of which would tell them a wipe code exists and was used.
  testWidgets('the duress secret looks exactly like a successful unlock', (tester) async {
    final core = _DuressCore();
    await tester.pumpWidget(NightdropApp(core: core));
    await tester.pump();

    await tester.enterText(find.byType(TextField).first, 'the wipe code');
    await tester.tap(find.text('Unlock'));
    await tester.pumpAndSettle();

    expect(core.wiped, isTrue, reason: 'the duress secret must trigger the wipe');
    expect(find.text('Night Drop is locked'), findsNothing);
    expect(find.text("That didn't unlock it. Try again."), findsNothing);
  });

  // Regression (field report, 2026-08-01): the wipe-code flow took the current secret, then asked
  // for the new code, and only validated the first field at the very end — so a wrong secret walked
  // you through the whole flow before failing. Reject it on the spot.
  testWidgets('a wrong current secret is refused before the wipe code is asked for',
      (tester) async {
    final core = _DuressSettingsCore();
    await tester.pumpWidget(MaterialApp(
      localizationsDelegates: AppLocalizations.localizationsDelegates,
      supportedLocales: AppLocalizations.supportedLocales,
      home: Scaffold(
        body: Builder(
          builder: (context) => ElevatedButton(
            onPressed: () => showDuressSettings(context, core),
            child: const Text('open'),
          ),
        ),
      ),
    ));
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();

    // Nothing is armed, so "remove" must not be on offer at all.
    expect(find.text('Remove the wipe code'), findsNothing);
    await tester.tap(find.text('Set a wipe code'));
    await tester.pumpAndSettle();

    await tester.enterText(find.byType(TextField).first, 'wrong one');
    await tester.tap(find.text('Confirm'));
    await tester.pumpAndSettle();

    expect(core.verified, contains('wrong one'));
    expect(core.armed, isFalse);
    // Still on the secret prompt with an error — never advanced to choosing a wipe code.
    expect(find.text('New wipe code'), findsNothing);
    expect(find.text("That didn't match. Nothing was changed."), findsOneWidget);
  });

  // Regression (field report, 2026-08-01): disabling the app lock dropped the user on the lock
  // screen. `inactive` fires while the app is still on screen — a dialog taking focus, the keyboard
  // opening — and re-locking there ran mid-disable, while the lock file still existed. Only a real
  // departure from the foreground should re-lock.
  testWidgets('a transient inactive state does not re-lock', (tester) async {
    final core = _RelockCore();
    await tester.pumpWidget(NightdropApp(core: core));
    await tester.pump();

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    await tester.pump();
    expect(core.lockCalls, 0, reason: 'a dialog or keyboard must not re-lock the app');

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
    await tester.pump();
    expect(core.lockCalls, 1, reason: 'actually leaving the foreground still re-locks');
  });

  // Regression (field report, 2026-08-01): after a wipe the phone came up saying "Couldn't open
  // your saved session". `logout()` dropped Dart's reference to the core without shutting the Rust
  // instance down, so it re-persisted the state file just deleted — while the keystore key stayed
  // deleted. The next launch then found saved data with no key and offered to recover an identity
  // that had been destroyed on purpose. The core must be stopped before its files are removed.
  testWidgets('the wipe shuts the core down before deleting what it owns', (tester) async {
    final core = _WipeOrderCore();
    await tester.pumpWidget(NightdropApp(core: core));
    await tester.pump();

    await tester.enterText(find.byType(TextField).first, 'the wipe code');
    await tester.tap(find.text('Unlock'));
    await tester.pumpAndSettle();

    expect(core.steps, ['notify-peers', 'shutdown', 'delete-files'],
        reason: 'a core still running will re-persist the state file after it is deleted');
  });

  // The other half of the same report: "remove" is offered only when there is something to remove.
  testWidgets('remove is offered only once a wipe code is armed', (tester) async {
    final core = _DuressSettingsCore()..armed = true;
    await tester.pumpWidget(MaterialApp(
      localizationsDelegates: AppLocalizations.localizationsDelegates,
      supportedLocales: AppLocalizations.supportedLocales,
      home: Scaffold(
        body: Builder(
          builder: (context) => ElevatedButton(
            onPressed: () => showDuressSettings(context, core),
            child: const Text('open'),
          ),
        ),
      ),
    ));
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();

    expect(find.text('Remove the wipe code'), findsOneWidget);
    expect(find.text('Replace it'), findsOneWidget);
  });
}

/// A locked core that also knows a duress secret, wiping instead of opening.
class _DuressCore extends MockNightdropCore {
  bool wiped = false;
  String? _key;

  @override
  Future<bool> isStoreLocked() async => true;

  @override
  bool get storeUnlocked => _key != null;

  @override
  bool get needsUnlock => _key == null && !wiped;

  @override
  Future<bool> unlockStore(String secret) async {
    if (secret == 'the wipe code') {
      // Mirrors the real path: destroy, then report success like any other unlock.
      wiped = true;
      notifyListeners();
      return true;
    }
    if (secret != 'the right passphrase') return false;
    _key = 'unwrapped';
    notifyListeners();
    return true;
  }
}

/// A locked core that tracks the wipe-code state, for the settings flow.
class _DuressSettingsCore extends MockNightdropCore {
  bool armed = false;
  final List<String> verified = [];

  @override
  Future<bool> isStoreLocked() async => true;

  @override
  Future<bool> isDuressArmed() async => armed;

  @override
  Future<bool> verifyStoreSecret(String secret) async {
    verified.add(secret);
    return secret == 'the right passphrase';
  }

  @override
  Future<void> setDuressSecret(String secret, String duress) async {
    if (secret != 'the right passphrase') throw StateError('wrong secret');
    armed = true;
  }
}

/// Counts re-lock attempts, to pin down which lifecycle states trigger one.
class _RelockCore extends MockNightdropCore {
  int lockCalls = 0;

  @override
  Future<void> lockStore() async => lockCalls++;
}

/// Records the ORDER of the wipe's steps. The ordering is the invariant: a core that is still
/// running owns the persist path and will write the state file back out from under the delete.
class _WipeOrderCore extends MockNightdropCore {
  final List<String> steps = [];
  bool _wiped = false;

  @override
  Future<bool> isStoreLocked() async => true;

  @override
  bool get needsUnlock => !_wiped;

  @override
  Future<bool> unlockStore(String secret) async {
    if (secret != 'the wipe code') return false;
    // Mirrors _duressWipe in RustNightdropCore.
    steps.add('notify-peers');
    steps.add('shutdown');
    steps.add('delete-files');
    _wiped = true;
    notifyListeners();
    return true;
  }
}
