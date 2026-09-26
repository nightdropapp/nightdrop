import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/features/chat/chat_screen.dart';
import 'package:night_drop/src/features/chat/verify_screen.dart';

void main() {
  // On a phone, seven header icons filled the bar and put the verify shield right beside Back.
  // Only the two state indicators stay; the rest live in the overflow menu.
  testWidgets('the chat header keeps only state icons; the rest are in the menu',
      (tester) async {
    tester.view.physicalSize = const Size(1080, 2340); // phone-sized
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);

    final core = MockNightdropCore();
    late final String contactId;
    await tester.runAsync(() async {
      await core.createIdentity();
      contactId = (await core.joinWithShortCode('4-cedar-lantern-river')).id;
    });
    await tester.pumpWidget(
      NightdropScope(
        core: core,
        child: MaterialApp(
          localizationsDelegates: AppLocalizations.localizationsDelegates,
          supportedLocales: AppLocalizations.supportedLocales,
          home: ChatScreen(contactId: contactId),
        ),
      ),
    );
    await tester.pump();

    final bar = find.byType(AppBar);
    for (final gone in [
      Icons.shield_outlined,
      Icons.drive_file_rename_outline,
      Icons.badge_outlined,
      Icons.delete_outline,
    ]) {
      expect(find.descendant(of: bar, matching: find.byIcon(gone)), findsNothing,
          reason: '$gone belongs in the overflow menu');
    }
    expect(find.descendant(of: bar, matching: find.byIcon(Icons.cloud_off)),
        findsOneWidget);
    expect(find.descendant(of: bar, matching: find.byIcon(Icons.timer_off_outlined)),
        findsOneWidget);

    await tester.tap(find.byIcon(Icons.more_vert));
    await tester.pumpAndSettle();
    for (final label in [
      'Verify safety number',
      'Name this contact (only you see it)',
      'Rename yourself in this chat',
      'Back up this chat…',
      'Delete this chat',
    ]) {
      expect(find.text(label), findsOneWidget);
    }

    await tester.tap(find.text('Verify safety number'));
    await tester.pumpAndSettle();
    expect(find.byType(VerifyScreen), findsOneWidget);

    await tester.pumpWidget(const SizedBox());
    await tester.pump(const Duration(seconds: 5));
  });
}
