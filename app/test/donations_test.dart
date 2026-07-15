import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:ghost_chat/l10n/app_localizations.dart';
import 'package:ghost_chat/src/features/donations/donations_screen.dart';

void main() {
  testWidgets('donations screen lists privacy-coin addresses', (tester) async {
    await tester.pumpWidget(const MaterialApp(
      localizationsDelegates: AppLocalizations.localizationsDelegates,
      supportedLocales: AppLocalizations.supportedLocales,
      home: DonationsScreen(),
    ));

    expect(find.text('Monero (XMR)'), findsOneWidget);
    expect(find.text('Copy address'), findsNWidgets(kDonationCoins.length));
  });
}
