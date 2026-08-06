import 'package:flutter_test/flutter_test.dart';
import 'package:night_drop/src/core/background_delivery.dart';

/// The foreground-service decision, which is the part of item 3 that can be got wrong silently.
///
/// A download over Tor runs for minutes. If the service is not up, Doze and App Standby may
/// freeze the process partway through and the user comes back to a transfer that stopped — with
/// nothing on screen to say so.
void main() {
  bool run({
    bool foreground = false,
    bool hasIdentity = true,
    bool optedIn = false,
    int holds = 0,
  }) =>
      BackgroundDelivery.shouldRun(
        foreground: foreground,
        hasIdentity: hasIdentity,
        optedIn: optedIn,
        holds: holds,
      );

  test('a download keeps the service up even when the user returns to the app', () {
    // The regression this exists for. Returning to the foreground stops the service — so someone
    // who reopened Night Drop to watch the progress bar would have killed the thing protecting
    // their download.
    expect(run(foreground: true, holds: 1), isTrue);
    expect(run(foreground: false, holds: 1), isTrue);
  });

  test('a download keeps the service up without the background-delivery opt-in', () {
    // Declining passive message delivery is not declining to finish a download you just asked
    // for. Tying the two together would leave the service off for exactly the users who never
    // opted in — most of them.
    expect(run(optedIn: false, holds: 1), isTrue);
    // And an identity is beside the point: this is work the user started, not delivery for an
    // account.
    expect(run(hasIdentity: false, optedIn: false, holds: 1), isTrue);
  });

  test('with no download running, the opt-in alone decides', () {
    expect(run(foreground: false, optedIn: true), isTrue);
    expect(run(foreground: false, optedIn: false), isFalse);
    // Foreground needs no service: the UI is polling directly, and a persistent notification
    // while the user is looking at the app is noise.
    expect(run(foreground: true, optedIn: true), isFalse);
    // Nothing to deliver without an identity.
    expect(run(foreground: false, hasIdentity: false, optedIn: true), isFalse);
  });

  test('the service goes away once the last download finishes', () {
    // Holds are counted, not a flag: two overlapping jobs must not have the first one to finish
    // drop the service out from under the second.
    expect(run(foreground: true, holds: 2), isTrue);
    expect(run(foreground: true, holds: 1), isTrue);
    expect(run(foreground: true, holds: 0), isFalse);
  });
}
