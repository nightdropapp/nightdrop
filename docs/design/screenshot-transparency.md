# Design draft — Screenshot transparency (#1)

**Status:** 🟢 **implemented and verified on hardware** (2026-08-01), including delivery to an
offline peer via the relay; see §7 for what was exercised and §8 for what is still open.
**Relates to:** `ARCHITECTURE.md` §11 (authenticated control plane), the `SECURITY.md` entry on
detection limits, and the "Only you can control this" section of `website/limits.html`.

## 1. Problem

Everything else in Night Drop protects a message in transit and at rest. None of it survives the
other person pointing a camera at their screen, or pressing the screenshot key: at that instant a
plaintext copy exists somewhere we have no reach — outside disappearing timers, outside the 24h
remote-storage cap, outside `unsend`.

## 2. Why not just block screenshots

`FLAG_SECURE` set permanently would block them, and that was rejected:

* It doesn't work. Someone who wants a copy photographs the screen with a second phone, which **no
  API can ever detect**. Blocking converts a detectable act into an undetectable one.
* It breaks a legitimate need. People screenshot conversations to keep a receipt, to report abuse,
  to remember an address.

So screenshots are **allowed and made visible** instead. The user chose this framing directly:
"Allow screenshots, but show a message saying that a screenshot was taken."

The **Recents thumbnail** is the opposite case and *is* blocked — Android snapshots the window on
backgrounding with no user intent behind it, and that image sits in the task switcher for anyone who
opens it. `FLAG_SECURE` is therefore held only while backgrounded (`MainActivity.onPause` →
`onResume`), which is what makes deliberate screenshots keep working.

## 3. The signal

`Frame::Screenshot { from, message }`, appended at the **end** of the `Frame` enum because variant
order is wire-visible. It reuses the existing authenticated-control-frame machinery exactly as
`BackedUp` does: `authed_control` encrypts a fixed domain marker
(`MARK_SCREENSHOT = b"nightdrop/ctl/screenshot/v1"`) on the chat's ratchet, and `verify_control`
accepts only a frame that decrypts to precisely that marker.

Authentication is not decoration here. Unauthenticated, anyone who knew an identity key could
**manufacture distrust** between two people — a fabricated "they screenshotted your chat" is a
social attack with no technical countermeasure available to the victim. The domain separation also
stops a *cross-type splice*: a genuine `BackedUp` ciphertext from a real peer, relabelled as a
`Screenshot`, fails verification. That case is covered by a test, because it is the one an attacker
with a valid session would actually reach for.

**An event, not a state flag.** `BackedUp` sets `peer_backed_up` once and dedupes. Screenshots are
logged *every time*: "they screenshotted this once, months ago" and "they are screenshotting it
right now" are different facts, and collapsing them would destroy the informative part. Both sides
get a system message — the capturer sees `📸 You took a screenshot. The other person was told.`,
so nobody is surprised by what was disclosed on their behalf.

## 4. Detection, and what it cannot see

`Activity.ScreenCaptureCallback` (Android 14 / API 34) plus the `DETECT_SCREEN_CAPTURE` permission
— a *normal* permission, no prompt, and it conveys **no ability to read screen content**; it only
reports that a capture occurred. Registered in `onResume`, unregistered in `onPause`, since it fires
only while the activity is visible and leaving it registered leaks it.

It is blind to all of:

| Case | Reported? |
| --- | --- |
| Screenshot, Android 14+ | ✅ |
| Screenshot, Android ≤ 13 | ❌ |
| Screenshot, desktop | ❌ |
| Screen **recording** | ❌ |
| A camera pointed at the screen | ❌ — and always will be |

**Therefore: no notice is not evidence that nothing was captured.** This is stated in
`SECURITY.md` and on the public limits page, and the code comments repeat it, because it is exactly
the kind of feature users over-trust. `ScreenshotDetector.canDetect()` exists as the hook for any UI
that wants to be explicit about which world the device is in.

## 5. Wiring

`MainActivity` → `MethodChannel('app.nightdrop/screenshots')` → `ScreenshotDetector` →
`ChatScreen.initState` → `core.reportScreenshot(contactId)` → `Node::report_screenshot`.

Registration is **per open chat**, not global, and `dispose` clears it: a screenshot taken on the
chat list must not be attributed to whichever conversation the user was last looking at. Both
properties are covered by widget tests that drive the platform channel directly.

## 6. Delivery when the peer is offline

The peer being offline is the **normal** case, not an edge case: the capturer has no idea whether
the other client is running, and a notice that only lands when both sides happen to be online would
be silently useless most of the time. Three tiers, in order:

1. **Direct** — `deliver` dials the peer's onion. Deliberately tried first (unlike `delete_chat`,
   which is relay-first): "they are screenshotting this *right now*" is the informative case, and it
   shouldn't wait on the peer's next relay poll.
2. **Relay** — on a failed dial, the sealed frame is queued on the recipient's mailbox for their
   next drain, within the standard 24h window.
3. **Held for retry** — if *neither* path is reachable, the frame goes into `pending_control` (the
   same queue a chat-delete `Closed` uses) and the poller retries until a copy lands. Screenshotting
   on a phone with no signal is ordinary, and the local side has already told the user "the other
   person was told" — there is no user-visible resend to make good on that promise, so the core
   must keep it.

## 7. Verified on hardware (2026-08-01)

Galaxy S25, Android 16 (API 36), release build, against the dev relay and a Linux desktop peer:

* A hand-taken screenshot fires `ScreenCaptureCallback` → channel → `report_screenshot`, and both
  sides log it. **An adb-injected `KEYCODE_SYSRQ` does not fire the callback** — it produces a real
  screenshot with no notice, so it is useless for testing this and misleading if mistaken for a
  failure of the feature.
* With the peer's client **closed**: the direct dial failed after the 15s `PEER_DIAL_TIMEOUT`, the
  frame was queued on the relay (`deliver: queued on the relay for the peer to drain`), and the
  peer drained and rendered it ~4 minutes later on reopening — while its own onion was still
  publishing, which does not block an outbound drain.

## 8. Not yet done

* ~~**The Recents thumbnail was not re-checked on this build.**~~ Checked 2026-08-01, and it was
  **broken**: the card showed the conversation. `FLAG_SECURE` added in `onPause` does not blank a
  snapshot the system has already captured as the transition begins — the code asserted an ordering
  the device does not honour. Now fixed with `setRecentsScreenshotEnabled(false)` on API 33+, which
  stops the snapshot being taken at all and, unlike `FLAG_SECURE`, leaves deliberate screenshots
  working. Verified blank on the S25 with the task still present in the recents list. Below API 33
  the old approach remains as a documented best effort, not a guarantee.
* **`canDetect` is not surfaced in the UI.** The user is not currently told whether their *own*
  screenshots will be announced to their peer, which is a consent-relevant fact on Android ≤ 13.
* **No capability advertisement to the peer.** Considered and dropped for now: it would let a
  receiver know whether the other side *can* report screenshots, but it also tells a peer what OS
  version you run, which is a fingerprinting bit for a small privacy gain over simply documenting
  that the signal is never a guarantee.
