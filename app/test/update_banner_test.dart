import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';

/// Lets a test say what the onion site reported, without any network.
class _UpdateCore extends MockNightdropCore {
  String? version;
  int checks = 0;
  int downloads = 0;
  String? hidden;

  @override
  String? get updateAvailable => version;

  bool reachable = true;

  @override
  Future<void> maybeCheckForUpdate() async {
    checks++;
  }

  @override
  Future<bool> checkForUpdateNow() async {
    checks++;
    return reachable;
  }

  @override
  Future<String?> downloadUpdate() async {
    downloads++;
    return '/tmp/NightDrop-update.apk';
  }

  @override
  Future<void> hideUpdateBanner() async {
    hidden = version;
    version = null;
    notifyListeners();
  }

  void report(String? v) {
    version = v;
    notifyListeners();
  }
}

void main() {
  Future<_UpdateCore> pumpHome(WidgetTester tester) async {
    final core = _UpdateCore();
    await tester.runAsync(core.createIdentity);
    await tester.pumpWidget(NightdropApp(core: core));
    await tester.pump();
    return core;
  }

  testWidgets('nothing is shown while this build is current', (tester) async {
    final core = await pumpHome(tester);
    // The common case by far. An update banner that is present-but-empty, or that appears while
    // the check is still running, trains people to ignore it — so there must be no widget at all.
    expect(core.updateAvailable, isNull);
    expect(find.byIcon(Icons.system_update_alt), findsNothing);
  });

  testWidgets('a newer release is named, and the banner clears itself', (tester) async {
    final core = await pumpHome(tester);

    core.report('0.1.18');
    await tester.pumpAndSettle();
    expect(find.byIcon(Icons.system_update_alt), findsOneWidget);
    // The version must be stated. "An update is available" gives the user nothing to check
    // against what they are running.
    expect(find.textContaining('0.1.18'), findsWidgets);

    // Once the running build catches up the banner disappears on its own. It is deliberately not
    // dismissible: there is nothing to dismiss, and a dismissed security notice is a lost one.
    core.report(null);
    await tester.pumpAndSettle();
    expect(find.byIcon(Icons.system_update_alt), findsNothing);
  });

  testWidgets('Hide dismisses without downloading, and is scoped to that version', (tester) async {
    final core = await pumpHome(tester);
    core.report('0.1.18');
    await tester.pumpAndSettle();

    await tester.tap(find.text('Hide'));
    await tester.pumpAndSettle();

    // Hiding must not be mistaken for "get it": the whole point is the user is not ready.
    expect(core.downloads, 0, reason: 'Hide must never start a download');
    expect(core.hidden, '0.1.18');
    expect(find.byIcon(Icons.system_update_alt), findsNothing);

    // A LATER release must still be announced — otherwise one dismissal silences the notice
    // that carries the next security fix.
    core.report('0.1.19');
    await tester.pumpAndSettle();
    expect(find.byIcon(Icons.system_update_alt), findsOneWidget);
  });

  testWidgets('tapping the banner downloads, and nothing installs it', (tester) async {
    final core = await pumpHome(tester);
    core.report('0.1.18');
    await tester.pumpAndSettle();

    await tester.tap(find.byIcon(Icons.system_update_alt));
    await tester.pumpAndSettle();
    expect(core.downloads, 1);

    // The app hands over a verified file and stops. It never installs: that would be a
    // remote-code path into a security tool, and Android's own signature check is what decides
    // whether a build may replace this one.
    for (final word in ['Install', 'Update now']) {
      expect(find.text(word), findsNothing,
          reason: '"$word" implies the app acts on the user\'s behalf');
    }
  });

  testWidgets('a check that could not reach the site is never reported as up to date', (tester) async {
    // The bug this pins was live on hardware: the fetch timed out and the menu said "Night Drop is
    // up to date." That is a confident lie on the one screen where the user deliberately asked,
    // and it hides exactly the security fix the feature exists to surface.
    final core = await pumpHome(tester);
    core.reachable = false;

    await tester.tap(find.byIcon(Icons.more_vert));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Update app'));
    await tester.pumpAndSettle();

    // The result queues behind the "Checking…" snackbar. A real check takes seconds over Tor, by
    // which time the first has expired on its own; the mock answers instantly, so wait it out —
    // otherwise both expectations below pass vacuously against the wrong snackbar.
    await tester.pump(const Duration(seconds: 5));
    await tester.pumpAndSettle();

    expect(find.textContaining('up to date'), findsNothing,
        reason: 'a failed check must not claim the app is current');
    expect(find.textContaining('Could not reach'), findsOneWidget);
  });
}
