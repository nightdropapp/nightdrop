//! A single device's real messaging logic (`ARCHITECTURE.md` §5–§6): identity, an
//! established Olm session per contact, and framed send/receive over a [`Transport`].
//! This is the genuine protocol — two `Node`s on any transport actually pair and
//! converse. `api::NightdropCore` wraps a `Node` for the app (and, for now, an in-process
//! demo peer); the network transport (Tor) and relay slot in behind the same trait.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use vodozemac::olm::{Account, AccountPickle, Session, SessionPickle};
use zeroize::Zeroize as _;

use crate::api::{ChatMessage, Contact};
use crate::crypto;
use crate::identity::{LocalIdentity, PreKeyBundle};
use crate::relay_client::RelayClient;
use crate::storage::{PersistedChat, PersistedMessage, PersistedState, StoreKey};
use crate::transport::{Address, Transport};
use crate::wire::{self, Frame, WireOlm};
use crate::{Result, DEFAULT_NAME};

/// How long an offline message sits in the relay before expiring (§6). Also the device-side
/// time-bomb horizon for opt-in server-storage (ephemeral) chats (§11.4), and the age past
/// which a still-unacked queued message is shown as "expired" (§11.3).
const RELAY_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long after sending a text message it can still be edited (product rule; a message
/// still queued on the relay stays editable regardless, since the peer never saw it).
const EDIT_WINDOW: Duration = Duration::from_secs(15 * 60);

/// Maximum attachment size (100 MB). Larger files are rejected — they would be slow over
/// Tor and memory-heavy to carry through one frame.
const MAX_MEDIA_BYTES: u64 = 100 * 1024 * 1024;

/// Backup salt length and the server-backup retention cap (§7c: default 24h, max 36h).
const BACKUP_SALT_LEN: usize = 16;
const SERVER_BACKUP_MAX_TTL: Duration = Duration::from_secs(36 * 60 * 60);
const SERVER_BACKUP_DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Fixed domain markers encrypted inside the authenticated control frames
/// ([`Closed`](Frame::Closed)/[`Ack`](Frame::Ack)/[`BackedUp`](Frame::BackedUp)). The receiver
/// decrypts on the peer's ratchet and checks the plaintext equals the expected marker — this both
/// authenticates the sender (only their session decrypts) and binds the ciphertext to its frame
/// type (so an `Ack`'s ciphertext can't be repackaged as a `Closed`). Distinct, domain-separated.
const MARK_CLOSED: &[u8] = b"nightdrop/ctl/closed/v1";
const MARK_ACK: &[u8] = b"nightdrop/ctl/ack/v1";
const MARK_BACKEDUP: &[u8] = b"nightdrop/ctl/backedup/v1";

/// Dev-only logging. Identity keys, invite codes, and decrypted display names must never
/// reach release logs (Android's logcat persists them for any `adb`-connected observer);
/// `cfg!` compiles the whole call out of release builds.
macro_rules! devlog {
    ($($t:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!($($t)*);
        }
    };
}

/// Unlinkable store-and-forward mailbox handle for a recipient (§6/§11.2): a truncated
/// SHA-256 of their long-term identity key. The relay sees only this opaque token —
/// never an onion address (which it could probe) or a raw identity key. Both ends can
/// compute it: senders know their contact's identity key, receivers their own.
fn mailbox_handle(recipient_identity_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"nightdrop/mailbox/v1");
    h.update(recipient_identity_key.as_bytes());
    format!("mbx:{}", base64_handle(&h.finalize()[..15]))
}

/// Key for sealing relay-queued envelopes, derived from the recipient's identity key
/// (domain-separated from the mailbox handle). Wire frames carry routing metadata in the
/// clear — sender identity keys, and the sender's onion address in `Hello` — which is fine
/// peer-to-peer (Tor encrypts to the endpoint) but must not sit readable on the relay.
/// Sealing under this key hides all of it: the relay stores ciphertext addressed by an
/// opaque token, and only someone who knows the recipient's identity key (the recipient
/// and their contacts) can even parse the envelope. Message *content* is additionally
/// Double-Ratchet encrypted inside, as always.
fn relay_wrap_key(recipient_identity_key: &str) -> crate::storage::StoreKey {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"nightdrop/relay-wrap/v1");
    h.update(recipient_identity_key.as_bytes());
    h.finalize().into()
}

/// Seal a wire frame for the relay queue (see [`relay_wrap_key`]).
fn relay_wrap(recipient_identity_key: &str, frame_bytes: &[u8]) -> Result<Vec<u8>> {
    crate::storage::seal(&relay_wrap_key(recipient_identity_key), frame_bytes)
}

/// Open a relay-queued envelope addressed to us. Fails on blobs not sealed to our key
/// (garbage posted to our mailbox) — callers skip those rather than abort the drain.
fn relay_unwrap(own_identity_key: &str, blob: &[u8]) -> Result<Vec<u8>> {
    crate::storage::open(&relay_wrap_key(own_identity_key), blob)
}

/// Build a [`RelayClient`] for an arbitrary relay address: over Tor via the transport's dialer
/// (anonymized), or a direct connection when the transport can't dial relays by address
/// (tests/TCP). Free fn (not a method) so call sites keep disjoint field borrows (#17).
fn build_relay(transport: &dyn Transport, addr: &str) -> RelayClient {
    match transport.relay_dialer(addr) {
        Some(dialer) => RelayClient::with_dialer(dialer),
        None => RelayClient::new(addr),
    }
}

/// Create a directory with owner-only permissions (0700 on unix) — for the decrypted-media
/// scratch (§1.4), which must not be readable by other apps/users on a shared device.
fn create_private_dir(dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write a file with owner-only permissions (0600 on unix) — for the decrypted-media scratch (§1.4).
fn write_private_file(path: &std::path::Path, data: &[u8]) -> Result<()> {
    use std::io::Write as _;
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::File::create(path)?;
    f.write_all(data)?;
    Ok(())
}

/// What a relay poll needs to do its blocking round-trips, snapshotted under the core lock so the
/// round-trips themselves can run **without** the lock (§1.5.2). The clients are cheap clones (a
/// TCP address or an `Arc` dialer). Built by [`Node::relay_drain_plan`], consumed by
/// [`drain_relay_mailboxes`].
pub(crate) struct RelayDrainPlan {
    handle: String,
    /// `(advertised-address, client)` per relay; `None` address = the primary/default relay.
    clients: Vec<(Option<String>, RelayClient)>,
}

/// The result of draining the relay mailboxes lock-free: the raw blobs (fan-out duplicates still
/// present — de-duplicated later in [`Node::apply_relay_harvest`]) and each *addressed* relay's
/// reachability, to fold back into relay-health once the lock is re-acquired.
pub(crate) struct RelayHarvest {
    blobs: Vec<Vec<u8>>,
    reachability: Vec<(String, bool)>,
}

/// Drain every relay mailbox in `plan` — the blocking `take` round-trips — with **no** core lock
/// held (§1.5.2), so UI calls aren't stalled for seconds behind an in-flight Tor relay poll. A
/// relay that errors is recorded unreachable and skipped, never aborting the drain from the rest.
pub(crate) fn drain_relay_mailboxes(plan: &RelayDrainPlan) -> RelayHarvest {
    let mut blobs = Vec::new();
    let mut reachability = Vec::new();
    for (addr, client) in &plan.clients {
        match client.take(&plan.handle) {
            Ok(taken) => {
                if let Some(addr) = addr {
                    reachability.push((addr.clone(), true));
                }
                blobs.extend(taken);
            }
            Err(_) => {
                if let Some(addr) = addr {
                    reachability.push((addr.clone(), false));
                }
            }
        }
    }
    RelayHarvest {
        blobs,
        reachability,
    }
}

/// Seal `bytes` **once** and queue the identical blob on every relay that hosts `contact_id`'s
/// mailbox — the primary (shared default) plus the recipient's advertised `peer_relays` (#17
/// fan-out). Best-effort: succeeds if ≥1 relay accepts. Returns each `(relay, receipt)` so an
/// edit/unsend can later recall every copy. Posting the identical sealed bytes to all relays
/// lets the receiver de-duplicate by blob hash.
fn queue_on_relays(
    transport: &dyn Transport,
    primary: &Option<RelayClient>,
    peer_relays: &[String],
    contact_id: &str,
    bytes: &[u8],
) -> Result<Vec<QueuedReceipt>> {
    let sealed = relay_wrap(contact_id, bytes)?;
    let handle = mailbox_handle(contact_id);
    let mut copies = Vec::new();
    if let Some(primary) = primary {
        if let Ok(r) = primary.post(&handle, &sealed, RELAY_TTL) {
            copies.push(QueuedReceipt {
                relay_addr: None,
                msg_id: r.msg_id,
                delete_token: r.delete_token,
            });
        }
    }
    for addr in peer_relays {
        let relay = build_relay(transport, addr);
        if let Ok(r) = relay.post(&handle, &sealed, RELAY_TTL) {
            copies.push(QueuedReceipt {
                relay_addr: Some(addr.clone()),
                msg_id: r.msg_id,
                delete_token: r.delete_token,
            });
        }
    }
    if copies.is_empty() {
        anyhow::bail!("peer offline and no relay accepted the message");
    }
    Ok(copies)
}

/// Recall every still-queued copy named by `receipts` — reconstructing each [`RelayClient`] from its
/// `relay_addr` (so this works after a restart, when no live client is held) and deleting the blob by
/// its `delete_token`. Returns true only when **every** recorded copy was recalled.  A partial
/// recall is not enough to call an edit/delete invisible: a sibling relay may still deliver the
/// original, so callers must send the normal E2E edit/unsend frame in that case. A free fn (not a
/// method) so callers keep disjoint field borrows while `self.chats` is borrowed (#17).
fn recall_receipts(
    transport: &dyn Transport,
    primary: &Option<RelayClient>,
    contact_id: &str,
    receipts: &[QueuedReceipt],
) -> bool {
    let handle = mailbox_handle(contact_id);
    if receipts.is_empty() {
        return false;
    }
    let mut all_recalled = true;
    for r in receipts {
        let relay = match &r.relay_addr {
            None => primary.clone(),
            Some(addr) => Some(build_relay(transport, addr)),
        };
        let Some(relay) = relay else {
            all_recalled = false;
            continue;
        };
        let receipt = crate::relay_client::PostReceipt {
            msg_id: r.msg_id.clone(),
            delete_token: r.delete_token.clone(),
        };
        if !relay.recall(&handle, &receipt).unwrap_or(false) {
            all_recalled = false;
        }
    }
    all_recalled
}

/// Mark this chat's relay-"queued" outgoing messages as "delivered" — called when the peer is
/// observed reachable (a direct send succeeded, we received a message from them, or they acked
/// a relay drain), meaning they have the messages.
fn flip_queued_delivered(history: &mut [ChatMessage]) {
    for m in history.iter_mut() {
        if m.from_me && m.delivery == "queued" {
            m.delivery = "delivered".to_string();
        }
    }
}

/// The sender identity of a frame representing a delivered **user message** (so draining it from
/// the relay warrants a delivery ack). `None` for control frames (Hello/Approved/Closed/Ack/…).
fn user_frame_sender(frame: &Frame) -> Option<String> {
    match frame {
        Frame::Message { from, .. }
        | Frame::Media { from, .. }
        | Frame::MediaIncoming { from, .. }
        | Frame::Edit { from, .. }
        | Frame::Unsend { from, .. } => Some(from.clone()),
        _ => None,
    }
}

/// A random per-message id (96 bits, URL-safe base64). Shared by both sides via the wire
/// frame so edits can name their target; carries no identity or ordering information.
fn random_msg_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    base64_handle(&b)
}

/// A recall receipt for one still-queued copy of a message, in the form we can both **use**
/// (reconstruct a [`RelayClient`] from `relay_addr` and delete the blob) and **persist**. `relay_addr`
/// is `None` for the primary relay, or an advertised extra relay's address (#17). See
/// [`Chat::relay_receipts`] and [`recall_receipts`].
#[derive(Clone)]
struct QueuedReceipt {
    /// `None` = the primary (baked-in) relay; `Some(addr)` = an advertised extra relay.
    relay_addr: Option<String>,
    /// The relay's own id for the queued blob.
    msg_id: String,
    /// The secret token that authorizes deleting (recalling) the blob.
    delete_token: String,
}

struct Chat {
    contact: Contact,
    peer_address: Address,
    session: Session,
    history: Vec<ChatMessage>,
    /// False while an inbound request awaits the local user's approval (§5). A chat we
    /// initiated is authorized immediately; one opened by a stranger's `Hello` is not.
    authorized: bool,
    /// The pairing/invite code this chat was established under, if any. On the inviter
    /// side it lets the approval signal echo the code back; it also lets us detect a
    /// re-used code. QR-paired chats carry `None`.
    code: Option<String>,
    /// True once the chat has been torn down (the peer deleted it, or our join code was
    /// already used). The conversation stays visible with its closing notice, but no
    /// further messages can be sent until a new chat is created.
    closed: bool,
    /// Relay receipts for our still-queued messages, by `msg_id`: lets an edit/unsend *recall*
    /// the undelivered blob(s) and post the new text in their place, so the peer never sees the
    /// old version. With multi-relay fan-out (#17) a message is queued on **several** relays, so
    /// we hold one receipt per copy and recall them all. **Persisted** (`queued_receipts`) so a
    /// recall still works after an app restart — otherwise an unsent-but-undelivered message would
    /// reach the peer and only then be tombstoned (§11.3, `IMPROVEMENT_PLAN.md` §1.1).
    relay_receipts: HashMap<String, Vec<QueuedReceipt>>,
    /// Whether the **last** opt-in-server-storage attempt for this chat reached a relay while the
    /// peer was already online (§6). Goes false when server storage is on but no relay accepted a
    /// copy — the message still reached the peer directly, but was **not** stored server-side, so
    /// the UI downgrades the storage banner instead of silently pretending the copy exists.
    /// In-memory only (recomputed on the next send); starts optimistic.
    remote_storage_healthy: bool,
}

/// One device. Owns the identity, the transport endpoint, and all chats. A contact is
/// keyed by the peer's long-term Curve25519 identity key (base64), which both sides learn
/// during the handshake.
pub struct Node {
    identity: LocalIdentity,
    transport: Box<dyn Transport>,
    /// The **primary** relay — the shared, baked-in default both peers fall back to. Kept for
    /// all existing code paths; multi-relay (#17) fans out *in addition* to this.
    relay: Option<RelayClient>,
    /// Extra relay addresses that also host **our** mailbox, advertised to contacts so their
    /// mail is redundantly deliverable (#17). We poll the primary + these; peers post to their
    /// own `peer_relays`. Persisted.
    my_relays: Vec<String>,
    chats: HashMap<String, Chat>,
    /// When true, an inbound `Hello` from a new contact must be authorized before any
    /// message is delivered or sent (§5 authorization-before-first-message).
    require_authorization: bool,
    /// The most recently minted invite code (short code), remembered so the approval
    /// signal we send back to a joiner can echo the code they used (§5).
    last_invite_code: Option<String>,
    /// Where media attachments are stored at rest: `(dir, key)`. Each attachment is sealed
    /// under `key` into its own file in `dir`, and the message only references its id — so
    /// large media never inflates the JSON state blob. `None` disables media (demo/tests).
    media_store: Option<(String, StoreKey)>,
    /// Attachments decoded from a restored backup, waiting to be written into the media store
    /// once one is configured (the store is set after the node is built). See [`set_media_store`].
    pending_media: Vec<crate::storage::PersistedMedia>,
    /// Arti's base state dir (the one passed to the Tor transport), so a backup can fold in the
    /// onion keystore and restore the same `.onion`. `None` when Tor/persistence isn't used.
    tor_state_dir: Option<String>,
    /// Set whenever an inbound frame mutated state (including silent control frames like Ack),
    /// so the driver refreshes/persists the UI even when no user message was produced.
    dirty: bool,
    /// Outstanding short-code invites this device is hosting (§5b). Each holds the SPAKE2
    /// secret words and the pre-key/onion payload to hand out; the background poller answers a
    /// joiner's SPAKE2 opener from these (see [`service_pending_invites`](Self::service_pending_invites)).
    /// In-memory only — a code not completed before the app closes is simply reissued.
    pending_invites: Vec<PendingInvite>,
    /// Our own address as of the last persisted state (empty for a brand-new node). Compared
    /// against the live transport address on startup: if the onion changed (e.g. a rebuilt Tor
    /// keystore), we announce the new address to contacts so they can still reach us (§5c, #11).
    restored_address: String,
    /// Hashes of relay blobs we've already processed, to de-duplicate a message a sender fanned
    /// out to several relays (#17) — including the case where one relay was down when its sibling
    /// was drained. Bounded (cleared past a cap); in-memory only.
    seen_relay_blobs: std::collections::HashSet<[u8; 32]>,
    /// Reachability of **our own** advertised extra relays (`my_relays`, #17), keyed by address,
    /// as observed on the last [`poll_relay`](Self::poll_relay). `false` = that relay (e.g. one the
    /// user self-hosts) did not answer our drain, so contacts' mail to us via it may be stuck. Drives
    /// the "your relay is offline — add a backup" warning. Absent = not yet probed (treated as up).
    relay_reachable: std::collections::HashMap<String, bool>,
    /// Sent messages that reached neither the peer directly nor any relay yet — typically because
    /// arti's Tor circuits were still cold when the user hit send. Re-queued on every relay poll
    /// (which also warms arti) until a relay accepts the copy, so a message composed during Tor
    /// warm-up still gets delivered instead of silently failing. In-memory only; a restart drops
    /// the retry (the message stays "queued" in history), and the common case — app kept open
    /// through the ~warm-up window — recovers on its own.
    pending_relay: Vec<PendingRelaySend>,
    /// Messages composed while a **non-synchronous** transport (Tor) is in use: [`Node::send`]
    /// seals + stores them "queued" and defers the network here so composing never blocks the UI
    /// on a dial. The poller drains this via [`flush_pending_sends`](Self::flush_pending_sends),
    /// attempting direct-peer delivery with relay fallback. In-memory only, and drained on the very
    /// next poll tick (~80 ms), so a restart in that window just leaves the message "queued" — the
    /// same recovery profile as [`pending_relay`](Self::pending_relay).
    pending_sends: Vec<PendingRelaySend>,
    /// Authenticated `Closed` signals (chat deletes, §11.6) that reached neither the peer nor any
    /// relay when the chat was torn down (arti still cold, or the relay briefly unreachable). Unlike
    /// [`pending_relay`](Self::pending_relay) these are **chat-independent** — the chat is already
    /// gone — so they carry their own recipient + sealed bytes and are retried by the poller until a
    /// relay accepts a copy. **Persisted** (a delete is a one-shot the peer must eventually see), so
    /// the retry survives a restart mid-outage; re-delivery is idempotent (the peer's ratchet rejects
    /// a replayed marker).
    pending_control: Vec<PendingControl>,
    /// Shared default relays learned from the operator-signed **relay directory** (§3.1): fetched
    /// from a live relay on the poll, verified against the baked-in [`directory::DIRECTORY_PUBKEY`],
    /// and treated like additional primaries (drained, paired over, and posted to). This is how the
    /// relay set rotates **without an app update** — a lost/rotated relay onion no longer strands
    /// users once a newer signed list names its replacement. Persisted.
    discovered_relays: Vec<String>,
    /// Version of the last relay directory we accepted; a fetched list is applied only if newer
    /// (monotonic anti-rollback). Persisted.
    directory_version: u64,
}

/// A sent message awaiting a relay to accept its store-and-forward copy (see [`Node::pending_relay`]).
struct PendingRelaySend {
    contact_id: String,
    msg_id: String,
    /// The already-sealed `Frame::Message` bytes — re-posted verbatim so the ratchet is not
    /// re-advanced on retry.
    bytes: Vec<u8>,
}

/// An authenticated control signal (a chat-delete `Closed`) awaiting delivery after its chat was
/// already removed (see [`Node::pending_control`]). Carries its own routing so it survives the chat.
struct PendingControl {
    /// The peer's identity key — seals + addresses the relay copy (its mailbox handle).
    recipient_ik: String,
    /// The peer's onion/address, for the direct-send fallback.
    peer_address: String,
    /// The relay fan-out set captured at delete time (peer_relays + discovered), since the chat —
    /// and thus its `peer_relays` — is gone by the time we retry.
    relays: Vec<String>,
    /// The already-sealed control frame bytes — re-posted verbatim (ratchet not re-advanced).
    bytes: Vec<u8>,
}

/// One in-flight short-code invite awaiting a joiner (§5b, `TODO.md` #3). The `secret` never
/// leaves the device; only SPAKE2 protocol messages and a payload sealed under the derived
/// key ever reach the untrusted rendezvous, so the relay cannot brute-force the code offline.
struct PendingInvite {
    slot: String,
    secret: String,
    /// The `nightdrop://pair?…` payload (our onion + a fresh pre-key bundle) handed to the
    /// joiner, sealed under the SPAKE2 key so only a holder of the right code can read it.
    payload: String,
    /// TTL for the sealed response we post back to the joiner.
    ttl: Duration,
    /// Stop hosting this invite after this instant (the code has expired).
    expiry: Instant,
}

mod backup;
mod frames;
mod messaging;
mod pairing;

impl Node {
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Self::with_identity(LocalIdentity::generate(), transport)
    }

    /// Build a node from a restored identity (used by `storage::` on app restart).
    pub fn with_identity(identity: LocalIdentity, transport: Box<dyn Transport>) -> Self {
        Self {
            identity,
            transport,
            relay: None,
            my_relays: Vec::new(),
            chats: HashMap::new(),
            require_authorization: false,
            last_invite_code: None,
            media_store: None,
            pending_media: Vec::new(),
            tor_state_dir: None,
            dirty: false,
            pending_invites: Vec::new(),
            restored_address: String::new(),
            seen_relay_blobs: std::collections::HashSet::new(),
            relay_reachable: std::collections::HashMap::new(),
            pending_relay: Vec::new(),
            pending_sends: Vec::new(),
            pending_control: Vec::new(),
            discovered_relays: Vec::new(),
            directory_version: 0,
        }
    }

    /// Take and clear the "inbound frame changed state" flag (see [`dirty`]). The driver ORs
    /// this into its change detection so silent control frames (Ack, name, etc.) refresh the UI.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Tell the node where arti keeps its state, so [`backup`](Self::backup) can fold in the
    /// onion keystore (and restore can reproduce the same `.onion`).
    #[allow(dead_code)] // used by the Tor-backed api path (`--features tor`)
    pub fn set_tor_state_dir(&mut self, dir: String) {
        self.tor_state_dir = Some(dir);
    }

    /// Enable at-rest media storage: attachments are sealed under `key` into `dir`. If a
    /// restored backup carried attachments, they are sealed into the store now.
    #[allow(dead_code)] // used by the Tor-backed api path (`--features tor`) + tests
    pub fn set_media_store(&mut self, dir: String, key: StoreKey) {
        std::fs::create_dir_all(&dir).ok();
        // Flush any backup-carried attachments into the store, preserving their original ids so
        // the history references resolve. (Done before `dir`/`key` move into `media_store`.)
        if !self.pending_media.is_empty() {
            use base64::Engine as _;
            let pending = std::mem::take(&mut self.pending_media);
            for item in pending {
                if let Ok(bytes) =
                    base64::engine::general_purpose::STANDARD.decode(item.data.as_bytes())
                {
                    if let Ok(sealed) = crate::storage::seal(&key, &bytes) {
                        let _ = std::fs::write(format!("{dir}/{}.bin", item.id), sealed);
                    }
                }
            }
        }
        // §1.4: sweep any decrypted-media scratch left by a previous run (a crash between
        // decrypt-to-file and cleanup) — plaintext must not outlive the process that wrote it.
        let scratch = std::path::Path::new(&dir).with_file_name("nightdrop-open");
        let _ = std::fs::remove_dir_all(&scratch);
        self.media_store = Some((dir, key));
    }

    /// Require explicit approval of inbound chat requests (the recipient-side bouncer).
    pub fn set_require_authorization(&mut self, require: bool) {
        self.require_authorization = require;
    }

    /// Number of pending inbound requests, without cloning the contacts. The background
    /// poller compares counts every tick (~80ms), so this stays allocation-free.
    pub fn pending_count(&self) -> usize {
        self.chats.values().filter(|c| !c.authorized).count()
    }

    /// Number of authorized contacts (see [`pending_count`](Self::pending_count)).
    pub fn contact_count(&self) -> usize {
        self.chats.values().filter(|c| c.authorized).count()
    }

    /// Contacts whose inbound request is awaiting the local user's approval.
    pub fn pending_authorizations(&self) -> Vec<Contact> {
        let mut pending: Vec<Contact> = self
            .chats
            .values()
            .filter(|c| !c.authorized)
            .map(|c| c.contact.clone())
            .collect();
        // Deterministic order (the chat map is a HashMap) so the UI list is stable and
        // re-used-code resolution always approves the same one first.
        pending.sort_by(|a, b| a.id.cmp(&b.id));
        pending
    }

    /// Approve (`accept`) or decline an inbound chat request. Declining drops the chat.
    ///
    /// On approval we send an [`Approved`](Frame::Approved) signal back to the joiner so
    /// their side learns the chat went live (without it, the joiner is left hanging). The
    /// signal echoes the join code that opened the chat. If another active chat already
    /// uses that code, we instead send a [`CodeInUse`](Frame::CodeInUse) signal and refuse
    /// the duplicate (§5).
    pub fn authorize(&mut self, contact_id: &str, accept: bool) -> Result<()> {
        if !accept {
            devlog!("[nightdrop] authorize: declining request {contact_id}");
            self.chats.remove(contact_id);
            return Ok(());
        }
        let (peer_address, code, already_authorized) = {
            let chat = self
                .chats
                .get(contact_id)
                .ok_or_else(|| anyhow::anyhow!("unknown request"))?;
            (
                chat.peer_address.clone(),
                chat.code.clone(),
                chat.authorized,
            )
        };
        // Idempotent: a second approval (e.g. an impatient double-tap while the Approved
        // signal is still going out over Tor) must NOT send another Approved frame.
        if already_authorized {
            devlog!("[nightdrop] authorize: {contact_id} already approved; skipping resend");
            return Ok(());
        }
        // Reject a re-used code: if another *authorized* chat already uses it, tell the
        // joiner the code is spent rather than opening a duplicate chat.
        if let Some(code) = code.as_deref() {
            let already_used = self
                .chats
                .iter()
                .any(|(id, c)| id != contact_id && c.authorized && c.code.as_deref() == Some(code));
            if already_used {
                devlog!(
                    "[nightdrop] authorize: code '{code}' already in use by an active chat; \
                     refusing {contact_id} and signalling CodeInUse"
                );
                let from = self.identity_key();
                let _ = self.deliver(
                    &peer_address,
                    contact_id,
                    &Frame::CodeInUse {
                        from,
                        code: code.to_string(),
                    },
                );
                self.chats.remove(contact_id);
                return Ok(());
            }
        }
        if let Some(chat) = self.chats.get_mut(contact_id) {
            chat.authorized = true;
        }
        let from = self.identity_key();
        let code = code.unwrap_or_default();
        devlog!("[nightdrop] authorize: approved {contact_id}; sending approval (code='{code}')");
        self.deliver(&peer_address, contact_id, &Frame::Approved { from, code })?;
        Ok(())
    }

    /// Delete a chat: signal the peer that we are tearing it down (so their side shows a
    /// "chat deleted" notice), then drop it locally. Returns once the local chat is gone;
    /// the signal is best-effort (queued on the relay if the peer is offline).
    pub fn delete_chat(&mut self, contact_id: &str) -> Result<()> {
        if !self.chats.contains_key(contact_id) {
            anyhow::bail!("unknown contact");
        }
        devlog!("[nightdrop] delete_chat: tearing down {contact_id}, signalling peer");
        // Authenticated Closed so the peer can trust it's really us tearing the chat down. Deliver
        // it RELAY-FIRST (store-and-forward), like logout (§11.6): after a delete the peer is usually
        // reachable only via the relay — they may be offline, or (commonly) their onion is
        // mid-republish — and a direct send that "succeeds" onto a dead circuit would drop the notice
        // with no retry. Posting to the relay set lets them pick it up on their next drain.
        if let Some((addr, frame)) =
            self.authed_control(contact_id, MARK_CLOSED, |from, message| Frame::Closed {
                from,
                message,
            })
        {
            let bytes = wire::encode(&frame);
            // Same relay set send() uses: the peer's advertised relays (#17) + our shared discovered
            // relays (§3.1), alongside the implicit primary.
            let mut targets = self
                .chats
                .get(contact_id)
                .map(|c| c.contact.peer_relays.clone())
                .unwrap_or_default();
            for r in &self.discovered_relays {
                if !targets.contains(r) {
                    targets.push(r.clone());
                }
            }
            let queued = queue_on_relays(
                self.transport.as_ref(),
                &self.relay,
                &targets,
                contact_id,
                &bytes,
            )
            .is_ok();
            // Direct send only if no relay accepted (relay-less onion mode).
            let direct = !queued && self.transport.send(&addr, &bytes).is_ok();
            // Reached neither the peer nor any relay (arti cold / relay briefly down): retry from the
            // poller so the "chat deleted" notice isn't silently dropped. The chat is about to be
            // removed, so this retry is chat-independent (carries its own recipient + sealed bytes).
            if !queued && !direct {
                self.pending_control.push(PendingControl {
                    recipient_ik: contact_id.to_string(),
                    peer_address: addr,
                    relays: targets,
                    bytes,
                });
            }
        }
        self.chats.remove(contact_id);
        // Onion client auth (#22): drop their authorized-client key so they can no longer fetch our
        // (restricted) descriptor or reach our onion. No-op off Tor.
        let _ = self.transport.revoke_client(contact_id);
        Ok(())
    }

    /// Record that `contact_ids` are now covered by a backup (#7). For a **Full** backup
    /// (`full`), also tell each peer via a [`BackedUp`](Frame::BackedUp) frame so they see the
    /// transparency warning that their messages persist in our backup. Lite backups set the flag
    /// (for logout) but don't notify, since they carry no messages. Best-effort delivery.
    pub fn mark_backed_up(&mut self, contact_ids: &[String], full: bool) {
        let mut notify: Vec<String> = Vec::new();
        for id in contact_ids {
            if let Some(chat) = self.chats.get_mut(id) {
                chat.contact.backed_up = true;
                if full && !chat.closed {
                    notify.push(id.clone());
                }
            }
        }
        for id in &notify {
            if let Some((addr, frame)) = self.authed_control(id, MARK_BACKEDUP, |from, message| {
                Frame::BackedUp { from, message }
            }) {
                let _ = self.deliver(&addr, id, &frame);
            }
        }
    }

    /// The peer-facing side of deleting this identity (#7 / §11.6): before the app wipes the
    /// local state file, signal [`Closed`](Frame::Closed) to the peer of every **un-backed**
    /// chat — otherwise their messages to a since-deleted identity would sit undeliverable.
    /// **Backed-up** chats are left silent, so the peer's mail queues on the relay and delivers
    /// when we restore within its 24h window. Clears all chats. Returns how many un-backed chats we
    /// could **not** get the notice to (0 = all queued/sent), so the app can tell the user.
    pub fn logout(&mut self) -> usize {
        let notify: Vec<String> = self
            .chats
            .iter()
            .filter(|(_, c)| !c.contact.backed_up && !c.closed)
            .map(|(id, _)| id.clone())
            .collect();
        let mut failed = 0usize;
        for id in &notify {
            let Some((addr, frame)) = self.authed_control(id, MARK_CLOSED, |from, message| {
                Frame::Closed { from, message }
            }) else {
                continue;
            };
            let bytes = wire::encode(&frame);
            let peer_relays = self
                .chats
                .get(id)
                .map(|c| c.contact.peer_relays.clone())
                .unwrap_or_default();
            // Prefer the relay store-and-forward so an **offline** peer still gets the "chat
            // deleted" notice within its 24h window (§1.3): the identity is about to be wiped, so a
            // direct-only send that races with the peer being briefly unreachable would be lost with
            // no retry. Fall back to a direct send only if no relay accepted (relay-less onion mode),
            // and count a chat we couldn't reach either way.
            let queued = queue_on_relays(
                self.transport.as_ref(),
                &self.relay,
                &peer_relays,
                id,
                &bytes,
            )
            .is_ok();
            let direct = !queued && self.transport.send(&addr, &bytes).is_ok();
            if !queued && !direct {
                failed += 1;
            }
        }
        // Onion client auth (#22): revoke every contact's reachability to our onion on logout.
        for id in self.chats.keys() {
            let _ = self.transport.revoke_client(id);
        }
        self.chats.clear();
        // §1.4: wipe any decrypted-media scratch so plaintext attachments don't outlive the wiped
        // identity. (The sealed store + state file are removed by the app's data-dir wipe.)
        self.clear_open_cache();
        failed
    }

    /// Remember the most recent invite code so a later approval can echo it back (§5).
    pub fn set_last_invite_code(&mut self, code: String) {
        self.last_invite_code = Some(code);
    }

    /// Build an **authenticated** control frame (`Closed`/`Ack`/`BackedUp`): encrypt a fixed domain
    /// `marker` on the chat's ratchet so the receiver can prove it came from us and reject a forged
    /// or replayed one. Returns `(peer_address, frame)` for [`deliver`](Self::deliver), or `None`
    /// if the chat is gone. Paired with [`verify_control`](Self::verify_control) on the receiver.
    fn authed_control(
        &mut self,
        contact_id: &str,
        marker: &[u8],
        build: impl FnOnce(String, WireOlm) -> Frame,
    ) -> Option<(Address, Frame)> {
        let from = self.identity_key();
        let chat = self.chats.get_mut(contact_id)?;
        let sealed = WireOlm::from_olm(&crypto::encrypt(&mut chat.session, marker));
        Some((chat.peer_address.clone(), build(from, sealed)))
    }

    /// Verify an authenticated control frame: decrypt its `message` on the sender's session and
    /// confirm it is exactly `marker`. True only for a genuine, non-replayed frame from the real
    /// peer — a forgery (wrong/no session), a replay (message key already spent), or a cross-type
    /// splice (marker mismatch) all return false. Consumes a message key, like any ratchet decrypt.
    fn verify_control(&mut self, from: &str, message: &WireOlm, marker: &[u8]) -> bool {
        let Some(chat) = self.chats.get_mut(from) else {
            return false;
        };
        let Ok(olm) = message.to_olm() else {
            return false;
        };
        matches!(crypto::decrypt(&mut chat.session, &olm), Ok(pt) if pt == marker)
    }

    /// Send a control/relay frame to `peer_address`, falling back to the relay mailbox when
    /// the peer is unreachable directly (so approvals/closes survive an offline peer).
    /// `recipient_ik` (the peer's identity key) addresses and seals the relay copy — the
    /// relay never sees the address or the frame's routing metadata.
    fn deliver(&self, peer_address: &str, recipient_ik: &str, frame: &Frame) -> Result<()> {
        let bytes = wire::encode(frame);
        if self.transport.send(peer_address, &bytes).is_err() {
            // The direct onion dial failed. Expected while their descriptor is (re)publishing, but
            // also what a *restricted* onion looks like to a peer it hasn't authorized yet (#22) —
            // in which case the relay is the only way in until our ClientKey reaches them.
            crate::diag!("deliver: direct dial failed — falling back to the relay");
            // Fan out to the recipient's relay set (primary + their advertised extras, #17).
            let peer_relays = self
                .chats
                .get(recipient_ik)
                .map(|c| c.contact.peer_relays.clone())
                .unwrap_or_default();
            queue_on_relays(
                self.transport.as_ref(),
                &self.relay,
                &peer_relays,
                recipient_ik,
                &bytes,
            )
            .inspect_err(|_| {
                crate::diag!(
                    "deliver: relay fallback ALSO failed ({} peer relay(s) known) — the frame is \
                     lost and the peer will never see it",
                    peer_relays.len()
                );
            })?;
            crate::diag!("deliver: queued on the relay for the peer to drain");
        }
        Ok(())
    }

    /// Onion client authorization (#22): mint our client descriptor-encryption key for `contact`'s
    /// onion at `peer_address` and hand it over in a [`ClientKey`](Frame::ClientKey) frame, so the
    /// peer authorizes us to fetch their (restricted) descriptor and connect. Best-effort with relay
    /// fallback, and a no-op on transports without restricted discovery (`make_client_key` → `None`)
    /// or when `peer_address` isn't an onion — so non-Tor pairing is unaffected. Called whenever we
    /// (re)learn a peer's onion: on pairing and on address rotation.
    fn announce_client_key(&self, contact: &str, peer_address: &str) {
        let Some(key_result) = self.transport.make_client_key(peer_address) else {
            return; // transport has no restricted-discovery client keys (e.g. tests, LAN)
        };
        let Ok(client_key) = key_result else {
            return; // couldn't parse the peer onion / mint a key — leave us a public onion
        };
        let frame = Frame::ClientKey {
            from: self.identity_key(),
            client_key,
        };
        let _ = self.deliver(peer_address, contact, &frame);
    }

    /// Mint (and store in arti's keystore) this device's client access key for a **private** relay's
    /// onion (restricted discovery, §3.2), returning the public `descriptor:x25519:…` string to give
    /// the relay operator. arti stores the private half keyed by the relay's onion and presents it
    /// automatically on future connects, so once the operator authorizes the returned key this
    /// device can reach the restricted relay. `None` on transports without restricted discovery
    /// (tests, LAN); `Some(Err)` if the onion won't parse / key generation fails.
    pub(crate) fn relay_access_key(&self, relay_onion: &str) -> Option<Result<String>> {
        self.transport.make_client_key(relay_onion)
    }

    /// Attach the primary relay for offline store-and-forward (§6). Without one (and no
    /// advertised extras), sending to an offline peer errors instead of being queued.
    // Wired into NightdropCore/the app in step 8 (opt-in 24h server storage); used in tests now.
    #[allow(dead_code)]
    pub fn set_relay(&mut self, relay: RelayClient) {
        self.relay = Some(relay);
    }

    /// Tear down the network side: swap the live transport for an inert [`ClosedTransport`] and
    /// drop the primary relay, so everything they hold is released. Identity and chats survive,
    /// but the node can no longer send or receive.
    ///
    /// This exists for Tor: arti holds an **exclusive on-disk lock** on its state directory, so a
    /// second instance over the same `state_dir` cannot start while the first is alive. Restoring
    /// a backup builds a whole new node, so the old one must be closed first or the new one fails
    /// to launch its onion service (§6). The relay is dropped too — its dialer holds a clone of
    /// the same arti client, and the lock is only released once *every* handle is gone.
    pub fn close_transport(&mut self) {
        let address = self.transport.address();
        self.transport = Box::new(crate::transport::ClosedTransport::new(address));
        self.relay = None;
    }

    /// Set the **extra** relay addresses that also host our mailbox (#17) — advertised to
    /// contacts and polled alongside the primary. The primary (baked-in default) is implicit.
    pub fn set_my_relays(&mut self, relays: Vec<String>) {
        self.my_relays = relays;
        // Forget health for relays we no longer advertise (and start freshly-added ones "unknown").
        let keep = self.my_relays.clone();
        self.relay_reachable.retain(|addr, _| keep.contains(addr));
    }

    /// Our advertised extra relays (does not include the implicit primary).
    pub fn my_relays(&self) -> Vec<String> {
        self.my_relays.clone()
    }

    /// Reachability of each of **our** advertised extra relays (#17), as observed on the last
    /// [`poll_relay`](Self::poll_relay): `(address, reachable)`. A relay not yet probed reports
    /// `true` (optimistic — don't cry wolf before the first poll). Lets the UI warn "your relay is
    /// offline — add a backup" for a self-hosted relay that stops answering.
    pub fn relay_health(&self) -> Vec<(String, bool)> {
        self.my_relays
            .iter()
            .map(|addr| {
                (
                    addr.clone(),
                    *self.relay_reachable.get(addr).unwrap_or(&true),
                )
            })
            .collect()
    }

    pub fn address(&self) -> Address {
        self.transport.address()
    }

    /// Whether our address is published/reachable (Tor: onion descriptor up). See
    /// [`Transport::published`].
    pub fn onion_published(&self) -> bool {
        self.transport.published()
    }

    pub fn identity_id(&self) -> String {
        self.identity.id()
    }

    /// Our long-term Curve25519 identity key (base64) — how peers key us as a contact.
    pub fn identity_key(&self) -> String {
        self.identity.curve25519().to_base64()
    }

    pub fn contacts(&self) -> Vec<Contact> {
        self.chats
            .values()
            .filter(|c| c.authorized)
            .map(|c| {
                // `remote_storage_healthy` is live per-chat state, not part of the stored contact.
                let mut dto = c.contact.clone();
                dto.remote_storage_healthy = c.remote_storage_healthy;
                dto
            })
            .collect()
    }

    pub fn messages(&self, contact_id: &str) -> Vec<ChatMessage> {
        self.chats
            .get(contact_id)
            .map(|c| c.history.clone())
            .unwrap_or_default()
    }
}

// --- Media envelope (de)serialization (encrypted inside Media/MediaIncoming frames) ---
// All length prefixes are big-endian u32; a trailing field consumes the rest.

fn put_field(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn take_field(buf: &[u8], p: &mut usize) -> Result<Vec<u8>> {
    if *p + 4 > buf.len() {
        anyhow::bail!("corrupt media envelope");
    }
    let n = u32::from_be_bytes(buf[*p..*p + 4].try_into().unwrap()) as usize;
    *p += 4;
    if *p + n > buf.len() {
        anyhow::bail!("corrupt media envelope");
    }
    let v = buf[*p..*p + n].to_vec();
    *p += n;
    Ok(v)
}

/// Pack an `Edit` envelope: `[target_msg_id][new_text...]` (encrypted on the session).
fn pack_edit(target_msg_id: &str, new_text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(target_msg_id.len() + new_text.len() + 4);
    put_field(&mut out, target_msg_id.as_bytes());
    out.extend_from_slice(new_text.as_bytes()); // trailing
    out
}

/// Inverse of [`pack_edit`]: `(target_msg_id, new_text)`.
fn unpack_edit(buf: &[u8]) -> Result<(String, String)> {
    let mut p = 0;
    let target = String::from_utf8(take_field(buf, &mut p)?)?;
    let text = String::from_utf8(buf[p..].to_vec())?;
    Ok((target, text))
}

/// Pack an `Unsend` envelope: just the target `msg_id` (encrypted on the session).
fn pack_unsend(target_msg_id: &str) -> Vec<u8> {
    target_msg_id.as_bytes().to_vec()
}

/// Inverse of [`pack_unsend`]: the target `msg_id`.
fn unpack_unsend(buf: &[u8]) -> Result<String> {
    Ok(String::from_utf8(buf.to_vec())?)
}

/// A human label for a disappearing-messages timer value (for the in-chat system notice).
fn disappearing_label(secs: u64) -> String {
    match secs {
        0 => "off".to_string(),
        s if s % 604800 == 0 => format!("{} week(s)", s / 604800),
        s if s % 86400 == 0 => format!("{} day(s)", s / 86400),
        s if s % 3600 == 0 => format!("{} hour(s)", s / 3600),
        s if s % 60 == 0 => format!("{} minute(s)", s / 60),
        s => format!("{s} second(s)"),
    }
}

/// Render a 32-byte safety fingerprint as 12 space-separated 5-digit groups (Signal-style). The
/// full security lives in the 256-bit digest; this is a lossy but deterministic display form so
/// two people can compare it by eye/voice. Identical inputs → identical string on both devices.
fn render_safety_number(digest: &[u8; 32]) -> String {
    let mut groups = Vec::with_capacity(12);
    for i in 0..12 {
        // Read 5 bytes (wrapping over the 32-byte digest) as a big number, take 5 decimal digits.
        let mut acc: u64 = 0;
        for j in 0..5 {
            acc = (acc << 8) | digest[(i * 5 + j) % 32] as u64;
        }
        groups.push(format!("{:05}", acc % 100_000));
    }
    groups.join(" ")
}

/// Rebuild a UI [`ChatMessage`] from its persisted form (restore + scoped-backup merge).
fn persisted_to_message(m: &crate::storage::PersistedMessage) -> ChatMessage {
    ChatMessage {
        from_me: m.from_me,
        text: m.text.clone(),
        system: m.system,
        kind: m.kind.clone(),
        mime: m.mime.clone(),
        media_id: m.media_id.clone(),
        media_size: m.media_size,
        transfer_id: m.transfer_id.clone(),
        thumb_id: m.thumb_id.clone(),
        delivery: m.delivery.clone(),
        msg_id: m.msg_id.clone(),
        edited: m.edited,
        at: m.at,
    }
}

/// Turn a message into a "deleted" tombstone in place: it keeps its id/position but loses its
/// text and renders as a deletion notice on both sides. Idempotent.
fn make_tombstone(msg: &mut ChatMessage) {
    msg.text = String::new();
    msg.kind = "deleted".to_string();
    msg.edited = false;
    msg.delivery = String::new();
}

/// Pack a `Media` envelope: `[transfer_id][kind][mime][data...]`.
fn pack_media(transfer_id: &str, kind: &str, mime: &str, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + transfer_id.len() + kind.len() + mime.len() + 16);
    put_field(&mut out, transfer_id.as_bytes());
    put_field(&mut out, kind.as_bytes());
    put_field(&mut out, mime.as_bytes());
    out.extend_from_slice(data); // trailing
    out
}

/// Inverse of [`pack_media`]: `(transfer_id, kind, mime, data)`.
fn unpack_media(buf: &[u8]) -> Result<(String, String, String, Vec<u8>)> {
    let mut p = 0;
    let transfer_id = String::from_utf8(take_field(buf, &mut p)?)?;
    let kind = String::from_utf8(take_field(buf, &mut p)?)?;
    let mime = String::from_utf8(take_field(buf, &mut p)?)?;
    Ok((transfer_id, kind, mime, buf[p..].to_vec()))
}

/// Pack a `MediaIncoming` envelope: `[transfer_id][kind][mime][size:u64][thumbnail...]`.
fn pack_media_incoming(
    transfer_id: &str,
    kind: &str,
    mime: &str,
    size: u64,
    thumb: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(thumb.len() + 32);
    put_field(&mut out, transfer_id.as_bytes());
    put_field(&mut out, kind.as_bytes());
    put_field(&mut out, mime.as_bytes());
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(thumb); // trailing
    out
}

/// Inverse of [`pack_media_incoming`]: `(transfer_id, kind, mime, size, thumb)`.
fn unpack_media_incoming(buf: &[u8]) -> Result<(String, String, String, u64, Vec<u8>)> {
    let mut p = 0;
    let transfer_id = String::from_utf8(take_field(buf, &mut p)?)?;
    let kind = String::from_utf8(take_field(buf, &mut p)?)?;
    let mime = String::from_utf8(take_field(buf, &mut p)?)?;
    if p + 8 > buf.len() {
        anyhow::bail!("corrupt media-incoming envelope");
    }
    let size = u64::from_be_bytes(buf[p..p + 8].try_into().unwrap());
    p += 8;
    Ok((transfer_id, kind, mime, size, buf[p..].to_vec()))
}

// -------- Interactive SPAKE2 short-code pairing (§5b, `TODO.md` #3) --------

/// The two rendezvous legs for one slot: the joiner posts its SPAKE2 opener under the
/// joiner leg; the inviter posts its answer under the inviter leg. Distinct handles keep the
/// two directions from colliding (and blunt trivial reflection of a message to its sender).
const RDV_JOINER: &str = "j";
const RDV_INVITER: &str = "i";

/// How long the joiner posts its opener for, and how often it re-checks for the answer while
/// pairing. The poll interval is bounded below by the relay round-trip (a Tor hop) anyway.
const RENDEZVOUS_TTL: Duration = Duration::from_secs(600);
const RENDEZVOUS_POLL: Duration = Duration::from_millis(500);

/// How long the joiner keeps trying to get its opener onto *some* relay before giving up, and how
/// long it waits between rounds. A flaky Tor path can take many circuit attempts to reach the
/// relay's onion (the relay may take dozens to publish its own descriptor), so one failed round is
/// not "unreachable" — only a full window of failures is.
#[cfg(not(test))]
const POST_ESTABLISH_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(test)]
const POST_ESTABLISH_TIMEOUT: Duration = Duration::from_millis(300);
const POST_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// How often the joiner re-posts its opener while waiting for an answer.
///
/// The inviter **drains** the joiner leg ([`RelayClient::take`]) before it has posted its answer,
/// so anything that goes wrong in between — the answer failing to post, the inviter being
/// backgrounded or killed mid-handshake — consumes the opener for good. Without a re-post the
/// joiner would then poll out its whole timeout waiting for an answer that can never come, and
/// the user would need a fresh code. Re-posting makes the handshake self-healing: an inviter that
/// comes back finds an opener waiting. Re-sending the same `msg_j`/KEM public is safe — both are
/// public handshake values, and the joiner keeps one SPAKE2 state, so whichever answer lands
/// completes it.
#[cfg(not(test))]
const RENDEZVOUS_REPOST: Duration = Duration::from_secs(20);
/// Tests drive the same path against a real relay; keep them quick.
#[cfg(test)]
const RENDEZVOUS_REPOST: Duration = Duration::from_millis(200);

/// The rendezvous mailbox handle for one leg of a slot (§5c). The slot is non-secret; all
/// security rides on the SPAKE2 secret words, never on the handle.
fn rendezvous_handle(slot: &str, leg: &str) -> String {
    format!("rdv:{slot}:{leg}")
}

/// Split a two-field length-prefixed blob (the joiner's opener `msg_j || kem_public`). Returns
/// `None` for anything malformed, so garbage in the mailbox is skipped without ceremony.
fn two_fields(blob: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut p = 0;
    let a = take_field(blob, &mut p).ok()?;
    let b = take_field(blob, &mut p).ok()?;
    Some((a, b))
}

/// Split a three-field length-prefixed blob (the inviter's answer `msg_i || kem_ct || sealed`).
fn three_fields(blob: &[u8]) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let mut p = 0;
    let a = take_field(blob, &mut p).ok()?;
    let b = take_field(blob, &mut p).ok()?;
    let c = take_field(blob, &mut p).ok()?;
    Some((a, b, c))
}

/// Inviter's half of the handshake: run SPAKE2 against the joiner's opener (`msg_j || kem_public`),
/// encapsulate an ML-KEM secret to the joiner's key, and return `msg_i || kem_ct || seal(K_hybrid,
/// payload)` (§4.1). Safe to run for any opener: only a joiner who used the same code *and* holds
/// the ML-KEM secret key derives the same hybrid `K` and can open the seal.
fn build_invite_response(secret: &str, payload: &str, opener: &[u8]) -> Result<Vec<u8>> {
    let (msg_j, kem_public) =
        two_fields(opener).ok_or_else(|| anyhow::anyhow!("malformed pairing opener"))?;
    let (bouncer, msg_i) = crate::pake::start(secret.as_bytes());
    let key = bouncer.finish(&msg_j)?;
    let (kem_ct, kem_ss) = crate::pqkem::encapsulate(&kem_public)?;
    let seal_key = crate::pqkem::hybrid_seal_key(&key, &kem_ss);
    let sealed = crate::storage::seal(&seal_key, payload.as_bytes())?;
    let mut out = Vec::new();
    put_field(&mut out, &msg_i);
    put_field(&mut out, &kem_ct);
    put_field(&mut out, &sealed);
    Ok(out)
}

/// Joiner's half of the handshake, driven **outside** the core lock (it blocks on relay
/// round-trips up to `timeout`): post our SPAKE2 opener, then poll for the inviter's answer,
/// finish SPAKE2, and open the sealed payload. A wrong code makes the `open` fail → a clear
/// "wrong short code" error rather than any signal a dictionary attacker could use.
///
/// Note: an attacker who knows the (non-secret) slot could flood the answer leg to disrupt a
/// pairing attempt — a denial of service, not a break: they still cannot read the payload or
/// impersonate the inviter without the code. The user simply retries with a fresh code.
pub fn run_join_handshake(
    relays: &[RelayClient],
    slot: &str,
    secret: &str,
    timeout: Duration,
) -> Result<String> {
    if relays.is_empty() {
        anyhow::bail!("no relay configured");
    }
    let (bouncer, msg_j) = crate::pake::start(secret.as_bytes());
    // §4.1: also ship a one-time ML-KEM-768 public key so the inviter can encapsulate a PQ secret;
    // the payload seal key mixes it with the SPAKE2 secret (hybrid, harvest-now-decrypt-later-safe).
    let kem = crate::pqkem::generate();
    let mut opener = Vec::new();
    put_field(&mut opener, &msg_j);
    put_field(&mut opener, &kem.public);
    // §3.1: broadcast our opener to every relay in the set and poll them all for the inviter's
    // answer, so the handshake completes over whichever relay the two of us share — no single
    // relay is a pairing chokepoint. At least one post must land (else we can't be answered).
    let joiner_handle = rendezvous_handle(slot, RDV_JOINER);
    // Get the opener onto at least one relay before we start polling for an answer. On a flaky
    // network a single attempt often fails to build a circuit to the relay's onion — the relay
    // itself may take dozens of circuit tries to publish its descriptor — so retry with backoff
    // for a bounded window rather than giving up after one round. Bailing immediately was the
    // "could not reach any relay" people hit on a slow Tor path even when the relay was healthy.
    let post_deadline = Instant::now() + POST_ESTABLISH_TIMEOUT;
    let mut posted = 0usize;
    let mut attempt = 0usize;
    while posted == 0 {
        attempt += 1;
        for relay in relays {
            match relay.post(&joiner_handle, &opener, RENDEZVOUS_TTL) {
                Ok(_) => posted += 1,
                // Transport-level cause (dial failed / timed out / relay rejected) — names why a
                // round came back 0, which `opener posted to 0/N` alone can't.
                Err(e) => crate::diag!("join: post attempt {attempt} to a relay FAILED: {e:#}"),
            }
        }
        if posted > 0 {
            break;
        }
        if Instant::now() >= post_deadline {
            crate::diag!(
                "join: gave up posting the opener after {attempt} attempt(s) / {}s — no relay \
                 reachable",
                POST_ESTABLISH_TIMEOUT.as_secs()
            );
            anyhow::bail!("could not reach any relay to start pairing — check your connection");
        }
        std::thread::sleep(POST_RETRY_INTERVAL);
    }
    crate::diag!(
        "join: opener posted to {posted}/{} relays (attempt {attempt})",
        relays.len()
    );

    let inviter_handle = rendezvous_handle(slot, RDV_INVITER);
    let deadline = Instant::now() + timeout;
    let mut last_post = Instant::now();
    let mut reposts = 0usize;
    loop {
        if Instant::now() >= deadline {
            crate::diag!(
                "join: TIMED OUT after {}s with no answer ({reposts} re-posts) — the inviter \
                 never answered our opener",
                timeout.as_secs()
            );
            anyhow::bail!("timed out waiting for the other device — ask for a fresh code");
        }
        std::thread::sleep(RENDEZVOUS_POLL);
        // Keep an opener available to the inviter for as long as we're waiting (see
        // `RENDEZVOUS_REPOST`): the inviter's read is destructive, so the one we posted up front
        // may already have been consumed without an answer ever being posted back.
        if last_post.elapsed() >= RENDEZVOUS_REPOST {
            for relay in relays {
                let _ = relay.post(&joiner_handle, &opener, RENDEZVOUS_TTL);
            }
            last_post = Instant::now();
            reposts += 1;
            crate::diag!("join: re-posted opener (#{reposts}) — still no answer");
        }
        // Gather answers from every relay; a single relay being down must not abort the poll.
        let answer = relays
            .iter()
            .filter_map(|r| r.take(&inviter_handle).ok())
            .flatten()
            .find_map(|b| three_fields(&b));
        let Some((msg_i, kem_ct, sealed)) = answer else {
            continue; // no well-formed answer on any relay yet
        };
        crate::diag!("join: got the inviter's answer — completing SPAKE2");
        let key = bouncer.finish(&msg_i).map_err(|_| {
            crate::diag!("join: SPAKE2 finish FAILED on the inviter's answer");
            anyhow::anyhow!("pairing handshake failed")
        })?;
        // Hybrid seal key = KDF(SPAKE2 secret ‖ ML-KEM secret). A wrong code (SPAKE2) *or* a forged
        // ciphertext (ML-KEM) yields a different key ⇒ `open` fails ⇒ "wrong short code".
        let kem_ss = kem
            .decapsulate(&kem_ct)
            .map_err(|_| anyhow::anyhow!("wrong short code"))?;
        let seal_key = crate::pqkem::hybrid_seal_key(&key, &kem_ss);
        let payload = crate::storage::open(&seal_key, &sealed)
            .map_err(|_| anyhow::anyhow!("wrong short code"))?;
        return String::from_utf8(payload).map_err(|_| anyhow::anyhow!("corrupt invite"));
    }
}

/// The relay handle a server backup is stored under. Derived from the password with a
/// fixed salt, so the user needs only their recovery password to both locate and decrypt.
fn backup_handle(password: &str) -> Result<String> {
    let key = crate::storage::derive_key(password, b"nightdrop-backup-handle")?;
    Ok(format!("bkp:{}", base64_handle(&key[..16])))
}

fn base64_handle(bytes: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests_a;
#[cfg(test)]
mod tests_b;
