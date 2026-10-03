import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/models.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/features/bridges/bridges_screen.dart';

const _line1 = 'webtunnel [2001:db8::1]:443 AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA url=https://a.example/x ver=0.0.3';
const _line2 = 'webtunnel [2001:db8::2]:443 BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB url=https://b.example/y ver=0.0.3';

/// Records what the screen asks the network for, and never saves on its own.
class _FetchCore extends MockNightdropCore {
  final calls = <String?>[];
  int saves = 0;
  Object? failWith;

  @override
  Future<String> readBridges() async => '';

  @override
  Future<BridgeSave> writeBridges(String text) async {
    saves++;
    return const BridgeSave(accepted: 0, rejected: []);
  }

  @override
  Future<FetchedBridges> fetchBridges({String? country}) async {
    calls.add(country);
    if (failWith != null) throw failWith!;
    return const FetchedBridges(lines: [_line1, _line2], country: 'ir');
  }
}

void main() {
  Future<_FetchCore> pumpBridges(WidgetTester tester) async {
    final core = _FetchCore();
    await tester.pumpWidget(NightdropScope(
      core: core,
      child: const MaterialApp(
        localizationsDelegates: AppLocalizations.localizationsDelegates,
        supportedLocales: AppLocalizations.supportedLocales,
        home: BridgesScreen(),
      ),
    ));
    await tester.pumpAndSettle();
    return core;
  }

  Future<void> openConsent(WidgetTester tester) async {
    await tester.tap(find.text('Get WebTunnel bridges from the Tor Project'));
    await tester.pumpAndSettle();
  }

  testWidgets('the consent says it is not through Tor and nothing goes to Night Drop', (tester) async {
    final core = await pumpBridges(tester);
    await openConsent(tester);
    // The two things the user is agreeing to must be in the words they read before agreeing.
    expect(find.textContaining('does not go through Tor'), findsOneWidget);
    expect(find.textContaining('nothing is sent to Night Drop'), findsOneWidget);
    expect(find.textContaining('see your IP address'), findsOneWidget);

    // Declining sends nothing at all.
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(core.calls, isEmpty);
  });

  testWidgets('agreeing fetches, and the lines land in the editor unsaved', (tester) async {
    final core = await pumpBridges(tester);
    await openConsent(tester);
    await tester.enterText(find.widgetWithText(TextField, 'Country (optional)'), 'IR');
    await tester.tap(find.text('Get bridges'));
    await tester.pumpAndSettle();

    expect(core.calls, ['ir'], reason: 'one request, with the country normalised');
    final editor = tester.widget<TextField>(find.byType(TextField)).controller!.text;
    expect(editor, contains(_line1));
    expect(editor, contains(_line2));
    expect(editor, contains('# WebTunnel bridges from the Tor Project'));
    expect(find.text('Added 2 WebTunnel bridges. Tap Save to use them.'), findsOneWidget);
    // The user still decides: nothing is saved until they tap Save.
    expect(core.saves, 0);

    // A second fetch adds nothing already there.
    await openConsent(tester);
    await tester.tap(find.text('Get bridges'));
    await tester.pumpAndSettle();
    expect(core.calls, ['ir', null], reason: 'empty country = let the Tor Project detect it');
    final again = tester.widget<TextField>(find.byType(TextField)).controller!.text;
    expect(RegExp(RegExp.escape(_line1)).allMatches(again).length, 1);
  });

  testWidgets('a malformed country cannot be sent', (tester) async {
    final core = await pumpBridges(tester);
    await openConsent(tester);
    await tester.enterText(find.widgetWithText(TextField, 'Country (optional)'), 'i1');
    await tester.pump();
    final go = tester.widget<FilledButton>(find.widgetWithText(FilledButton, 'Get bridges'));
    expect(go.onPressed, isNull);
    expect(core.calls, isEmpty);
  });

  testWidgets('a failure says why and points to the other ways', (tester) async {
    final core = await pumpBridges(tester)..failWith = StateError('the Tor Project has no webtunnel bridges that work in this country');
    await openConsent(tester);
    await tester.tap(find.text('Get bridges'));
    await tester.pumpAndSettle();
    expect(find.textContaining('no webtunnel bridges that work in this country'), findsOneWidget);
    expect(find.textContaining('use one of the other ways below'), findsOneWidget);
    expect(core.saves, 0);
  });
}
