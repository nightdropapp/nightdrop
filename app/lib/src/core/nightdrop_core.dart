import 'package:flutter/foundation.dart';

import 'models.dart';

/// Strip the `AnyhowException(...)` wrapper the Rust bridge puts around core errors, so the UI
/// shows the human-readable message the core actually wrote rather than a developer-ish string.
String cleanCoreError(Object error) {
  final text = error.toString();
  final match = RegExp(r'^AnyhowException\((.*)\)$', dotAll: true).firstMatch(text);
  return (match != null ? match.group(1)! : text).trim();
}

/// A user-facing message for a failed identity setup. The most common **desktop** cause is arti
/// being unable to launch the Tor onion service because another instance already holds the shared
/// Tor state lock (`~/.local/share/<app>/arti-state`) — e.g. a `flutter run` debug build open
/// alongside the release app. arti's error carries the core's `launch onion service` context, so
/// we detect that and surface actionable guidance instead of the raw arti/lock error.
String identitySetupError(Object error) {
  final msg = cleanCoreError(error);
  if (msg.toLowerCase().contains('launch onion service')) {
    return 'Night Drop may already be running, or another copy is using its data. '
        'Close the other window, then try again.';
  }
  return 'Could not set up your identity: $msg';
}

/// The UI's single seam to the security core.
///
/// Today this is backed by [MockNightdropCore] (in-memory, no crypto) so the app runs and
/// the flows are clickable. Later, a `RustNightdropCore` will back it with the real
/// `flutter_rust_bridge` calls into `core/` — without the UI changing.
///
/// It extends [ChangeNotifier] so widgets can simply `ListenableBuilder` on it; the
/// real implementation will notify on bridge events (incoming messages, etc.).
abstract class NightdropCore extends ChangeNotifier {
  /// The current identity, or null before onboarding.
  Identity? get identity;

  /// True while the app is attempting to auto-restore a persisted identity at launch (show
  /// a loading screen). Becomes false once [start] completes, after which a null [identity]
  /// means onboarding and a non-null one means the home screen.
  bool get isBooting;

  /// Called once at app launch: restore a persisted identity (and its chats) if one exists,
  /// so the user isn't asked to create an identity every launch. No-op if nothing is saved.
  Future<void> start();

  /// True when a saved state file exists but could not be opened at launch (corrupt, wrong key,
  /// or a transient I/O error). The app must NOT silently drop to onboarding in this case — a
  /// fresh identity would overwrite the still-on-disk data. Defaults false (e.g. the mock).
  bool get loadError => false;

  /// Re-attempt [start] after a [loadError] (the failure may have been transient).
  Future<void> retryStart() async {}

  /// Dismiss a [loadError] to proceed to onboarding deliberately. The unreadable state has been
  /// preserved as a sidecar first, so setting up a new identity won't destroy the original bytes.
  void dismissLoadError() {}

  /// Whether to nudge the user to make a backup: they have chats worth losing, have never backed
  /// up, and haven't recently dismissed the reminder. Losing the recovery password (or never
  /// making a backup) means losing the data by design (§7), so a gentle reminder prevents sad
  /// surprises. Defaults false (e.g. the mock).
  Future<bool> shouldSuggestBackup() async => false;

  /// Record that a backup was saved, so the reminder stops.
  Future<void> recordBackupDone() async {}

  /// Snooze the backup reminder for a while (on "Later").
  Future<void> snoozeBackupReminder() async {}

  /// Tell the core whether the app is foreground or background. Backgrounded, the core polls
  /// far less often (battery/data), and new messages raise a local notification.
  void setLifecycle(bool foreground);

  /// Whether this device's address is published/reachable yet. On Tor it's false for the
  /// ~1–3 min after launch while the onion descriptor (re)publishes; until then peers can't
  /// reach us to pair. Always true for the mock/non-Tor cores.
  Future<bool> onionReady();

  /// Reset the Tor connection: drop the entry guards (keeping the `.onion` identity) and rebuild
  /// on fresh ones. The remedy for a device whose guard set has churned out of the network, which
  /// a plain restart cannot fix because it reuses the same guards.
  ///
  /// Offered manually because the automatic heal fires at most once per launch and only on
  /// evidence it can see; a device can be unable to reach anyone while believing itself healthy.
  /// No-op on the mock/non-Tor cores.
  Future<void> resetTorConnection() async {}

  /// The newer release our onion site advertises, or `null` when this build is current, the
  /// check hasn't run, or it failed. Never a reason to block the user — only to tell them.
  String? get updateAvailable => null;

  /// Whether an update download is running, whichever screen started it.
  ///
  /// The banner and the "Update app" menu item are two entry points to one operation, so neither
  /// may keep this state itself: a banner that only knew about its own tap once let a user start a
  /// second concurrent download by tapping it to *watch* the first.
  bool get downloadInProgress => false;

  /// How far a running update download has got, 0.0–1.0, or `null` when nothing is downloading
  /// **or** the server did not say how big the file is.
  ///
  /// Null therefore means "no determinate figure", not "no download" — a caller showing a bar
  /// should treat it as indeterminate rather than as finished. The download is minutes long over
  /// Tor, so a spinner alone leaves the user unable to tell progress from a stall.
  double? get downloadProgress => null;

  /// Ask the onion site whether a newer release exists — over Tor, at most once a day.
  ///
  /// Safe to call on every launch: it returns immediately when a check isn't due, so the caller
  /// doesn't need its own timer. Never throws and never blocks startup; a site that is down, or
  /// a transport with no anonymized path, is silence rather than an error the user must dismiss.
  ///
  /// This matters most for the desktop AppImage, which has no auto-update — a user there can be
  /// months behind a security fix with nothing in the app to say so.
  Future<void> maybeCheckForUpdate() async {}

  /// Download the published build over Tor, verifying its hash, and return the file path.
  ///
  /// Nothing is installed — the caller shows the user where it landed and they decide. Slow (tens
  /// of megabytes over Tor), so callers must not block the UI on it. Returns null on failure.
  Future<String?> downloadUpdate() async => null;

  /// Check now, ignoring the once-a-day limit and any earlier "hide". For the menu item: a user
  /// who deliberately asks must get a fresh answer, not yesterday's cached one.
  ///
  /// Returns whether the site actually answered. This is NOT the same question as
  /// [updateAvailable]: a check that could not reach the site must never be reported as "you are
  /// up to date", which is the one wrong answer this feature exists to prevent.
  Future<bool> checkForUpdateNow() async => false;

  /// Hide the update banner until a *newer* version than this one is published. Deliberately
  /// version-scoped: hiding 0.1.18 must not also hide 0.1.19 and its security fixes.
  Future<void> hideUpdateBanner() async {}

  /// All 1:1 contacts.
  List<Contact> get contacts;

  /// Inbound chat requests awaiting the user's approval (authorization-before-first
  /// -message, §5). Approve/decline with [authorize].
  List<Contact> get incomingRequests;

  /// Messages for a given contact, oldest first.
  List<Message> messagesFor(String contactId);

  /// How many received messages in this chat the user hasn't seen yet (for the list badge).
  int unreadCount(String contactId);

  /// Mark a chat as read (clears its unread badge). Called when the chat is opened/viewed.
  void markRead(String contactId);

  /// Generate a fresh anonymous identity (onboarding).
  Future<void> createIdentity();

  /// Create a pairing invite: QR pre-auth bundle + short code with PAKE secret words.
  Future<PairingInvite> createInvite();

  /// Join a chat from a short code (`slot-secret-words`). Runs rendezvous fetch + PAKE.
  Future<Contact> joinWithShortCode(String code);

  /// Approve or decline a pending inbound request. On approval it becomes a contact.
  Future<void> authorize(String contactId, bool accept);

  /// Begin an encrypted backup: prepare the blob and return the recovery password to show
  /// the user once (§7). [full] selects the content matrix (#7): true = Full (history + media),
  /// false = Lite (identity + onion + contacts + sessions only). After the user acknowledges
  /// the password, call [saveBackup] with the chosen location. Losing the password loses it.
  Future<String> createBackup(bool full);

  /// Write the backup prepared by [createBackup] to the user-chosen [path] (desktop).
  Future<void> saveBackup(String path);

  /// The prepared (password-encrypted) backup bytes, for handing to the OS file picker so
  /// the user can save it to an accessible location (Documents/Downloads) — needed on mobile.
  Future<List<int>> backupBytes();

  /// Begin a **single-chat scoped backup** (#8): prepare a blob carrying only [contactId]'s
  /// chat ([full] = with history + media) and return the one-time recovery password. Held
  /// pending exactly like [createBackup] — acknowledge, then [saveBackup]/[backupBytes].
  Future<String> createChatBackup(String contactId, bool full);

  /// Merge a scoped backup file at [path] into the current identity (#8): add the chat(s) it
  /// carries without disturbing our identity or an active session (existing chats only gain
  /// missing history). Returns how many messages were merged.
  Future<int> mergeBackup(String path, String password);

  /// The human-comparable safety number for [contactId] (12×5 digits, identical on both
  /// devices) — compare out-of-band to confirm no MITM on pairing (key-verification design).
  Future<String> safetyNumber(String contactId);

  /// The raw safety fingerprint (base64url) to render as a QR for scan-to-verify.
  Future<String> safetyQr(String contactId);

  /// Compare a scanned safety-QR payload against [contactId]; on match, mark verified. Returns
  /// whether it matched.
  Future<bool> verifySafetyQr(String contactId, String scanned);

  /// Set the contact's verified flag after comparing the safety number by hand.
  Future<void> setVerified(String contactId, bool verified);

  /// Our advertised **extra** relay addresses (#17) — the ones peers fan our offline mail out
  /// to, in addition to the shared primary. Does not include the implicit primary relay.
  Future<List<String>> myRelays();

  /// Replace our advertised extra relay set (#17) and announce it to contacts so their offline
  /// mail is redundantly delivered. Blank/duplicate entries are dropped by the core.
  Future<void> setMyRelays(List<String> relays);

  /// Reachability of each of our advertised extra relays (#17) as of the last poll. A relay we
  /// self-host that stops answering reports `reachable == false`, so the UI can warn the user and
  /// suggest adding a backup relay.
  Future<List<RelayHealth>> relayHealth();

  /// Generate this device's **access key** for a PRIVATE relay (restricted discovery, §3.2).
  /// Returns the public `descriptor:x25519:…` string to hand the relay operator, who authorizes it
  /// with `nightdrop-relay authorize-client`. arti stores the private half and uses it automatically
  /// when this device dials that relay. Only meaningful on the Tor transport.
  Future<String> createRelayAccessKey(String relayOnion);

  /// Enable an opt-in **server backup** (§7c): store an opaque, password-encrypted copy on the
  /// relay for [ttlHours] (clamped 1..=36). Returns the one-time recovery password (shown once,
  /// never persisted) and the exact expiry — both required by the acknowledgment invariant.
  Future<ServerBackup> createServerBackup(int ttlHours, bool full);

  /// Restore identity + chats from a password-encrypted backup file at [path] (§7 / TODO
  /// #5). Replaces the current (uninitialised) session, so this is an onboarding entry.
  Future<void> importBackup(String path, String password);

  /// Restore from an opt-in **server backup** using the recovery [password] (§7c / #9): fetch
  /// the opaque blob from the relay and rebuild identity + chats. Tor mode only (the relay is
  /// reached over Tor); the device comes back on a new `.onion`, which is announced to contacts.
  Future<void> importServerBackup(String password);

  /// Delete a chat and tell the peer (who then sees a "chat deleted" notice). A new chat
  /// must be created to keep talking (TODO #1).
  Future<void> deleteChat(String contactId);

  /// Terminate this device's identity: wipe the identity, contacts, and messages from the
  /// device and return to onboarding. Irreversible unless a backup was made — the only way
  /// back to these chats is restoring that backup. Peers must re-pair.
  ///
  /// Returns the number of un-backed chats whose "chat deleted" notice could **not** be
  /// delivered (neither queued on a relay nor sent directly), so the UI can warn that some
  /// contacts may not have been notified. `0` means every notice was queued/sent.
  Future<int> logout();

  /// Send a 1:1 message.
  Future<void> sendMessage(String contactId, String text);

  /// Edit one of our own text messages ([Message.msgId]). Allowed within 15 minutes of
  /// sending, or at any time while still queued on the relay (the peer never saw it —
  /// the queued copy is replaced outright). The bubble shows an "edited" tag.
  Future<void> editMessage(String contactId, String msgId, String text);

  /// Unsend ("delete for both") one of our own messages ([Message.msgId]). Same eligibility
  /// as [editMessage]; a still-queued message is recalled so the peer never receives it.
  /// The message becomes a "deleted" tombstone on both sides.
  Future<void> unsendMessage(String contactId, String msgId);

  /// Send an image/video attachment (E2E-encrypted, sealed at rest). [kind] is
  /// "image"/"video", [mime] like "image/jpeg". [thumb] is an optional preview frame for
  /// videos (empty otherwise). Throws if it exceeds the size cap.
  Future<void> sendMedia(
      String contactId, List<int> data, String mime, String kind, List<int> thumb);

  /// Decrypt and return an attachment's bytes (for inline image display).
  Future<List<int>> mediaBytes(String mediaId);

  /// Decrypt an attachment to a temp file and return its path (to open a video externally).
  Future<String> mediaToFile(String mediaId, String ext);

  /// Rename yourself within a single chat (your own per-chat display name).
  void setMyNameInChat(String contactId, String name);

  /// Toggle opt-in 24h server storage for a chat. Enabling it surfaces the persistent
  /// in-chat warning to both parties (the banner lives in the chat UI).
  Future<void> setRemoteStorage(String contactId, bool enabled);

  /// Set a chat's disappearing-messages timer in seconds (0 = off). A shared setting synced
  /// to the peer; messages older than the timer are deleted on both devices.
  Future<void> setDisappearing(String contactId, int secs);

  /// Report that the user screenshotted this chat (#1): logs it locally and tells the peer.
  ///
  /// Only Android 14+ can detect a screenshot, and nothing can detect a photo of the screen — so
  /// this makes captures visible when they happen, and must never be presented as a guarantee that
  /// captures will be visible. Default is a no-op for implementations without a peer to tell.
  Future<void> reportScreenshot(String contactId) async {}

  // --- App lock (see docs/design/app-lock.md) ------------------------------------------
  // The at-rest key normally sits in the OS keystore, which anyone holding the unlocked device
  // can read through the app. A lock re-derives it from a secret the user knows instead. Both a
  // PIN and a passphrase are offered: a PIN stops someone picking up the phone, but only a
  // passphrase survives someone imaging the device — the UI has to say which is which.
  //
  // Defaults keep every implementation that doesn't support a lock (the mock, the in-process demo)
  // behaving exactly as before: never locked, so nothing gates on an unlock.

  /// Whether a lock is set on this device.
  Future<bool> isStoreLocked() async => false;

  /// Whether this session has unlocked the store. Meaningless unless [isStoreLocked].
  bool get storeUnlocked => true;

  /// Whether launch stopped short because the store is locked. Synchronous, because the widget
  /// deciding between the lock screen and onboarding can't await. **The UI must check this before
  /// `identity == null`:** a locked store has no readable identity, and treating that as a fresh
  /// install would offer to overwrite data the user can still recover.
  bool get needsUnlock => false;

  /// Try `secret`; false if it doesn't open the lock. Callers must not report *why* it failed.
  Future<bool> unlockStore(String secret) async => true;

  /// Put the at-rest key behind `secret` and remove the keystore copy.
  Future<void> enableStoreLock(String secret) async {}

  /// Remove the lock, restoring the keystore copy. Throws on a wrong secret.
  Future<void> disableStoreLock(String secret) async {}

  /// Arm (or replace) the **duress** secret (#3) — the second secret that wipes instead of
  /// opening. Needs the normal secret, so someone who coerced one unlock can't re-arm it. Throws
  /// if the normal secret is wrong or if `duress` would also open the normal slot.
  ///
  /// **The UI must never display whether this is armed**, and must warn about it only here: a
  /// persistent "duress is on" anywhere would tell whoever picks up the phone exactly what the
  /// design hides. See `docs/design/duress-wipe.md` §6.
  Future<void> setDuressSecret(String secret, String duress) async {}

  /// Disarm duress. Needs the normal secret. Succeeds whether or not anything was armed.
  Future<void> clearDuressSecret(String secret) async {}

  /// Whether cover traffic (#4) is on.
  Future<bool> coverTrafficEnabled() async => false;

  /// Turn cover traffic on or off. Opt-in: it costs battery and bandwidth continuously, and the
  /// UI must be honest that it *degrades* traffic analysis rather than defeating it.
  Future<void> setCoverTraffic(bool enabled) async {}

  /// Give a contact a nickname only you see. Never sent — the peer can neither read nor set it.
  /// Empty clears it, falling back to their chosen name plus their identity tag.
  Future<void> setLocalName(String contactId, String name) async {}

  /// Whether a wipe code is currently armed, so the UI can offer *remove* only when there is
  /// something to remove. Readable only because the app is unlocked — the flag is sealed under the
  /// store key, so an imaged device still gives nothing away.
  Future<bool> isDuressArmed() async => false;

  /// Whether `secret` is the normal unlock secret. Lets a settings flow reject a wrong secret
  /// before asking for anything else. A wipe code answers false and wipes nothing here.
  Future<bool> verifyStoreSecret(String secret) async => false;

  /// Forget the unlocked key. A no-op while background delivery is on, which needs it resident.
  Future<void> lockStore() async {}
}
