import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/core/models.dart';
import 'package:night_drop/src/features/chat/chat_screen.dart';
import 'package:night_drop/src/features/home/home_screen.dart';
import 'package:night_drop/src/features/onboarding/setup_screen.dart';
import 'package:night_drop/src/features/pairing/pairing_screen.dart';
import 'package:night_drop/src/features/relays/my_relays_screen.dart';

/// `docs/design/optional-relay.md` §6–§7: the relay choice at setup, "My relays", and what direct
/// only turns off, each with its reason.

const _onion = 'abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion';

Widget _app(MockNightdropCore core, Widget home) => NightdropScope(
      core: core,
      child: MaterialApp(
        localizationsDelegates: AppLocalizations.localizationsDelegates,
        supportedLocales: AppLocalizations.supportedLocales,
        home: home,
      ),
    );

/// A page that opens [SetupScreen] and keeps what it returns.
class _SetupHost extends StatefulWidget {
  const _SetupHost();
  @override
  State<_SetupHost> createState() => _SetupHostState();
}

class _SetupHostState extends State<_SetupHost> {
  SetupChoices? choices;
  @override
  Widget build(BuildContext context) => Scaffold(
        body: TextButton(
          onPressed: () async {
            final c = await Navigator.of(context).push<SetupChoices>(
                MaterialPageRoute(builder: (_) => const SetupScreen()));
            setState(() => choices = c);
          },
          child: const Text('open'),
        ),
      );
}

Future<SetupChoices?> _runSetup(
    WidgetTester tester, Future<void> Function() choose) async {
  await tester.pumpWidget(_app(MockNightdropCore(), const _SetupHost()));
  await tester.tap(find.text('open'));
  await tester.pumpAndSettle();
  await choose();
  await tester.scrollUntilVisible(find.byKey(const Key('setup-create')), 200,
      scrollable: find.byType(Scrollable).first);
  await tester.tap(find.byKey(const Key('setup-create')));
  await tester.pumpAndSettle();
  return tester.state<_SetupHostState>(find.byType(_SetupHost)).choices;
}

void main() {
  testWidgets('setup defaults to the Night Drop relay', (tester) async {
    final choices = await _runSetup(tester, () async {});
    expect(choices!.relays, [const RelayEntry.nightDrop()]);
  });

  testWidgets('direct only at setup hides cover traffic and starts with no relay',
      (tester) async {
    final choices = await _runSetup(tester, () async {
      await tester.tap(find.byKey(const Key('setup-relay-direct')));
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('setup-cover')), findsNothing,
          reason: 'no relay mailbox to cover');
    });
    expect(choices!.relays, isEmpty);
    expect(choices.coverTraffic, isFalse);
  });

  testWidgets('my own relay at setup takes only an onion', (tester) async {
    await tester.pumpWidget(_app(MockNightdropCore(), const _SetupHost()));
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('setup-relay-own')));
    await tester.pumpAndSettle();
    await tester.enterText(
        find.byKey(const Key('setup-relay-address')), '10.0.0.5:9080');
    await tester.scrollUntilVisible(find.byKey(const Key('setup-create')), 200,
      scrollable: find.byType(Scrollable).first);
    await tester.tap(find.byKey(const Key('setup-create')));
    await tester.pumpAndSettle();
    expect(find.textContaining("isn't an onion address"), findsOneWidget,
        reason: 'a plain address would bypass Tor');

    await tester.scrollUntilVisible(
        find.byKey(const Key('setup-relay-address')), -200,
        scrollable: find.byType(Scrollable).first);
    await tester.enterText(find.byKey(const Key('setup-relay-address')), _onion);
    await tester.scrollUntilVisible(find.byKey(const Key('setup-create')), 200,
      scrollable: find.byType(Scrollable).first);
    await tester.tap(find.byKey(const Key('setup-create')));
    await tester.pumpAndSettle();
    expect(
      tester.state<_SetupHostState>(find.byType(_SetupHost)).choices!.relays,
      [const RelayEntry.onion(_onion)],
    );
  });

  testWidgets('removing the last relay says what turns off, and Cancel keeps it',
      (tester) async {
    final core = MockNightdropCore();
    await tester.pumpWidget(_app(core, const MyRelaysScreen()));
    await tester.pumpAndSettle();
    expect(find.text('Night Drop relay'), findsOneWidget);

    Future<void> removeNightDrop() async {
      await tester.tap(find.byKey(const Key('relay-menu-nightdrop')));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Remove'));
      await tester.pumpAndSettle();
    }

    await removeNightDrop();
    expect(find.text('Remove your last relay?'), findsOneWidget);
    expect(find.textContaining('QR code only'), findsOneWidget);
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(await core.relayList(), [const RelayEntry.nightDrop()]);

    await removeNightDrop();
    await tester.tap(find.byKey(const Key('relays-confirm-direct')));
    await tester.pumpAndSettle();
    expect(await core.relayList(), isEmpty);
    expect((await core.relayFeatures()).directOnly, isTrue);
    expect(find.textContaining('No relays: direct only'), findsOneWidget);

    // And back again.
    await tester.tap(find.byKey(const Key('relays-add-nightdrop')));
    await tester.pumpAndSettle();
    expect(await core.relayList(), [const RelayEntry.nightDrop()]);
  });

  testWidgets('My relays adds an onion, refuses anything else, and stops at the cap',
      (tester) async {
    final core = MockNightdropCore();
    await tester.pumpWidget(_app(core, const MyRelaysScreen()));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('relays-field')), 'example.com:80');
    await tester.tap(find.byKey(const Key('relays-add')));
    await tester.pumpAndSettle();
    expect(find.textContaining("isn't an onion address"), findsOneWidget);
    expect(await core.relayList(), [const RelayEntry.nightDrop()]);

    for (final c in ['a', 'b', 'c']) {
      await tester.enterText(find.byKey(const Key('relays-field')),
          _onion.replaceFirst('a', c, 0).replaceFirst('abc', '${c}bc'));
      await tester.tap(find.byKey(const Key('relays-add')));
      await tester.pumpAndSettle();
    }
    expect((await core.relayList()).length, 4);
    expect(find.byKey(const Key('relays-field')), findsNothing);
    expect(find.text('You can have up to 4 relays.'), findsOneWidget);
  });

  testWidgets('on direct only the invite says why there is no short code',
      (tester) async {
    final core = MockNightdropCore();
    await tester.runAsync(() => core.createIdentity(relays: const []));
    await tester.pumpWidget(_app(core, const PairingScreen()));
    // The mock's invite takes a (fake-clock) moment; a spinner runs meanwhile, so no settle.
    await tester.pump(const Duration(milliseconds: 300));
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.textContaining('Short codes need a relay'), findsOneWidget);
  });

  testWidgets('a direct-only contact is named in the chat, and storage says why it is off',
      (tester) async {
    final core = MockNightdropCore();
    late final String contactId;
    await tester.runAsync(() async {
      await core.createIdentity();
      contactId = (await core.joinWithShortCode('4-cedar-lantern-river')).id;
    });
    final contact = core.contacts.single
      ..peerRelayState = 'direct'
      ..serverStorageAvailable = false;
    await tester.pumpWidget(_app(core, ChatScreen(contactId: contactId)));
    await tester.pumpAndSettle();
    expect(
        find.text('${contact.displayName} receives only while online'), findsOneWidget);

    await tester.tap(find.byIcon(Icons.cloud_off));
    await tester.pump();
    expect(find.textContaining("doesn't use one"), findsOneWidget);
    expect(contact.remoteStorage, isFalse);
  });

  testWidgets('the home screen says when this device is on direct only', (tester) async {
    for (final direct in [false, true]) {
      final core = MockNightdropCore();
      await tester.runAsync(
          () => core.createIdentity(relays: direct ? const [] : null));
      await tester.pumpWidget(_app(core, const HomeScreen()));
      await tester.pump();
      await tester.pump();
      expect(find.textContaining('Direct only: you receive messages only'),
          direct ? findsOneWidget : findsNothing);
    }
  });
}
