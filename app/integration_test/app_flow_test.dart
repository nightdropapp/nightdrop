// Integration test driving the FULL app widget tree against the REAL Rust core
// (RustNightdropCore over flutter_rust_bridge), not the mock.
//
// Run on a device/emulator, or on Linux desktop:
//   flutter test integration_test -d linux        # needs a display, or `xvfb-run`
// On a packaged build cargokit bundles libnightdrop; here we fall back to the dev build.

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:ghost_chat/src/app.dart';
import 'package:ghost_chat/src/core/rust_nightdrop_core.dart';
import 'package:ghost_chat/src/rust/frb_generated.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  setUpAll(() async {
    final dev = '${Directory.current.parent.path}/target/debug/libnightdrop.so';
    if (File(dev).existsSync()) {
      await RustLib.init(externalLibrary: ExternalLibrary.open(dev));
    } else {
      await RustLib.init(); // cargokit-bundled lib on a packaged build
    }
  });

  testWidgets('onboarding -> create identity -> approve invite -> chat', (tester) async {
    await tester.pumpWidget(GhostApp(core: RustNightdropCore()));
    await tester.pumpAndSettle();

    // Onboarding -> create the anonymous identity.
    await tester.tap(find.text('Create my identity'));
    await tester.pumpAndSettle();
    expect(find.text('Chats'), findsOneWidget);

    // Open pairing: the Invite tab creates an invite, which (in the demo) produces a
    // pending request from a simulated peer.
    await tester.tap(find.text('New chat'));
    await tester.pumpAndSettle();
    await tester.pageBack();
    await tester.pumpAndSettle();

    // Approve the incoming request -> it becomes a contact.
    expect(find.text('Chat request'), findsOneWidget);
    await tester.tap(find.byTooltip('Approve'));
    await tester.pumpAndSettle();
    expect(find.text('Ghosty'), findsWidgets);

    // Open the chat and send a message; the peer echoes it back.
    await tester.tap(find.text('Ghosty').first);
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).last, 'hello there');
    await tester.testTextInput.receiveAction(TextInputAction.send);
    await tester.pumpAndSettle();
    expect(find.text('hello there'), findsOneWidget);
    expect(find.text('(echo) hello there'), findsOneWidget);
  });
}

