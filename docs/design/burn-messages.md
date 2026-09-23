# Design — Per-message burn (blur, reveal, countdown)

**Status:** design agreed 2026-09-22, not implemented.
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

**Recall the relay copy.** `QueuedReceipt` exists so an edit or unsend can pull back every queued
copy. A burn message must do the same once delivered, or a "burned" message sits in a mailbox for
up to 24 hours after it vanished from both screens.

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
