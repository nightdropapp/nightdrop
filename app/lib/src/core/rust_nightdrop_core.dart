import 'dart:async';
import 'dart:io' show File, Directory, Platform;

import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:path_provider/path_provider.dart';

import '../rust/api.dart' as rust;
import 'nightdrop_core.dart';
import 'media_cache.dart';
import 'models.dart';
import 'notifications.dart';

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
  static const _kStoreKeyName = 'ghost_store_key';
  static const _kStateFile = 'ghost-state.bin';

  Future<String> _stateFilePath() async =>
      '${(await getApplicationSupportDirectory()).path}/$_kStateFile';

  /// Tor's state base dir (also where the persisted state file lives). We pin it to the app
  /// support dir on every platform — mobile needs an explicit writable dir, and on desktop a
  /// known location lets a backup fold in the onion keystore so restore keeps the same `.onion`.
  Future<String?> _torStateDir() async =>
      (await getApplicationSupportDirectory()).path;

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
  // physical devices talk. It is selected by GHOST_LISTEN + GHOST_RELAY, resolved from
  // either a compile-time --dart-define (the only option on Android) or, on desktop, a
  // process env var. If neither is set we fall back to the in-process demo core.
  // Tor mode (GHOST_TOR=1) is the production WAN path: each device bootstraps an embedded
  // Tor client and gets a reachable .onion, so peers pair (by QR — no relay needed) and
  // chat over any network including LTE. It takes precedence over TCP networked mode.
  static const String _defineTor = String.fromEnvironment('GHOST_TOR');
  static const String _defineListen = String.fromEnvironment('GHOST_LISTEN');
  static const String _defineRelay = String.fromEnvironment('GHOST_RELAY');

  static String? _config(String key, String define) {
    if (define.isNotEmpty) return define;
    final env = Platform.environment[key];
    return (env != null && env.isNotEmpty) ? env : null;
  }

  static String? get _listenAddr => _config('GHOST_LISTEN', _defineListen);
  static String? get _relayAddr => _config('GHOST_RELAY', _defineRelay);
  static bool get _torEnabled {
    final v = _config('GHOST_TOR', _defineTor)?.toLowerCase();
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

  static const _kBackedUp = 'ghost_backed_up';
  static const _kBackupSnoozeUntil = 'ghost_backup_snooze_until';

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

  @override
  Future<void> start() async {
    // Auto-restore a persisted identity on launch (Tor mode only). If a secure-store key and
    // a saved state file both exist, rebuild the same identity + chats; else fall through to
    // onboarding.
    try {
      if (_torEnabled) {
        final key = await _secure.read(key: _kStoreKeyName);
        final statePath = await _stateFilePath();
        if (key != null && File(statePath).existsSync()) {
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
          } catch (e) {
            _core = null;
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
        }
      }
    } catch (e) {
      // An error OUTSIDE the load of an existing file (e.g. reading the keystore, resolving the
      // state dir). No confirmed on-disk identity to protect here — fall back to onboarding.
      _core = null;
      _identity = null;
    } finally {
      _booting = false;
      notifyListeners();
    }
  }

  /// Copy an unreadable state file aside before anything can overwrite it, so a transient failure
  /// (or a bug fixed in a later build) doesn't cost the user their data. Best-effort.
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
    final listen = _listenAddr;
    final relay = _relayAddr;
    if (_torEnabled) {
      // Embedded Tor: a reachable .onion, WAN-capable. Bootstrapping takes a while.
      // On mobile, arti needs an explicit writable state dir (the app's support dir). A
      // persistence key (held in the OS secure store) makes the identity survive restarts.
      final key = await _secure.read(key: _kStoreKeyName) ?? await rust.randomStoreKey();
      await _secure.write(key: _kStoreKeyName, value: key);
      _core = await rust.NightdropCore.newTor(
        stateDir: await _torStateDir(),
        relayAddr: relay,
        persistPath: await _stateFilePath(),
        persistKey: key,
      );
      _tor = true;
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
    if (_torEnabled) {
      // Restore the backup onto Tor and persist it (so it survives future restarts too).
      final key = await _secure.read(key: _kStoreKeyName) ?? await rust.randomStoreKey();
      await _secure.write(key: _kStoreKeyName, value: key);
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
    final key = await _secure.read(key: _kStoreKeyName) ?? await rust.randomStoreKey();
    await _secure.write(key: _kStoreKeyName, value: key);
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
  Future<int> logout() async {
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

    // Best-effort cleanup, after the UI has already moved on.
    unawaited(events?.cancel());
    // Peer-facing logout (#7 / §11.6): tell un-backed chats' peers the chat closed (so their
    // mail isn't lost); backed-up chats stay silent. The core prefers the relay store-and-forward
    // so an offline peer still gets the notice, and returns how many it couldn't reach (§1.3). This
    // must run BEFORE the local files are wiped just below (it needs the live transport + relay).
    int notNotified = 0;
    try {
      notNotified = await core?.logout() ?? 0;
    } catch (_) {}
    // Decrypted media must not outlive the identity: drop the in-memory caches and delete
    // the plaintext `ghost-media-*` temp files written for the system player.
    unawaited(MediaCache.wipe());
    try {
      await _secure.delete(key: _kStoreKeyName);
      final support = (await getApplicationSupportDirectory()).path;
      final state = File('$support/$_kStateFile');
      if (state.existsSync()) state.deleteSync();
      final media = Directory('$support/ghost-media');
      if (media.existsSync()) media.deleteSync(recursive: true);
      if (Platform.isAndroid || Platform.isIOS) {
        final artiState = Directory('$support/arti-state');
        if (artiState.existsSync()) artiState.deleteSync(recursive: true);
      }
    } catch (_) {
      // Ignore wipe failures — the in-memory identity is already gone.
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
        peerRelays: c.peerRelays,
        remoteStorageHealthy: c.remoteStorageHealthy,
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
