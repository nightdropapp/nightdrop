import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/l10n/app_localizations.dart';
import 'package:night_drop/src/app.dart';
import 'package:night_drop/src/core/models.dart';
import 'package:night_drop/src/core/mock_nightdrop_core.dart';
import 'package:night_drop/src/core/screenshot_detector.dart';
import 'package:night_drop/src/features/chat/chat_screen.dart';

/// Records what the platform screenshot callback caused, so the wiring can be asserted without a
/// real device.
class _RecordingCore extends MockNightdropCore {
  final List<String> reported = [];

  @override
  Future<void> reportScreenshot(String contactId) async {
    reported.add(contactId);
  }

  /// Stand in for a `Captures` frame arriving from the peer.
  void peerSaysCapturesSilent(bool? silent) {
    contacts.first.peerCapturesSilent = silent;
    notifyListeners();
  }
}

/// Pretend Android fired `ScreenCaptureCallback` — i.e. the user took a screenshot.
Future<void> _fireScreenshot() async {
  await TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
      .handlePlatformMessage(
    'app.nightdrop/screenshots',
    const StandardMethodCodec().encodeMethodCall(const MethodCall('screenshot')),
    (_) {},
  );
}

void main() {
  Future<(_RecordingCore, Contact)> pumpChat(WidgetTester tester) async {
    final core = _RecordingCore();
    late final Contact contact;
    await tester.runAsync(() async {
      await core.createIdentity();
      contact = await core.joinWithShortCode('4-cedar-lantern-river');
    });
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

  // #1: screenshots are allowed, but not silent — the open chat reports them so the core can log it
  // locally and tell the peer.
  testWidgets('a screenshot while a chat is open is reported for that chat', (tester) async {
    final (core, contact) = await pumpChat(tester);

    await _fireScreenshot();
    await tester.pump();

    expect(core.reported, [contact.id]);
  });

  // The honesty default: where the platform cannot report screenshots — desktop, Android below 14,
  // and this test host — `canDetect` must be false, so no UI can claim a protection that isn't
  // there. A silent capture is the normal case, not an error case.
  test('canDetect is false where the platform cannot report screenshots', () async {
    expect(ScreenshotDetector.supported, isFalse, reason: 'test host is not Android');
    expect(await ScreenshotDetector.canDetect(), isFalse);
  });

  // Attribution matters: after the chat closes, a screenshot taken on the chat list (or anywhere
  // else) must not be blamed on the conversation the user happened to be in last.
  testWidgets('a screenshot after the chat closes is not attributed to it', (tester) async {
    final (core, _) = await pumpChat(tester);

    // Replace the chat with a bare screen, disposing ChatScreen.
    await tester.pumpWidget(const MaterialApp(home: Scaffold()));
    await tester.pump();

    await _fireScreenshot();
    await tester.pump();

    expect(core.reported, isEmpty);
  });

  // The peer-capability banner (#1) is shown to the SENDER — the person deciding what to send —
  // because on a device that cannot report captures the peer's silence proves nothing.
  testWidgets('the chat warns when the peer cannot report screenshots', (tester) async {
    final (core, contact) = await pumpChat(tester);
    final l10n = await AppLocalizations.delegate.load(const Locale('en'));

    // Nothing said yet: unknown must render as nothing at all. Showing the warning here would cry
    // wolf; showing a reassurance would be the false guarantee the signal exists to remove.
    expect(core.contacts.first.peerCapturesSilent, isNull);
    expect(find.text(l10n.peerCapturesSilentBanner), findsNothing);

    core.peerSaysCapturesSilent(true);
    await tester.pump();
    expect(find.text(l10n.peerCapturesSilentBanner), findsOneWidget);

    // A peer that *can* report is the quiet case — no banner, since the ordinary notice covers it.
    core.peerSaysCapturesSilent(false);
    await tester.pump();
    expect(find.text(l10n.peerCapturesSilentBanner), findsNothing);

    expect(contact.id, isNotEmpty);
  });
}
