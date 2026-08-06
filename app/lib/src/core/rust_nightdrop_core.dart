import 'dart:async';
import 'dart:io' show Directory, File, FileSystemEntity, Platform;

import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:path_provider/path_provider.dart';

import '../rust/api.dart' as rust;
import 'background_delivery.dart';
import 'app_version.dart';
import 'nightdrop_core.dart';
import 'media_cache.dart';
import 'models.dart';
import 'notifications.dart';
import 'public_downloads.dart';

/// [NightdropCore] backed by the real Rust security core via flutter_rust_bridge.
///
/// Holds an opaque `rust.NightdropCore` handle and caches UI-facing state, refreshing it
/// from Rust after each mutating call and notifying listeners. `RustLib.init(...)` must
/// have run before the first call (done in `main()` for the app; in the test harness the
/// built `libnightdrop.so` is loaded explicitly).
class RustNightdropCore extends NightdropCore {
  rust.NightdropCore? _core;
  StreamSubscription<rust.AppEvent>? _events;
  bool _networked = false;
  bool _booting = true;
  // Lifecycle + notification bookkeeping: while backgrounded, new received messages/requests
  // raise a notification. Counts are baselined after the first load so existing history
  // doesn't fire alerts.
  bool _foreground = true;
  bool _countsReady = false;
  int _knownReceived = 0;
  int _knownRequests = 0;
  Identity? _identity;
  List<Contact> _contacts = const [];
  List<Contact> _requests = const [];
  final Map<String, List<Message>> _messages = {};

  /// Optimistic outgoing messages awaiting core confirmation, kept SEPARATE from [_messages]
  /// (the core history) so overlapping sends to an offline peer don't wipe each other when one
  /// reconciles. Displayed after the core history; each send removes only its own on completion.
  final Map<String, List<Message>> _pending = {};
  int _pendingSeq = 0;

  /// Per-chat count of received (non-self, non-system) messages the user has "seen". Unread =
  /// current received count minus this. Baselined on first load so existing history isn't unread.
  final Map<String, int> _readReceived = {};
  bool _unreadReady = false;

  /// Cached received-message count per chat (§1.5.5), refreshed only when that chat's history is
  /// re-pulled — so `unreadCount` (called per list tile per rebuild) and `_maybeNotify` don't
  /// rescan message lists on every rebuild/event.
  final Map<String, int> _receivedCache = {};

  int _receivedCountFor(String contactId) => (_messages[contactId] ?? const [])
      .where((m) => !m.fromMe && !m.system)
      .length;

  @override
  int unreadCount(String contactId) {
    final n = (_receivedCache[contactId] ?? 0) - (_readReceived[contactId] ?? 0);
    return n < 0 ? 0 : n;
  }

  @override
  void markRead(String contactId) {
    final n = _receivedCache[contactId] ?? 0;
    if ((_readReceived[contactId] ?? 0) != n) {
      _readReceived[contactId] = n; // idempotent: no notify if already current (avoids loops)
      notifyListeners();
    }
  }

  // --- Persistence (OS secure-store key + encrypted state file) -----------------------
  // flutter_secure_storage 10 dropped the EncryptedSharedPreferences backend (Jetpack
  // Security is deprecated); existing entries migrate automatically on first access.
  static const _secure = FlutterSecureStorage();
  static const _kStoreKeyName = 'nightdrop_store_key';
  static const _kStateFile = 'nightdrop-state.bin';

  Future<String> _stateFilePath() async =>
      '${(await getApplicationSupportDirectory()).path}/$_kStateFile';

  // --- App lock (see docs/design/app-lock.md) ------------------------------------------
  // Unlocked at-rest key, held only in memory. Non-null means "this session has unlocked the
  // store"; it is also the *only* copy when a lock is set, because enabling a lock deletes the
  // keystore entry. Dart can't zeroize a String, so dropping the reference is as far as this
  // goes — the Rust side never keeps the key beyond the call.
  String? _unlockedKey;

  /// Set when launch stopped because the store is locked. Cached because `build` can't await
  /// [isStoreLocked], and the UI has to decide between the lock screen and onboarding synchronously.
  bool _lockedOut = false;

  @override
  bool get needsUnlock => _lockedOut;

  /// Whether a passphrase/PIN lock is set on this device.
  @override
  Future<bool> isStoreLocked() async => rust.storeIsLocked(dir: (await _torStateDir())!);

  /// Whether this session has already unlocked the store.
  @override
  bool get storeUnlocked => _unlockedKey != null;

  /// The at-rest key, or null when there is none to be had — either because no identity exists
  /// yet, or because a lock is set and this session hasn't unlocked it.
  ///
  /// Every path to the key goes through here or [_ensureStoreKey]. It used to be read inline in
  /// four places, which is exactly how one of them would keep reading a keystore copy that
  /// enabling a lock was supposed to have removed.
  Future<String?> _readStoreKey() async =>
      await isStoreLocked() ? _unlockedKey : _secure.read(key: _kStoreKeyName);

  /// The at-rest key, creating and persisting one on first use.
  ///
  /// When a lock is set this must **not** write to the keystore: the whole point of the lock is
  /// that the key is not retrievable without the secret.
  Future<String> _ensureStoreKey() async {
    if (await isStoreLocked()) {
      final key = _unlockedKey;
      if (key == null) {
        throw StateError('The store is locked — it must be unlocked before it can be opened.');
      }
      return key;
    }
    final key = await _secure.read(key: _kStoreKeyName) ?? await rust.randomStoreKey();
    await _secure.write(key: _kStoreKeyName, value: key);
    return key;
  }

  /// Try `secret` against the lock. Returns false on a wrong secret; deliberately says nothing
  /// about *why* it failed, so a damaged lock file and a wrong secret look alike from outside.
  @override
  Future<bool> unlockStore(String secret) async {
    try {
      final outcome = await rust.unlockStoreKey(
        dir: (await _torStateDir())!,
        secret: secret,
      );
      // The duress secret (#3): destroy the identity instead of opening it, and report success so
      // the screen moves on exactly as a normal unlock would. What the person holding the phone
      // sees is an app that opened and happens to be empty.
      if (outcome.duress) {
        await _duressWipe();
        return true;
      }
      _unlockedKey = outcome.keyB64;
      _lockedOut = false;
      // The key was the only thing missing: boot the identity now, so a successful unlock lands
      // on the chat list rather than on onboarding (which would offer to overwrite it).
      _booting = true;
      notifyListeners();
      await start();
      return true;
    } catch (_) {
      return false;
    }
  }

  /// Put the existing at-rest key behind `secret`, then delete the keystore copy — without that
  /// deletion the lock is decoration, since the key would still be readable without the secret.
  @override
  Future<void> enableStoreLock(String secret) async {
    final key = await _ensureStoreKey();
    final dir = (await _torStateDir())!;
    await rust.setStorePassphrase(dir: dir, keyB64: key, passphrase: secret);
    await _secure.delete(key: _kStoreKeyName);
    _unlockedKey = key; // already unlocked; don't make the user re-enter it immediately
    notifyListeners();
  }

  /// Drop the lock, restoring the keystore copy. The key comes back out of the lock file, so a
  /// wrong secret throws and changes nothing.
  @override
  Future<void> disableStoreLock(String secret) async {
    final key = await rust.clearStorePassphrase(
      dir: (await _torStateDir())!,
      passphrase: secret,
    );
    await _secure.write(key: _kStoreKeyName, value: key);
    _unlockedKey = key;
    // There is no lock any more, so nothing can be locked *out*. Clearing this defensively matters
    // because a re-lock can race the disable — anything that briefly backgrounds the app mid-flow
    // sets the flag while the lock file still exists, and it would otherwise survive the removal
    // and strand the user on a lock screen no secret can dismiss.
    _lockedOut = false;
    notifyListeners();
  }

  /// Arm or replace the duress secret (#3). Requires the normal secret; the core also refuses a
  /// duress secret that would open the normal slot, and self-checks the one it writes.
  @override
  Future<void> setDuressSecret(String secret, String duress) async {
    await rust.setDuressSecret(
      dir: (await _torStateDir())!,
      passphrase: secret,
      duress: duress,
    );
  }

  /// Disarm duress. Succeeds whether or not anything was armed, on purpose.
  @override
  Future<void> clearDuressSecret(String secret) async {
    await rust.clearDuressSecret(
      dir: (await _torStateDir())!,
      passphrase: secret,
    );
  }

  /// Whether a wipe code is armed. Needs the unlocked store key; without one (locked, or no lock
  /// at all) the answer is a plain no.
  @override
  Future<bool> coverTrafficEnabled() async => rust.coverTrafficEnabled();

  @override
  Future<void> setCoverTraffic(bool enabled) async {
    await rust.setCoverTraffic(enabled: enabled);
    notifyListeners();
  }

  @override
  Future<void> setLocalName(String contactId, String name) async {
    await _core!.setLocalName(contactId: contactId, name: name);
    await _refresh();
  }

  @override
  Future<bool> isDuressArmed() async {
    final key = _unlockedKey ?? await _readStoreKey();
    if (key == null) return false;
    return rust.duressIsArmed(dir: (await _torStateDir())!, keyB64: key);
  }

  @override
  Future<bool> verifyStoreSecret(String secret) async =>
      rust.storeSecretIsCorrect(dir: (await _torStateDir())!, secret: secret);

  /// The duress wipe (#3). Destroys the identity, then lands on onboarding — indistinguishable,
  /// from the outside, from unlocking an app that was never used.
  ///
  /// Ordering is the whole design here, so don't rearrange it casually:
  ///
  ///  * The **lock file goes first**, before anything that can throw or stall. If the wipe is
  ///    interrupted after this point the app comes up as a fresh install rather than showing a
  ///    lock screen for a store that no longer exists.
  ///  * The peer notice runs on a **hard time bound**, and the wipe proceeds whatever it returns.
  ///    A wipe that can be prevented by taking the phone off the network is not a wipe.
  ///  * The notice is the ordinary "chat deleted" — nothing says *duress*. See
  ///    `docs/design/duress-wipe.md` §5.
  Future<void> _duressWipe() async {
    final dir = await _torStateDir();
    if (dir != null) {
      try {
        await rust.destroyStoreLock(dir: dir);
      } catch (_) {
        // Even if this fails, keep going: the state blob below is what holds the messages.
      }
    }
    _unlockedKey = null;
    _lockedOut = false;
    // Reuse the ordinary teardown, which already routes to onboarding before any await, so the
    // screen is empty immediately — `duress: true` only changes which chats are told.
    await logout(duress: true);
  }

  /// Forget the unlocked key, if we are allowed to.
  ///
  /// With background delivery **on** the key has to stay resident or the foreground service
  /// couldn't decrypt anything it receives while locked — the same trade Signal makes. With it
  /// off, nothing needs the store until the next unlock, so the key goes.
  @override
  Future<void> lockStore() async {
    if (!await isStoreLocked()) return;
    if (await BackgroundDelivery.isEnabled()) return;
    _unlockedKey = null;
    _lockedOut = true;
    await _closeCore();
    notifyListeners();
  }

  /// Tor's state base dir (also where the persisted state file lives). We pin it to the app
  /// support dir on every platform — mobile needs an explicit writable dir, and on desktop a
  /// known location lets a backup fold in the onion keystore so restore keeps the same `.onion`.
  Future<String?> _torStateDir() async =>
      (await getApplicationSupportDirectory()).path;

  /// Release the current core before another is built over the same Tor state dir.
  ///
  /// arti takes an **exclusive on-disk lock** on that directory, so only one instance may live
  /// at a time. Clearing `_core` is not enough: the Rust object is freed by a Dart finalizer at
  /// an unpredictable time, and its poller thread keeps the transport alive regardless — so the
  /// lock outlives the reference and the next bootstrap dies with "State already locked".
  ///
  /// `shutdown()` releases it synchronously: it stops the poller, tears the transport down, and
  /// **waits** (bounded, a few seconds at worst) for the poller to actually let go. Awaiting it is
  /// therefore load-bearing — a caller that fires it off and rebuilds immediately gets the
  /// read-only arti client this is here to prevent. No-op when there is no core.
  Future<void> _closeCore() async {
    // Drop the Rust `StreamSink` FIRST. Cancelling the Dart subscription waits for the stream to
    // close, and only dropping the sink closes it — so awaiting the cancel while the sink is still
    // held in the `EVENTS` static waits for something that can never happen. That is a deadlock on
    // every teardown path there is: the guard heal, the manual reset, logout and restore all come
    // through here. Measured on a phone (2026-08-03): "Reset Tor connection" logged "closing the
    // core" and then nothing, forever, with no error and a still-running old client.
    //
    // Bounded as well as ordered: this is teardown, and no event-stream quirk is worth wedging it.
    rust.unsubscribe();
    await _events
        ?.cancel()
        .timeout(const Duration(seconds: 2), onTimeout: () {});
    _events = null;
    final core = _core;
    _core = null;
    if (core == null) return;
    try {
      await core.shutdown();
    } catch (_) {
      // Best-effort: a core that won't shut down cleanly must not block the path replacing it.
    }
    core.dispose();
  }

  /// Automatically recover from a wedged Tor entry-guard set — the in-app equivalent of deleting
  /// `guards.json` by hand. A guard set that has churned out of the network can't be recovered by a
  /// plain re-bootstrap, because that reuses the same guards; only resetting them (keeping the
  /// `.onion` identity) breaks the loop. Runs at most once per launch.
  ///
  /// **What counts as evidence, and what does not.** Two triggers have been removed from here
  /// after being measured on hardware, and both were removed for the same reason: they inferred
  /// "the guards must be bad" from something that was not evidence of that.
  ///
  ///  * `!onionReady()` — arti's aggregate onion-service state is *bootstrap progress*, not
  ///    liveness. A phone with its descriptor on 8/8 HSDirs for both time periods, 4/4 introduction
  ///    points and zero upload failures reported not-published for eight minutes (2026-08-03), so
  ///    this destroyed a healthy guard set 2.5 minutes into every session.
  ///  * arti's `bootstrap_status()` — `BlockageKind::CantReachTor` turns out to be unreachable in
  ///    arti 0.43 (its `_` arm has no matching `ConnBlockage` variant), and the kinds that *are*
  ///    reachable can't be acted on: `online` is derived from connections to relays, so a dead
  ///    guard set and a dead network are indistinguishable, and rotating guards because the device
  ///    is offline burns anonymity margin for someone else's problem.
  ///
  /// What survives is the one signal that is positive, end-to-end evidence rather than inference:
  /// [`NightdropCore.directPathWedged`] — several sends failed and *neither* the direct path nor
  /// the relay has ever succeeded this run. It is self-corroborating, because a working relay means
  /// Tor works and the problem is the peer.
  ///
  /// The bar is high on purpose. Entry guards are meant to be sticky for weeks, and arti repairs a
  /// bad set by itself: with a router dropping every packet to all four of a device's confirmed
  /// guards, a cold start still reached `Running` and published in ~80 s by sampling a replacement
  /// (2026-08-04). Anything automatic here has to beat that, and only user-visible failure does.
  Future<void> _scheduleGuardHeal(String statePath, String key) async {
    if (!_tor || _guardHealDone) return;
    final core = _core;
    await Future.delayed(_guardHealTimeout);
    while (!_guardHealDone && _tor && identical(_core, core) && core != null) {
      if (await core.directPathWedged()) {
        // Measurement build (`NIGHTDROP_NO_HEAL=1`): log what the heal would have done and carry
        // on. This is how the false triggers above were caught; keep it.
        if (_noHeal) {
          await rust.diagNote(
              line: 'heal: SUPPRESSED (NIGHTDROP_NO_HEAL) — directWedged=true '
                  'onionReady=${await onionReady()}');
          await Future.delayed(_guardHealRecheck);
          continue;
        }
        await rust.diagNote(
            line: 'heal: resetting guards — no send has reached a peer or a relay this run');
        await _resetTorConnection(statePath, key);
        return;
      }
      await Future.delayed(_guardHealRecheck);
    }
  }

  /// Reset the entry-guard state and rebuild the core on fresh guards, keeping the `.onion`
  /// identity. Shared by the automatic heal and the manual [resetTorConnection] action.
  Future<void> _resetTorConnection(String statePath, String key) async {
    final stateDir = await _torStateDir();
    if (stateDir == null) return; // no writable Tor state dir — nothing to reset
    _guardHealDone = true;
    // Shut the core down BEFORE touching the guard files. `resetTorGuards` says so in its own doc
    // ("Call this with the core shut down, then build a fresh core") and this called it the other
    // way round: the live arti client can re-persist the guards we just deleted, so the heal
    // quietly undoes itself and the next launch inherits the same wedged set. Seen on a desktop
    // 2026-08-02, healing repeatedly with no improvement.
    await _closeCore();
    await rust.resetTorGuards(stateDir: stateDir);
    _core = await rust.NightdropCore.newTor(
      stateDir: stateDir,
      relayAddr: _relayAddr,
      persistPath: statePath,
      persistKey: key,
    );
    _tor = true;
    _events = rust.subscribe().listen((e) => _refresh(e));
    final id = await _core!.identity();
    _identity = Identity(id: id.id);
    await _refresh();
    notifyListeners();
  }

  /// Manually reset the Tor connection (menu action). The automatic heal only fires once per
  /// launch and only on its own evidence; this is the escape hatch for a device that is wedged in
  /// a way no heuristic caught — previously there was none, and a user in that state had nothing
  /// to try but reinstalling.
  @override
  Future<void> resetTorConnection() async {
    if (!_tor) {
      await rust.diagNote(line: 'reset: ignored — not running on Tor');
      return;
    }
    final key = await _readStoreKey();
    if (key == null) {
      // Locked, or the key is not retrievable. Said out loud: a menu action that silently does
      // nothing is indistinguishable from one that ran and failed to help.
      await rust.diagNote(line: 'reset: no store key available — nothing to rebuild');
      return;
    }
    await rust.diagNote(line: 'reset: user asked for a fresh Tor connection');
    await _resetTorConnection(await _stateFilePath(), key);
  }

  @override
  void dispose() {
    _events?.cancel();
    rust.unsubscribe();
    super.dispose();
  }

  @override
  void setLifecycle(bool foreground) {
    _foreground = foreground;
    _core?.setBackground(background: !foreground);
  }

  @override
  bool get isBooting => _booting;

  @override
  Identity? get identity => _identity;

  @override
  List<Contact> get contacts => List.unmodifiable(_contacts);

  @override
  List<Contact> get incomingRequests => List.unmodifiable(_requests);

  @override
  List<Message> messagesFor(String contactId) => List.unmodifiable([
        ...?_messages[contactId],
        ...?_pending[contactId], // optimistic sends, shown after the confirmed history
      ]);

  // --- Transport configuration -------------------------------------------------------
  //
  // Networked mode (real two-client comms over TCP + a shared relay) is what lets two
  // physical devices talk. It is selected by NIGHTDROP_LISTEN + NIGHTDROP_RELAY, resolved from
  // either a compile-time --dart-define (the only option on Android) or, on desktop, a
  // process env var. If neither is set we fall back to the in-process demo core.
  // Tor mode (NIGHTDROP_TOR=1) is the production WAN path: each device bootstraps an embedded
  // Tor client and gets a reachable .onion, so peers pair (by QR — no relay needed) and
  // chat over any network including LTE. It takes precedence over TCP networked mode.
  static const String _defineTor = String.fromEnvironment('NIGHTDROP_TOR');
  static const String _defineListen = String.fromEnvironment('NIGHTDROP_LISTEN');
  static const String _defineRelay = String.fromEnvironment('NIGHTDROP_RELAY');
  // Opt-in field diagnostics (NIGHTDROP_DIAG=1). Records protocol outcomes only — never keys,
  // codes, onion addresses, or names — so it is safe to enable on a build you hand to someone
  // for a repro. Off unless asked for; a normal release is silent.
  static const String _defineDiag = String.fromEnvironment('NIGHTDROP_DIAG');
  // Measurement-only (NIGHTDROP_NO_HEAL=1): suppress the automatic guard heal so a device that
  // trips its conditions is observed instead of reset. Exists to answer whether the heal is a net
  // win or whether it fires on network-wide trouble and makes the next minutes worse. Not a user
  // setting and not documented in the app — absent from every normal build.
  static const String _defineNoHeal = String.fromEnvironment('NIGHTDROP_NO_HEAL');
  static bool get _noHeal => _defineNoHeal == '1';

  static String? _config(String key, String define) {
    if (define.isNotEmpty) return define;
    final env = Platform.environment[key];
    return (env != null && env.isNotEmpty) ? env : null;
  }

  static String? get _listenAddr => _config('NIGHTDROP_LISTEN', _defineListen);
  static String? get _relayAddr => _config('NIGHTDROP_RELAY', _defineRelay);
  static bool get _torEnabled {
    final v = _config('NIGHTDROP_TOR', _defineTor)?.toLowerCase();
    return v == '1' || v == 'true' || v == 'yes';
  }

  static bool get _diagEnabled {
    final v = _config('NIGHTDROP_DIAG', _defineDiag)?.toLowerCase();
    return v == '1' || v == 'true' || v == 'yes';
  }

  bool _tor = false;

  bool _loadError = false;

  @override
  bool get loadError => _loadError;

  @override
  Future<void> retryStart() async {
    _loadError = false;
    _booting = true;
    notifyListeners();
    await start();
  }

  @override
  void dismissLoadError() {
    // The unreadable file was already preserved as a sidecar in start(); onboarding from here
    // will write a fresh state file but cannot destroy those original bytes.
    _loadError = false;
    notifyListeners();
  }

  static const _kBackedUp = 'nightdrop_backed_up';
  static const _kBackupSnoozeUntil = 'nightdrop_backup_snooze_until';
  static const _kUpdateCheckedAt = 'nightdrop_update_checked_at';
  static const _kUpdateHidden = 'nightdrop_update_hidden_version';

  /// How often we ask the onion site. Daily is often enough to matter for a security fix and
  /// rare enough that the request is not a heartbeat: a beacon every launch would let anyone
  /// counting requests infer how many installs exist and how often they run.
  static const _updateCheckInterval = Duration(hours: 24);

  String? _updateAvailable;

  @override
  String? get updateAvailable => _updateAvailable;

  @override
  Future<void> hideUpdateBanner() async {
    final v = _updateAvailable;
    if (v == null) return;
    await _secure.write(key: _kUpdateHidden, value: v);
    _updateAvailable = null;
    notifyListeners();
  }

  @override
  Future<String?> downloadUpdate() async {
    // Held for the whole download, not just the fetch: tens of megabytes over Tor takes minutes,
    // and Doze/App Standby are free to freeze the process partway through. Taken here, while the
    // app is still foreground from the tap that got us here — Android 12+ refuses to start a
    // foreground service once the app has already left.
    return BackgroundDelivery.holdDuring(
      _downloadUpdate,
      notificationText: 'Downloading update',
    );
  }

  Future<String?> _downloadUpdate() async {
    try {
      // Downloaded and verified app-privately first, then published. Rust writes nothing until the
      // hash matches what the onion site said, so the file only ever becomes visible to the user
      // after it has passed — there is no window where a file manager can offer a bad build.
      final dir = await PublicDownloads.staging();
      final dest = '${dir.path}/NightDrop-update.apk';
      // Which ABI's build to fetch is Rust's call, not ours — it reads the architecture the core
      // was compiled for, which is the one Android actually chose. See `update::native_abi`.
      final n = await _core?.downloadUpdate(destPath: dest);
      if (n == null || n <= BigInt.zero) {
        // Rust has already said why on its own diag line; this one marks that the UI gave up, so
        // a log without it means the failure was somewhere after the download.
        await rust.diagNote(line: 'update: download returned nothing — reporting failure');
        return null;
      }
      // Named for the version it actually is. "NightDrop-update.apk" tells a user nothing months
      // later, and collides with the last one they downloaded.
      final version = _updateAvailable;
      final where = await PublicDownloads.publish(
        File(dest),
        displayName:
            version == null ? 'NightDrop.apk' : 'NightDrop-$version.apk',
        mimeType: 'application/vnd.android.package-archive',
      );
      // Which of the three routes ran is invisible otherwise, and it is the difference between a
      // file the user can find and one they cannot. Paths are app-private or public storage —
      // nothing identity-linked — but log only the leaf, not the full path.
      await rust.diagNote(
        line: where == dest
            ? 'update: published NOWHERE — file left in app-private storage'
            : 'update: published to ${where.startsWith('Downloads/') ? 'MediaStore Downloads' : 'external storage'}',
      );
      return where;
    } catch (e) {
      // This used to swallow everything, so a failure here was indistinguishable from a failed
      // download and the UI could only ever say "failed".
      await rust.diagNote(line: 'update: download/publish threw: $e');
      return null;
    }
  }

  @override
  Future<bool> checkForUpdateNow() async {
    // Clear both gates: the daily timer and the hidden-version marker. Asking explicitly is the
    // user overriding both, and a menu item that answers "nothing" because of a hide they forgot
    // about is worse than useless.
    await _secure.delete(key: _kUpdateCheckedAt);
    await _secure.delete(key: _kUpdateHidden);
    return _check();
  }

  @override
  Future<void> maybeCheckForUpdate() async {
    await _check();
  }

  /// The check itself. Returns whether the onion site answered.
  Future<bool> _check() async {
    // Never let this fail a launch. Every branch below is best-effort: no answer is the normal
    // outcome on a slow or offline network, and it must look exactly like "nothing to report".
    try {
      final last = int.tryParse(await _secure.read(key: _kUpdateCheckedAt) ?? '') ?? 0;
      final since = DateTime.now().millisecondsSinceEpoch - last;
      if (last != 0 && since < _updateCheckInterval.inMilliseconds) return false;

      final result = await _core?.checkForUpdate(currentVersion: kAppVersion);
      // Record the attempt, not the success: a site that is down must not turn into a retry on
      // every launch, which is the beacon we are avoiding.
      await _secure.write(
        key: _kUpdateCheckedAt,
        value: '${DateTime.now().millisecondsSinceEpoch}',
      );
      // Null means the site did not answer (down, slow, or no anonymized path). Do NOT let that
      // read as "no update": the caller reports it separately.
      if (result == null) return false;
      var next = result.updateAvailable ? result.latest : null;
      // Respect a hide, but only for the exact version that was hidden — a later release, which
      // may be the one carrying a fix that matters, shows again.
      if (next != null && await _secure.read(key: _kUpdateHidden) == next) next = null;
      if (next != _updateAvailable) {
        _updateAvailable = next;
        notifyListeners();
      }
      return true;
    } catch (_) {
      return false; // Silence for the banner; the menu turns this into a real message.
    }
  }

  @override
  Future<bool> shouldSuggestBackup() async {
    // Nothing worth backing up until there's at least one chat.
    if (contacts.isEmpty) return false;
    if (await _secure.read(key: _kBackedUp) == 'yes') return false;
    final snooze = await _secure.read(key: _kBackupSnoozeUntil);
    if (snooze != null) {
      final until = int.tryParse(snooze) ?? 0;
      if (DateTime.now().millisecondsSinceEpoch < until) return false;
    }
    return true;
  }

  @override
  Future<void> recordBackupDone() async {
    await _secure.write(key: _kBackedUp, value: 'yes');
    notifyListeners();
  }

  @override
  Future<void> snoozeBackupReminder() async {
    final until = DateTime.now().add(const Duration(days: 7)).millisecondsSinceEpoch;
    await _secure.write(key: _kBackupSnoozeUntil, value: '$until');
    notifyListeners();
  }

  // If the onion hasn't published within this long, the entry-guard set is almost certainly wedged
  // (a healthy publish finishes well under it) — the app then resets guards and rebuilds itself.
  static const _guardHealTimeout = Duration(seconds: 150);
  // How often the health signals are re-read after that first check. Two cheap FFI reads, so the
  // cost is nil; the point is that a session which wedges an hour in is still noticed.
  static const _guardHealRecheck = Duration(seconds: 30);
  // At most one automatic guard-heal per launch, so a genuinely offline device can't loop.
  bool _guardHealDone = false;

  @override
  Future<void> start() async {
    // Auto-restore a persisted identity on launch (Tor mode only). If a secure-store key and
    // a saved state file both exist, rebuild the same identity + chats; else fall through to
    // onboarding.
    //
    // Before anything else, so a failure in the launch path itself is on the record.
    if (_diagEnabled) await rust.setDiagnostics(enabled: true);
    _guardHealDone = false;
    // Close anything already running first: this runs again via `retryStart` after a failure,
    // and a second bootstrap over the same (still-locked) Tor state dir would fail no matter
    // how many times the user pressed "Try again".
    await _closeCore();
    try {
      if (_torEnabled) {
        // A locked store has no readable key yet, and the check below would then see
        // "no key + saved state" and fall through to onboarding — which is precisely the
        // overwrite-recoverable-data path the comment there warns about. Stop and let the UI
        // ask for the secret instead.
        if (await isStoreLocked() && !storeUnlocked) {
          _lockedOut = true;
          _booting = false;
          notifyListeners();
          return;
        }
        final key = await _readStoreKey();
        final statePath = await _stateFilePath();
        final hasState = File(statePath).existsSync();
        if (key != null && hasState) {
          // A state file EXISTS, so this is not a first run. If opening it fails, we must not
          // fall through to onboarding — creating a new identity would overwrite recoverable
          // data. Preserve the bytes and surface an explicit error instead.
          try {
            _core = await rust.NightdropCore.newTor(
              stateDir: await _torStateDir(),
              relayAddr: _relayAddr,
              persistPath: statePath,
              persistKey: key,
            );
            _tor = true;
            _events = rust.subscribe().listen((e) => _refresh(e));
            final id = await _core!.identity();
            _identity = Identity(id: id.id);
            await _refresh();
            unawaited(_scheduleGuardHeal(statePath, key));
          } catch (e) {
            // Tear the core down rather than just forgetting it: `newTor` may well have
            // succeeded (arti bootstrapped, lock taken) and a *later* step thrown. Dropping the
            // reference would strand that instance holding the lock, so every subsequent retry
            // or backup restore would fail with "State already locked".
            await _closeCore();
            _identity = null;
            // Distinguish a Tor-connection failure (data is fine — just retry) from an actual
            // unreadable/corrupt state (preserve the bytes before anything can overwrite them).
            final msg = e.toString().toLowerCase();
            final connectFailure = msg.contains('tor') ||
                msg.contains('connect') ||
                msg.contains('bootstrap') ||
                msg.contains('circuit');
            if (!connectFailure) {
              await _preserveUnreadableState(statePath);
            }
            _loadError = true;
          }
        } else if (key == null && hasState) {
          // The encrypted state file exists but its store key is unreadable from the OS keyring —
          // commonly the login keyring / KDE Wallet is simply locked when the app launches. Do NOT
          // fall through to onboarding: creating a new identity would overwrite this recoverable
          // file. Surface the recovery screen instead; "Try again" reloads once the key is readable
          // (e.g. after the keyring is unlocked). The bytes are never touched here.
          _identity = null;
          _loadError = true;
        }
      }
    } catch (e) {
      // An error OUTSIDE the load of an existing file (e.g. reading the keystore, resolving the
      // state dir). No confirmed on-disk identity to protect here — fall back to onboarding.
      await _closeCore();
      _identity = null;
    } finally {
      _booting = false;
      notifyListeners();
      // Fire and forget, deliberately unawaited: a launch must never wait on the network, and
      // this one dials Tor. It self-limits to one check a day, so calling it on every start is
      // free after the first.
      unawaited(maybeCheckForUpdate());
    }
  }

  /// Copy an unreadable state file aside before anything can overwrite it, so a transient failure
  /// (or a bug fixed in a later build) doesn't cost the user their data. Best-effort.
  /// Move any persisted state out of the way before a **new** identity is created.
  ///
  /// The core restores from `persistPath` whenever that file exists — it has no other way to tell
  /// "create" from "restore". So an unreadable state file, which is the exact reason the load-error
  /// screen is on screen, made "set up a new identity" fail with "wrong key or corrupt store": the
  /// recovery path blocked by the thing it exists to recover from. Found on a device, 2026-08-02,
  /// straight after the same shape of bug in the sealed onion key.
  ///
  /// **Renamed, never deleted.** These bytes may be the user's only copy of an identity they are
  /// abandoning under duress of a failed launch, possibly recoverable later with the right key.
  /// The wipe removes the sidecars, so they never outlive a deliberate destruction.
  Future<void> _setAsideOldState() async {
    try {
      final file = File(await _stateFilePath());
      if (!file.existsSync()) return;
      await file.rename('${file.path}.replaced-${DateTime.now().millisecondsSinceEpoch}');
    } catch (_) {
      // Non-Tor demo modes have no persistence and no plugin to ask; never block onboarding.
    }
  }

  Future<void> _preserveUnreadableState(String path) async {
    try {
      final file = File(path);
      if (!file.existsSync()) return;
      final backup = '$path.unreadable-${DateTime.now().millisecondsSinceEpoch}';
      if (!File(backup).existsSync()) await file.copy(backup);
    } catch (_) {
      // Preserving the copy is a safety net; never let it throw into the launch path.
    }
  }

  @override
  Future<void> createIdentity() async {
    // Reachable from the load-error screen ("set up new identity"), where a failed start may
    // have left a core holding the Tor state lock.
    await _closeCore();
    _guardHealDone = false;
    await _setAsideOldState();
    final listen = _listenAddr;
    final relay = _relayAddr;
    if (_torEnabled) {
      // Embedded Tor: a reachable .onion, WAN-capable. Bootstrapping takes a while.
      // On mobile, arti needs an explicit writable state dir (the app's support dir). A
      // persistence key (held in the OS secure store) makes the identity survive restarts.
      final key = await _ensureStoreKey();
      final statePath = await _stateFilePath();
      _core = await rust.NightdropCore.newTor(
        stateDir: await _torStateDir(),
        relayAddr: relay,
        persistPath: statePath,
        persistKey: key,
      );
      _tor = true;
      unawaited(_scheduleGuardHeal(statePath, key));
    } else if (listen != null && relay != null) {
      _core = await rust.NightdropCore.newNetworked(listenAddr: listen, relayAddr: relay);
      _networked = true;
    } else {
      _core = await rust.NightdropCore.newInstance();
    }
    // Subscribe to push events from Rust; refresh our view whenever state changes.
    _events = rust.subscribe().listen((e) => _refresh(e));
    final id = await _core!.identity();
    _identity = Identity(id: id.id);
    notifyListeners();
  }

  @override
  Future<PairingInvite> createInvite() async {
    if (_tor) {
      // Two ways to pair over Tor, both surfaced on the invite screen:
      //  - QR: carries our .onion + a pre-authorized pre-key bundle (relay-free).
      //  - short code: a REAL `slot-secret-words` staged in the rendezvous mailbox (§5b),
      //    completed by SPAKE2 over the relay — for when the peer can't scan a QR.
      // createInvite runs first for the QR; createShortCodeInvite runs last so the staged
      // code (not createInvite's throwaway random one) becomes last_invite_code — the one an
      // approval echoes back to the joiner. Staging needs a relay; if none is configured it
      // throws, so it's best-effort — the QR still works and we fall back to a QR-only invite.
      final invite = await _core!.createInvite();
      String code = '';
      try {
        code = await _core!.createShortCodeInvite();
      } catch (_) {
        // No relay reachable → no rendezvous short code; the QR remains fully usable.
      }
      return PairingInvite(shortCode: code, qrPayload: invite.qrPayload);
    }
    if (_networked) {
      // Real rendezvous short code (no QR); the peer joins with this code.
      final code = await _core!.createShortCodeInvite();
      return PairingInvite(shortCode: code, qrPayload: '');
    }
    final invite = await _core!.createInvite();
    await _refresh(); // demo: a simulated peer joined -> a pending request
    return PairingInvite(shortCode: invite.shortCode, qrPayload: invite.qrPayload);
  }

  @override
  Future<Contact> joinWithShortCode(String code) async {
    // A scanned/typed QR pre-auth payload (the Tor pairing path) goes through connectViaQr;
    // a bare short code uses the rendezvous (networked) or the in-process demo.
    final rust.Contact created;
    if (code.startsWith('nightdrop://')) {
      created = await _core!.connectViaQr(payload: code);
    } else if (_networked || _tor) {
      // A bare `slot-secret-words` short code: run the SPAKE2 rendezvous handshake over the
      // relay. Works over Tor too — the relay is reached through the onion transport, so the
      // secret words never leave the device and the rendezvous only ever sees ciphertext.
      created = await _core!.joinViaShortCode(code: code);
    } else {
      created = await _core!.openChat(code: code);
    }
    await _refresh();
    return _contacts.firstWhere((c) => c.id == created.id, orElse: () => _map(created));
  }

  @override
  Future<void> authorize(String contactId, bool accept) async {
    await _core!.authorize(contactId: contactId, accept: accept);
    await _refresh();
  }

  @override
  Future<String> createBackup(bool full) => _core!.createBackup(full: full);

  @override
  Future<void> saveBackup(String path) => _core!.saveBackup(path: path);

  @override
  Future<bool> onionReady() async => _core == null ? true : await _core!.onionReady();

  @override
  Future<List<int>> backupBytes() => _core!.backupBytes();

  @override
  Future<String> createChatBackup(String contactId, bool full) =>
      _core!.createChatBackup(contactId: contactId, full: full);

  @override
  Future<int> mergeBackup(String path, String password) =>
      _core!.mergeBackup(path: path, password: password);

  @override
  Future<String> safetyNumber(String contactId) =>
      _core!.safetyNumber(contactId: contactId);

  @override
  Future<String> safetyQr(String contactId) => _core!.safetyQr(contactId: contactId);

  @override
  Future<bool> verifySafetyQr(String contactId, String scanned) =>
      _core!.verifySafetyQr(contactId: contactId, scanned: scanned);

  @override
  Future<void> setVerified(String contactId, bool verified) =>
      _core!.setVerified(contactId: contactId, verified: verified);

  @override
  Future<List<String>> myRelays() => _core!.myRelays();

  @override
  Future<void> setMyRelays(List<String> relays) async {
    await _core!.setMyRelays(relays: relays);
    await _refresh();
  }

  @override
  Future<List<RelayHealth>> relayHealth() async {
    // The home screen's health banner starts polling from `initState`, which runs before `start()`
    // has finished building the core (Tor bootstrap takes seconds). No core yet means no relay is
    // known to be unreachable, so report nothing and let the next 6s tick pick it up — same shape
    // as `onionReady`'s guard above.
    if (_core == null) return const [];
    final health = await _core!.relayHealth();
    return health
        .map((h) => RelayHealth(address: h.address, reachable: h.reachable))
        .toList();
  }

  @override
  Future<String> createRelayAccessKey(String relayOnion) =>
      _core!.createRelayAccessKey(relayOnion: relayOnion);

  @override
  Future<ServerBackup> createServerBackup(int ttlHours, bool full) async {
    final info = await _core!
        .createServerBackup(ttlHours: BigInt.from(ttlHours), full: full);
    return ServerBackup(
      password: info.password,
      expiresAt: DateTime.fromMillisecondsSinceEpoch(
          info.expiresAtSecs.toInt() * 1000),
    );
  }

  @override
  Future<void> importBackup(String path, String password) async {
    // Restoring builds a whole new core over the same Tor state dir, so whatever is running now
    // has to go first — otherwise arti can't launch the restored identity's onion service.
    await _closeCore();
    if (_torEnabled) {
      // Restore the backup onto Tor and persist it (so it survives future restarts too).
      final key = await _ensureStoreKey();
      _core = await rust.NightdropCore.restoreBackupTor(
        backupPath: path,
        password: password,
        stateDir: await _torStateDir(),
        relayAddr: _relayAddr,
        persistPath: await _stateFilePath(),
        persistKey: key,
      );
      _tor = true;
    } else {
      // Mirror createIdentity's transport selection for non-Tor builds.
      final listen = _listenAddr;
      final relay = _relayAddr;
      _core = await rust.NightdropCore.restoreBackup(
        path: path,
        password: password,
        listenAddr: listen,
        relayAddr: relay,
      );
      _networked = listen != null && relay != null;
    }
    _events = rust.subscribe().listen((e) => _refresh(e));
    final id = await _core!.identity();
    _identity = Identity(id: id.id);
    await _refresh();
  }

  @override
  Future<void> importServerBackup(String password) async {
    if (!_torEnabled) {
      throw StateError('Server backup restore requires Tor mode.');
    }
    final relay = _relayAddr;
    if (relay == null) {
      throw StateError('No relay configured — cannot restore from server.');
    }
    // As in `importBackup`: release the Tor state lock before the restored core takes it.
    await _closeCore();
    final key = await _ensureStoreKey();
    _core = await rust.NightdropCore.restoreServerBackupTor(
      password: password,
      stateDir: await _torStateDir(),
      relayAddr: relay,
      persistPath: await _stateFilePath(),
      persistKey: key,
    );
    _tor = true;
    _events = rust.subscribe().listen((e) => _refresh(e));
    final id = await _core!.identity();
    _identity = Identity(id: id.id);
    await _refresh();
  }

  @override
  Future<void> deleteChat(String contactId) async {
    // Optimistic: drop the chat from the UI immediately so it disappears at once, then signal
    // the peer and tear it down in the background (the Tor send can be slow or block).
    _contacts = _contacts.where((c) => c.id != contactId).toList();
    _requests = _requests.where((c) => c.id != contactId).toList();
    _messages.remove(contactId);
    notifyListeners();
    unawaited(() async {
      try {
        await _core?.deleteChat(contactId: contactId);
      } catch (_) {}
      await _refresh();
    }());
  }

  @override
  Future<int> logout({bool duress = false}) async {
    // Tear down in-memory state and route to onboarding IMMEDIATELY — before any await that
    // could stall (FRB stream cancel / secure storage / file IO). Earlier, awaiting the
    // stream cancel first could hang and leave the screen stuck.
    final events = _events;
    _events = null;
    final core = _core; // keep a handle for the peer-facing logout signal below
    try {
      rust.unsubscribe();
    } catch (_) {}
    _core = null;
    _identity = null;
    _contacts = const [];
    _requests = const [];
    _messages.clear();
    _pending.clear();
    _networked = false;
    _tor = false;
    _countsReady = false;
    notifyListeners(); // _Root -> onboarding now

    // Stop background delivery and FORGET the preference, on every path that destroys an
    // identity — not just the menu one, which is where the `stop()` used to live alone.
    //
    // The duress wipe (§ app-lock #3) went through `logout` without ever touching this, so the
    // foreground service kept running and its permanent notification stayed up until the
    // lifecycle handler happened to notice there was no identity left. For a wipe whose whole
    // purpose is that the app looks untouched, that is backwards.
    //
    // Clearing the flag, not merely stopping the service, is deliberate: identity is
    // one-per-install here and a wipe is meant to be a clean break, so a device preference that
    // silently spans two identities the user believes are unrelated is the wrong default. The
    // cost is one prompt at the next setup — exactly the moment to reconsider it.
    unawaited(BackgroundDelivery.setEnabled(false)); // also stops a running service

    // Best-effort cleanup, after the UI has already moved on.
    unawaited(events?.cancel());
    // Peer-facing logout (#7 / §11.6): tell un-backed chats' peers the chat closed (so their
    // mail isn't lost); backed-up chats stay silent. The core prefers the relay store-and-forward
    // so an offline peer still gets the notice, and returns how many it couldn't reach (§1.3). This
    // must run BEFORE the local files are wiped just below (it needs the live transport + relay).
    int notNotified = 0;
    try {
      if (duress) {
        // Every chat is told, not just un-backed ones (no restore is coming), and the whole thing
        // is capped: under coercion the wipe must not be delayed by a peer that won't answer, and
        // must not be preventable by pulling the phone off the network.
        notNotified = await core
                ?.duressLogout()
                .timeout(const Duration(seconds: 5), onTimeout: () => 0) ??
            0;
      } else {
        notNotified = await core?.logout() ?? 0;
      }
    } catch (_) {}
    // Stop the core BEFORE deleting anything it owns. Nulling `_core` above only drops Dart's
    // reference — the Rust instance keeps running, and its next dirty tick re-persists the state
    // file we are about to remove. The resurrected file then outlives the keystore key deleted
    // below, so the next launch finds "saved data, no key" and shows the recovery screen for an
    // identity that was deliberately destroyed. The duress path made this near-certain, since
    // `duressLogout()` marks the state dirty immediately beforehand.
    //
    // Under duress, stop *awaiting* it after a couple of seconds. `shutdown()` waits for the poller
    // to release everything, and on Tor the tail of that is arti's runtime shutting down — seconds
    // of task unwinding whose only purpose is releasing the state lock for a core we are not going
    // to rebuild. The part the wipe actually needs (the poller stopped, its last save done) is over
    // well inside this window; the rest finishes on its own while the files are deleted. A coerced
    // user must not be left watching a spinner.
    try {
      final stopping = core?.shutdown();
      if (stopping != null) {
        await (duress
            ? stopping.timeout(const Duration(seconds: 2), onTimeout: () {})
            : stopping);
      }
    } catch (_) {
      // Best-effort: a core that won't stop cleanly must not block the wipe.
    }
    // Decrypted media must not outlive the identity: drop the in-memory caches and delete
    // the plaintext `nightdrop-media-*` temp files written for the system player.
    unawaited(MediaCache.wipe());
    // Each target is removed independently. This was one `try` around the lot, with a silent
    // catch, and the first statement in it was the keystore delete — so on Android, where that
    // call can throw, EVERY file below it was skipped and nothing said so. The identity still
    // looked destroyed (onboarding overwrites the state file), while the store key, the sealed
    // onion identity, arti's state and the authorized-client files all survived on disk. A wipe
    // that half-succeeds in silence is the one failure mode this code must not have.
    final failed = <String>[];
    Future<void> step(String what, FutureOr<void> Function() act) async {
      try {
        await act();
      } catch (_) {
        failed.add(what);
      }
    }

    // An absent target is not a failure — a wipe with no media, no contacts and no lock is
    // ordinary — so existence is checked first rather than letting `delete()` throw for it.
    void rm(FileSystemEntity target) {
      if (target.existsSync()) target.deleteSync(recursive: true);
    }

    // Overwrite before deleting. If `delete` fails — which is how this whole wipe came to be
    // skipped on Android — the entry that survives is then a random key that unlocks nothing,
    // instead of the key that protected everything just destroyed (and that still unseals any
    // copy of the state file or the sealed onion identity taken off the device). It also lands
    // where a fresh identity wants to be: `_ensureStoreKey` reads it back and treats it as the
    // new key, which is exactly right, since `randomStoreKey` returns a well-formed one.
    await step('store key', () async {
      try {
        await _secure.write(key: _kStoreKeyName, value: await rust.randomStoreKey());
      } catch (_) {
        // Deletion is still worth attempting on its own.
      }
      await _secure.delete(key: _kStoreKeyName);
    });
    String? support;
    await step('app dir', () async {
      support = (await getApplicationSupportDirectory()).path;
    });
    if (support case final dir?) {
      // The state file AND its sidecars: `nightdrop-state.bin.unreadable-*` from a failed launch
      // and `.replaced-*` from a new identity created over an old one. Both are encrypted copies
      // of the identity being destroyed, and deleting the original while leaving them would make
      // the wipe a rename.
      await step('state file', () {
        for (final f in Directory(dir).listSync()) {
          if (f.path.split('/').last.startsWith(_kStateFile)) rm(f);
        }
      });
      await step('media', () => rm(Directory('$dir/nightdrop-media')));
      // Every platform, not just mobile. This used to be Android/iOS only, which left desktop
      // logouts holding the whole Tor state: the onion identity key we were supposedly deleting,
      // and one keystore directory per peer *named after their onion address* — a contact list
      // that outlived the identity. The core drops those keys individually as chats are cleared,
      // but removing the directory is what guarantees nothing is left behind.
      await step('arti state', () => rm(Directory('$dir/arti-state')));
      // Authorized-client files (#22) name one file per contact. They hold public keys, so nothing
      // secret, but the count is still a contact list and it has no reason to outlive the identity.
      await step('client auth', () => rm(Directory('$dir/client-auth')));
      // The sealed onion identity (docs/design/onion-key-at-rest.md). It must go for two reasons:
      // it *is* the identity being destroyed, and leaving it behind breaks the next start outright
      // — a fresh identity has a new store key, the stale file will not unseal under it, and the
      // core treats an unreadable identity as an error rather than silently minting a new address.
      await step('onion key', () => rm(File('$dir/onion-key.sealed')));
    }
    if (failed.isNotEmpty && _diagEnabled) {
      // ignore: avoid_print — the wipe's outcome belongs on the record; names only, no paths.
      print('[nd-diag] wipe: could not remove ${failed.join(', ')}');
    }
    return notNotified;
  }

  @override
  Future<void> sendMessage(String contactId, String text) async {
    // Show the message immediately (faded = not delivered yet), then solidify once the core
    // has handed it to the transport — or remove it and surface the error if the send fails.
    final pending = Message(
      id: 'pending-${_pendingSeq++}',
      contactId: contactId,
      text: text,
      fromMe: true,
      at: DateTime.now(),
      sending: true,
    );
    (_pending[contactId] ??= []).add(pending);
    notifyListeners();
    try {
      final history = await _core!.sendMessage(contactId: contactId, text: text);
      _messages[contactId] = _mapMessages(contactId, history);
      _pending[contactId]?.removeWhere((m) => m.id == pending.id);
      notifyListeners();
    } catch (e) {
      _pending[contactId]?.removeWhere((m) => m.id == pending.id);
      notifyListeners();
      rethrow;
    }
  }

  @override
  Future<void> editMessage(String contactId, String msgId, String text) async {
    await _core!.editMessage(contactId: contactId, msgId: msgId, text: text);
    await _refresh();
  }

  @override
  Future<void> unsendMessage(String contactId, String msgId) async {
    await _core!.unsendMessage(contactId: contactId, msgId: msgId);
    await _refresh();
  }

  @override
  Future<void> sendMedia(String contactId, List<int> data, String mime, String kind,
      List<int> thumb) async {
    // Show an optimistic preview with a spinner immediately, while the (slow over Tor) send
    // runs; _refresh then replaces it with the real, delivered message. For videos the
    // preview is the thumbnail (if any).
    final preview = (kind == 'video') ? (thumb.isEmpty ? null : thumb) : data;
    final pending = Message(
      id: 'pending-${_pendingSeq++}',
      contactId: contactId,
      text: '',
      fromMe: true,
      at: DateTime.now(),
      kind: kind,
      mime: mime,
      mediaSize: data.length,
      sending: true,
      localBytes: preview,
    );
    (_pending[contactId] ??= []).add(pending);
    notifyListeners();
    try {
      await _core!.sendMedia(
          contactId: contactId, data: data, mime: mime, kind: kind, thumb: thumb);
    } finally {
      _pending[contactId]?.removeWhere((m) => m.id == pending.id);
      await _refresh();
    }
  }

  @override
  Future<List<int>> mediaBytes(String mediaId) => _core!.mediaBytes(mediaId: mediaId);

  @override
  Future<String> mediaToFile(String mediaId, String ext) =>
      _core!.mediaToFile(mediaId: mediaId, ext: ext);

  @override
  void setMyNameInChat(String contactId, String name) {
    // Fire-and-refresh: the Rust call is async but the UI seam is synchronous.
    _core!.setMyName(contactId: contactId, name: name).then((_) => _refresh());
  }

  @override
  Future<void> setRemoteStorage(String contactId, bool enabled) async {
    await _core!.setRemoteStorage(contactId: contactId, enabled: enabled);
    await _refresh();
  }

  @override
  Future<void> reportScreenshot(String contactId) async {
    await _core!.reportScreenshot(contactId: contactId);
    await _refresh();
  }

  @override
  Future<void> setDisappearing(String contactId, int secs) async {
    await _core!.setDisappearing(contactId: contactId, secs: BigInt.from(secs));
    await _refresh();
  }

  Future<void> _refresh([rust.AppEvent? event]) async {
    // Contact/request lists are small — always re-read them (a roster change is cheap).
    _contacts = (await _core!.contacts()).map(_map).toList();
    _requests = (await _core!.incomingRequests()).map(_map).toList();
    final known = {..._contacts, ..._requests}.map((c) => c.id).toSet();

    // Pull message history only for the chats that actually changed (§1.5.5). The event names them;
    // an empty/absent hint means "refresh broadly" (roster/control-frame churn), so pull all. Always
    // include any known chat we have no history for yet, so a brand-new chat's first messages show.
    final hinted = event?.contacts.toSet() ?? const <String>{};
    final Iterable<String> toPull = hinted.isEmpty
        ? known
        : {...hinted.where(known.contains), ...known.where((id) => !_messages.containsKey(id))};
    for (final id in toPull) {
      final history = await _core!.messages(contactId: id);
      _messages[id] = _mapMessages(id, history);
      _receivedCache[id] = _receivedCountFor(id);
    }
    // Forget chats that disappeared (deleted/declined) so their history/counts don't linger.
    _messages.removeWhere((id, _) => !known.contains(id));
    _receivedCache.removeWhere((id, _) => !known.contains(id));

    if (!_unreadReady) {
      // Baseline existing history as "read" so a restart doesn't mark old messages unread.
      for (final id in known) {
        _readReceived[id] = _receivedCache[id] ?? 0;
      }
      _unreadReady = true;
    }
    _maybeNotify();
    notifyListeners();
  }

  /// Raise a local notification when, while backgrounded, the number of received messages or
  /// pending requests grows. Generic text only — no message content in the notification.
  void _maybeNotify() {
    final received = _receivedCache.values.fold<int>(0, (a, b) => a + b);
    final requests = _requests.length;
    if (!_countsReady) {
      _knownReceived = received;
      _knownRequests = requests;
      _countsReady = true;
      return;
    }
    if (!_foreground) {
      if (received > _knownReceived) {
        final n = received - _knownReceived;
        NotificationService.show('Night Drop', n == 1 ? 'New message' : '$n new messages');
      }
      if (requests > _knownRequests) {
        NotificationService.show('Night Drop', 'New chat request');
      }
    }
    _knownReceived = received;
    _knownRequests = requests;
  }

  Contact _map(rust.Contact c) => Contact(
        id: c.id,
        theirName: c.theirName,
        myName: c.myName,
        remoteStorage: c.remoteStorage,
        disappearingSecs: c.disappearingSecs.toInt(),
        backedUp: c.backedUp,
        peerBackedUp: c.peerBackedUp,
        verified: c.verified,
        peerVerified: c.peerVerified,
        peerRelays: c.peerRelays,
        remoteStorageHealthy: c.remoteStorageHealthy,
        lastSeenSecs: c.lastSeenSecs.toInt(),
        localName: c.localName,
        identityTag: c.identityTag,
      );

  List<Message> _mapMessages(String contactId, List<rust.ChatMessage> history) {
    var i = 0;
    return history
        .map((m) => Message(
              id: '$contactId-${i++}',
              contactId: contactId,
              text: m.text,
              fromMe: m.fromMe,
              // Core timestamps are unix seconds; 0 = pre-timestamp message (treat as
              // now so it can't accidentally look freshly editable... it has no msgId
              // anyway, so canEdit stays false).
              at: m.at == BigInt.zero
                  ? DateTime.now()
                  : DateTime.fromMillisecondsSinceEpoch(m.at.toInt() * 1000),
              msgId: m.msgId,
              edited: m.edited,
              system: m.system,
              kind: m.kind,
              mime: m.mime,
              mediaId: m.mediaId,
              mediaSize: m.mediaSize.toInt(),
              transferId: m.transferId,
              thumbId: m.thumbId,
              delivery: m.delivery,
            ))
        .toList();
  }
}
