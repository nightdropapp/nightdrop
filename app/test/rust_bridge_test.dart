@TestOn('vm')
library;

import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/core/rust_nightdrop_core.dart';
import 'package:night_drop/src/rust/frb_generated.dart';

/// Exercises the real Rust security core through the flutter_rust_bridge bindings by
/// loading the built `libnightdrop.so` directly (no GUI / GTK needed). Run after
/// `cargo build -p nightdrop`.
void main() {
  // Needed by the wipe test's platform-channel mocks; harmless for the rest.
  TestWidgetsFlutterBinding.ensureInitialized();

  setUpAll(() async {
    final root = Directory.current.parent.path; // app/ -> repo root
    final lib = '$root/target/debug/libnightdrop.so';
    if (!File(lib).existsSync()) {
      fail('Build the core first: `cargo build -p nightdrop` (missing $lib)');
    }
    await RustLib.init(externalLibrary: ExternalLibrary.open(lib));
  });

  test('onboarding creates a real identity', () async {
    final core = RustNightdropCore();
    expect(core.identity, isNull);
    await core.createIdentity();
    expect(core.identity, isNotNull);
    expect(core.identity!.id, isNotEmpty);
  });

  test('loopback chat round-trips through real Olm encryption', () async {
    final core = RustNightdropCore();
    await core.createIdentity();

    final contact = await core.joinWithShortCode('4-cedar-lantern-river');
    expect(core.contacts, hasLength(1));
    expect(contact.theirName, 'Anon');

    await core.sendMessage(contact.id, 'hello');
    final messages = core.messagesFor(contact.id);
    expect(messages, hasLength(2));
    expect(messages[0].fromMe, isTrue);
    expect(messages[0].text, 'hello');
    expect(messages[1].fromMe, isFalse);
    expect(messages[1].text, '(echo) hello');
  });

  test('pairing invite has a slot-secret-words code and a real QR payload', () async {
    final core = RustNightdropCore();
    await core.createIdentity();
    final invite = await core.createInvite();
    expect(invite.shortCode.split('-'), hasLength(4));
    expect(invite.qrPayload, startsWith('nightdrop://pair?addr='));
    expect(invite.qrPayload, contains('&ik='));
  });

  test('an invite surfaces a request that must be authorized before chatting', () async {
    final core = RustNightdropCore();
    await core.createIdentity();
    await core.createInvite();

    expect(core.incomingRequests, hasLength(1));
    expect(core.contacts, isEmpty);

    final requestId = core.incomingRequests.first.id;
    await core.authorize(requestId, true);
    expect(core.incomingRequests, isEmpty);
    expect(core.contacts, hasLength(1));

    await core.sendMessage(requestId, 'hi');
    expect(core.messagesFor(requestId).last.text, '(echo) hi');
  });

  // Found on a device, 2026-08-02. The wipe was one `try` around every deletion, with a silent
  // catch, and its FIRST statement was the keystore delete — which throws on Android. So the
  // whole wipe was skipped: the store key, the sealed onion identity, arti's state and the
  // authorized-client files all survived, while the app looked wiped because onboarding
  // overwrites the state file. The next identity then came up on the WIPED IDENTITY'S ONION
  // ADDRESS, which is the linkage a wipe exists to break.
  //
  // Deliberately drives the real logout() rather than a mock mirroring its deletion list: the
  // list was already mirrored in a test, and that test passed throughout the bug.
  test('a keystore delete that throws does not abort the rest of the wipe', () async {
    final binding = TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    final support = await Directory.systemTemp.createTemp('nd-wipe');
    addTearDown(() => support.deleteSync(recursive: true));

    binding.setMockMethodCallHandler(
      const MethodChannel('plugins.flutter.io/path_provider'),
      (call) async => support.path,
    );
    // What Android does, and the whole point of the test.
    binding.setMockMethodCallHandler(
      const MethodChannel('plugins.it_nomads.com/flutter_secure_storage'),
      (call) async => throw PlatformException(code: 'keystore'),
    );
    addTearDown(() {
      binding.setMockMethodCallHandler(
          const MethodChannel('plugins.flutter.io/path_provider'), null);
      binding.setMockMethodCallHandler(
          const MethodChannel('plugins.it_nomads.com/flutter_secure_storage'), null);
    });

    for (final name in [
      'nightdrop-state.bin',
      'onion-key.sealed',
      // Encrypted copies of the identity being destroyed: a failed launch preserves one, and
      // creating a new identity over an old one renames another aside. Deleting only the
      // original would make the wipe a rename.
      'nightdrop-state.bin.unreadable-123',
      'nightdrop-state.bin.replaced-456',
    ]) {
      File('${support.path}/$name').writeAsStringSync('x');
    }
    for (final name in ['arti-state', 'client-auth', 'nightdrop-media']) {
      Directory('${support.path}/$name').createSync();
      File('${support.path}/$name/f').writeAsStringSync('x');
    }

    await RustNightdropCore().logout();

    expect(support.listSync(), isEmpty,
        reason: 'one failing step must not skip the others — a half-wipe that reports '
            'success is worse than a wipe that fails loudly');
  });

  // The second half of the same device session. With the sealed-onion-key gate fixed, "set up a
  // new identity" still failed — "wrong key or corrupt store" — because the unreadable state file
  // was only ever COPIED to a sidecar, and the core restores from `persistPath` whenever that path
  // exists. So the one way off the load-error screen was blocked by the file that put the user
  // there. The bytes must survive the move: they may be the only copy of that identity.
  test('creating a new identity moves an old state file aside rather than restoring it', () async {
    final binding = TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    final support = await Directory.systemTemp.createTemp('nd-new-id');
    addTearDown(() => support.deleteSync(recursive: true));
    binding.setMockMethodCallHandler(
      const MethodChannel('plugins.flutter.io/path_provider'),
      (call) async => support.path,
    );
    addTearDown(() => binding.setMockMethodCallHandler(
        const MethodChannel('plugins.flutter.io/path_provider'), null));

    final state = File('${support.path}/nightdrop-state.bin');
    state.writeAsStringSync('an identity that will not open');

    await RustNightdropCore().createIdentity();

    expect(state.existsSync(), isFalse,
        reason: 'left in place, the core restores from it instead of creating an identity');
    final aside = support
        .listSync()
        .where((f) => f.path.contains('nightdrop-state.bin.replaced-'))
        .toList();
    expect(aside, hasLength(1), reason: 'abandoned is not the same as destroyed');
    expect(File(aside.single.path).readAsStringSync(), 'an identity that will not open');
  });

  // NOTE: the FRB event stream (rust.subscribe()) is wired into RustNightdropCore and used by
  // the app to refresh on push events. A standalone stream assertion is omitted here
  // because an open FRB stream keeps the `flutter test` VM alive past teardown (a known
  // harness limitation), not a defect in the streaming code.
}
