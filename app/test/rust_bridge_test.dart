@TestOn('vm')
library;

import 'dart:io';

import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:ghost_chat/src/core/rust_nightdrop_core.dart';
import 'package:ghost_chat/src/rust/frb_generated.dart';

/// Exercises the real Rust security core through the flutter_rust_bridge bindings by
/// loading the built `libnightdrop.so` directly (no GUI / GTK needed). Run after
/// `cargo build -p nightdrop`.
void main() {
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

    final contact = await core.joinWithShortCode('4-ghost-lantern-river');
    expect(core.contacts, hasLength(1));
    expect(contact.theirName, 'Ghosty');

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

  // NOTE: the FRB event stream (rust.subscribe()) is wired into RustNightdropCore and used by
  // the app to refresh on push events. A standalone stream assertion is omitted here
  // because an open FRB stream keeps the `flutter test` VM alive past teardown (a known
  // harness limitation), not a defect in the streaming code.
}
