import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/features/lock/lock_screen.dart';

/// A locked core that reports a wait, as the real one does after five wrong attempts in a row
/// (`core/src/storage/lock.rs`), and counts how many unlocks the screen tries.
class _WaitingCore extends MockNightdropCore {
  _WaitingCore(this.wait);
  int wait;
  int tries = 0;

  @override
  Future<int> unlockWaitSecs() async => wait;

  @override
  Future<bool> unlockStore(String secret) async {
    tries++;
    return false;
  }
}

Future<void> _pump(WidgetTester tester, _WaitingCore core) async {
  await tester.pumpWidget(NightdropScope(
    core: core,
    child: const MaterialApp(
      localizationsDelegates: AppLocalizations.localizationsDelegates,
      supportedLocales: AppLocalizations.supportedLocales,
      home: LockScreen(),
    ),
  ));
  await tester.pump();
}

void main() {
  testWidgets('a running wait is shown and blocks unlocking, even right after a restart',
      (tester) async {
    // Self-audit 2026-10-13, round 5: the wait lives in the core, so a freshly opened lock screen
    // (a restarted app) must show it and refuse to try.
    final core = _WaitingCore(90);
    await _pump(tester, core);
    expect(find.textContaining('Too many wrong tries'), findsOneWidget);
    expect(find.textContaining('2 min'), findsOneWidget);
    await tester.enterText(find.byType(TextField), '1234');
    await tester.testTextInput.receiveAction(TextInputAction.go);
    await tester.pump();
    expect(core.tries, 0, reason: 'no attempt reaches the core during a wait');
    final button = tester.widget<FilledButton>(find.byType(FilledButton));
    expect(button.onPressed, isNull);
    // The countdown runs, and when the core says the wait is over the screen accepts input again.
    core.wait = 0;
    await tester.pump(const Duration(seconds: 90));
    await tester.pump();
    expect(find.textContaining('Too many wrong tries'), findsNothing);
    expect(tester.widget<FilledButton>(find.byType(FilledButton)).onPressed, isNotNull);
  });

  testWidgets('no wait: a wrong secret shows the ordinary failure', (tester) async {
    final core = _WaitingCore(0);
    await _pump(tester, core);
    await tester.enterText(find.byType(TextField), '0000');
    await tester.testTextInput.receiveAction(TextInputAction.go);
    await tester.pump();
    expect(core.tries, 1);
    expect(find.textContaining("didn't unlock"), findsOneWidget);
    await tester.pump(const Duration(seconds: 1));
  });
}
