// "Also connect on the same Wi-Fi" (docs/design/nearby.md): the chat menu asks before turning it
// on, and the chat says which of the four states it is in.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/features/chat/chat_screen.dart';

void main() {
  Future<(MockNightdropCore, String)> pumpChat(WidgetTester tester) async {
    final core = MockNightdropCore();
    late final String id;
    await tester.runAsync(() async {
      await core.createIdentity();
      id = (await core.joinWithShortCode('4-cedar-lantern-river')).id;
    });
    await tester.pumpWidget(
      NightdropScope(
        core: core,
        child: MaterialApp(
          localizationsDelegates: AppLocalizations.localizationsDelegates,
          supportedLocales: AppLocalizations.supportedLocales,
          home: ChatScreen(contactId: id),
        ),
      ),
    );
    await tester.pump();
    return (core, id);
  }

  testWidgets('turning it on asks first, then waits for the contact', (tester) async {
    final (core, _) = await pumpChat(tester);
    final l10n = await AppLocalizations.delegate.load(const Locale('en'));
    expect(find.text(l10n.nearbyWaitingSummary), findsNothing, reason: 'off by default');

    await tester.tap(find.byType(PopupMenuButton<String>));
    await tester.pumpAndSettle();
    await tester.tap(find.text(l10n.nearbyOnMenu));
    await tester.pumpAndSettle();
    expect(find.text(l10n.nearbyTitle), findsOneWidget);

    // Cancel leaves it off.
    await tester.tap(find.text(l10n.cancel));
    await tester.pumpAndSettle();
    expect(core.contacts.first.nearby, isFalse);

    await tester.tap(find.byType(PopupMenuButton<String>));
    await tester.pumpAndSettle();
    await tester.tap(find.text(l10n.nearbyOnMenu));
    await tester.pumpAndSettle();
    await tester.tap(find.text(l10n.nearbyConfirm));
    await tester.pumpAndSettle();
    expect(core.contacts.first.nearby, isTrue);
    expect(find.text(l10n.nearbyWaitingSummary), findsOneWidget);
  });

  testWidgets('the banner follows both sides and the connection', (tester) async {
    final (core, _) = await pumpChat(tester);
    final l10n = await AppLocalizations.delegate.load(const Locale('en'));
    final c = core.contacts.first;

    c.peerNearby = true;
    core.notifyListeners();
    await tester.pump();
    expect(find.text(l10n.nearbyAskedSummary), findsOneWidget);

    c.nearby = true;
    core.notifyListeners();
    await tester.pump();
    expect(find.text(l10n.nearbyApartSummary), findsOneWidget);

    c.nearbyConnected = true;
    core.notifyListeners();
    await tester.pump();
    expect(find.text(l10n.nearbyConnectedSummary), findsOneWidget);

    // Turning it off needs no confirmation.
    await tester.tap(find.byType(PopupMenuButton<String>));
    await tester.pumpAndSettle();
    await tester.tap(find.text(l10n.nearbyOffMenu));
    await tester.pumpAndSettle();
    expect(c.nearby, isFalse);
    expect(find.text(l10n.nearbyTitle), findsNothing);
  });
}
