import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';

/// Records each lifecycle report as (focused, on screen).
class _LifecycleCore extends MockNightdropCore {
  final List<(bool, bool)> reports = [];

  @override
  void setLifecycle(bool foreground, {bool? visible}) =>
      reports.add((foreground, visible ?? foreground));
}

void main() {
  // Regression (2026-11-14): a desktop window behind another app is `inactive`. It was reported as
  // background, so relay checks slowed to every 5 minutes while the window was in plain view, and a
  // contact turning Nearby off took 5½ minutes to show.
  testWidgets('an unfocused window stays on the fast cadence but still notifies', (tester) async {
    final core = _LifecycleCore();
    await tester.pumpWidget(NightdropApp(core: core));
    await tester.pump();

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    await tester.pump();
    expect(core.reports.last, (false, true),
        reason: 'not focused (so new messages notify), but on screen (so the cadence stays fast)');

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    await tester.pump();
    expect(core.reports.last, (false, false), reason: 'minimised or covered: slow cadence');

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump();
    expect(core.reports.last, (true, true));
  });
}
