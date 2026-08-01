import 'dart:async';
import 'dart:math';

import 'nightdrop_core.dart';
import 'models.dart';

/// In-memory [NightdropCore] for development. No crypto, no network — it just makes the UI
/// flows real and clickable. Replace with `RustNightdropCore` (flutter_rust_bridge) later.
///
/// To make the chat feel alive, sent messages get a canned "(echo)" reply from the peer.
class MockNightdropCore extends NightdropCore {
  final Random _rng = Random();
  final Map<String, List<Message>> _messages = {};

  Identity? _identity;
  final List<Contact> _contacts = [];
  final List<Contact> _requests = [];

  @override
  Identity? get identity => _identity;

  @override
  bool get isBooting => false;

  @override
  Future<void> start() async {} // no persistence in the mock

  @override
  void setLifecycle(bool foreground) {}

  @override
  Future<bool> onionReady() async => true;

  @override
  List<Contact> get contacts => List.unmodifiable(_contacts);

  @override
  List<Contact> get incomingRequests => List.unmodifiable(_requests);

  @override
  List<Message> messagesFor(String contactId) =>
      List.unmodifiable(_messages[contactId] ?? const []);

  @override
  int unreadCount(String contactId) => 0;

  @override
  void markRead(String contactId) {}

  @override
  Future<void> createIdentity() async {
    await _fakeWork();
    _identity = Identity(id: 'nightdrop:${_token(10)}');
    notifyListeners();
  }

  @override
  Future<PairingInvite> createInvite() async {
    await _fakeWork();
    // Wormhole-style `slot-secret-words`: slot is non-secret, words feed the PAKE.
    final slot = _rng.nextInt(99) + 1;
    final code = '$slot-${_word()}-${_word()}-${_word()}';
    // Simulate a peer joining via this invite -> a pending request to approve.
    _requests.add(Contact(id: 'nightdrop:${_token(10)}'));
    notifyListeners();
    return PairingInvite(
        shortCode: code, qrPayload: 'nightdrop://pair?b=${_token(24)}');
  }

  @override
  Future<String> createBackup(bool full) async {
    await _fakeWork();
    return '${_word()}-${_word()}-${_word()}-${_token(4)}'.toUpperCase();
  }

  @override
  Future<void> saveBackup(String path) => _fakeWork();

  @override
  Future<List<int>> backupBytes() async => List<int>.filled(64, 7);

  @override
  Future<String> createChatBackup(String contactId, bool full) async {
    await _fakeWork();
    return '${_word()}-${_word()}-${_word()}-${_token(4)}'.toUpperCase();
  }

  @override
  Future<int> mergeBackup(String path, String password) async {
    await _fakeWork();
    return 0;
  }

  @override
  Future<String> safetyNumber(String contactId) async =>
      '12345 67890 12345 67890 12345 67890 12345 67890 12345 67890 12345 67890';

  @override
  Future<String> safetyQr(String contactId) async => 'mock-safety-qr';

  @override
  Future<bool> verifySafetyQr(String contactId, String scanned) async => true;

  @override
  Future<void> setVerified(String contactId, bool verified) async {
    final i = _contacts.indexWhere((c) => c.id == contactId);
    if (i != -1) _contacts[i].verified = verified;
    notifyListeners();
  }

  final List<String> _myRelays = [];

  @override
  Future<List<String>> myRelays() async => List.of(_myRelays);

  @override
  Future<void> setMyRelays(List<String> relays) async {
    _myRelays
      ..clear()
      ..addAll(relays.where((r) => r.trim().isNotEmpty).map((r) => r.trim()));
    notifyListeners();
  }

  @override
  Future<List<RelayHealth>> relayHealth() async =>
      // Mock: pretend every advertised relay is reachable.
      _myRelays.map((a) => RelayHealth(address: a, reachable: true)).toList();

  @override
  Future<String> createRelayAccessKey(String relayOnion) async =>
      // Mock: a plausible-looking descriptor key so UI can be exercised without Tor.
      'descriptor:x25519:${_token(52).toUpperCase()}';

  @override
  Future<ServerBackup> createServerBackup(int ttlHours, bool full) async {
    await _fakeWork();
    return ServerBackup(
      password: '${_word()}-${_word()}-${_word()}-${_token(4)}'.toUpperCase(),
      expiresAt: DateTime.now().add(Duration(hours: ttlHours.clamp(1, 36))),
    );
  }

  @override
  Future<void> importBackup(String path, String password) async {
    await _fakeWork();
    _identity = Identity(id: 'nightdrop:${_token(10)}');
    notifyListeners();
  }

  @override
  Future<void> importServerBackup(String password) async {
    await _fakeWork();
    _identity = Identity(id: 'nightdrop:${_token(10)}');
    notifyListeners();
  }

  @override
  Future<void> deleteChat(String contactId) async {
    await _fakeWork();
    _contacts.removeWhere((c) => c.id == contactId);
    _requests.removeWhere((c) => c.id == contactId);
    _messages.remove(contactId);
    notifyListeners();
  }

  @override
  Future<int> logout() async {
    await _fakeWork();
    _identity = null;
    _contacts.clear();
    _requests.clear();
    _messages.clear();
    notifyListeners();
    return 0; // the mock reaches every (fake) peer
  }

  @override
  Future<void> authorize(String contactId, bool accept) async {
    await _fakeWork();
    final i = _requests.indexWhere((c) => c.id == contactId);
    if (i < 0) return;
    final request = _requests.removeAt(i);
    if (accept) _contacts.add(request);
    notifyListeners();
  }

  @override
  Future<Contact> joinWithShortCode(String code) async {
    await _fakeWork();
    if (code.trim().isEmpty) {
      throw const FormatException('Enter a code like 4-cedar-lantern-river');
    }
    final contact = Contact(id: 'nightdrop:${_token(10)}');
    _contacts.add(contact);
    notifyListeners();
    return contact;
  }

  @override
  Future<void> sendMessage(String contactId, String text) async {
    final trimmed = text.trim();
    if (trimmed.isEmpty) return;
    final id = _token(8);
    _append(Message(
      id: id,
      contactId: contactId,
      text: trimmed,
      fromMe: true,
      at: DateTime.now(),
      msgId: id, // mock: reuse the id so editing works
    ));
    notifyListeners();

    // Simulate a peer reply so the UI updates.
    unawaited(Future<void>.delayed(const Duration(milliseconds: 600), () {
      _append(Message(
        id: _token(8),
        contactId: contactId,
        text: '(echo) $trimmed',
        fromMe: false,
        at: DateTime.now(),
      ));
      notifyListeners();
    }));
  }

  @override
  Future<void> editMessage(String contactId, String msgId, String text) async {
    final list = _messages[contactId];
    if (list == null) return;
    final i = list.indexWhere((m) => m.msgId == msgId && m.fromMe);
    if (i < 0) return;
    final old = list[i];
    list[i] = Message(
      id: old.id,
      contactId: old.contactId,
      text: text,
      fromMe: true,
      at: old.at,
      msgId: old.msgId,
      edited: true,
      delivery: old.delivery,
    );
    notifyListeners();
  }

  @override
  Future<void> unsendMessage(String contactId, String msgId) async {
    final list = _messages[contactId];
    if (list == null) return;
    final i = list.indexWhere((m) => m.msgId == msgId && m.fromMe);
    if (i < 0) return;
    final old = list[i];
    list[i] = Message(
      id: old.id,
      contactId: old.contactId,
      text: '',
      fromMe: true,
      at: old.at,
      msgId: old.msgId,
      kind: 'deleted',
    );
    notifyListeners();
  }

  final Map<String, List<int>> _media = {};

  @override
  Future<void> sendMedia(String contactId, List<int> data, String mime,
      String kind, List<int> thumb) async {
    await _fakeWork();
    final id = _token(10);
    _media[id] = data;
    _append(Message(
      id: _token(8),
      contactId: contactId,
      text: '',
      fromMe: true,
      at: DateTime.now(),
      kind: kind,
      mime: mime,
      mediaId: id,
      mediaSize: data.length,
    ));
    notifyListeners();
  }

  @override
  Future<List<int>> mediaBytes(String mediaId) async =>
      _media[mediaId] ?? const [];

  @override
  Future<String> mediaToFile(String mediaId, String ext) async => '';

  @override
  void setMyNameInChat(String contactId, String name) {
    final c = _contacts.firstWhere((c) => c.id == contactId);
    c.myName = name.trim().isEmpty ? kDefaultName : name.trim();
    notifyListeners();
  }

  @override
  Future<void> setLocalName(String contactId, String name) async {
    final c = _contacts.firstWhere((c) => c.id == contactId);
    c.localName = name.trim();
    notifyListeners();
  }

  @override
  Future<void> setRemoteStorage(String contactId, bool enabled) async {
    await _fakeWork();
    final c = _contacts.firstWhere((c) => c.id == contactId);
    c.remoteStorage = enabled;
    c.remoteStorageExpiresAt =
        enabled ? DateTime.now().add(const Duration(hours: 24)) : null;
    notifyListeners();
  }

  @override
  Future<void> setDisappearing(String contactId, int secs) async {
    await _fakeWork();
    _contacts.firstWhere((c) => c.id == contactId).disappearingSecs = secs;
    notifyListeners();
  }

  // --- helpers ---

  void _append(Message m) => (_messages[m.contactId] ??= []).add(m);

  Future<void> _fakeWork() =>
      Future<void>.delayed(const Duration(milliseconds: 250));

  String _token(int n) {
    const chars = 'abcdefghijklmnopqrstuvwxyz0123456789';
    return List.generate(n, (_) => chars[_rng.nextInt(chars.length)]).join();
  }

  String _word() {
    const words = [
      'cedar',
      'lantern',
      'river',
      'ember',
      'willow',
      'cobalt',
      'harbor',
      'thistle',
      'quartz',
      'meadow',
      'cinder',
      'aurora',
    ];
    return words[_rng.nextInt(words.length)];
  }
}
