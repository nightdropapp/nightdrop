# Design — Per-message burn (blur, reveal, countdown)

**Status:** implemented for text and media 2026-09-23 (§7 records what shipped and what did not).
**Relates to:** the existing per-chat disappearing timer (`Frame::Disappearing`),
`screenshot-transparency.md`, `SECURITY.md`.

## 1. What this is, and how it differs from what exists

Night Drop already has **per-chat** disappearing messages: `setDisappearing(contactId, secs)`,
synced over the session by `Frame::Disappearing` so both devices expire on the same horizon.

This is **per message**, and anchored differently:

* the message arrives **blurred** — present, unread, unreadable at a glance;
* the recipient taps to reveal;
* a circular timer runs from that moment, and when it completes the message is **deleted**;
* **unviewed** messages are deleted after **24 hours** regardless, with the UI showing that
  expiry approaching.

The first-view anchor is the point. A send- or delivery-anchored timer burns a message while it
sits in a 24-hour relay queue waiting for a recipient who is simply offline.

## 2. What it is not — and the UI must say so

**This cannot be enforced.** The recipient controls their device: a screenshot, a photograph of
the screen, or a modified client that never deletes. That is true of every implementation of this
feature anywhere, including Signal's; the difference is whether the product admits it.

`SECURITY.md` already takes this line on screenshots — deliberately not blocked, reporting
best-effort, *"the UI/website must never imply otherwise."* **The same sentence governs this
feature.** It is a courtesy against a careless or forgetful recipient, not a control over a
hostile one.

Hence **blur, not a padlock**. A lock icon claims security this does not have. A blur says only
"not shown yet", which is exactly true. The honest one-liner for the UI is roughly: *hidden until
opened, then deleted — a screenshot or a camera still works.*

Screenshot reporting pairs well here (burn a message, learn if it was captured) but does not
rescue the guarantee: it is blind on **Android below 14**, on **all desktop**, on **screen
recording**, and on **a camera pointed at the screen**.

## 3. Mechanics

**Timer is wall-clock and does not pause.** If it stopped while the app was backgrounded,
"30 seconds" would mean nothing — backgrounding would hold a message open indefinitely.

**Delete means delete**, from the encrypted store, not hide. The `Edit`/unsend path already
removes messages and is the precedent to follow.

**Recall the relay copy — and the conflict found when building it.** `QueuedReceipt` exists so an
edit or unsend can pull back every queued copy, and this note originally said a burn must do the
same. **It cannot.** Recall is sender-driven, and the sender does not know when the message burned
— because there are deliberately no read receipts (below). Honouring one would require the other.

What actually holds, and it is tidier than it sounds: the relay's TTL is 24h and the unviewed burn
horizon is also 24h, so an undelivered burn message expires from the mailbox at exactly the moment
it would have expired on the device. A burn message's maximum life is 24h everywhere.

The residual case is **opt-in server storage**: with it on, a relay copy can outlive the
recipient's burn by up to 24h. Bounded, equal to the message's own maximum life, and it needs
saying in the UI rather than being quietly true.

**No read receipt.** Deliberate: the sender learns nothing about when — or whether — it was
opened. It keeps the feature from leaking recipient behaviour, and there is no `Read`/`Viewed`
frame today (only `Delivered`, `Ack`, `Screenshot`), so none is introduced.

A consequence to accept: the sender cannot distinguish *read and burned* from *expired unread*
from *landed on a client too old to burn it*. The last of those is handled below, at send time,
which is the only moment it can be acted on.

## 4. Already handled

* **Notification previews.** `NotificationService.show('Night Drop', 'New message')` — the body
  never carries content. Verified 2026-09-22. If that ever changes, this feature breaks with it:
  a lock-screen preview displays the message in the place most likely to be read by someone else.
* **App-switcher thumbnail** is suppressed on Android 13+ via `setRecentsScreenshotEnabled`
  (best-effort on older releases — see `screenshot-transparency.md`).

## 5. The migration hazard — worse here than usual

A 0.1.22 recipient does not understand a per-message burn flag. It renders the message as an
ordinary one and **never deletes it**, while the sender believes it burned.

That is not an ordinary wire incompatibility. It manufactures a **false security belief**, and the
person harmed is the one who trusted the feature. So:

* **capability-gated**, announced per contact in the manner of `Frame::Captures`;
* **the sender is warned before sending** — "this contact's version cannot burn messages" — in the
  same shape as the existing "this device cannot report screenshots" notice.

Since there are no read receipts, that warning is the *only* moment the sender can learn the
feature will not work for this contact. It cannot be an afterthought.

## 6. Open

* Does burn apply to **media** as well as text? Media is stored as sealed sidecar files, so
  deletion has a second path to cover.
* Default burn duration, and whether the sender chooses it per message or per chat.
* Whether an expired-unviewed message leaves a tombstone ("a message expired") or nothing at all.
  Nothing is quieter; a tombstone is less confusing. Leaning tombstone, on the same reasoning that
  made `"sent"` a visible state rather than silence.

## 7. What shipped, and what did not (2026-09-23)

**Shipped**, core and UI: `Frame::Burn` as a separate variant (fails closed on an old build),
`Frame::Burns` capability gating with refusal at send time, the duration inside the ciphertext,
long-press/right-click send with an immediate-send duration menu, blurred placeholder bars, tap to
reveal, a wall-clock countdown anchored to first view and persisted across restarts, deletion from
the encrypted store, and the 24h unviewed horizon.

One thing came out stronger than designed: §2 called for the sender to be *warned* when a contact
cannot burn. The core **refuses** instead, which is the same information delivered at the same
moment with no way to ignore it.

**Not built:**

*(Media landed the same day — see below.)*
* **A tombstone on expiry.** An expired-unviewed message currently just vanishes. §6 leaned
  towards leaving a "a message expired" marker and that reasoning still stands; it was left out to
  keep the first version small.
* **The server-storage caveat in the UI** (see §3). Known, unstated to the user, and the one
  honesty gap in what shipped.

### 7.1 Media (added 2026-09-23)

`Frame::BurnMedia`, `send_burn_media`, and long-press / right-click on the **attach** button —
the same gesture as text, on the button that starts the same job.

The design question that decided the shape: **a burn attachment sends no thumbnail and no
`MediaIncoming` pre-signal.** Both exist to show a preview while a video's payload uploads, and a
preview of a message that has not been revealed gives away precisely what the feature withholds.
So `send_burn_media` never generates a thumbnail (the app does not even compute one) and skips the
pre-signal entirely. The recipient sees a tile carrying the kind and size, nothing more, and the
sealed bytes stay encrypted until they choose to open it. A test asserts no thumbnail crosses and
no placeholder is created.

This is stricter than the text case, where placeholder bars leak approximate length. An
attachment leaks its size, which it must — the bytes have to arrive.

Reveal works by `transfer_id` rather than `msg_id`, because attachments carry no `msg_id`;
`mark_burn_viewed` accepts either. Burning deletes the sealed file as well as the message, which
is asserted on disk rather than assumed.