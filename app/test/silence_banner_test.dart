import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/models.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/features/chat/chat_screen.dart';

/// A core whose single contact has a settable "last heard from" reading.
class _SilenceCore extends MockNightdropCore {
  void setLastSeen(String contactId, int secs) {
    for (final c in contacts) {
      if (c.id == contactId) c.lastSeenSecs = secs;
    }
    notifyListeners();
  }
}

int _daysAgo(int days) =>
    DateTime.now().subtract(Duration(days: days)).millisecondsSinceEpoch ~/ 1000;

void main() {
  Future<(_SilenceCore, Contact)> pumpChat(WidgetTester tester, int lastSeenSecs) async {
    final core = _SilenceCore();
    late final Contact contact;
    await tester.runAsync(() async {
      await core.createIdentity();
      contact = await core.joinWithShortCode('4-cedar-lantern-river');
    });
    core.setLastSeen(contact.id, lastSeenSecs);
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

  // This banner is what stands in for a duress-wipe notice, which cannot exist (the wipe has no
  // store key, so nothing can be authenticated to a peer — see docs/design/duress-wipe.md §5).
  testWidgets('a long silence is surfaced in the chat', (tester) async {
    await pumpChat(tester, _daysAgo(30));
    expect(find.textContaining('No sign of this person'), findsOneWidget);
    expect(find.textContaining('30 days'), findsOneWidget);
  });

  // The false-positive case that matters: people go quiet for ordinary reasons, and crying wolf
  // would train users to ignore the one time it means something.
  testWidgets('ordinary quiet does not raise it', (tester) async {
    await pumpChat(tester, _daysAgo(3));
    expect(find.textContaining('No sign of this person'), findsNothing);
  });

  // No reading yet (a state file from before this feature, say) must read as *unknown*, never as
  // silent — an unknown that renders as "they're gone" is worse than showing nothing.
  testWidgets('no reading is treated as unknown, not as silence', (tester) async {
    await pumpChat(tester, 0);
    expect(find.textContaining('No sign of this person'), findsNothing);
  });

  // The wording must stay observational. A banner that named a cause would be a record on this
  // device that the other person used an anti-forensics feature — precisely what the duress design
  // refuses to broadcast, and it would be a guess regardless: a wipe, a seized phone, a lost phone
  // and a holiday are indistinguishable from here.
  testWidgets('it reports silence without claiming a cause', (tester) async {
    await pumpChat(tester, _daysAgo(30));
    final banner = tester.widget<Text>(find.textContaining('No sign of this person'));
    final text = banner.data!.toLowerCase();
    for (final claim in ['wipe', 'deleted', 'destroyed', 'seized', 'blocked']) {
      expect(text.contains(claim), isFalse, reason: 'must not claim a cause: "$claim"');
    }
  });
}
