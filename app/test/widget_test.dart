import 'package:flutter_test/flutter_test.dart';
import 'package:ghost_chat/src/app.dart';
import 'package:ghost_chat/src/core/mock_nightdrop_core.dart';

void main() {
  testWidgets('onboarding -> create identity -> chat list', (tester) async {
    await tester.pumpWidget(GhostApp(core: MockNightdropCore()));

    // Onboarding is shown first.
    expect(find.text('Create my identity'), findsOneWidget);

    await tester.tap(find.text('Create my identity'));
    await tester.pumpAndSettle();

    // After creating an identity we land on the (empty) chat list.
    expect(find.text('Chats'), findsOneWidget);
    expect(find.text('New chat'), findsOneWidget);
  });
}
