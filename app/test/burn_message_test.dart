import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/models.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/features/chat/chat_screen.dart';
import 'package:night_drop/src/features/home/home_screen.dart';

/// Burn messages (`docs/design/burn-messages.md`) at the UI edge.
class _BurnCore extends MockNightdropCore {
  final List<String> viewed = [];
  final List<(String, int)> sent = [];

  @override
  Future<void> markBurnViewed(String contactId, String msgId) async {
    viewed.add(msgId);
    await super.markBurnViewed(contactId, msgId);
  }

  @override
  Future<void> sendBurnMessage(String contactId, String text, int burnSecs) async {
    sent.add((text, burnSecs));
    await super.sendBurnMessage(contactId, text, burnSecs);
  }

  /// Stand in for an inbound `Frame::BurnMedia` from the peer.
  void receiveBurnMedia(String contactId, int secs) {
    appendForTest(Message(
      id: 'bm-1',
      contactId: contactId,
      text: '',
      fromMe: false,
      at: DateTime.now(),
      msgId: '',
      kind: 'image',
      mime: 'image/jpeg',
      mediaId: 'sealed-1',
      mediaSize: 4096,
      transferId: 'bm-1',
      burnSecs: secs,
    ));
    notifyListeners();
  }

  /// Stand in for an inbound `Frame::Burn` from the peer.
  void receiveBurn(String contactId, String text, int secs) {
    final id = 'burn-1';
    appendForTest(Message(
      id: id,
      contactId: contactId,
      text: text,
      fromMe: false,
      at: DateTime.now(),
      msgId: id,
      burnSecs: secs,
    ));
    notifyListeners();
  }

  void setServerStorage(bool on) {
    contacts.first.remoteStorage = on;
    notifyListeners();
  }

  void peerSupportsBurn(bool? v) {
    contacts.first.peerSupportsBurn = v;
    notifyListeners();
  }
}

void main() {
  // The setting is off by default and the dialog must state the cost BEFORE the switch: the
  // sender's copy vanishing at the moment of reading is itself the receipt, so there is no
  // version where they get the early deletion without the timing.
  testWidgets('burn read receipts are off by default and disclose what they leak',
      (tester) async {
    final core = _BurnCore();
    await tester.runAsync(() async {
      await core.createIdentity();
      await core.joinWithShortCode('4-cedar-lantern-river');
    });
    expect(await core.burnReceiptsEnabled(), isFalse,
        reason: 'the recipient has to opt in to disclosing their own reading behaviour');

    await tester.pumpWidget(
      NightdropScope(
        core: core,
        child: MaterialApp(
          localizationsDelegates: AppLocalizations.localizationsDelegates,
          supportedLocales: AppLocalizations.supportedLocales,
          home: const HomeScreen(),
        ),
      ),
    );
    await tester.pump();

    await tester.tap(find.byIcon(Icons.more_vert));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Burn read receipts'));
    await tester.pumpAndSettle();

    expect(find.textContaining('WHEN you read it'), findsOneWidget,
        reason: 'the cost is stated before the switch, not after');
    expect(find.text('Turn on'), findsOneWidget, reason: 'so it is currently off');

    await tester.tap(find.text('Turn on'));
    await tester.pumpAndSettle();
    expect(await core.burnReceiptsEnabled(), isTrue);

    await tester.pumpWidget(const SizedBox());
    await tester.pump(const Duration(seconds: 5));
  });


  /// Replace the tree so StatefulWidgets dispose inside the test: the burn countdown's periodic
  /// ticker and the chat screen's auto-scroll callbacks are both cancelled in dispose, and
  /// flutter_test fails a test that ends with either still armed.
  Future<void> settleScroll(WidgetTester tester) async {
    await tester.pumpWidget(const SizedBox());
    await tester.pump();
  }

  Future<(_BurnCore, Contact)> pumpChat(WidgetTester tester) async {
    final core = _BurnCore();
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
    return (core, contact);
  }

  // The property the whole hidden state depends on. Blurring the REAL text would leave it in the
  // widget tree — read aloud by a screen reader, present in a screenshot, and a known de-blurring
  // target. The content must not be there at all until it is revealed.
  testWidgets('an unrevealed burn message does not contain its text anywhere in the tree',
      (tester) async {
    final (core, contact) = await pumpChat(tester);
    core.peerSupportsBurn(true);
    core.receiveBurn(contact.id, 'the safehouse is on elm', 30);
    await tester.pump();

    expect(
      find.text('the safehouse is on elm'),
      findsNothing,
      reason: 'hidden burn text must never be rendered, blurred or otherwise',
    );
    expect(find.text('Tap to reveal'), findsOneWidget);
    await settleScroll(tester);
  });

  testWidgets('tapping a blurred burn message reveals it and starts the countdown',
      (tester) async {
    final (core, contact) = await pumpChat(tester);
    core.peerSupportsBurn(true);
    core.receiveBurn(contact.id, 'the safehouse is on elm', 30);
    await tester.pump();

    await tester.tap(find.text('Tap to reveal'));
    await tester.pump();

    expect(core.viewed, ['burn-1']);
    expect(find.text('the safehouse is on elm'), findsOneWidget,
        reason: 'revealed text is shown normally');
    expect(find.byType(CircularProgressIndicator), findsWidgets,
        reason: 'a countdown runs once revealed');
  });

  // Unknown support must read as unsupported. A burn that silently lands as a permanent message
  // is the one failure this feature cannot have, and with no read receipts the sender would never
  // find out — so the refusal has to happen before anything is sent.
  testWidgets('long-pressing send refuses to offer burn when the peer has not announced support',
      (tester) async {
    final (core, contact) = await pumpChat(tester);
    core.peerSupportsBurn(null);
    await tester.enterText(find.byType(TextField).last, 'secret');
    await tester.pump();

    await tester.longPress(find.byIcon(Icons.send));
    await tester.pumpAndSettle();

    expect(find.textContaining("can't burn messages"), findsOneWidget);
    expect(core.sent, isEmpty, reason: 'nothing may be sent when burn is unavailable');
    expect(find.text('10 seconds'), findsNothing,
        reason: 'the duration menu must not open at all');
    await settleScroll(tester);
  });

  testWidgets('long-pressing send offers durations and sends immediately on choosing one',
      (tester) async {
    final (core, contact) = await pumpChat(tester);
    core.peerSupportsBurn(true);
    await tester.enterText(find.byType(TextField).last, 'secret');
    await tester.pump();

    await tester.longPress(find.byIcon(Icons.send));
    await tester.pumpAndSettle();

    // The caveat sits in the menu, at the moment of choosing — not buried in settings.
    expect(find.textContaining('screenshot'), findsOneWidget);

    await tester.tap(find.text('30 seconds'));
    await tester.pumpAndSettle();

    expect(core.sent, [('secret', 30)]);
  });

  // The one residual honesty gap in this feature, and it must be visible at the moment of
  // choosing. With server storage on, a relay copy outlives the burn by up to 24h and CANNOT be
  // recalled — recall is sender-driven and there is deliberately no read receipt to trigger it.
  testWidgets('the burn menu discloses the server-storage copy, but only when it applies',
      (tester) async {
    final (core, contact) = await pumpChat(tester);
    core.peerSupportsBurn(true);
    await tester.enterText(find.byType(TextField).last, 'secret');
    await tester.pump();

    // Off: no claim either way.
    await tester.longPress(find.byIcon(Icons.send));
    await tester.pumpAndSettle();
    expect(find.textContaining('sits on the relay'), findsNothing);
    await tester.tapAt(const Offset(10, 10)); // dismiss
    await tester.pumpAndSettle();

    // On: said plainly, in the menu, before anything is sent.
    core.setServerStorage(true);
    await tester.pump();
    await tester.longPress(find.byIcon(Icons.send));
    await tester.pumpAndSettle();
    expect(find.textContaining('sits on the relay'), findsOneWidget);

    await tester.tapAt(const Offset(10, 10));
    await tester.pumpAndSettle();
    await settleScroll(tester);
  });

  // A burn attachment carries no thumbnail by construction (the core never sends one), so the
  // hidden state has nothing to leak — the tile shows only kind and size, and the sealed bytes
  // are not decrypted until the message is revealed.
  testWidgets('a hidden burn attachment shows a tile, not the picture', (tester) async {
    final (core, contact) = await pumpChat(tester);
    core.peerSupportsBurn(true);
    core.receiveBurnMedia(contact.id, 30);
    await tester.pump();

    expect(find.byType(Image), findsNothing,
        reason: 'no attachment may be rendered before it is revealed');
    expect(find.text('Tap to reveal'), findsOneWidget);
    expect(find.byIcon(Icons.image_outlined), findsOneWidget,
        reason: 'the tile says what kind of attachment is waiting, and nothing more');

    await settleScroll(tester);
  });

  // An attachment has no msgId — the core names it by transferId. Revealing it by msgId sent an
  // empty id, matched nothing, and left every burn attachment blurred until its 24h horizon.
  testWidgets('revealing a burn attachment names it by its transfer id', (tester) async {
    final (core, contact) = await pumpChat(tester);
    core.peerSupportsBurn(true);
    core.receiveBurnMedia(contact.id, 30);
    await tester.pump();

    await tester.tap(find.text('Tap to reveal'));
    await tester.pump();

    expect(core.viewed, ['bm-1']);
    expect(core.messagesFor(contact.id).last.isBurning, isTrue);
    await settleScroll(tester);
  });

  // A plain tap must never burn. The gesture split is what keeps this from being a mode, where a
  // forgotten toggle sends the wrong kind of message in either direction.
  testWidgets('a plain tap on send is always an ordinary message', (tester) async {
    final (core, contact) = await pumpChat(tester);
    core.peerSupportsBurn(true);
    await tester.enterText(find.byType(TextField).last, 'ordinary');
    await tester.pump();

    await tester.tap(find.byIcon(Icons.send));
    await tester.pumpAndSettle();

    expect(core.sent, isEmpty, reason: 'tap must not route through the burn path');
    expect(
      core.messagesFor(contact.id).any((m) => m.text == 'ordinary' && m.burnSecs == 0),
      isTrue,
    );
    await settleScroll(tester);
  });
}
