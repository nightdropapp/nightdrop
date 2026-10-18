import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';

/// Records the cover-traffic toggle so the opt-in flow can be asserted.
class _CoverCore extends MockNightdropCore {
  bool cover = false;

  @override
  Future<bool> coverTrafficEnabled() async => cover;

  @override
  Future<void> setCoverTraffic(bool enabled) async {
    cover = enabled;
    notifyListeners();
  }
}

void main() {
  Future<_CoverCore> pumpHome(WidgetTester tester) async {
    final core = _CoverCore();
    await tester.runAsync(core.createIdentity);
    await tester.pumpWidget(NightdropApp(core: core));
    await tester.pump();
    return core;
  }

  Future<void> openCoverDialog(WidgetTester tester) async {
    await tester.tap(find.byIcon(Icons.more_vert));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Cover traffic').last);
    await tester.pumpAndSettle();
  }

  // #4 is opt-in: it costs battery and bandwidth continuously, and relay operators carry the load.
  testWidgets('cover traffic is off until deliberately turned on', (tester) async {
    final core = await pumpHome(tester);
    expect(core.cover, isFalse);

    await openCoverDialog(tester);
    await tester.tap(find.text('Turn on'));
    await tester.pumpAndSettle();

    expect(core.cover, isTrue);
    expect(find.text('Cover traffic is on.'), findsOneWidget);
  });

  // The limit has to be in front of the user at the moment they choose. This is chaff, not
  // constant-rate transmission — someone who believes it makes them untrackable is worse off than
  // someone who knows it only raises the cost. See docs/design/cover-traffic.md §4.
  testWidgets('the dialog states what it does not protect against', (tester) async {
    await pumpHome(tester);
    await openCoverDialog(tester);

    expect(find.textContaining('does not stop it'), findsOneWidget);
    expect(find.textContaining('costs battery and data'), findsOneWidget);
  });

  // Offered once at setup, before the identity exists (optional-relay.md §7): off unless chosen,
  // with the same limit stated, and applied before the identity is created.
  testWidgets('setup offers cover traffic before the identity is created', (tester) async {
    final core = _CoverCore();
    await tester.pumpWidget(NightdropApp(core: core));
    await tester.tap(find.text('Create my identity'));
    await tester.pumpAndSettle();

    expect(find.text('Before you start'), findsOneWidget);
    expect(find.textContaining('does not stop it'), findsOneWidget);
    expect(core.identity, isNull, reason: 'nothing is created until the user continues');

    // On by default (2026-10-18: security settings start on); turning it off is the user's choice
    // and is saved as an explicit "no".
    await tester.scrollUntilVisible(find.byKey(const Key('setup-cover')), 200);
    await tester.tap(find.byKey(const Key('setup-cover')));
    await tester.pump();
    await tester.scrollUntilVisible(find.byKey(const Key('setup-create')), 200);
    await tester.tap(find.byKey(const Key('setup-create')));
    await tester.pumpAndSettle();

    expect(core.cover, isFalse);
    expect(core.identity, isNotNull);
    expect(find.text('Chats'), findsOneWidget);
  });

  testWidgets('setup turns cover traffic on by default', (tester) async {
    final core = _CoverCore();
    await tester.pumpWidget(NightdropApp(core: core));
    await tester.tap(find.text('Create my identity'));
    await tester.pumpAndSettle();
    await tester.scrollUntilVisible(find.byKey(const Key('setup-create')), 200);
    await tester.tap(find.byKey(const Key('setup-create')));
    await tester.pumpAndSettle();

    expect(core.cover, isTrue);
    expect(core.identity, isNotNull);
  });

  testWidgets('backing out of setup creates nothing', (tester) async {
    final core = _CoverCore();
    await tester.pumpWidget(NightdropApp(core: core));
    await tester.tap(find.text('Create my identity'));
    await tester.pumpAndSettle();
    await tester.pageBack();
    await tester.pumpAndSettle();

    expect(core.identity, isNull);
    expect(find.text('Create my identity'), findsOneWidget);
  });
}
