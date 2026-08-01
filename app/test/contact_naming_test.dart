import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/models.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/features/chat/chat_screen.dart';

/// A core with two contacts who both left themselves as "Anon" — the reported problem.
class _TwoAnonsCore extends MockNightdropCore {
  Future<List<Contact>> pairTwo() async {
    await createIdentity();
    final a = await joinWithShortCode('4-cedar-lantern-river');
    final b = await joinWithShortCode('5-ember-harbor-quill');
    a.identityTag = 'K7QF2M';
    b.identityTag = 'X3TWB9';
    return [a, b];
  }
}

void main() {
  Future<_TwoAnonsCore> pumpList(WidgetTester tester) async {
    final core = _TwoAnonsCore();
    await tester.runAsync(core.pairTwo);
    await tester.pumpWidget(NightdropApp(core: core));
    await tester.pump();
    return core;
  }

  // The reported failure: a list of identical "Anon"s, with no way to tell which chat is which.
  // Sending to the wrong person is a confidentiality failure produced by the UI.
  testWidgets('two unnamed contacts are distinguishable by their identity tag', (tester) async {
    await pumpList(tester);

    expect(find.text('Anon'), findsNWidgets(2));
    expect(find.text('K7QF2M'), findsOneWidget);
    expect(find.text('X3TWB9'), findsOneWidget);
  });

  // A nickname is yours: it replaces what they call themselves, and the tag steps aside because
  // you have now vouched for who this is.
  testWidgets('a local nickname replaces the name and hides the tag', (tester) async {
    final core = await pumpList(tester);
    final contacts = core.contacts;

    await core.setLocalName(contacts.first.id, 'Dana from the shop');
    await tester.pump();

    expect(find.text('Dana from the shop'), findsOneWidget);
    expect(find.text('K7QF2M'), findsNothing, reason: 'named contacts need no tag');
    // The other one is untouched — naming is per contact, and local.
    expect(find.text('X3TWB9'), findsOneWidget);
  });

  // The tag must never read as a name, or users will treat "same tag = same person" as
  // verification. It is rendered monospace and beside the name, never replacing it.
  testWidgets('the tag is styled as an identifier, not a name', (tester) async {
    await pumpList(tester);

    final tag = tester.widget<Text>(find.text('K7QF2M'));
    expect(tag.style?.fontFamily, 'monospace');
    expect(find.byType(IdentityTag), findsNWidgets(2));
  });
}
