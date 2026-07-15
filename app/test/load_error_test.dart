import 'package:flutter_test/flutter_test.dart';
import 'package:ghost_chat/src/app.dart';
import 'package:ghost_chat/src/core/mock_nightdrop_core.dart';

/// A core that reports a launch-time load failure (saved data exists but wouldn't open), so we can
/// drive the _LoadErrorScreen path.
class _LoadErrorCore extends MockNightdropCore {
  bool _err = true;

  @override
  bool get loadError => _err;

  @override
  void dismissLoadError() {
    _err = false;
    notifyListeners();
  }

  @override
  Future<void> retryStart() async {
    _err = false;
    notifyListeners();
  }
}

void main() {
  // Guards the data-loss fix: when saved data can't be opened at launch, the app must show a
  // recovery screen (not silently drop to onboarding, which would overwrite the file). Dismissing
  // it deliberately then continues to onboarding.
  testWidgets('load failure shows recovery screen, not silent onboarding', (tester) async {
    await tester.pumpWidget(GhostApp(core: _LoadErrorCore()));
    await tester.pump(); // let _Root run start() in its post-frame callback

    expect(find.textContaining('open your saved'), findsOneWidget);
    // Must NOT have jumped straight to onboarding.
    expect(find.text('Create my identity'), findsNothing);

    await tester.tap(find.text('Set up a new identity or restore a backup'));
    await tester.pumpAndSettle();

    // Now onboarding is reached deliberately.
    expect(find.text('Create my identity'), findsOneWidget);
  });
}
