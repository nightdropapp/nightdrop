import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/models.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/features/bridges/bridges_screen.dart';

/// A core that validates bridges the way the real one does: anything that isn't `host:port
/// FINGERPRINT` comes back rejected, with a reason.
class _BridgeCore extends MockNightdropCore {
  String saved = '';

  @override
  Future<String> readBridges() async => saved;

  @override
  Future<BridgeSave> writeBridges(String text) async {
    final kept = <String>[];
    final rejected = <RejectedBridge>[];
    for (final raw in text.split('\n')) {
      final line = raw.trim();
      if (line.isEmpty || line.startsWith('#')) continue;
      if (RegExp(r'^[\d.]+:\d+\s+[0-9A-Fa-f]{40}$').hasMatch(line)) {
        kept.add(line);
      } else {
        rejected.add(RejectedBridge(line: line, reason: 'not a valid bridge line'));
      }
    }
    saved = kept.join('\n');
    return BridgeSave(accepted: kept.length, rejected: rejected);
  }
}

void main() {
  Future<_BridgeCore> pumpBridges(WidgetTester tester) async {
    final core = _BridgeCore();
    await tester.pumpWidget(NightdropScope(
      core: core,
      // ignore: prefer_const_constructors — the delegates list is not a const expression here
      child: MaterialApp(
        localizationsDelegates: AppLocalizations.localizationsDelegates,
        supportedLocales: AppLocalizations.supportedLocales,
        home: const BridgesScreen(),
      ),
    ));
    await tester.pumpAndSettle();
    return core;
  }

  // Android's Tor state dir is app-private, so this screen is the only way a user behind a
  // firewall can configure bridges at all. See docs/design/android-bridges.md.
  testWidgets('a valid bridge line is saved', (tester) async {
    final core = await pumpBridges(tester);

    await tester.enterText(find.byType(TextField),
        '38.229.33.83:80 0BAC39417268B96B9F514E7F63FA6FBA1A788955');
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();

    expect(core.saved, contains('38.229.33.83:80'));
    expect(find.text('1 bridge saved'), findsOneWidget);
  });

  // A silently dropped line is the worst outcome: someone copying bridges over a censored link
  // would believe they were configured. Rejects come back with the parser's reason.
  testWidgets('a bad line is reported back, not swallowed', (tester) async {
    final core = await pumpBridges(tester);

    await tester.enterText(find.byType(TextField), 'this is not a bridge');
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();

    expect(core.saved, isEmpty);
    expect(find.text('1 line was not understood'), findsOneWidget);
    // Twice: still in the box the user typed it into, and again in the rejects list below.
    expect(find.text('this is not a bridge'), findsNWidgets(2));
    expect(find.text('not a valid bridge line'), findsOneWidget);
  });

  // The screen must not let a user in a heavily censored country conclude that vanilla bridges
  // are enough. DPI-based blocking needs obfs4/Snowflake, which this build cannot run on Android.
  testWidgets('the limit is stated before the input', (tester) async {
    await pumpBridges(tester);

    expect(find.textContaining('blocks Tor by how it looks'), findsOneWidget);
    expect(find.textContaining('obfs4'), findsOneWidget);
  });
}
