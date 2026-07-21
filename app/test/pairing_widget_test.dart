import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/core/models.dart';
import 'package:night_drop/src/features/chat/chat_screen.dart';
import 'package:night_drop/src/features/chat/verify_screen.dart';
import 'package:night_drop/src/features/pairing/scan_screen.dart';

void main() {
  // On a desktop/test host there is no camera backend (canScanQr == false). The scanner must
  // degrade to a clear message + paste-the-link guidance instead of mounting a camera that would
  // never fire (and must not touch the permission plugin). This also guards the no-camera branch
  // that shipped for desktop pairing.
  testWidgets('ScanScreen without a camera shows the paste-the-link guidance', (tester) async {
    await tester.pumpWidget(const MaterialApp(
      localizationsDelegates: AppLocalizations.localizationsDelegates,
      supportedLocales: AppLocalizations.supportedLocales,
      home: ScanScreen(),
    ));
    await tester.pump();

    expect(find.textContaining('Camera scanning isn’t available'), findsOneWidget);
    expect(find.textContaining('paste it here'), findsOneWidget);
    // The live scanner UI must not be present on a device with no camera backend.
    expect(find.text('Point the camera at your contact’s invite QR.'), findsNothing);
  });

  // 1c: an unverified live chat shows a subtle nudge that disappears once the safety number is
  // verified. Drives the real ChatScreen against the mock core.
  testWidgets('ChatScreen nudges to verify, then clears after verification', (tester) async {
    final core = MockNightdropCore();
    // The mock's join path uses a real Future.delayed; run it via runAsync so its timer actually
    // fires (a bare await here would deadlock against testWidgets' fake-async clock).
    late final Contact contact;
    await tester.runAsync(() async {
      await core.createIdentity();
      contact = await core.joinWithShortCode('4-cedar-lantern-river');
    });

    await tester.pumpWidget(
      NightdropScope(
        core: core,
        child: MaterialApp(
          localizationsDelegates: AppLocalizations.localizationsDelegates,
          supportedLocales: AppLocalizations.supportedLocales,
          home: ChatScreen(contactId: contact.id),
        ),
      ),
    );
    await tester.pump();

    expect(find.textContaining('Not verified'), findsOneWidget);

    await core.setVerified(contact.id, true);
    await tester.pump();

    expect(find.textContaining('Not verified'), findsNothing);
  });

  // #3: the peer's verification is shown as an *informational* banner and must never imply our own
  // verification (no auto-trust). The banner appears only once the peer has signaled, and even then
  // our own state stays "Not verified" until we compare the number ourselves.
  testWidgets('VerifyScreen shows the peer-verified banner without auto-trusting us',
      (tester) async {
    final core = MockNightdropCore();
    late final Contact contact;
    await tester.runAsync(() async {
      await core.createIdentity();
      contact = await core.joinWithShortCode('4-cedar-lantern-river');
    });

    await tester.pumpWidget(
      NightdropScope(
        core: core,
        child: MaterialApp(
          localizationsDelegates: AppLocalizations.localizationsDelegates,
          supportedLocales: AppLocalizations.supportedLocales,
          home: VerifyScreen(contactId: contact.id, name: 'Anon'),
        ),
      ),
    );
    await tester.pump();

    // Peer has not signaled yet → no informational banner.
    expect(find.textContaining('marked this chat verified'), findsNothing);

    // Peer signals they verified. (setVerified(false) only fires notifyListeners here — it does
    // not change our own state — standing in for the Verified frame the real core would deliver.)
    contact.peerVerified = true;
    await core.setVerified(contact.id, false);
    await tester.pump();

    expect(find.textContaining('marked this chat verified'), findsOneWidget);
    // No auto-trust: our own chip stays "Not verified" despite the peer's signal.
    expect(find.text('Not verified'), findsOneWidget);
  });
}
