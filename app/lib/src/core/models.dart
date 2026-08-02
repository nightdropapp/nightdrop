/// Plain data models shared by the UI and the [NightdropCore] seam.
///
/// These mirror the Rust FFI types in `core/src/api.rs`. When the
/// `flutter_rust_bridge` bindings are generated, keep the two in sync (or re-export the
/// generated types from here).
library;

/// Default per-chat display name for both parties (ARCHITECTURE.md §4).
const String kDefaultName = 'Anon';

/// A compact, readable form of a long base64 identity id for display (e.g. in chat headers
/// and request tiles). Full ids remain available via "view identity" surfaces.
String shortId(String id) {
  if (id.length <= 16) return id;
  return '${id.substring(0, 8)}…${id.substring(id.length - 6)}';
}

/// The result of enabling an opt-in server backup (§7c): the one-time recovery [password]
/// (shown once, never persisted) and the exact time the server copy [expiresAt].
class ServerBackup {
  const ServerBackup({required this.password, required this.expiresAt});

  final String password;
  final DateTime expiresAt;
}

/// An anonymous, device-held identity. No account, no registration.
class Identity {
  const Identity({required this.id});

  /// Stable public identifier derived from the identity key — not a username.
  final String id;
}

/// How a pairing was initiated.
enum PairingMethod {
  /// QR code: pre-authorized, embeds the onion address, skips the rendezvous (§5a).
  qr,

  /// Short code `slot-secret-words`: rendezvous lookup + PAKE bouncer (§5b).
  shortCode,
}

/// An invite the inviter shows to the other person.
class PairingInvite {
  const PairingInvite({required this.shortCode, required this.qrPayload});

  /// Wormhole-style `slot-secret-words`. The secret words feed the PAKE and are never
  /// sent to the rendezvous.
  final String shortCode;

  /// Pre-authorized bundle encoded into the QR (embeds the onion address).
  final String qrPayload;
}

/// A 1:1 conversation partner.
class Contact {
  Contact({
    required this.id,
    this.theirName = kDefaultName,
    this.myName = kDefaultName,
    this.remoteStorage = false,
    this.remoteStorageExpiresAt,
    this.disappearingSecs = 0,
    this.backedUp = false,
    this.peerBackedUp = false,
    this.verified = false,
    this.peerVerified = false,
    this.peerRelays = const [],
    this.remoteStorageHealthy = true,
    this.lastSeenSecs = 0,
    this.localName = '',
    this.identityTag = '',
  });

  final String id;

  /// The other party's display name in this chat (default "Anon").
  String theirName;

  /// Your own display name in this chat — you can rename yourself per-chat (§4).
  String myName;

  /// Whether opt-in 24h server storage is active for this chat (§6). While true, both
  /// parties must see a persistent in-chat warning.
  bool remoteStorage;

  /// When remote storage lapses (the 24h cap), if active.
  DateTime? remoteStorageExpiresAt;

  /// Per-chat disappearing-messages timer in seconds (0 = off). A shared setting synced to
  /// the peer; messages older than this are deleted on both devices.
  int disappearingSecs;

  /// Whether we have this chat in a backup (#7). If false, the peer is told the chat closed
  /// when we delete our identity (§11.6).
  bool backedUp;

  /// Whether the peer keeps a (Full) backup of this chat (#7) — drives a persistent
  /// transparency warning that our messages may persist in their backup.
  bool peerBackedUp;

  /// Whether the user has verified this contact's safety number out-of-band (key-verification).
  /// A re-paired (new-key) contact starts unverified.
  bool verified;

  /// Whether the **peer** signaled that *they* verified this chat's safety number —
  /// informational only. Shown as "the other person marked this verified"; it never implies
  /// our own [verified]. Each side must still confirm the number itself. Resets on a re-pair.
  bool peerVerified;

  /// The peer's advertised **extra** relay addresses (#17). We fan offline mail out to these
  /// in addition to the shared primary relay, so a message reaches them even if one relay is
  /// down or censored. Synced in-band via the peer's relay-set announcement.
  List<String> peerRelays;

  /// Whether opt-in server storage is currently working. `false` when it's on but the last send
  /// couldn't reach any relay to store the copy (the message still reached the peer directly), so
  /// the storage banner is downgraded to "not currently stored". Always `true` when storage is off.
  bool remoteStorageHealthy;

  /// Unix seconds of the last **authenticated** contact from this peer — a decrypted message, or a
  /// control frame that verified on their ratchet (including the silent delivery ack, so a peer who
  /// reads without replying still counts as alive). `0` = no reading yet.
  ///
  /// Drives the "no sign of them" banner, which reports **silence and never a cause**: a wiped
  /// identity, a seized phone and a holiday are indistinguishable from here. See
  /// `docs/design/silence-detection.md`.
  int lastSeenSecs;

  /// A nickname **you** gave this contact. Local only — never sent, never announced. Takes
  /// precedence over [theirName], which is whatever the peer chose (or "Anon" forever).
  String localName;

  /// Six characters derived from the contact's identity key, so two unnamed contacts are still
  /// distinguishable. **Not verification** — short enough to grind, so a matching tag proves
  /// nothing; the safety number is the check. See `docs/design/contact-naming.md`.
  String identityTag;

  /// What to call this contact in a list or title: your nickname if you set one, else the name
  /// they chose. Never empty.
  String get displayName => localName.isNotEmpty ? localName : theirName;

  /// Whether the identity tag should be shown alongside [displayName]. Suppressed once you have
  /// named them yourself — that is the point at which you have vouched for who this is; until
  /// then, two contacts can both call themselves "Anon", or both call themselves "Alex".
  bool get showIdentityTag => localName.isEmpty && identityTag.isNotEmpty;
}

/// Reachability of one of *our* advertised extra relays (#17), for the relay-status surface.
class RelayHealth {
  const RelayHealth({required this.address, required this.reachable});

  /// The relay address we advertise (`.onion` or `host:port`).
  final String address;

  /// Whether it answered our mailbox drain on the last poll. `false` = likely offline.
  final bool reachable;
}

/// A single message in a 1:1 conversation.
class Message {
  const Message({
    required this.id,
    required this.contactId,
    required this.text,
    required this.fromMe,
    required this.at,
    this.msgId = '',
    this.edited = false,
    this.system = false,
    this.kind = 'text',
    this.mime = '',
    this.mediaId = '',
    this.mediaSize = 0,
    this.transferId = '',
    this.thumbId = '',
    this.delivery = '',
    this.sending = false,
    this.localBytes,
  });

  final String id;
  final String contactId;
  final String text;
  final bool fromMe;
  final DateTime at;

  /// Core-assigned per-message token (shared with the peer) that names this message for
  /// edits. Empty for system notices, media, and optimistic pending messages.
  final String msgId;

  /// True once the text was replaced by an edit — the bubble shows an "edited" tag.
  final bool edited;

  /// Whether this message can still be edited: our own delivered/sent text within the
  /// 15-minute window, or at any age while still queued on the relay (the peer hasn't
  /// seen it). Mirrors the rule enforced by the Rust core.
  bool get canEdit =>
      fromMe &&
      isText &&
      !system &&
      !sending &&
      msgId.isNotEmpty &&
      (delivery == 'queued' ||
          DateTime.now().difference(at) < const Duration(minutes: 15));

  /// A local system notice (e.g. a chat was deleted or approved) rather than an exchanged
  /// message. Rendered centered, without a sender name or bubble side.
  final bool system;

  /// "text" (default), "image", or "video". For media the bytes are fetched on demand from
  /// the core by [mediaId]; [mime] and [mediaSize] (bytes) describe the attachment.
  final String kind;
  final String mime;
  final String mediaId;
  final int mediaSize;

  /// Correlates a video's "incoming" placeholder with its arriving payload.
  final String transferId;

  /// Sealed-file id of a video's preview thumbnail (empty if none).
  final String thumbId;

  /// Delivery state of an outgoing message. Drives the sender badge:
  ///
  /// * `"sent"` — handed to the peer's onion. Their device has **not** confirmed it: the dial
  ///   succeeding says the service answered, not that the app processed the frame.
  /// * `"queued"` — held on a relay for an offline peer.
  /// * `"delivered"` — their device sent a receipt naming *this* message (`Frame::Delivered`).
  /// * `"expired"` — reaped from the relay unread.
  ///
  /// The `"sent"`/`"delivered"` split is load-bearing: `"sent"` used to be terminal and drew no
  /// badge, so a message lost in flight was indistinguishable from one that arrived.
  final String delivery;

  /// True for a locally-shown optimistic media message that is still uploading over Tor.
  final bool sending;

  /// Bytes for an optimistic preview (so a sent image shows instantly without waiting for
  /// the core round-trip). Dart-only; null for messages loaded from the core.
  final List<int>? localBytes;

  /// Empty kind comes from messages persisted before media existed — treat as text.
  bool get isText => kind == 'text' || kind.isEmpty;
  bool get isImage => kind == 'image';
  bool get isVideo => kind == 'video';

  /// An "unsent" (deleted-for-both) message: rendered as a tombstone, not editable.
  bool get isDeleted => kind == 'deleted';

  /// Whether we can unsend this message — identical eligibility to [canEdit] (our own
  /// recent/queued text). Kept separate so the bubble menu can label the action distinctly.
  bool get canUnsend => canEdit;

  /// A received video whose payload hasn't arrived yet (only the incoming placeholder).
  bool get receiving => isVideo && mediaId.isEmpty && !sending;
}
