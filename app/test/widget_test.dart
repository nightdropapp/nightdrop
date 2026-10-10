import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';

void main() {
  testWidgets('onboarding -> create identity -> chat list', (tester) async {
    await tester.pumpWidget(NightdropApp(core: MockNightdropCore()));

    // Onboarding is shown first.
    expect(find.text('Create my identity'), findsOneWidget);

    await tester.tap(find.text('Create my identity'));
    await tester.pumpAndSettle();

    // The setup step comes first; its button creates the identity.
    expect(find.text('Before you start'), findsOneWidget);
    await tester.scrollUntilVisible(find.byKey(const Key('setup-create')), 200);
    await tester.tap(find.byKey(const Key('setup-create')));
    await tester.pumpAndSettle();

    // After creating an identity we land on the (empty) chat list.
    expect(find.text('Chats'), findsOneWidget);
    expect(find.text('New chat'), findsOneWidget);
  });
}
