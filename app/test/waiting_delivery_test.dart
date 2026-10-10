import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/core/models.dart';
import 'package:night_drop/src/features/chat/chat_screen.dart';

/// A core whose one chat holds a single outgoing message in a chosen delivery state.
class _OneMessageCore extends MockNightdropCore {
  _OneMessageCore(this.delivery, this.at);
  final String delivery;
  final DateTime at;

  @override
  List<Message> messagesFor(String contactId) => [
        Message(
          id: 'm1',
          contactId: contactId,
          text: 'held for you',
          fromMe: true,
          at: at,
          msgId: 'm1',
          delivery: delivery,
        ),
      ];
}

void main() {
  // `docs/design/optional-relay.md` §4: with no relay between the two people, a message to an
  // offline contact waits on the sender's device, and the bubble says so by name.
  testWidgets('a message held on this device says who it is waiting for', (tester) async {
    final core = _OneMessageCore('waiting', DateTime.now());
    late final String contactId;
    await tester.runAsync(() async {
      await core.createIdentity();
      contactId = (await core.joinWithShortCode('4-cedar-lantern-river')).id;
    });
    final name = core.contacts.single.displayName;
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
    expect(find.text('Waiting for $name to come online'), findsOneWidget);
    expect(find.byIcon(Icons.hourglass_empty), findsOneWidget);
  });

  // They have not seen it, so it stays editable past the 15-minute window, as a queued one does.
  test('a waiting message stays editable at any age', () {
    final old = DateTime.now().subtract(const Duration(days: 3));
    Message msg(String delivery) => Message(
          id: 'm',
          contactId: 'c',
          text: 't',
          fromMe: true,
          at: old,
          msgId: 'm',
          delivery: delivery,
        );
    expect(msg('waiting').canEdit, isTrue);
    expect(msg('queued').canEdit, isTrue);
    expect(msg('delivered').canEdit, isFalse);
  });

  // A contact still on 0.1 cannot read what 0.2 sends: the chat says so by name, and each message
  // waiting for them says what it is waiting for.
  testWidgets('a contact on 0.1 is named in the chat, and messages wait for their update',
      (tester) async {
    final core = _OneMessageCore('update', DateTime.now());
    late final String contactId;
    await tester.runAsync(() async {
      await core.createIdentity();
      contactId = (await core.joinWithShortCode('4-cedar-lantern-river')).id;
    });
    final contact = core.contacts.single..peerOutdated = true;
    final name = contact.displayName;
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
    expect(find.text('$name needs to update Night Drop'), findsOneWidget);
    expect(find.text('Waiting for $name to update'), findsOneWidget);
    await tester.tap(find.text('$name needs to update Night Drop'));
    await tester.pump();
    expect(find.textContaining('go out by themselves once they update'), findsOneWidget);
  });
}
