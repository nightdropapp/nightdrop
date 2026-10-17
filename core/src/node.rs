//! A single device's real messaging logic (`ARCHITECTURE.md` §5–§6): identity, an
//! established Olm session per contact, and framed send/receive over a [`Transport`].
//! This is the genuine protocol — two `Node`s on any transport actually pair and
//! converse. `api::NightdropCore` wraps a `Node` for the app (and, for now, an in-process
//! demo peer); the network transport (Tor) and relay slot in behind the same trait.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vodozemac::olm::{Account, AccountPickle, Session, SessionPickle};
use zeroize::Zeroize as _;

use crate::api::{ChatMessage, Contact};
use crate::crypto;
use crate::identity::{LocalIdentity, PreKeyBundle};
use crate::relay_client::RelayClient;
use crate::storage::{PersistedChat, PersistedMessage, PersistedState, StoreKey};
pub use crate::storage::{RelayEntry, RelaySet};
use crate::transport::{Address, Transport};
use crate::wire::{self, Frame, WireOlm};
use crate::{Result, DEFAULT_NAME};

/// How long an offline message sits in the relay before expiring (§6). Also the device-side
/// time-bomb horizon for opt-in server-storage (ephemeral) chats (§11.4), and the age past
/// which a still-unacked queued message is shown as "expired" (§11.3).
const RELAY_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Whether this device tells every contact it has just come online, by re-sending its standing
/// announcements (burn support, app version, address) on each launch instead of only when they
/// change (`optional-relay.md` §4). **Off (decided 2026-10-05, made real 2026-10-09):** it would
/// tell every contact when we open the app. On, a contact holding messages for a direct-only
/// device flushes them as soon as it hears from it, rather than at its next retry. Kept as one
/// switch so turning it on later is a one-line change; `node/tests_announce.rs` runs both.
pub(crate) const ANNOUNCE_ONLINE_ON_START: bool = false;

/// Retry backoff for a contact with no relay route, so held frames go only direct
/// (`optional-relay.md` §4): the first retry after a minute, doubling to a cap of five minutes while
/// the app is open and fifteen in the background. Every retry is a descriptor fetch plus a circuit,
/// so the background cap is the battery cost. Hearing from the contact directly resets it.
const HELD_RETRY_FIRST_SECS: u64 = 60;
const HELD_RETRY_CAP_OPEN_SECS: u64 = 5 * 60;
const HELD_RETRY_CAP_BACKGROUND_SECS: u64 = 15 * 60;

/// The oldest app version that speaks our protocol: 0.2 does not talk to 0.1.x (wire v3 against
/// v2). A contact known to run anything older is **outdated**: what we send it is held, not sent
/// for its app to take and discard, until it announces a version at least this.
const MIN_PEER_VERSION: (u32, u32) = (0, 2);

/// Whether to tell a contact still on 0.1.x, once, in the format its app can read, that we have
/// updated and they need to (the maintainer, 2026-10-10: "keep that safeguard for a few versions").
/// **Remove after 0.4**, with [`wire::encode_legacy`]: by then a contact still on 0.1 has been
/// unreachable for several releases.
const SEND_UPGRADE_NOTICE: bool = true;

/// The note in our own chat after the upgrade notice went out ([`Node::send_upgrade_notices`]),
/// removed once the contact is seen on 0.2 ([`Chat::forget_upgrade_notice`]).
const UPGRADE_NOTE_TEXT: &str = "🔄 Their app is too old to read your messages, so Night Drop \
                                 sent them a message saying you've updated and asking them to \
                                 update too.";

/// The message a 0.1.x contact's app shows (`send_upgrade_notices`). Plain text in a `Message`, so
/// it reads as from the user, which is why it says it is automatic. English: the sender cannot
/// know the reader's language, and their old app cannot translate it.
const UPGRADE_NOTICE_TEXT: &str = "🔄 Automatic message from Night Drop: I've updated to Night \
     Drop 0.2, which can't exchange messages with the version you're using. Please update Night \
     Drop to keep talking — my messages to you will arrive once you do.";

/// Whether an announced app version predates [`MIN_PEER_VERSION`].
pub(crate) fn version_outdated(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    (major, minor) < MIN_PEER_VERSION
}

/// The most entries our own relay list holds (`optional-relay.md` §2). Everyone writing to us
/// posts each offline message to every entry, so this bounds the copies one message makes. A
/// contact's announced set is held to the same cap, so nobody can make us post a message a
/// thousand times.
pub const MAX_RELAYS: usize = 4;

/// How long after sending a text message it can still be edited (product rule; a message
/// still queued on the relay stays editable regardless, since the peer never saw it).
const EDIT_WINDOW: Duration = Duration::from_secs(15 * 60);

/// Maximum attachment size (100 MB). Larger files are rejected — they would be slow over
/// Tor and memory-heavy to carry through one frame.
pub(crate) const MAX_MEDIA_BYTES: u64 = 100 * 1024 * 1024;

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
/// A screenshot of the chat was taken on the peer's device (#1).
const MARK_SCREENSHOT: &[u8] = b"nightdrop/ctl/screenshot/v1";
/// Two markers for the safety-number verification signal (§5b′): the state is carried by *which*
/// one the receiver's ratchet decrypts, so it's authenticated with no tamperable plaintext flag.
const MARK_VERIFIED: &[u8] = b"nightdrop/ctl/verified/v1";
const MARK_UNVERIFIED: &[u8] = b"nightdrop/ctl/unverified/v1";
/// The peer's build understands burn messages (`Frame::Burns`).
const MARK_BURNS_V1: &[u8] = b"nightdrop/ctl/burns/v1";
/// Prefix of a [`Frame::Version`] plaintext; the app version follows it (`"...:0.1.27"`).
const MARK_VERSION_PREFIX: &[u8] = b"nightdrop/ctl/version/v1:";
/// Prefix of a [`Frame::ClientKeySealed`] plaintext; the client key follows it.
const MARK_CLIENT_KEY_PREFIX: &[u8] = b"nightdrop/ctl/client-key/v1:";
/// Two markers for the screenshot-capability signal (#1), same shape as the verification pair: the
/// state is *which* marker the receiver's ratchet decrypts, so there is no plaintext flag to flip.
const MARK_CAPTURES_VISIBLE: &[u8] = b"nightdrop/ctl/captures-visible/v1";
const MARK_CAPTURES_SILENT: &[u8] = b"nightdrop/ctl/captures-silent/v1";

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
        Some(dialer) => RelayClient::with_dialer_for(addr, dialer),
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
    /// One job per (relay, polling fragment), in this round's random order. Each carries a client on
    /// its fragment's isolated circuits (`mailbox.rs`, `poll_fragments`), so a relay sees one reader
    /// per fragment, never one reader of everything (`mailbox-handles.md` §5a/§5c).
    jobs: Vec<DrainJob>,
    /// The epoch this round can retire ([`Node::settling_epoch`]): if every one of its jobs answers,
    /// yesterday's handles are empty past the skew margin and need not be polled again.
    settles: Option<u64>,
    /// Pause a random moment between jobs. Only where circuits are really isolated (Tor): the point
    /// is that fragments do not arrive at fixed offsets from each other, and without isolation there
    /// is nothing to protect — tests would only be slowed down.
    stagger: bool,
}

pub(crate) struct DrainJob {
    /// `None` = the primary relay; `Some(addr)` = an advertised extra.
    addr: Option<String>,
    client: RelayClient,
    /// The fragment's UTC day; `None` for the v1 handle.
    epoch: Option<u64>,
    handles: Vec<String>,
}

/// The result of draining the relay mailboxes lock-free: the raw blobs (fan-out duplicates still
/// present — de-duplicated later in [`Node::apply_relay_harvest`]) and each *addressed* relay's
/// reachability, to fold back into relay-health once the lock is re-acquired.
pub(crate) struct RelayHarvest {
    blobs: Vec<Vec<u8>>,
    reachability: Vec<(String, bool)>,
    /// The plan's settling epoch, if every one of its jobs on every relay answered this round.
    settled: Option<u64>,
}

/// Drain every relay mailbox in `plan` — the blocking `take` round-trips — with **no** core lock
/// held (§1.5.2), so UI calls aren't stalled for seconds behind an in-flight Tor relay poll. A
/// relay that errors is recorded unreachable and skipped, never aborting the drain from the rest.
pub(crate) fn drain_relay_mailboxes(plan: &RelayDrainPlan) -> RelayHarvest {
    use rand::Rng as _;
    let mut blobs = Vec::new();
    // A relay is reachable if any take on it answered. One failed take must not stop the rest: the
    // others may hold mail, and a take that errors has not removed anything.
    let mut answered: Vec<(String, bool)> = Vec::new();
    // Per relay this round: (fragments answered, fragments failed). A relay that has failed
    // [`DEAD_AFTER`] fragments and answered none is skipped for the rest of the round: each
    // remaining fragment would open a fresh isolated circuit and wait out the full dial timeout, so
    // one dead relay would hold up every other relay's mail for fragments × timeout. One failure is
    // not enough: arti keeps onion-service state per isolation group, so each fragment is a cold
    // connection (its own descriptor fetch and circuits), and on a phone a cold build fails often
    // enough that one miss says little about the relay. A relay that answered anything is alive and
    // keeps its fragments. Skipping loses nothing — a failed take removed nothing, and the next
    // round asks again.
    const DEAD_AFTER: usize = 2;
    let mut tally: Vec<(&Option<String>, usize, usize)> = Vec::new();
    let (mut ok_jobs, mut failed_jobs, mut skipped_jobs) = (0usize, 0usize, 0usize);
    let mut slowest_ok = Duration::ZERO;
    // Retiring yesterday needs every one of its fragments answered on every relay: a skipped or
    // failed one may still hold mail, which stops being polled once the epoch is retired.
    let mut settle_ok = plan.settles.is_some();
    for (i, job) in plan.jobs.iter().enumerate() {
        let entry = match tally.iter().position(|(a, _, _)| *a == &job.addr) {
            Some(n) => n,
            None => {
                tally.push((&job.addr, 0, 0));
                tally.len() - 1
            }
        };
        if tally[entry].1 == 0 && tally[entry].2 >= DEAD_AFTER {
            skipped_jobs += 1;
            settle_ok &= job.epoch != plan.settles;
            continue;
        }
        if plan.stagger && i > 0 {
            std::thread::sleep(Duration::from_millis(rand::thread_rng().gen_range(0..=300)));
        }
        // One request per fragment where the relay supports it (`take_many`).
        let started = std::time::Instant::now();
        let ok = match job.client.take_many(&job.handles) {
            Ok(taken) => {
                blobs.extend(taken);
                tally[entry].1 += 1;
                ok_jobs += 1;
                slowest_ok = slowest_ok.max(started.elapsed());
                true
            }
            Err(e) => {
                crate::diag!(
                    "relay: fragment take failed ({}) after {}s: {e:#}",
                    if job.addr.is_some() {
                        "extra"
                    } else {
                        "primary"
                    },
                    started.elapsed().as_secs()
                );
                tally[entry].2 += 1;
                failed_jobs += 1;
                settle_ok &= job.epoch != plan.settles;
                false
            }
        };
        if let Some(addr) = &job.addr {
            match answered.iter_mut().find(|(a, _)| a == addr) {
                Some((_, seen)) => *seen |= ok,
                None => answered.push((addr.clone(), ok)),
            }
        }
    }
    crate::diag!(
        "relay: drain round: {} fragment jobs ({} handles) on {} relay(s) — {} ok (slowest {}s), \
         {} failed, {} skipped; {} blobs",
        plan.jobs.len(),
        plan.jobs.iter().map(|j| j.handles.len()).sum::<usize>(),
        tally.len(),
        ok_jobs,
        slowest_ok.as_secs(),
        failed_jobs,
        skipped_jobs,
        blobs.len()
    );
    RelayHarvest {
        blobs,
        reachability: answered,
        settled: plan.settles.filter(|_| settle_ok),
    }
}

/// Seal `bytes` **once** and queue the identical blob on every relay that hosts `contact_id`'s
/// mailbox — the primary (shared default) plus the recipient's advertised `peer_relays` (#17
/// fan-out). Best-effort: succeeds if ≥1 relay accepts. Returns each `(relay, receipt)` so an
/// edit/unsend can later recall every copy. Posting the identical sealed bytes to all relays
/// lets the receiver de-duplicate by blob hash.
///
/// `handle` is the mailbox to post under — [`Node::post_handle`], the pair's v2 handle; empty when
/// the pair has none, which posts nothing. It is recorded in each receipt, so a later recall
/// targets the handle the copy actually went to.
fn queue_on_relays(
    transport: &dyn Transport,
    primary: &Option<RelayClient>,
    peer_relays: &[String],
    contact_id: &str,
    handle: &str,
    bytes: &[u8],
) -> Result<Vec<QueuedReceipt>> {
    if primary.is_none() && peer_relays.is_empty() {
        // Nowhere to post (we or they are direct only, or their set is unknown): no relay is
        // contacted, so nothing is sealed and no post is logged.
        anyhow::bail!("peer offline and no relay accepted the message");
    }
    if handle.is_empty() {
        // No agreed handle means no relay route (`Node::route_to`), so a caller that got here
        // anyway must not post to a mailbox nobody polls.
        anyhow::bail!("peer offline and no relay accepted the message");
    }
    let sealed = relay_wrap(contact_id, bytes)?;
    // Each recipient on its own circuits: posts for two people over one circuit would tell the
    // relay they share a correspondent — the contact graph per-pair handles exist to hide
    // (`mailbox.rs`, `post_group`). A no-op off Tor.
    let group = mailbox::post_group(contact_id);
    let mut copies = Vec::new();
    // Each refusal is logged: "at least one relay took it" hid a copy that missed the relay the
    // recipient was actually polling (2026-10-11).
    let mut tried = 0usize;
    if let Some(primary) = primary {
        tried += 1;
        let primary = mailbox::isolated(transport, primary, group);
        match primary.post(handle, &sealed, RELAY_TTL) {
            Ok(r) => copies.push(QueuedReceipt {
                relay_addr: None,
                handle: handle.to_string(),
                msg_id: r.msg_id,
                delete_token: r.delete_token,
            }),
            Err(e) => crate::diag!("relay: copy not taken (primary): {e:#}"),
        }
    }
    // The primary under another name (the signed directory lists it) is the primary: posting there
    // again would store a second copy of every message on the same relay.
    let primary_addr = primary.as_ref().and_then(|p| p.addr());
    let mut posted: Vec<&str> = primary_addr.into_iter().collect();
    for addr in peer_relays {
        if posted.contains(&addr.as_str()) {
            continue;
        }
        posted.push(addr);
        tried += 1;
        let relay = mailbox::isolated(transport, &build_relay(transport, addr), group);
        match relay.post(handle, &sealed, RELAY_TTL) {
            Ok(r) => copies.push(QueuedReceipt {
                relay_addr: Some(addr.clone()),
                handle: handle.to_string(),
                msg_id: r.msg_id,
                delete_token: r.delete_token,
            }),
            Err(e) => crate::diag!("relay: copy not taken (extra): {e:#}"),
        }
    }
    crate::diag!(
        "relay: queued a copy under the pair's handle on {}/{tried} relay(s)",
        copies.len()
    );
    if copies.is_empty() {
        anyhow::bail!("peer offline and no relay accepted the message");
    }
    Ok(copies)
}

/// A relay list as stored: duplicates and blank onions dropped, onions trimmed, at most
/// [`MAX_RELAYS`] entries. Returns the list and what the cap cut, which the caller drains out.
fn normalize_relays(list: Vec<RelayEntry>) -> (Vec<RelayEntry>, Vec<RelayEntry>) {
    let mut kept = Vec::new();
    let mut overflow = Vec::new();
    for e in list {
        let e = match e {
            RelayEntry::Onion(addr) => match addr.trim() {
                "" => continue,
                a => RelayEntry::Onion(a.to_string()),
            },
            e => e,
        };
        if kept.contains(&e) || overflow.contains(&e) {
            continue;
        }
        if kept.len() < MAX_RELAYS {
            kept.push(e);
        } else {
            overflow.push(e);
        }
    }
    (kept, overflow)
}

/// A relay set a contact told us, made safe to post to: onions trimmed, blanks, implausible
/// values and duplicates dropped, at most [`MAX_RELAYS`] (`optional-relay.md` §2: a hostile
/// contact cannot make us post one message a thousand times).
pub(crate) fn sanitize_peer_set(set: RelaySet) -> RelaySet {
    let mut relays: Vec<String> = Vec::new();
    for addr in set.relays {
        let addr = addr.trim();
        if addr.is_empty() || addr.len() > 128 || relays.iter().any(|r| r == addr) {
            continue;
        }
        if relays.len() == MAX_RELAYS {
            break;
        }
        relays.push(addr.to_string());
    }
    RelaySet {
        nightdrop: set.nightdrop,
        relays,
    }
}

/// The plaintext inside a pairing `Hello`: the joiner's relay set (`optional-relay.md` §3 rule 3),
/// so the inviter knows where the joiner's offline mail goes from the first frame, and the
/// joiner's half of onion access (§5): its client key for the inviter's onion, and a **reply
/// pass** for its own. E2E as the pre-key message, so only the inviter reads them, and both stay
/// unused until the inviter accepts.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct HelloBody {
    pub(crate) relays: RelaySet,
    /// `descriptor:x25519:…`, for the inviter to authorize on its onion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) client_key: Option<String>,
    /// The secret of a key the joiner authorized on its own onion for the inviter, base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pass: Option<String>,
    /// The joiner's random seed for the pair's v2 mailbox secret, base64 (0.2,
    /// `mailbox-handles.md` §10). The Hello goes only directly and is E2E-encrypted, so both sides
    /// hold the secret before anything is posted to a relay, and no frame of a new pair ever needs
    /// a handle derived from an identity key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mailbox: Option<String>,
}

/// A `Hello` plaintext; `None` if we cannot read it.
pub(crate) fn hello_body(plaintext: &[u8]) -> Option<HelloBody> {
    serde_json::from_slice::<HelloBody>(plaintext).ok()
}

/// The relay set a `Hello` plaintext carries; `None` if it carries none we can read.
pub(crate) fn hello_relay_set(plaintext: &[u8]) -> Option<RelaySet> {
    hello_body(plaintext).map(|b| sanitize_peer_set(b.relays))
}

/// A 32-byte secret as it travels in a payload: URL-safe base64, no padding.
pub(crate) fn encode_secret(secret: &[u8; 32]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)
}

/// The reverse of [`encode_secret`]; `None` unless exactly 32 bytes.
pub(crate) fn decode_secret(s: &str) -> Option<[u8; 32]> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim())
        .ok()?
        .try_into()
        .ok()
}

/// How long an issued pairing pass stays authorized if nobody uses it (`optional-relay.md` §5).
const PASS_TTL_SECS: u64 = 24 * 60 * 60;

/// The relay list a saved state stands for (`optional-relay.md` §2), and what the cap cut from it.
/// A state from before 0.2 has no list: it starts as Night Drop + its extras, exactly what it was
/// using.
pub(crate) fn restored_relays(state: &PersistedState) -> (Vec<RelayEntry>, Vec<RelayEntry>) {
    let list = match &state.relays {
        Some(list) => list.clone(),
        None => std::iter::once(RelayEntry::NightDrop)
            .chain(state.my_relays.iter().cloned().map(RelayEntry::Onion))
            .collect(),
    };
    normalize_relays(list)
}

/// A version string worth storing or announcing: `MAJOR.MINOR.PATCH` digits only, kept short. A
/// peer's announce is untrusted input that ends up in the UI, so anything else is ignored.
fn is_plausible_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    v.len() <= 16
        && parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit()))
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
    if receipts.is_empty() {
        return false;
    }
    let mut all_recalled = true;
    let group = mailbox::post_group(contact_id);
    for r in receipts {
        // On the recipient's posting circuits, like the post itself.
        let relay = match &r.relay_addr {
            None => primary
                .as_ref()
                .map(|p| mailbox::isolated(transport, p, group)),
            Some(addr) => Some(mailbox::isolated(
                transport,
                &build_relay(transport, addr),
                group,
            )),
        };
        let Some(relay) = relay else {
            all_recalled = false;
            continue;
        };
        let receipt = crate::relay_client::PostReceipt {
            msg_id: r.msg_id.clone(),
            delete_token: r.delete_token.clone(),
        };
        // A copy no recall can reach (`QueuedReceipt::handed_directly`, or a 0.1.x v1 copy): the
        // caller then sends the E2E edit or unsend instead.
        if r.handle.is_empty() {
            all_recalled = false;
            continue;
        }
        if !relay.recall(&r.handle, &receipt).unwrap_or(false) {
            all_recalled = false;
        }
    }
    all_recalled
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

/// What a relay-list fetch needs off the core lock: the transport for our onion site, and the
/// relays to ask if the site cannot be reached.
pub(crate) struct DirectoryFetchPlan {
    transport: Arc<dyn Transport>,
    relays: Vec<RelayClient>,
}

/// What a relay-list fetch brought back, applied under the lock by
/// [`Node::finish_directory_fetch`].
pub(crate) struct DirectoryFetchResult {
    /// The site's answer: `None` when the transport has no anonymized path.
    pub(crate) site: Option<Result<String>>,
    /// Lists served by relays, asked only when the site gave nothing usable.
    pub(crate) relay_lists: Vec<String>,
}

/// Run a relay-list fetch: our onion site first, then (only if that gave nothing) each relay.
/// Network I/O, so never under the core lock.
pub(crate) fn fetch_directory(plan: &DirectoryFetchPlan) -> DirectoryFetchResult {
    let site = crate::directory::fetch_from_site(plan.transport.as_ref());
    let relay_lists = if matches!(site, Some(Ok(_))) {
        Vec::new()
    } else {
        plan.relays
            .iter()
            .filter_map(|c| c.get_directory().ok().flatten())
            .collect()
    };
    DirectoryFetchResult { site, relay_lists }
}

/// What [`Node::adopt_directory`] made of a signed relay list.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DirectoryOutcome {
    /// Did not parse, or did not verify against the operator key.
    Invalid,
    /// Genuine, but not newer than the list we already have.
    Current,
    /// Adopted; `changed` when the relay set itself moved.
    Applied { changed: bool },
}

/// A recall receipt for one still-queued copy of a message, in the form we can both **use**
/// (reconstruct a [`RelayClient`] from `relay_addr` and delete the blob) and **persist**. `relay_addr`
/// is `None` for the primary relay, or an advertised extra relay's address (#17). See
/// [`Chat::relay_receipts`] and [`recall_receipts`].
#[derive(Clone)]
pub(crate) struct QueuedReceipt {
    /// `None` = the primary (baked-in) relay; `Some(addr)` = an advertised extra relay.
    relay_addr: Option<String>,
    /// The mailbox handle the copy was posted under (a v2 day handle), so a recall targets it.
    /// **Empty for a copy no recall can reach** ([`recall_receipts`] counts it as not recalled): one
    /// persisted by 0.1.x under a v1 handle, or [`QueuedReceipt::handed_directly`].
    handle: String,
    /// The relay's own id for the queued blob.
    msg_id: String,
    /// The secret token that authorizes deleting (recalling) the blob.
    delete_token: String,
}

impl QueuedReceipt {
    /// The copy handed straight to the peer before a relay copy was put behind it (no receipt in
    /// time, [`Node::sweep_unconfirmed`]). It may still arrive, from a connection the peer has not
    /// read yet (an app the OS froze), so recalling every relay copy does not make an edit or an
    /// unsend invisible: this entry keeps the recall from claiming so, and the E2E frame goes too.
    fn handed_directly() -> Self {
        Self {
            relay_addr: None,
            handle: String::new(),
            msg_id: String::new(),
            delete_token: String::new(),
        }
    }
}

struct Chat {
    contact: Contact,
    peer_address: Address,
    session: Session,
    history: Vec<ChatMessage>,
    /// Unix seconds when we last had **cryptographic** evidence the peer was alive: a message we
    /// decrypted, or a control frame that verified on their ratchet. Not "when they last typed" —
    /// a silent delivery ack counts, because it still proves their device drained our mailbox.
    ///
    /// Only authenticated events update it. Anything a stranger could send would otherwise let
    /// them fake liveness for someone whose phone was taken (`silence-detection.md`).
    last_seen: Option<u64>,
    /// A nickname the local user gave this contact. **Never sent** — see `contact-naming.md` §2.
    local_name: String,
    /// Our 32-byte client secret for this peer's restricted onion (#22), kept here so it can be
    /// re-inserted into the in-memory keystore at startup. `None` on a chat paired before this
    /// existed: a fresh key is minted and re-announced, which the peer accepts on the existing
    /// chat, so nothing needs re-pairing.
    client_key: Option<[u8; 32]>,
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
    /// reach the peer and only then be tombstoned (§11.3).
    relay_receipts: HashMap<String, Vec<QueuedReceipt>>,
    /// Every change of the disappearing timer, `(unix secs, timer secs)`, oldest first: what
    /// [`timer_at`] reads to give each message the timer in effect when it was sent or received.
    disappearing_timeline: Vec<(u64, u64)>,
    /// Every switch of opt-in server storage, `(unix secs, on)`, oldest first: what
    /// [`storage_on_at`] reads so the 24h bomb reaches only messages sent while it was on.
    storage_timeline: Vec<(u64, bool)>,
    /// Whether the **last** opt-in-server-storage attempt for this chat reached a relay while the
    /// peer was already online (§6). Goes false when server storage is on but no relay accepted a
    /// copy — the message still reached the peer directly, but was **not** stored server-side, so
    /// the UI downgrades the storage banner instead of silently pretending the copy exists.
    /// In-memory only (recomputed on the next send); starts optimistic.
    remote_storage_healthy: bool,
    /// Our side of the v2 mailbox agreement (`mailbox.rs`). `None` until we first announce.
    mailbox: Option<mailbox::MailboxPair>,
    /// The peer's complete relay set (`Frame::RelaySet`, `optional-relay.md` §3): where their
    /// offline mail goes. `None` = not told yet, which holds every relay copy rather than guess
    /// (§3 rule 3); a chat from before 0.2 is seeded instead (rule 5). Persisted.
    relay_set: Option<RelaySet>,
    /// Our relay set as the peer last confirmed hearing it; [`Node::announce_relay_set`] tells
    /// every chat where this differs from ours. Persisted.
    relays_told: Option<RelaySet>,
    /// What the peer last confirmed hearing of our standing announcements: that we can burn, our
    /// app version, our address. Each is re-sent only when ours differs, so a launch tells nobody
    /// we are online ([`ANNOUNCE_ONLINE_ON_START`]). Persisted; a chat from before 0.2 has none,
    /// so the first 0.2 launch tells it once.
    told_burns: bool,
    told_version: Option<String>,
    told_address: Option<String>,
    /// Whether the peer last confirmed hearing that we can report screenshots (#1).
    told_captures: Option<bool>,
    /// Pairing passes in waiting (`optional-relay.md` §5). Joiner: our own client secret for the
    /// inviter's onion, installed in place of their QR pass once their `RelaySet` proves they
    /// accepted. Inviter: the joiner's client key for our onion and their reply pass, both held
    /// unused until the user taps Accept, and dropped on Decline. Persisted.
    own_key_pending: Option<[u8; 32]>,
    peer_key_pending: Option<String>,
    pass_pending: Option<[u8; 32]>,
    /// The peer last reached us in an older wire version (`wire::legacy_sender`): it runs 0.1.x
    /// and cannot read what we send. Cleared by any frame from them we can read. Persisted.
    peer_wire_old: bool,
    /// We sent this contact's 0.1.x app the upgrade notice ([`Node::send_upgrade_notices`]).
    /// Persisted, so it is sent once, not on every launch.
    told_upgrade: bool,
    /// Bumped by every authenticated frame from the peer ([`Chat::mark_alive`]), so a caller can
    /// tell which chats it heard from in one round even within the same second, which `last_seen`
    /// cannot. In memory only.
    alive_count: u64,
}

impl Chat {
    /// The peer just proved it is alive: a frame that decrypted or verified on their ratchet.
    fn mark_alive(&mut self) {
        self.last_seen = Some(crate::api::now_secs());
        self.alive_count += 1;
        // Only a frame in our own wire version gets this far: they are no longer on 0.1, whatever
        // version we last heard. Forgetting a stale one matters: if both sides upgraded while each
        // still held the other's 0.1 version, each would hold everything for the other forever.
        self.peer_wire_old = false;
        if self
            .contact
            .peer_app_version
            .as_deref()
            .is_some_and(version_outdated)
        {
            self.contact.peer_app_version = None;
        }
        self.forget_upgrade_notice();
    }

    /// The peer is on 0.2 or later now: the note saying we sent their old app the upgrade notice
    /// ([`UPGRADE_NOTE_TEXT`]) no longer holds, so it goes. The second of two contacts to update
    /// sends one too, since it cannot yet know the other updated first, and without this its chat
    /// would keep saying "their app is too old" about someone already on 0.2. `told_upgrade` goes
    /// with it: should they ever be found on 0.1 again, they are told again.
    fn forget_upgrade_notice(&mut self) {
        if self.told_upgrade {
            self.told_upgrade = false;
            self.history
                .retain(|m| !(m.system && m.text == UPGRADE_NOTE_TEXT));
        }
    }

    /// The peer runs a release older than ours can talk to (0.1.x): known from the version they
    /// last announced, or from a frame of theirs in the older wire format.
    fn outdated(&self) -> bool {
        self.peer_wire_old
            || self
                .contact
                .peer_app_version
                .as_deref()
                .is_some_and(version_outdated)
    }
}

impl Chat {
    /// A chat just opened by pairing (either side): no history, no names, nothing verified.
    /// Pairing is itself contact, so the silence clock starts now rather than reporting a brand-new
    /// chat as silent. The client key is minted and announced right after pairing.
    fn new_paired(
        contact_id: &str,
        peer_address: &str,
        session: Session,
        authorized: bool,
        code: Option<String>,
    ) -> Self {
        Chat {
            contact: Contact {
                id: contact_id.to_string(),
                their_name: DEFAULT_NAME.to_string(),
                my_name: DEFAULT_NAME.to_string(),
                remote_storage: false,
                disappearing_secs: 0,
                backed_up: false,
                peer_backed_up: false,
                verified: false,
                peer_verified: false,
                peer_captures_silent: None,
                peer_relays: Vec::new(),
                peer_supports_burn: None,
                peer_app_version: None,
                remote_storage_healthy: true,
                // These four are filled in `contacts()` from the chat.
                last_seen_secs: 0,
                local_name: String::new(),
                identity_tag: String::new(),
                peer_on_old_version: false,
                peer_relay_state: String::new(),
                server_storage_available: false,
                peer_outdated: false,
            },
            peer_address: peer_address.to_string(),
            session,
            history: Vec::new(),
            last_seen: Some(crate::api::now_secs()),
            local_name: String::new(),
            client_key: None,
            authorized,
            code,
            closed: false,
            relay_receipts: HashMap::new(),
            disappearing_timeline: Vec::new(),
            storage_timeline: Vec::new(),
            remote_storage_healthy: true,
            mailbox: None,
            relay_set: None,
            relays_told: None,
            told_burns: false,
            told_version: None,
            told_address: None,
            told_captures: None,
            own_key_pending: None,
            peer_key_pending: None,
            pass_pending: None,
            peer_wire_old: false,
            told_upgrade: false,
            alive_count: 0,
        }
    }
}

/// One device. Owns the identity, the transport endpoint, and all chats. A contact is
/// keyed by the peer's long-term Curve25519 identity key (base64), which both sides learn
/// during the handshake.
pub struct Node {
    identity: LocalIdentity,
    /// `Arc`, not `Box`, so a blocking send can take a handle and do its network I/O **without the
    /// core lock** — the same split the relay drain already uses (§1.5.2). Constructors still take
    /// a `Box` (nothing else changes) and convert here.
    transport: Arc<dyn Transport>,
    /// The client for the baked-in **Night Drop relay**. Used only for sets that include
    /// [`RelayEntry::NightDrop`] (with the directory's relays beside it, `discovered_relays`), and
    /// to recall copies already posted to it.
    relay: Option<RelayClient>,
    /// Our relay list (`optional-relay.md` §2): where we receive, in the user's order, at most
    /// [`MAX_RELAYS`]. Contacts post our offline mail to exactly this set, and we poll it. Empty =
    /// direct only: no relay is used at all, for receiving or sending. Persisted.
    relays: Vec<RelayEntry>,
    /// Our relay set as it went out in each `RelaySet` still in flight, by contact, so a delivery
    /// records exactly what the peer heard ([`Chat::relays_told`]). In memory only.
    relay_set_in_flight: HashMap<String, RelaySet>,
    /// Relays removed from [`relays`](Self::relays), each with the unix time we stop draining it.
    /// Posting there stops at once, but mail already queued for us lives a day. Persisted.
    relays_draining: Vec<(RelayEntry, u64)>,
    chats: HashMap<String, Chat>,
    /// When true, an inbound `Hello` from a new contact must be authorized before any
    /// message is delivered or sent (§5 authorization-before-first-message).
    require_authorization: bool,
    /// The most recently minted invite code (short code), remembered so the approval
    /// signal we send back to a joiner can echo the code they used (§5).
    last_invite_code: Option<String>,
    /// What we last told peers about whether this device can report screenshots (#1). `None` until
    /// the UI says; only a change is announced, so a restart does not re-broadcast to every chat.
    captures_visible: Option<bool>,
    /// Whether to tell senders when we open their burn message (`Frame::Viewed`). **Off by
    /// default**: it discloses our reading behaviour, and the person whose behaviour it is should
    /// choose. Nothing depends on it — with it off, a sender's copy simply lives its full 24h.
    burn_receipts: bool,
    /// Contacts told this run that we understand burn messages, or being told (see
    /// [`Node::announce_burns`]). A failure removes the contact so the next tick retries; what
    /// a contact confirmed is kept per chat ([`Chat::told_burns`]), so nothing is re-sent on a
    /// later launch unless [`announce_online`](Self::announce_online).
    burns_announced: std::collections::HashSet<String>,
    /// [`ANNOUNCE_ONLINE_ON_START`]; a field so tests can run both settings.
    announce_online: bool,
    /// Contacts whose upgrade notice is on its way ([`Node::send_upgrade_notices`]). In memory.
    upgrade_notice_in_flight: std::collections::HashSet<String>,
    /// Contacts told our screenshot capability this run, or being told, with the value that went
    /// out (see [`Node::announce_captures`]). Like [`burns_announced`](Self::burns_announced).
    captures_announced: HashMap<String, bool>,
    /// This build's app version (`"0.1.27"`), set by the app via
    /// [`set_app_version`](Self::set_app_version) — the core crate does not know it. `None` until set, and nothing is
    /// announced until then: a guessed version would be worse than none.
    app_version: Option<String>,
    /// Contacts told our app version this run, or being told, with the version that went out (see
    /// [`Node::announce_version`]). Like [`burns_announced`](Self::burns_announced).
    version_announced: HashMap<String, String>,
    /// Contacts this run has sent our v2 mailbox contribution to (see [`Node::announce_mailbox`]).
    /// Per run, like [`burns_announced`](Self::burns_announced): an unconfirmed pair is retried on
    /// every launch, which is what heals a lost frame.
    mailbox_announced: std::collections::HashSet<String>,
    /// Contacts told our address this run, or being told, with the address that went out (see
    /// [`Node::reannounce_address`]). Like [`burns_announced`](Self::burns_announced).
    address_announced: HashMap<String, String>,
    /// Keys our polling-fragment partition (`mailbox.rs`, `poll_fragments`). Random per device,
    /// persisted, so a partition stays fixed for its epoch across restarts.
    poll_seed: [u8; 32],
    /// Our access keys for PRIVATE relays (§3.2): `(relay onion, public key string, secret)`.
    /// Persisted and re-inserted at startup ([`restore_client_keys`](Self::restore_client_keys)),
    /// because the keystore is in memory and the operator authorized this exact key.
    relay_keys: Vec<(String, String, [u8; 32])>,
    /// Yesterday's epoch once a drain past the skew margin emptied it on every relay, so it is no
    /// longer polled (`mailbox.rs`, `polled_epochs`). In memory only: after a restart it is polled
    /// again until the next such drain, which costs a few requests and loses nothing.
    prev_epoch_drained: Option<u64>,
    /// When this node started, for [`announce_ready`](Self::announce_ready)'s fallback.
    started: std::time::Instant,
    /// Tests only: behave like a build from before v2 mailboxes — never send a
    /// [`Frame::MailboxKey`] or a mailbox seed in a `Hello`, and drop any `MailboxKey` that arrives
    /// undecoded, as an older build does. A pair it makes has no mailbox until it stops.
    #[cfg(test)]
    legacy_no_mailbox: bool,
    /// Where media attachments are stored at rest: `(dir, key)`. Each attachment is sealed
    /// under `key` into its own file in `dir`, and the message only references its id — so
    /// large media never inflates the JSON state blob. `None` disables media (demo/tests).
    media_store: Option<(String, StoreKey)>,
    /// Attachments decoded from a restored backup, waiting to be written into the media store
    /// once one is configured (the store is set after the node is built). See [`set_media_store`](Self::set_media_store).
    pending_media: Vec<crate::storage::PersistedMedia>,
    /// Arti's base state dir (the one passed to the Tor transport). A backup copies any keystore
    /// files arti still keeps on disk there (installs not yet migrated off the 0.1.15 layout); the
    /// onion identity itself comes from the transport. `None` when Tor isn't used.
    tor_state_dir: Option<String>,
    /// Set whenever an inbound frame mutated state (including silent control frames like Ack),
    /// so the driver refreshes/persists the UI even when no user message was produced.
    dirty: bool,
    /// Outstanding short-code invites this device is hosting (§5b). Each holds the SPAKE2
    /// secret words and the pre-key/onion payload to hand out; the background poller answers a
    /// joiner's SPAKE2 opener from these (see [`service_invites`](crate::node::service_invites)).
    /// **Persisted** (`export_pending_invites`), so a code survives a core rebuild or restart within
    /// its TTL — a rebuild mid-pairing used to leave the joiner waiting on an inviter that had
    /// forgotten the code (`ARCHITECTURE.md` §5b).
    pending_invites: Vec<PendingInvite>,
    /// Our own address as of the last persisted state (empty for a brand-new node). Compared
    /// against the live transport address on startup: if the onion changed (e.g. a rebuilt Tor
    /// keystore), we announce the new address to contacts so they can still reach us (§5c, #11).
    restored_address: String,
    /// Hashes of the **sealed frames** we've already processed, whichever way they reached us.
    ///
    /// De-duplicates a message a sender fanned out to several relays (#17), including the case
    /// where one relay was down when its sibling was drained — and, because it spans both intake
    /// paths, the copy that arrives directly *and* on the relay. That happens on every message
    /// once server storage is on, and the second copy is the same Olm ciphertext, whose message
    /// key the first decrypt already consumed: it cannot decrypt, and before this it took the rest
    /// of the drain down with it (see [`apply_relay_harvest`](Self::apply_relay_harvest)).
    ///
    /// Hashed over the frame *inside* the relay envelope, so the same frame matches whether it
    /// arrived wrapped or not. A re-sent message is re-sealed and so hashes differently — that is
    /// what the id-level dedup in `process_frame` is for. Bounded (cleared past a cap); in-memory
    /// only.
    seen_frames: std::collections::HashSet<[u8; 32]>,
    /// Reachability of **our own** advertised extra relays (`my_relays`, #17), keyed by address,
    /// as observed on the last relay drain. `false` = that relay (e.g. one the
    /// user self-hosts) did not answer our drain, so contacts' mail to us via it may be stuck. Drives
    /// the "your relay is offline — add a backup" warning. Absent = not yet probed (treated as up).
    relay_reachable: std::collections::HashMap<String, bool>,
    /// The **held queue** (`optional-relay.md` §4): frames that reached neither the peer nor any
    /// relay, in the order they were sent. Messages, and control frames to a contact with no relay
    /// route (an edit, a rename, an approval), which would otherwise be lost. A contact with relays
    /// gets a relay copy on each relay tick, as before (arti still cold); one without is retried
    /// directly, oldest first and one at a time, on [`held_backoff`](Self::held_backoff), and at once
    /// when it is heard from. **Persisted**: for a direct-only pair this is the only copy, and it is
    /// kept until delivered or the chat is deleted. Only frames explicitly held are here, never
    /// everything in the "sent" state (see [`awaiting_receipt`](Self::awaiting_receipt)).
    held: Vec<HeldSend>,
    /// Pairing passes we issued and have not revoked: `(auth name, invite one-time key, expiry)`
    /// (`optional-relay.md` §5). Persisted.
    passes: Vec<(String, String, u64)>,
    /// When each contact with no relay route is next tried directly: `(delay, next_at)` in seconds,
    /// see [`HELD_RETRY_FIRST_SECS`]. Absent = due now. In memory: after a restart everything is
    /// due once, which is one dial per held contact.
    held_backoff: HashMap<String, (u64, u64)>,
    /// Whether the app is in the background, for the retry cap. Set by the app.
    background: bool,
    /// Messages composed while a **non-synchronous** transport (Tor) is in use: [`Node::send`]
    /// seals + stores them "queued" and defers the network here so composing never blocks the UI
    /// on a dial. The poller drains this via [`plan_pending_sends`](Self::plan_pending_sends),
    /// attempting direct-peer delivery with relay fallback. In-memory only, and drained on the very
    /// next poll tick (~80 ms), so a restart in that window just leaves the message "queued" — the
    /// same recovery profile as [`pending_relay`](Self::pending_relay).
    pending_sends: Vec<PendingRelaySend>,
    /// Authenticated control frames that reached neither the peer nor any relay when sent (arti
    /// still cold, or the relay briefly unreachable): `Closed` signals (chat deletes, §11.6) and
    /// screenshot notices ([`report_screenshot`](Self::report_screenshot)). Unlike
    /// [`pending_relay`](Self::pending_relay) these are **chat-independent** — a deleted chat is
    /// already gone — so they carry their own recipient + sealed bytes and are retried by the poller
    /// until a relay accepts a copy. **Persisted** (each is a one-shot the peer must eventually see),
    /// so the retry survives a restart mid-outage; re-delivery is idempotent (the peer's ratchet rejects
    /// a replayed marker).
    pending_control: Vec<PendingControl>,
    /// Frames sealed by a UI call and waiting to be sent **off** the lock by that call's caller
    /// (see [`DetachedSend`]). Drained by [`Node::take_detached_sends`]; in memory only.
    detached_sends: Vec<DetachedSend>,
    /// Control frames and relay retries sealed under the lock and waiting to be delivered without
    /// it ([`Outbound`]). Filled only on asynchronous transports; drained by the poller
    /// ([`take_outbox`](Node::take_outbox)) or by the API call waiting on its own entries. In
    /// memory: whatever must survive a restart is also held in `pending_control`.
    outbox: Vec<Outbound>,
    /// Delivery receipts and relay acks for messages this tick received, sealed under the lock and
    /// waiting for the poller to send them **after** it has surfaced those messages and released
    /// the lock. Sent inline instead, they held back the "message" event — and so the notification
    /// — for a whole failed direct dial plus the relay fallback: ~45 s on a phone in Doze
    /// (2026-09-28). Drained by [`Node::take_receipt_sends`]; empty on synchronous transports, which
    /// still send inline. In memory only: a receipt lost to a restart is recovered by the sender's
    /// relay retry, which is receipted again when it lands.
    receipt_sends: Vec<DetachedSend>,
    /// Shared default relays learned from the operator-signed **relay directory** (§3.1): fetched
    /// daily from our onion site (a relay is asked only when the site cannot be reached), verified
    /// against the baked-in [`crate::directory::DIRECTORY_PUBKEY`],
    /// and treated like additional primaries (drained, paired over, and posted to). This is how the
    /// relay set rotates **without an app update** — a lost/rotated relay onion no longer strands
    /// users once a newer signed list names its replacement. Persisted.
    discovered_relays: Vec<String>,
    /// Version of the last relay directory we accepted; a fetched list is applied only if newer
    /// (monotonic anti-rollback). Persisted.
    directory_version: u64,
    /// Signing keys learned through rotations (`keyroll.rs`); None = the baked key. Persisted.
    pub(crate) update_key: Option<crate::keyroll::Trust>,
    pub(crate) directory_key: Option<crate::keyroll::Trust>,
    /// When the relay list is next fetched from our onion site (unix seconds; 0 = as soon as Tor is
    /// up). Persisted, so the daily schedule holds across restarts.
    directory_next_check: u64,
    /// Directly-sent messages still waiting for a receipt that names them; swept by
    /// [`sweep_unconfirmed`](Self::sweep_unconfirmed).
    ///
    /// **In memory only, on purpose.** Seeding it from persisted history on start-up would, on the
    /// first launch after upgrading, re-queue every message ever left in the pre-existing "sent"
    /// state — a burst of duplicates to every contact. The cost is that a send lost across a
    /// *sender* restart is not retried; the case this exists for is the receiver going away, with
    /// the sender still up.
    awaiting_receipt: Vec<AwaitingReceipt>,
    /// Consecutive failed direct (onion-to-onion) sends since the last successful one, and whether
    /// *any* direct send has succeeded this run. Together they answer "can this device reach anyone
    /// at all, or is it only ever falling back to the relay?" — see
    /// [`direct_path_wedged`](Self::direct_path_wedged). In memory only; a fresh run re-measures.
    direct_failures: u32,
    direct_ever_succeeded: bool,
    /// Whether any **relay** operation has succeeded this run. The discriminator between "this
    /// peer is offline" and "this device cannot reach the network": the relay is dialled over the
    /// same Tor path, so if it answers, our circuits work and an unreachable peer is their problem.
    relay_ever_succeeded: bool,
    /// `(peer, id)` for every user message we have **accepted**, owed a [`Frame::Delivered`].
    ///
    /// Recorded by the frame handlers themselves, at the point of acceptance, because that is the
    /// only place the answer is known: a media frame keeps its id *inside* the encrypted envelope,
    /// so nothing upstream of the decrypt can name it. Building it from the frame beforehand — as
    /// this used to — also risks receipting something that is then dropped.
    ///
    /// The id is the message's `msg_id` for text, and `t:<transfer_id>` for an attachment (which
    /// has no `msg_id`); see [`Frame::Delivered`]'s handler for the matching side.
    pending_receipts: Vec<(String, String)>,
}

/// Everything a batch of deferred sends needs to reach the network, snapshotted under the core lock
/// so the **blocking** part can run without it. Built by [`Node::plan_pending_sends`], consumed by
/// [`execute_sends`], applied by [`Node::apply_send_outcomes`].
///
/// This is the same three-phase split the relay drain uses (§1.5.2), for the same reason and then
/// some. A send does a peer dial (up to `PEER_DIAL_TIMEOUT`) and, on failure, a relay post per
/// target (up to `RELAY_DIAL_TIMEOUT` each) — and all of that used to happen inside `apply_tick`,
/// i.e. **holding the core lock**. On a healthy network nobody notices. On a device whose circuits
/// are timing out it means the lock is held for minutes, and everything else queues behind it: UI
/// reads, and — measured on a phone, 2026-08-03 — the teardown itself, so "Reset Tor connection"
/// did nothing at all. The app was least able to recover exactly when it most needed to.
pub(crate) struct SendPlan {
    transport: Arc<dyn Transport>,
    items: Vec<PlannedSend>,
}

struct PlannedSend {
    contact_id: String,
    /// The mailbox handle for a relay copy, chosen under the lock ([`Node::post_handle`]).
    handle: String,
    msg_id: String,
    bytes: Vec<u8>,
    peer_address: String,
    /// Where a relay copy goes ([`Node::route_to`]): the Night Drop relay's client when the
    /// recipient's set includes it, and the onions to post to besides.
    primary: Option<RelayClient>,
    relay_targets: Vec<String>,
    /// Opt-in server storage: post a copy even when the direct send succeeds (§6).
    remote_storage: bool,
}

/// What [`execute_sends`] observed, to be folded back into the node under the lock.
pub(crate) struct SendOutcomes {
    items: Vec<SendOutcome>,
}

struct SendOutcome {
    contact_id: String,
    msg_id: String,
    /// The sealed frame, kept so a send that reached neither the peer nor a relay can be retried
    /// verbatim (re-sealing would advance the ratchet again).
    bytes: Vec<u8>,
    delivered: bool,
    /// `Some` = at least one relay accepted a copy (receipts for a later edit/unsend recall).
    copies: Option<Vec<QueuedReceipt>>,
    /// A relay post was attempted and every relay refused it.
    relay_failed: bool,
}

/// Perform a batch of sends with **no core lock held** — the dials and relay posts that used to
/// block every other caller. Pure I/O: it changes no node state, and everything it learns comes
/// back in [`SendOutcomes`] for [`Node::apply_send_outcomes`] to record.
pub(crate) fn execute_sends(plan: &SendPlan) -> SendOutcomes {
    let mut items = Vec::with_capacity(plan.items.len());
    for p in &plan.items {
        let started = std::time::Instant::now();
        let sent = plan.transport.send(&p.peer_address, &p.bytes);
        match &sent {
            Ok(()) => crate::diag!(
                "send: handed to the peer's onion in {}ms",
                started.elapsed().as_millis()
            ),
            Err(e) => crate::diag!(
                "send: direct dial failed after {}s — relay fallback: {e:#}",
                started.elapsed().as_secs()
            ),
        }
        let delivered = sent.is_ok();
        let mut copies = None;
        let mut relay_failed = false;
        if !delivered || p.remote_storage {
            match queue_on_relays(
                plan.transport.as_ref(),
                &p.primary,
                &p.relay_targets,
                &p.contact_id,
                &p.handle,
                &p.bytes,
            ) {
                Ok(c) => copies = Some(c),
                Err(_) => relay_failed = true,
            }
        }
        items.push(SendOutcome {
            contact_id: p.contact_id.clone(),
            msg_id: p.msg_id.clone(),
            bytes: p.bytes.clone(),
            delivered,
            copies,
            relay_failed,
        });
    }
    SendOutcomes { items }
}

/// One already-sealed control frame to deliver **with no core lock held** — a single-frame
/// [`Node::deliver`] (direct dial, relay fallback) whose routing was snapshotted under the lock.
///
/// For frames sent from a **UI call** rather than the poller, where the caller is waiting on the
/// result. The burn-view receipt is the case that needed it: sent inline, it dialled the peer while
/// holding the lock, *after* the countdown had started — so the reveal took as long as a Tor dial
/// (plus a relay post when the peer was offline) to appear, and arrived with that time already gone
/// from its timer. Taken with [`Node::take_detached_sends`]; best-effort, nothing retries it.
pub(crate) struct DetachedSend {
    transport: Arc<dyn Transport>,
    primary: Option<RelayClient>,
    peer_relays: Vec<String>,
    recipient_ik: String,
    /// The mailbox handle for a relay copy, chosen under the lock ([`Node::post_handle`]).
    handle: String,
    peer_address: String,
    bytes: Vec<u8>,
    /// Dial a new connection rather than write into one kept open ([`Transport::send_fresh`]):
    /// for frames nothing confirms, which a dead kept-open connection would swallow.
    fresh: bool,
}

impl DetachedSend {
    /// Deliver it: the peer's onion first, the relay mailbox if that fails. True if either took it.
    pub(crate) fn execute(&self) -> bool {
        !matches!(self.run(Route::DirectThenRelay), Delivery::Failed)
    }

    /// Deliver it by `route`, reporting which path took it (and the relay receipts, so an edit or
    /// unsend can recall the copy later).
    pub(crate) fn run(&self, route: Route) -> Delivery {
        let direct = || {
            if self.peer_address.is_empty() {
                return false; // relay-only by construction (cover traffic)
            }
            if self.fresh {
                self.transport.send_fresh(&self.peer_address, &self.bytes)
            } else {
                self.transport.send(&self.peer_address, &self.bytes)
            }
            .is_ok()
        };
        let relay = || {
            queue_on_relays(
                self.transport.as_ref(),
                &self.primary,
                &self.peer_relays,
                &self.recipient_ik,
                &self.handle,
                &self.bytes,
            )
            .ok()
        };
        match route {
            Route::DirectThenRelay => {
                if direct() {
                    return Delivery::Direct;
                }
                if self.primary.is_none() && self.peer_relays.is_empty() {
                    crate::diag!("detached send: direct dial failed — no relay route, held for the next direct try");
                    return Delivery::Failed;
                }
                crate::diag!("detached send: direct dial failed — falling back to the relay");
                match relay() {
                    Some(copies) => {
                        crate::diag!("detached send: queued on the relay for the peer to drain");
                        Delivery::Relay(copies)
                    }
                    None => {
                        crate::diag!("detached send: relay fallback also failed this attempt");
                        Delivery::Failed
                    }
                }
            }
            Route::RelayThenDirect => match relay() {
                Some(copies) => Delivery::Relay(copies),
                None if direct() => Delivery::Direct,
                None => Delivery::Failed,
            },
            Route::RelayOnly => relay().map_or(Delivery::Failed, Delivery::Relay),
        }
    }
}

/// The relay copies an edit or unsend must recall before it knows which path it takes
/// ([`Node::begin_edit`]); run without the lock.
pub(crate) struct RecallPlan {
    transport: Arc<dyn Transport>,
    primary: Option<RelayClient>,
    contact_id: String,
    copies: Vec<QueuedReceipt>,
}

impl RecallPlan {
    /// Recall every copy. True only when all of them were (see [`recall_receipts`]).
    pub(crate) fn run(&self) -> bool {
        recall_receipts(
            self.transport.as_ref(),
            &self.primary,
            &self.contact_id,
            &self.copies,
        )
    }
}

/// Which path an [`Outbound`] tries, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// The peer's onion, then their relay set: most control frames.
    DirectThenRelay,
    /// The relay set, then the onion: a chat-delete `Closed`, whose peer is usually reachable only
    /// through the relay (§11.6), and the `pending_control` retries.
    RelayThenDirect,
    /// Only the relay set: retries of messages that already failed direct, and cover traffic.
    RelayOnly,
}

/// What took an [`Outbound`].
pub(crate) enum Delivery {
    Direct,
    Relay(Vec<QueuedReceipt>),
    Failed,
}

/// Which standing announcement an [`OutboundDone::Announced`] stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Announce {
    Burns,
    Version,
    Mailbox,
    Address,
    RelaySet,
    Captures,
    UpgradeNotice,
}

/// What a finished [`Outbound`] changes in the core, applied under the lock by
/// [`Node::apply_outbound`].
pub(crate) enum OutboundDone {
    /// Best-effort: nothing to record.
    Nothing,
    /// The API call that queued it is waiting on the result (approve, QR `Hello`, edit, unsend).
    /// With a `(contact, msg_id)`, the relay receipts are also recorded against that message: an
    /// edit's replacement copy, which a later edit or unsend must be able to recall.
    Caller(Option<(String, String)>),
    /// A standing announcement. Its flag is set when queued, so the next tick does not queue a
    /// second copy while this one is in flight, and cleared again if it fails, so the tick after
    /// retries.
    Announced(Announce, String),
    /// A `pending_control` entry, identified by its sealed bytes (unique: each is a fresh ratchet
    /// message). Dropped on success; otherwise left for the next poll.
    PendingControl(Vec<u8>),
    /// A [`HeldSend`] retry, named by its sealed bytes (unique: each is a fresh ratchet message).
    /// Removed once delivered; otherwise left for the next retry.
    Held(Vec<u8>),
    /// A relay copy behind an unreceipted message ([`Node::sweep_unconfirmed`]).
    Unconfirmed(AwaitingReceipt),
    /// Cover traffic: logged either way, never retried.
    Cover,
}

/// A frame sealed under the lock and delivered without it.
///
/// Every control frame and relay retry the core raises goes through here
/// ([`Node::send_or_queue`]). On a synchronous transport it runs at once, as before; on Tor it
/// waits in [`Node::outbox`] for the poller, which runs it on its own job after releasing the lock
/// and re-takes the lock only to [apply](Node::apply_outbound) what happened. An API call that
/// reports the result to its caller takes its own entries instead ([`Node::take_caller_outbox`]).
pub(crate) struct Outbound {
    pub(crate) send: DetachedSend,
    pub(crate) route: Route,
    pub(crate) done: OutboundDone,
}

/// An [`Outbound`] after its network round, ready for [`Node::apply_outbound`].
pub(crate) struct Finished {
    pub(crate) done: OutboundDone,
    pub(crate) delivery: Delivery,
    /// Who it was for, and the frame, so a failed frame to a contact with no relay route can be
    /// held rather than lost.
    pub(crate) recipient: String,
    pub(crate) bytes: Vec<u8>,
    /// The send had no relay to fall back on: a failure means the peer was offline, and the frame
    /// is held for direct delivery.
    pub(crate) relayless: bool,
}

impl Outbound {
    pub(crate) fn run(self) -> Finished {
        let delivery = self.send.run(self.route);
        Finished {
            done: self.done,
            delivery,
            recipient: self.send.recipient_ik,
            relayless: self.send.primary.is_none() && self.send.peer_relays.is_empty(),
            bytes: self.send.bytes,
        }
    }

    /// Never sent (the core is shutting down): applied as a failure, so anything that retries is
    /// put back to be persisted.
    pub(crate) fn abandon(self) -> Finished {
        Finished {
            done: self.done,
            delivery: Delivery::Failed,
            recipient: self.send.recipient_ik,
            relayless: self.send.primary.is_none() && self.send.peer_relays.is_empty(),
            bytes: self.send.bytes,
        }
    }

    pub(crate) fn is_caller(&self) -> bool {
        matches!(self.done, OutboundDone::Caller(_))
    }
}

impl Finished {
    /// A caller's entry that reached neither the peer nor any relay, and is not held for direct
    /// delivery either: a contact with no relay route being offline is not an error, it is what
    /// holding is for.
    pub(crate) fn caller_failed(&self) -> bool {
        matches!(self.done, OutboundDone::Caller(_))
            && matches!(self.delivery, Delivery::Failed)
            && !self.relayless
    }
}

/// Run queued [`Outbound`]s, in order, with no core lock held.
pub(crate) fn run_outbox(sends: Vec<Outbound>) -> Vec<Finished> {
    sends.into_iter().map(Outbound::run).collect()
}

/// A message handed to the peer's onion but not yet confirmed by a [`Frame::Delivered`] naming it
/// (see [`Node::awaiting_receipt`]).
///
/// A successful dial only means their service answered. The frame can still be lost — that is
/// exactly what happened on 2026-08-02, when a core was torn down mid-flight and the message
/// vanished with the sender's side showing it as sent. Once this has gone unconfirmed for
/// [`RECEIPT_TIMEOUT`](crate::node::messaging::RECEIPT_TIMEOUT), the poller re-queues the message on
/// the relay so it survives the peer being offline, restarted or rebuilt.
pub(crate) struct AwaitingReceipt {
    contact_id: String,
    msg_id: String,
    /// When the direct send succeeded (unix seconds).
    since: u64,
}

/// A sent message awaiting a relay to accept its store-and-forward copy (see [`Node::pending_relay`]).
/// One frame in the held queue ([`Node::held`]).
pub(crate) struct HeldSend {
    contact_id: String,
    /// The message it carries, or empty for a control frame.
    msg_id: String,
    /// The sealed frame, re-sent verbatim so the ratchet is not advanced again.
    bytes: Vec<u8>,
    /// Queued in the outbox right now. Not persisted.
    in_flight: bool,
}

pub(crate) struct PendingRelaySend {
    contact_id: String,
    msg_id: String,
    /// The already-sealed `Frame::Message` bytes — re-posted verbatim so the ratchet is not
    /// re-advanced on retry.
    bytes: Vec<u8>,
}

/// A frame awaiting delivery that must not be lost: a chat-delete `Closed` (its chat already
/// removed), a screenshot notice, or a joiner's first-contact `Hello` (see
/// [`Node::pending_control`]). Carries its own routing so it survives the chat.
pub(crate) struct PendingControl {
    /// The peer's identity key — seals + addresses the relay copy (its mailbox handle).
    recipient_ik: String,
    /// The peer's onion/address, for the direct-send fallback.
    peer_address: String,
    /// The recipient's relay set captured when it was queued, since the chat may be gone by the
    /// time we retry: their onions, and whether they use the Night Drop relay. Expanded at send
    /// time ([`Node::post_route`]), so a Night Drop copy follows a directory rotation.
    relays: Vec<String>,
    nightdrop: bool,
    /// The already-sealed control frame bytes — re-posted verbatim (ratchet not re-advanced).
    bytes: Vec<u8>,
    /// The mailbox handle the relay copy goes under, chosen while the chat still existed
    /// ([`Node::post_handle`]): the pair's v2 handle once agreed, which is what the peer polls.
    handle: String,
    /// Queued in [`Node::outbox`] right now, so the next poll does not send a second copy. Not
    /// persisted: after a restart nothing is in flight. The entry itself stays in
    /// `pending_control` (and so in the saved state) until a copy lands.
    in_flight: bool,
}

/// One in-flight short-code invite awaiting a joiner (§5b). The `secret` never
/// leaves the device; only SPAKE2 protocol messages and a payload sealed under the derived
/// key ever reach the untrusted rendezvous, so the relay cannot brute-force the code offline.
///
/// Online, each opener is one guess at the code, so an invite answers at most
/// [`MAX_OPENERS_PER_INVITE`] *distinct* openers and is then retired (`openers`).
#[derive(Clone)]
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
    /// SHA-256 of each distinct opener answered so far. A joiner re-posts the *same* opener while
    /// it waits, which is answered again; every *different* opener is another guess.
    openers: Vec<[u8; 32]>,
}

/// How many distinct SPAKE2 openers a short-code invite answers before it is retired.
///
/// SPAKE2 stops an observer testing guesses offline, but each opener is still one online guess:
/// the inviter answers it, and the answer lets whoever sent it check that one guess. The code's
/// secret is four words from sixty-four (about 24 bits), and the relay operator sees the slot's
/// mailbox, so an invite answering every opener could be ground through in its 10-minute life.
/// A real joiner sends one opener and re-posts it unchanged; three leaves room for someone who
/// gives up and tries the code again. The cost of a cap is that three junk openers burn a code,
/// which the joiner sees as a timeout and fixes with a fresh code.
pub(crate) const MAX_OPENERS_PER_INVITE: usize = 3;

mod backup;
mod frames;
mod limits;
pub(crate) use limits::sane_address;
mod mailbox;
mod media_kind;
mod media_scrub;
mod messaging;
mod pairing;
pub(crate) use pairing::InviteExtras;

/// How long a directly-sent message may sit unconfirmed before a relay copy goes behind it.
/// Re-exported for the api-layer test that drives a whole poll cycle, so it ages a message by the
/// same window the node actually uses rather than a number that can drift away from it.
#[cfg(test)]
pub(crate) use messaging::RECEIPT_TIMEOUT;
pub(crate) use pairing::{service_invites, InviteServiceOutcome};

impl Node {
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Self::with_identity(LocalIdentity::generate(), transport)
    }

    /// Build a node from a restored identity (used by `storage::` on app restart).
    pub fn with_identity(identity: LocalIdentity, transport: Box<dyn Transport>) -> Self {
        Self {
            identity,
            transport: Arc::from(transport),
            relay: None,
            relays: vec![RelayEntry::NightDrop],
            relays_draining: Vec::new(),
            relay_set_in_flight: HashMap::new(),
            chats: HashMap::new(),
            require_authorization: false,
            last_invite_code: None,
            captures_visible: None,
            burns_announced: std::collections::HashSet::new(),
            announce_online: ANNOUNCE_ONLINE_ON_START,
            captures_announced: HashMap::new(),
            upgrade_notice_in_flight: std::collections::HashSet::new(),
            app_version: None,
            version_announced: HashMap::new(),
            mailbox_announced: std::collections::HashSet::new(),
            address_announced: HashMap::new(),
            poll_seed: {
                use rand::RngCore;
                let mut seed = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut seed);
                seed
            },
            relay_keys: Vec::new(),
            prev_epoch_drained: None,
            started: std::time::Instant::now(),
            #[cfg(test)]
            legacy_no_mailbox: false,
            burn_receipts: false,
            media_store: None,
            pending_media: Vec::new(),
            tor_state_dir: None,
            dirty: false,
            pending_invites: Vec::new(),
            restored_address: String::new(),
            seen_frames: std::collections::HashSet::new(),
            relay_reachable: std::collections::HashMap::new(),
            held: Vec::new(),
            held_backoff: HashMap::new(),
            passes: Vec::new(),
            background: false,
            pending_sends: Vec::new(),
            pending_control: Vec::new(),
            detached_sends: Vec::new(),
            outbox: Vec::new(),
            receipt_sends: Vec::new(),
            discovered_relays: Vec::new(),
            directory_version: 0,
            update_key: None,
            directory_key: None,
            directory_next_check: 0,
            awaiting_receipt: Vec::new(),
            pending_receipts: Vec::new(),
            direct_failures: 0,
            direct_ever_succeeded: false,
            relay_ever_succeeded: false,
        }
    }

    /// Take and clear the "inbound frame changed state" flag (see [`dirty`](Self::dirty)). The driver ORs
    /// this into its change detection so silent control frames (Ack, name, etc.) refresh the UI.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Tell the node where arti keeps its state, so a backup can fold in any keystore files left
    /// on disk there (see [`tor_state_dir`](Self::tor_state_dir)).
    #[cfg_attr(not(feature = "tor"), allow(dead_code))]
    pub fn set_tor_state_dir(&mut self, dir: String) {
        self.tor_state_dir = Some(dir);
    }

    /// Enable at-rest media storage: attachments are sealed under `key` into `dir`. If a
    /// restored backup carried attachments, they are sealed into the store now.
    pub fn set_media_store(&mut self, dir: String, key: StoreKey) {
        std::fs::create_dir_all(&dir).ok();
        // The media store sits beside the state file, in the app's data dir: make that owner-only.
        // On desktop it was created 0755, so other users on the machine could list it and read the
        // contacts' onion client keys (.auth) — how many contacts, and when they changed. With the
        // dir at 0700 nothing beneath it is reachable, whatever each file's own mode (self-audit
        // 2026-10-13, round 5). Android's is private already; Windows ACLs are per-user.
        if let Some(data_dir) = std::path::Path::new(&dir).parent() {
            let _ = create_private_dir(data_dir);
        }
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
                        if messaging::is_media_id(&item.id) {
                            let _ = std::fs::write(format!("{dir}/{}.bin", item.id), sealed);
                        }
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
            // A pending request has already exchanged onion client keys (#22: we answer a `Hello`
            // with ours, and accept theirs), so a decline must undo both, as `delete_chat` does —
            // or a stranger we turned away keeps permission to reach us, and our key for them
            // stays on disk in a directory named after their onion.
            if let Some(chat) = self.chats.remove(contact_id) {
                let _ = self.transport.revoke_client(contact_id);
                if !chat.peer_address.is_empty() {
                    let _ = self.transport.forget_peer_key(&chat.peer_address);
                }
            }
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
        if !already_authorized {
            self.check_contact_cap(contact_id)?;
        }
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
        // Onion access follows the user's decision: only now does the request reach us.
        self.apply_pending_keys(contact_id);
        let from = self.identity_key();
        let code = code.unwrap_or_default();
        devlog!("[nightdrop] authorize: approved {contact_id}; sending approval (code='{code}')");
        // The caller (`Core::authorize`) sends this itself, off the lock, and reports a failure.
        self.deliver_with(
            &peer_address,
            contact_id,
            &Frame::Approved { from, code },
            false,
            OutboundDone::Caller(None),
        )?;
        // A request was told nothing about us; now it is a contact.
        self.announce_relay_set_to(contact_id);
        self.announce_captures_to(contact_id);
        self.announce_burns_to(contact_id);
        self.announce_version_to(contact_id);
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
            // The peer's set as of now: the chat is gone by the time a retry runs.
            let set = self.peer_relay_set(contact_id).unwrap_or_default();
            // Held in `pending_control` from the start, not only after a failed first attempt: the
            // send now happens off the lock, after this call returns, and the chat is gone by then.
            // A copy landing removes it; until then it is saved with the state and retried.
            let handle = self.post_handle(contact_id).unwrap_or_default();
            self.queue_pending_control(
                PendingControl {
                    recipient_ik: contact_id.to_string(),
                    peer_address: addr,
                    relays: set.relays,
                    nightdrop: set.nightdrop,
                    bytes,
                    handle,
                    in_flight: false,
                },
                Route::RelayThenDirect,
            );
        }
        // Read the address out before the chat goes: forgetting our client key needs it, and it is
        // the only place we hold it.
        let peer_onion = self
            .chats
            .get(contact_id)
            .map(|c| c.peer_address.clone())
            .unwrap_or_default();
        // Its attachments go with it: sealed files and any decrypted copy. They were left on disk,
        // unreferenced but readable by anyone holding the store key (self-audit 2026-10-13, round 6).
        let media: Vec<String> = self
            .chats
            .get(contact_id)
            .map(|c| {
                c.history
                    .iter()
                    .flat_map(|m| [m.media_id.clone(), m.thumb_id.clone()])
                    .filter(|id| !id.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        for id in &media {
            self.forget_media(id);
        }
        self.chats.remove(contact_id);
        // Onion client auth (#22), both directions. `revoke_client` drops *their* permission to
        // reach us. `forget_peer_key` drops *our* key for reaching them — which arti stores in a
        // directory named after their onion address, so skipping it leaves a deleted contact's
        // address on disk indefinitely. No-op off Tor.
        let _ = self.transport.revoke_client(contact_id);
        if !peer_onion.is_empty() {
            let _ = self.transport.forget_peer_key(&peer_onion);
        }
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

    /// Tell every open chat whether this device can report screenshots at all (#1).
    ///
    /// Sent to the PEER, not shown to us: we already know whether we took a screenshot. The person
    /// who needs this is the one deciding what to send, because below Android 14 a capture raises
    /// no notice and the absence of one therefore means nothing.
    ///
    /// Each chat is sent the value only when it differs from what that chat confirmed
    /// ([`Chat::told_captures`], persisted), and an undelivered one is re-sent on later ticks
    /// ([`reannounce_captures`](Self::reannounce_captures)). Until 0.2 it went to every chat on
    /// every launch, which told contacts when we came online ([`ANNOUNCE_ONLINE_ON_START`]).
    pub fn announce_captures(&mut self, visible: bool) {
        if self.captures_visible != Some(visible) {
            self.captures_visible = Some(visible);
            self.captures_announced.clear();
        }
        self.reannounce_captures();
    }

    /// [`announce_captures`](Self::announce_captures) again for every chat that has not confirmed
    /// the current value: the retry, run on the relay tick. Silent until the app has told us the
    /// value — guessing would be the false reassurance the whole signal exists to remove.
    pub fn reannounce_captures(&mut self) {
        let now = crate::api::now_secs();
        let Some(visible) = self.captures_visible else {
            return;
        };
        let ids: Vec<String> = self
            .chats
            .iter()
            .filter(|(id, c)| {
                c.authorized
                    && !c.closed
                    && !self.captures_announced.contains_key(id.as_str())
                    && (self.announce_online || c.told_captures != Some(visible))
                    && self.may_retry(id, now)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.announce_captures_to(&id);
        }
    }

    /// Tell **every** open chat that this build understands burn messages.
    ///
    /// Needed because [`announce_burns_to`](Self::announce_burns_to) only fires at pairing, so a
    /// chat that already existed before this feature shipped would never hear it — and an
    /// unannounced peer reads as unsupported, which would leave burn permanently unavailable for
    /// exactly the contacts someone already talks to. Runs on the relay tick and sends only to
    /// chats that have not confirmed it ([`Chat::told_burns`]), so a launch re-announces to
    /// nobody ([`ANNOUNCE_ONLINE_ON_START`]).
    pub fn announce_burns(&mut self) {
        let now = crate::api::now_secs();
        if !self.announce_ready() {
            return;
        }
        let ids: Vec<String> = self
            .chats
            .iter()
            .filter(|(id, c)| {
                c.authorized
                    && !c.closed
                    && !self.burns_announced.contains(id.as_str())
                    && (self.announce_online || !c.told_burns)
                    && self.may_retry(id, now)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.announce_burns_to(&id);
        }
    }

    /// Tell one chat that this build understands burn messages. Unconditional — support is a
    /// property of the build, not a user setting — and quiet: no history entry, because it is a
    /// standing fact rather than an event.
    fn announce_burns_to(&mut self, contact_id: &str) {
        let Some((addr, frame)) =
            self.authed_control(contact_id, MARK_BURNS_V1, |from, message| Frame::Burns {
                from,
                message,
            })
        else {
            return;
        };
        // `deliver`, not a bare `transport.send`: it falls back to the relay mailbox when the peer
        // is offline. A dropped announce is not cosmetic — an unannounced peer reads as "cannot
        // burn", so losing this frame silently disables the feature for that chat until a restart.
        // Flagged while in flight; a failure clears the flag, so the next tick retries.
        self.burns_announced.insert(contact_id.to_string());
        let done = OutboundDone::Announced(Announce::Burns, contact_id.to_string());
        let _ = self.deliver_with(&addr, contact_id, &frame, false, done);
    }

    /// This build's app version, as [`set_app_version`](Self::set_app_version) recorded it.
    pub(crate) fn app_version(&self) -> Option<&str> {
        self.app_version.as_deref()
    }

    /// Record this build's app version (the pubspec form, `"0.1.27+412"`, is accepted; the build
    /// number is dropped). A changed version re-announces to every chat.
    pub fn set_app_version(&mut self, version: &str) {
        let version = version.split('+').next().unwrap_or("").trim().to_string();
        if !is_plausible_version(&version) || self.app_version.as_deref() == Some(version.as_str())
        {
            return;
        }
        self.app_version = Some(version);
        self.version_announced.clear(); // chats told the old one now differ, and are told again
    }

    /// Tell every open chat which Night Drop version this build is (`Frame::Version`).
    ///
    /// Needed so a later build can tell which contacts are still on an older release: 0.2 does not
    /// talk to 0.1.x and has to say so per chat, which it can only do for contacts that announced.
    /// Same shape as [`announce_burns`](Self::announce_burns): to each chat whose confirmed
    /// version differs ([`Chat::told_version`]), retried while undelivered, and quiet. An older peer cannot decode the frame and skips it.
    pub fn announce_version(&mut self) {
        let now = crate::api::now_secs();
        if self.app_version.is_none() || !self.announce_ready() {
            return;
        }
        let ids: Vec<String> = self
            .chats
            .iter()
            .filter(|(id, c)| {
                c.authorized
                    && !c.closed
                    && !self.version_announced.contains_key(id.as_str())
                    && (self.announce_online || c.told_version != self.app_version)
                    // Even to a contact we think is on 0.1: it is how one that has updated learns
                    // that we have too (see `deliver_with`).
                    && self.retry_due_even_if_outdated(id, now)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.announce_version_to(&id);
        }
    }

    /// Tell one chat our app version. Flagged while in flight and cleared on failure, so an
    /// unreachable peer is retried on the next tick (`deliver` already falls back to the relay).
    pub(crate) fn announce_version_to(&mut self, contact_id: &str) {
        let Some(version) = self.app_version.clone() else {
            return;
        };
        let marker = [MARK_VERSION_PREFIX, version.as_bytes()].concat();
        let Some((addr, frame)) = self.authed_control(contact_id, &marker, |from, message| {
            Frame::Version { from, message }
        }) else {
            return;
        };
        self.version_announced
            .insert(contact_id.to_string(), version.clone());
        let done = OutboundDone::Announced(Announce::Version, contact_id.to_string());
        let _ = self.deliver_with(&addr, contact_id, &frame, false, done);
    }

    /// Tell one freshly paired chat what [`announce_captures`](Self::announce_captures) already told
    /// the others.
    ///
    /// Without this, a contact added *after* the launch-time announce would never hear it at all —
    /// the broadcast only walks the chats that existed when it ran, and the value never changes
    /// again. Silent for a node that has not been told its own capability yet: guessing here would
    /// be the false reassurance the whole signal exists to remove.
    fn announce_captures_to(&mut self, contact_id: &str) {
        let Some(visible) = self.captures_visible else {
            return;
        };
        let marker = if visible {
            MARK_CAPTURES_VISIBLE
        } else {
            MARK_CAPTURES_SILENT
        };
        if let Some((addr, frame)) = self.authed_control(contact_id, marker, |from, message| {
            Frame::Captures { from, message }
        }) {
            // Flagged while in flight, with the value; a failure clears it for the next tick.
            self.captures_announced
                .insert(contact_id.to_string(), visible);
            let done = OutboundDone::Announced(Announce::Captures, contact_id.to_string());
            let _ = self.deliver_with(&addr, contact_id, &frame, false, done);
        }
    }

    /// Report that the user took a **screenshot** of this chat (#1), to both sides of it.
    ///
    /// Screenshots are deliberately *allowed* — blocking them only pushes people to photograph the
    /// screen, which we cannot see at all — so the honest alternative is to make them visible. Logs
    /// a local note (so the user knows what their peer was told) and sends the peer a
    /// [`Screenshot`](Frame::Screenshot) frame — direct if they're reachable, else queued on the
    /// relay for their next drain, else held in `pending_control` for the poller to retry. The
    /// capture itself never fails; only the notice is retried.
    ///
    /// Every screenshot is reported; there is no dedupe flag, because the *count* and *timing* are
    /// the informative part.
    pub fn report_screenshot(&mut self, contact_id: &str) {
        let Some(chat) = self.chats.get_mut(contact_id) else {
            return;
        };
        if chat.closed {
            return; // nobody left to tell
        }
        chat.history.push(ChatMessage::system(
            "📸 You took a screenshot. The other person was told.".to_string(),
        ));
        if let Some((addr, frame)) =
            self.authed_control(contact_id, MARK_SCREENSHOT, |from, message| {
                Frame::Screenshot { from, message }
            })
        {
            // Direct-first (unlike `delete_chat`): when the peer *is* online, "they are
            // screenshotting this right now" is the informative case and should not wait for a
            // relay poll; an offline peer picks up the relay copy on their next drain.
            //
            // Held in `pending_control` from the start, exactly as a chat-delete `Closed` is: the
            // local side has already promised the user that "the other person was told", the phone
            // may well be offline while screenshotting, and unlike a chat message there is nothing
            // the user can resend to make good on it. A copy landing removes it.
            let set = self.peer_relay_set(contact_id).unwrap_or_default();
            let handle = self.post_handle(contact_id).unwrap_or_default();
            self.queue_pending_control(
                PendingControl {
                    recipient_ik: contact_id.to_string(),
                    peer_address: addr,
                    relays: set.relays,
                    nightdrop: set.nightdrop,
                    bytes: wire::encode(&frame),
                    handle,
                    in_flight: false,
                },
                Route::DirectThenRelay,
            );
        }
    }

    /// The peer-facing side of deleting this identity (#7 / §11.6): before the app wipes the
    /// local state file, signal [`Closed`](Frame::Closed) to the peer of every **un-backed**
    /// chat — otherwise their messages to a since-deleted identity would sit undeliverable.
    /// **Backed-up** chats are left silent, so the peer's mail queues on the relay and delivers
    /// when we restore within its 24h window. Clears all chats. Returns how many un-backed chats we
    /// could **not** get the notice to (0 = all queued/sent), so the app can tell the user.
    pub fn logout(&mut self) -> usize {
        self.logout_inner(false)
    }

    /// [`logout`](Self::logout) for the **duress wipe** (#3), differing in one way: *every* live
    /// chat is notified, not just un-backed ones. `logout` stays silent on backed-up chats so the
    /// peer's mail queues for a restore within the 24h window — after a duress wipe no restore is
    /// coming, so that silence would just strand them.
    ///
    /// The notice itself is the **ordinary** `Closed`, deliberately indistinguishable from a normal
    /// chat deletion: a duress-specific frame would be a permanent record on someone else's device
    /// that an anti-forensics feature was used. See `docs/design/duress-wipe.md` §5.
    pub fn duress_logout(&mut self) -> usize {
        self.logout_inner(true)
    }

    fn logout_inner(&mut self, notify_backed_up: bool) -> usize {
        let notify: Vec<String> = self
            .chats
            .iter()
            .filter(|(_, c)| (notify_backed_up || !c.contact.backed_up) && !c.closed)
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
            let (primary, targets) = self.route_to(id);
            // Prefer the relay store-and-forward so an **offline** peer still gets the "chat
            // deleted" notice within its 24h window (§1.3): the identity is about to be wiped, so a
            // direct-only send that races with the peer being briefly unreachable would be lost with
            // no retry. Fall back to a direct send only if no relay accepted (relay-less onion mode),
            // and count a chat we couldn't reach either way.
            let queued = queue_on_relays(
                self.transport.as_ref(),
                &primary,
                &targets,
                id,
                &self.post_handle(id).unwrap_or_default(),
                &bytes,
            )
            .is_ok();
            let direct = !queued && self.transport.send(&addr, &bytes).is_ok();
            if !queued && !direct {
                failed += 1;
            }
        }
        // Onion client auth (#22), both directions, for every chat. Dropping only their
        // permission would leave *our* client keys behind — one directory per peer, named by their
        // onion address — so a wiped identity would still yield a recoverable contact list.
        for (id, chat) in &self.chats {
            let _ = self.transport.revoke_client(id);
            if !chat.peer_address.is_empty() {
                let _ = self.transport.forget_peer_key(&chat.peer_address);
            }
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

    /// Build an **authenticated** control frame (`Closed`/`Ack`/`BackedUp`/`Screenshot`): encrypt a fixed domain
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
        // `None` too if the session is unusable (non-contributory ratchet key).
        let sealed = WireOlm::from_olm(&crypto::encrypt(&mut chat.session, marker).ok()?);
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
        let ok = matches!(crypto::decrypt(&mut chat.session, &olm), Ok(pt) if pt == marker);
        if ok {
            // Proof of life, and the *only* place most of it comes from: a silent delivery `Ack`
            // means their device drained our mailbox, even though they never typed anything.
            // Stamped here rather than per-branch so no authenticated frame is missed, and never
            // for a frame that failed to verify — otherwise a stranger could fake liveness for
            // someone whose phone has been taken.
            chat.mark_alive();
        }
        ok
    }

    /// Send a control/relay frame to `peer_address`, falling back to the relay mailbox when
    /// the peer is unreachable directly (so approvals/closes survive an offline peer).
    /// `recipient_ik` (the peer's identity key) addresses and seals the relay copy — the
    /// relay never sees the address or the frame's routing metadata.
    ///
    /// On an asynchronous transport this only **queues** the frame ([`Outbound`]) and returns
    /// `Ok`: the dial happens later, without the core lock (§6). A caller that must know whether
    /// it landed says so through the [`OutboundDone`] it passes to [`deliver_with`](Self::deliver_with).
    fn deliver(&mut self, peer_address: &str, recipient_ik: &str, frame: &Frame) -> Result<()> {
        self.deliver_with(
            peer_address,
            recipient_ik,
            frame,
            false,
            OutboundDone::Nothing,
        )
    }

    /// [`deliver`](Self::deliver), with what to record once it has gone out, and with `fresh` over
    /// a connection opened for this frame ([`Transport::send_fresh`]), so a peer that has gone away
    /// is a failed dial — and a relay copy — rather than a silent write into a connection left open
    /// from earlier traffic. `Err` only from a synchronous transport, which sends at once.
    fn deliver_with(
        &mut self,
        peer_address: &str,
        recipient_ik: &str,
        frame: &Frame,
        fresh: bool,
        done: OutboundDone,
    ) -> Result<()> {
        // An outdated contact's app (0.1.x) would take the frame and discard it, so it waits
        // instead: a one-off frame in the held queue, in order with their messages; a standing
        // announcement as not sent, which is retried once they have updated. Except our
        // **version**: it is how a contact who has in fact updated learns that we have too, so it
        // always goes out (an app that really is on 0.1 discards it, harmlessly).
        let version = matches!(done, OutboundDone::Announced(Announce::Version, _));
        if !version && self.peer_outdated(recipient_ik) {
            match done {
                OutboundDone::Announced(kind, id) => self.apply_announced(kind, &id, false),
                OutboundDone::Nothing | OutboundDone::Caller(_) => {
                    self.held.push(HeldSend {
                        contact_id: recipient_ik.to_string(),
                        msg_id: String::new(),
                        bytes: wire::encode(frame),
                        in_flight: false,
                    });
                    self.dirty = true;
                }
                _ => {}
            }
            return Ok(());
        }
        let send = self.detach(peer_address, recipient_ik, wire::encode(frame), fresh);
        match self.send_or_queue(send, Route::DirectThenRelay, done) {
            Some(false) => anyhow::bail!("peer offline and no relay accepted the message"),
            _ => Ok(()),
        }
    }

    /// Snapshot what delivering `bytes` to one peer needs, so it can run without the lock: the
    /// transport handle, the primary, the peer's advertised relays (#17) and the mailbox handle a
    /// relay copy goes under.
    fn detach(
        &self,
        peer_address: &str,
        recipient_ik: &str,
        bytes: Vec<u8>,
        fresh: bool,
    ) -> DetachedSend {
        let (primary, peer_relays) = self.route_to(recipient_ik);
        DetachedSend {
            transport: Arc::clone(&self.transport),
            primary,
            peer_relays,
            recipient_ik: recipient_ik.to_string(),
            handle: self.post_handle(recipient_ik).unwrap_or_default(),
            peer_address: peer_address.to_string(),
            bytes,
            fresh,
        }
    }

    /// Run `send` now on a synchronous transport (in-memory: instant, and tests expect the round
    /// trip done on return), recording the outcome at once; otherwise queue it in
    /// [`outbox`](Node::outbox) for delivery without the lock. `Some(delivered)` when it ran.
    pub(crate) fn send_or_queue(
        &mut self,
        send: DetachedSend,
        route: Route,
        done: OutboundDone,
    ) -> Option<bool> {
        let out = Outbound { send, route, done };
        if self.transport.is_synchronous() {
            let finished = out.run();
            let delivered = !matches!(finished.delivery, Delivery::Failed);
            self.apply_outbound(vec![finished]);
            return Some(delivered);
        }
        self.outbox.push(out);
        None
    }

    /// Everything queued for delivery, for the poller to run off the lock.
    pub(crate) fn take_outbox(&mut self) -> Vec<Outbound> {
        std::mem::take(&mut self.outbox)
    }

    /// Only the entries an API call is waiting on ([`OutboundDone::Caller`]); the rest stay for the
    /// poller, so a UI call never waits on someone else's dial.
    pub(crate) fn take_caller_outbox(&mut self) -> Vec<Outbound> {
        let (mine, rest) = std::mem::take(&mut self.outbox)
            .into_iter()
            .partition(Outbound::is_caller);
        self.outbox = rest;
        mine
    }

    /// Record what a round of [`Outbound`]s achieved. Returns the chats whose messages changed
    /// (a retry that queued), for the UI to re-pull.
    pub(crate) fn apply_outbound(&mut self, finished: Vec<Finished>) -> Vec<String> {
        let mut affected: Vec<String> = Vec::new();
        let me = self.identity_key();
        let now = crate::api::now_secs();
        for Finished {
            done,
            delivery,
            recipient,
            bytes,
            relayless,
        } in finished
        {
            let delivered = !matches!(delivery, Delivery::Failed);
            // Reaching the peer's device directly proves it online; a direct-only send failing
            // proves the opposite. Either moves its retry clock (`held_backoff`).
            if !recipient.is_empty() && recipient != me {
                if matches!(delivery, Delivery::Direct) {
                    self.held_backoff.remove(&recipient);
                } else if !delivered && relayless {
                    self.held_retry_failed(&recipient, now);
                }
            }
            // A one-off control frame (a rename, a timer, an approval, an edit) to a contact with
            // no relay route has no other retry: hold it, in order with their messages, rather
            // than lose it (`optional-relay.md` §4).
            if !delivered
                && relayless
                && matches!(done, OutboundDone::Nothing | OutboundDone::Caller(_))
                && self.chats.get(&recipient).is_some_and(|c| !c.closed)
            {
                self.held.push(HeldSend {
                    contact_id: recipient.clone(),
                    msg_id: String::new(),
                    bytes: bytes.clone(),
                    in_flight: false,
                });
                self.dirty = true;
            }
            match done {
                OutboundDone::Nothing => {}
                OutboundDone::Caller(record) => {
                    if let (Some((contact_id, msg_id)), Delivery::Relay(copies)) =
                        (record, delivery)
                    {
                        if let Some(chat) = self.chats.get_mut(&contact_id) {
                            chat.relay_receipts.insert(msg_id, copies);
                            self.dirty = true;
                        }
                    }
                }
                OutboundDone::Announced(kind, contact_id) => {
                    self.apply_announced(kind, &contact_id, delivered)
                }
                OutboundDone::PendingControl(bytes) => {
                    if delivered {
                        self.pending_control.retain(|p| p.bytes != bytes);
                        self.dirty = true; // persist the now-shorter queue
                    } else if let Some(p) =
                        self.pending_control.iter_mut().find(|p| p.bytes == bytes)
                    {
                        p.in_flight = false; // still unreachable — the next poll tries again
                    }
                }
                OutboundDone::Held(bytes) => {
                    let Some(i) = self.held.iter().position(|h| h.bytes == bytes) else {
                        continue; // its chat was deleted meanwhile
                    };
                    if !delivered {
                        self.held[i].in_flight = false; // the next retry tries again
                        continue;
                    }
                    let h = self.held.remove(i);
                    let direct = matches!(delivery, Delivery::Direct);
                    self.dirty = true;
                    if let Some(chat) = self.chats.get_mut(&h.contact_id) {
                        if let Some(m) = chat
                            .history
                            .iter_mut()
                            .find(|m| m.from_me && !h.msg_id.is_empty() && m.msg_id == h.msg_id)
                        {
                            m.delivery = if direct { "sent" } else { "queued" }.to_string();
                        }
                        if let Delivery::Relay(copies) = delivery {
                            if !h.msg_id.is_empty() {
                                chat.relay_receipts.insert(h.msg_id.clone(), copies);
                            }
                        }
                    }
                    if !h.msg_id.is_empty() && !affected.contains(&h.contact_id) {
                        affected.push(h.contact_id.clone());
                    }
                    if direct {
                        if !h.msg_id.is_empty() {
                            // Handed over, not confirmed: the receipt clock starts now.
                            self.awaiting_receipt.push(AwaitingReceipt {
                                contact_id: h.contact_id.clone(),
                                msg_id: h.msg_id.clone(),
                                since: now,
                            });
                        }
                        // They are online: the next one goes now, in order.
                        for id in self.flush_held_for(Some(&h.contact_id), true) {
                            if !affected.contains(&id) {
                                affected.push(id);
                            }
                        }
                    }
                }
                OutboundDone::Unconfirmed(a) => {
                    match delivery {
                        Delivery::Relay(mut copies) => {
                            if let Some(chat) = self.chats.get_mut(&a.contact_id) {
                                // The direct copy went first and may still land.
                                copies.push(QueuedReceipt::handed_directly());
                                chat.relay_receipts.insert(a.msg_id.clone(), copies);
                                // Only a message still waiting: a receipt that landed while the copy
                                // was being posted has already moved it on.
                                if let Some(m) = chat.history.iter_mut().find(|m| {
                                    m.from_me && m.msg_id == a.msg_id && m.delivery == "sent"
                                }) {
                                    m.delivery = "queued".to_string();
                                }
                                if !affected.contains(&a.contact_id) {
                                    affected.push(a.contact_id.clone());
                                }
                            }
                            crate::diag!(
                                "send: no delivery receipt within {}s — put a relay copy behind it",
                                crate::node::messaging::RECEIPT_TIMEOUT.as_secs()
                            );
                        }
                        // Re-sent directly (a contact with no relay route): wait for its receipt
                        // again from now.
                        Delivery::Direct => self
                            .awaiting_receipt
                            .push(AwaitingReceipt { since: now, ..a }),
                        // No relay took it. Keep waiting rather than dropping the retry: the next sweep
                        // tries again, and a receipt may still arrive in the meantime.
                        Delivery::Failed => self.awaiting_receipt.push(a),
                    }
                }
                OutboundDone::Cover => match delivery {
                    Delivery::Relay(copies) => crate::diag!(
                        "cover: posted a dummy to our own mailbox ({} relay(s))",
                        copies.len()
                    ),
                    _ => crate::diag!("cover: post failed — no chaff this interval"),
                },
            }
        }
        affected
    }

    /// The outcome of one standing announcement. Its flag went up when it was queued; a
    /// failure takes it down again so the next tick retries, and a delivery records what the
    /// peer now holds (`Chat::told_*`).
    fn apply_announced(&mut self, kind: Announce, contact_id: &str, delivered: bool) {
        if kind == Announce::Mailbox {
            crate::diag!(
                "mailbox: sent our contribution — {}",
                if delivered { "taken" } else { "not delivered" }
            );
            if delivered {
                if let Some(pair) = self
                    .chats
                    .get_mut(contact_id)
                    .and_then(|c| c.mailbox.as_mut())
                {
                    pair.announced_at.get_or_insert_with(crate::api::now_secs);
                    self.dirty = true;
                }
            }
        }
        if kind == Announce::Version && delivered {
            // Counts and versions only, never who: diag runs in field builds.
            crate::diag!(
                "version: told a contact we run {}",
                self.app_version.as_deref().unwrap_or("?")
            );
        }
        if kind == Announce::RelaySet {
            let sent = self.relay_set_in_flight.remove(contact_id);
            if delivered {
                if let Some(chat) = self.chats.get_mut(contact_id) {
                    chat.relays_told = sent;
                    self.dirty = true;
                }
            }
            return;
        }
        if delivered {
            // What the peer now holds, so a later launch does not send it again.
            let version = self.version_announced.get(contact_id).cloned();
            let address = self.address_announced.get(contact_id).cloned();
            let captures = self.captures_announced.get(contact_id).copied();
            if let Some(chat) = self.chats.get_mut(contact_id) {
                match kind {
                    Announce::Burns => chat.told_burns = true,
                    Announce::Captures if captures.is_some() => chat.told_captures = captures,
                    // Not while they are outdated: their old app discarded it, so they are told
                    // again once they update.
                    Announce::Version if version.is_some() && !chat.outdated() => {
                        chat.told_version = version
                    }
                    // Unless they turned out to be on 0.2 while it was on its way: then there is
                    // nothing to say ([`Chat::forget_upgrade_notice`]).
                    Announce::UpgradeNotice if chat.outdated() => {
                        chat.told_upgrade = true;
                        chat.history
                            .push(ChatMessage::system(UPGRADE_NOTE_TEXT.to_string()));
                    }
                    Announce::Address if address.is_some() => chat.told_address = address,
                    _ => {}
                }
                self.dirty = true;
            }
            return;
        }
        match kind {
            Announce::RelaySet => false,
            Announce::UpgradeNotice => self.upgrade_notice_in_flight.remove(contact_id),
            Announce::Burns => self.burns_announced.remove(contact_id),
            Announce::Version => self.version_announced.remove(contact_id).is_some(),
            Announce::Mailbox => self.mailbox_announced.remove(contact_id),
            Announce::Address => self.address_announced.remove(contact_id).is_some(),
            Announce::Captures => self.captures_announced.remove(contact_id).is_some(),
        };
    }

    /// Hold a control frame in `pending_control` (persisted) and send it now: the first attempt
    /// goes out by `route`, retries from [`flush_pending_control`](Self::flush_pending_control).
    fn queue_pending_control(&mut self, mut p: PendingControl, route: Route) {
        p.in_flight = true;
        let send = self.detach_pending(&p);
        let bytes = p.bytes.clone();
        self.pending_control.push(p);
        self.dirty = true;
        self.send_or_queue(send, route, OutboundDone::PendingControl(bytes));
    }

    /// A [`DetachedSend`] for a `pending_control` entry, which carries its own routing because its
    /// chat may be gone.
    pub(crate) fn detach_pending(&self, p: &PendingControl) -> DetachedSend {
        // No handle (a pair with no agreed mailbox, or an entry saved by 0.1.x for a v1 one): no
        // relay route, so it goes directly or waits.
        let (primary, peer_relays) = if p.handle.is_empty() {
            (None, Vec::new())
        } else {
            self.post_route(&RelaySet {
                nightdrop: p.nightdrop,
                relays: p.relays.clone(),
            })
        };
        DetachedSend {
            transport: Arc::clone(&self.transport),
            primary,
            peer_relays,
            recipient_ik: p.recipient_ik.clone(),
            handle: p.handle.clone(),
            peer_address: p.peer_address.clone(),
            bytes: p.bytes.clone(),
            fresh: false,
        }
    }

    /// Onion client authorization (#22): mint our client descriptor-encryption key for `contact`'s
    /// onion at `peer_address` and hand it over in a [`ClientKey`](Frame::ClientKey) frame, so the
    /// peer authorizes us to fetch their (restricted) descriptor and connect. Best-effort with relay
    /// fallback, and a no-op on transports without restricted discovery (`make_client_key` → `None`)
    /// or when `peer_address` isn't an onion — so non-Tor pairing is unaffected. Called whenever we
    /// (re)learn a peer's onion: on pairing and on address rotation.
    fn announce_client_key(&mut self, contact: &str, peer_address: &str) {
        let Some(key_result) = self.transport.make_client_key(peer_address) else {
            return; // transport has no restricted-discovery client keys (e.g. tests, LAN)
        };
        let Ok((client_key, secret)) = key_result else {
            return; // couldn't parse the peer onion / mint a key — leave us a public onion
        };
        // Keep the secret: the keystore is in memory, so this is the only copy that survives a
        // restart, and losing it would silently drop the peer to relay-only.
        if let Some(chat) = self.chats.get_mut(contact) {
            chat.client_key = Some(secret);
            self.dirty = true;
        }
        // Sealed on the pair's session, so only the real contact can set it (finding 5).
        let marker = [MARK_CLIENT_KEY_PREFIX, client_key.as_bytes()].concat();
        let Some((_, frame)) = self.authed_control(contact, &marker, |from, message| {
            Frame::ClientKeySealed { from, message }
        }) else {
            return;
        };
        let _ = self.deliver(peer_address, contact, &frame);
    }

    /// This device's client access key for a **private** relay's onion (restricted discovery, §3.2),
    /// as the public `descriptor:x25519:…` string to give the relay operator. Minted on the first
    /// request and inserted into arti's keystore, which presents it on every connect to that onion;
    /// a repeat request returns the **same** key, since the operator authorized that one and a new
    /// one would lock this device out. Kept in the sealed store and re-inserted at startup.
    /// `None` on transports without restricted discovery (tests, LAN); `Some(Err)` if the onion
    /// won't parse or key generation fails.
    pub(crate) fn relay_access_key(&mut self, relay_onion: &str) -> Option<Result<String>> {
        if let Some((_, public, _)) = self.relay_keys.iter().find(|(r, _, _)| r == relay_onion) {
            return Some(Ok(public.clone()));
        }
        Some(
            self.transport
                .make_client_key(relay_onion)?
                .map(|(public, secret)| {
                    self.relay_keys
                        .push((relay_onion.to_string(), public.clone(), secret));
                    self.dirty = true;
                    public
                }),
        )
    }

    /// Attach the primary relay for offline store-and-forward (§6). Without one (and no
    /// advertised extras), sending to an offline peer errors instead of being queued.
    pub fn set_relay(&mut self, relay: RelayClient) {
        self.relay = Some(relay);
    }

    /// The live transport's [`abort_handle`](Transport::abort_handle), taken while it is live so
    /// a later teardown can use it without the core lock.
    #[cfg(any(feature = "tor", test))]
    pub(crate) fn transport_abort_handle(&self) -> Option<crate::transport::AbortHandle> {
        self.transport.abort_handle()
    }

    /// Tear down the network side: swap the live transport for an inert [`ClosedTransport`](crate::transport::ClosedTransport) and
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
        self.transport = Arc::new(crate::transport::ClosedTransport::new(address));
        self.relay = None;
    }

    /// Tests only: replace the onions in our relay list, keeping the Night Drop entry as it is.
    #[cfg(test)]
    pub fn set_my_relays(&mut self, relays: Vec<String>) {
        let mut list: Vec<RelayEntry> = self
            .relays
            .iter()
            .filter(|e| **e == RelayEntry::NightDrop)
            .cloned()
            .collect();
        list.extend(relays.into_iter().map(RelayEntry::Onion));
        self.set_relay_list(list);
    }

    /// The onions in our relay list.
    pub fn my_relays(&self) -> Vec<String> {
        self.my_relay_set().relays
    }

    /// No relay at all: this device never talks to one (`optional-relay.md` §1).
    pub fn direct_only(&self) -> bool {
        self.relays.is_empty()
    }

    /// Whether our list holds the Night Drop relay, which the server backup lives on: a fresh
    /// device restoring one looks there, knowing nothing else yet.
    pub fn uses_nightdrop_relay(&self) -> bool {
        self.relays.contains(&RelayEntry::NightDrop) && self.relay.is_some()
    }

    /// Whether opt-in server storage can work in this chat: it posts to the *recipient's* relays
    /// and is sent by the *sender*, so it needs relays on **both** sides (`optional-relay.md` §6).
    pub(crate) fn server_storage_available(&self, contact_id: &str) -> bool {
        !self.direct_only()
            && self
                .peer_relay_set(contact_id)
                .is_some_and(|set| !set.is_empty())
    }

    /// Our relay list, in order (`optional-relay.md` §2). Empty = direct only.
    pub fn relay_list(&self) -> Vec<RelayEntry> {
        self.relays.clone()
    }

    /// The set our relay list stands for: what contacts are told and post to.
    pub fn my_relay_set(&self) -> RelaySet {
        RelaySet::of(&self.relays)
    }

    /// Replace our relay list. Duplicates and blank onions are dropped and anything past
    /// [`MAX_RELAYS`] is cut, so the result is what [`relay_list`](Self::relay_list) returns.
    /// A relay that leaves the list is drained for another [`RELAY_TTL`], since mail already
    /// queued there for us is still on its way; one that comes back stops draining.
    pub fn set_relay_list(&mut self, list: Vec<RelayEntry>) {
        self.set_relay_list_at(list, crate::api::now_secs());
    }

    /// A **new** identity's list, from the setup page (`optional-relay.md` §7): taken as it is,
    /// with nothing to drain, since nothing was ever queued for this identity anywhere.
    pub fn start_with_relay_list(&mut self, list: Vec<RelayEntry>) {
        let (list, _) = normalize_relays(list);
        self.relays = list;
        self.relays_draining.clear();
        self.dirty = true;
    }

    pub(crate) fn set_relay_list_at(&mut self, list: Vec<RelayEntry>, now: u64) {
        let (list, overflow) = normalize_relays(list);
        let until = now + RELAY_TTL.as_secs();
        for gone in std::mem::take(&mut self.relays)
            .into_iter()
            .filter(|e| !list.contains(e))
            .chain(overflow)
        {
            self.relays_draining.retain(|(e, _)| *e != gone);
            self.relays_draining.push((gone, until));
        }
        self.relays_draining
            .retain(|(e, until)| !list.contains(e) && *until > now);
        self.relays = list;
        // Forget health for relays we no longer poll (and start freshly-added ones "unknown").
        let keep = self.polled_relay_set(now).relays;
        self.relay_reachable.retain(|addr, _| keep.contains(addr));
        self.dirty = true;
    }

    /// What we poll as of `now`: our list plus the relays still draining.
    pub(crate) fn polled_relay_set(&self, now: u64) -> RelaySet {
        let mut entries = self.relays.clone();
        entries.extend(
            self.relays_draining
                .iter()
                .filter(|(_, until)| *until > now)
                .map(|(e, _)| e.clone()),
        );
        RelaySet::of(&entries)
    }

    /// The relay set `contact_id` told us (`Frame::RelaySet`). `None` while unknown.
    pub(crate) fn peer_relay_set(&self, contact_id: &str) -> Option<RelaySet> {
        self.chats.get(contact_id).and_then(|c| c.relay_set.clone())
    }

    /// Where a relay copy for `contact_id` goes: see [`post_route`](Self::post_route). Nowhere
    /// while their set is unknown: unknown is held, never treated as "use Night Drop"
    /// (`optional-relay.md` §3 rule 3).
    pub(crate) fn route_to(&self, contact_id: &str) -> (Option<RelayClient>, Vec<String>) {
        // A pair with no agreed mailbox has no handle its peer polls (`Node::post_handle`), so
        // no relay route either: its frames go directly or wait, like a direct-only contact's.
        if !self.mailbox_agreed(contact_id) {
            return (None, Vec::new());
        }
        self.relays_between(contact_id)
    }

    /// The relays between us and `contact_id` whether or not the pair has a mailbox yet: what
    /// the mailbox agreement itself needs ([`Node::relays_meet`]).
    fn relays_between(&self, contact_id: &str) -> (Option<RelayClient>, Vec<String>) {
        match self.peer_relay_set(contact_id) {
            Some(set) => self.post_route(&set),
            None => (None, Vec::new()),
        }
    }

    /// Whether we and `contact_id` both have relays, so the pair has a mailbox worth agreeing.
    pub(crate) fn relays_meet(&self, contact_id: &str) -> bool {
        let (primary, targets) = self.relays_between(contact_id);
        primary.is_some() || !targets.is_empty()
    }

    /// Whether a `pending_control` entry has a relay to go to (it carries its own set, since its
    /// chat may be gone).
    pub(crate) fn relay_route_for_pending(&self, p: &PendingControl) -> bool {
        let send = self.detach_pending(p);
        send.primary.is_some() || !send.peer_relays.is_empty()
    }

    /// Whether `contact_id` runs a release too old to read what we send (0.1.x): see
    /// [`MIN_PEER_VERSION`]. Messages and control frames to it are held until it updates.
    pub fn peer_outdated(&self, contact_id: &str) -> bool {
        self.chats.get(contact_id).is_some_and(Chat::outdated)
    }

    /// A frame in an older wire version arrived from `contact_id` (`wire::legacy_sender`): it runs
    /// 0.1.x. Unauthenticated, so it can only ever *hold* our messages to that contact, which any
    /// frame from them that we can read undoes.
    ///
    /// Not from a contact whose last version, read from a frame of theirs, is 0.2 or later: a 0.2
    /// app sends the older format only for its upgrade notice ([`Node::send_upgrade_notices`]),
    /// which it sends us when it still holds *our* 0.1 version. Arriving after their version
    /// announcement, that notice would mark them as 0.1 again, and neither side announces twice in
    /// a run, so both would hold everything for the other until a restart.
    pub(crate) fn note_legacy_frame(&mut self, contact_id: &str) {
        if let Some(chat) = self.chats.get_mut(contact_id) {
            let announced_new = chat
                .contact
                .peer_app_version
                .as_deref()
                .is_some_and(|v| !version_outdated(v));
            if !chat.peer_wire_old && !announced_new {
                crate::diag!("version: a contact sent an older-format frame — they are on 0.1");
                chat.peer_wire_old = true;
                self.dirty = true;
            }
        }
    }

    /// Whether a frame to `contact_id` has any relay to go to; without one it goes direct or is
    /// held (`optional-relay.md` §4).
    pub(crate) fn has_relay_route(&self, contact_id: &str) -> bool {
        let (primary, targets) = self.route_to(contact_id);
        primary.is_some() || !targets.is_empty()
    }

    /// Whether a retry to `contact_id` may go out now: always for a contact with a relay route
    /// (a relay post is how those retry), otherwise when its backoff has run out.
    pub(crate) fn may_retry(&self, contact_id: &str, now: u64) -> bool {
        // An outdated contact's app discards anything we send: nothing is retried until it updates.
        if self.peer_outdated(contact_id) {
            return false;
        }
        self.has_relay_route(contact_id)
            || self
                .held_backoff
                .get(contact_id)
                .is_none_or(|(_, next)| now >= *next)
    }

    /// [`may_retry`](Self::may_retry) without its outdated rule: for the frames that go to an
    /// outdated contact on purpose (our version, the upgrade notice).
    pub(crate) fn retry_due_even_if_outdated(&self, contact_id: &str, now: u64) -> bool {
        self.has_relay_route(contact_id)
            || self
                .held_backoff
                .get(contact_id)
                .is_none_or(|(_, next)| now >= *next)
    }

    /// Tell each contact still on 0.1.x, once, that we have updated and they need to: a plain
    /// `Message` in their own wire format (v2), which their app shows like any other
    /// ([`UPGRADE_NOTICE_TEXT`]). It is the only way they hear it, since nothing else 0.2 sends
    /// can be read by 0.1. Retried until it goes out; recorded per chat, with a note in our own
    /// chat saying what was sent on our behalf ([`SEND_UPGRADE_NOTICE`]).
    pub fn send_upgrade_notices(&mut self) {
        if !SEND_UPGRADE_NOTICE || !self.announce_ready() {
            return;
        }
        let now = crate::api::now_secs();
        let from = self.identity_key();
        let ids: Vec<String> = self
            .chats
            .iter()
            .filter(|(id, c)| {
                c.authorized
                    && !c.closed
                    && !c.told_upgrade
                    && c.outdated()
                    && !self.upgrade_notice_in_flight.contains(id.as_str())
                    && self.retry_due_even_if_outdated(id, now)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            let Some(chat) = self.chats.get_mut(&id) else {
                continue;
            };
            let Ok(message) = crypto::encrypt(&mut chat.session, UPGRADE_NOTICE_TEXT.as_bytes())
            else {
                continue;
            };
            let frame = Frame::Message {
                from: from.clone(),
                id: random_msg_id(),
                message: WireOlm::from_olm(&message),
            };
            let peer_address = chat.peer_address.clone();
            self.upgrade_notice_in_flight.insert(id.clone());
            let send = self.detach(&peer_address, &id, wire::encode_legacy(&frame), true);
            let done = OutboundDone::Announced(Announce::UpgradeNotice, id.clone());
            self.send_or_queue(send, Route::DirectThenRelay, done);
            crate::diag!("version: told a contact on 0.1 that we updated");
        }
    }

    /// A direct attempt to a contact with no relay route failed: wait longer before the next.
    fn held_retry_failed(&mut self, contact_id: &str, now: u64) {
        let cap = if self.background {
            HELD_RETRY_CAP_BACKGROUND_SECS
        } else {
            HELD_RETRY_CAP_OPEN_SECS
        };
        let delay = match self.held_backoff.get(contact_id) {
            Some((delay, _)) => (delay * 2).min(cap),
            None => HELD_RETRY_FIRST_SECS,
        };
        self.held_backoff
            .insert(contact_id.to_string(), (delay, now + delay));
    }

    /// `contact_id` was just heard from directly, so it is online: reset its retry clock and send
    /// what we hold for it now (`optional-relay.md` §4).
    pub(crate) fn heard_from(&mut self, contact_id: &str) {
        let waited = self.held_backoff.remove(contact_id).is_some();
        let holding = self.held.iter().any(|h| h.contact_id == contact_id)
            || self
                .pending_control
                .iter()
                .any(|p| p.recipient_ik == contact_id);
        if waited || holding {
            self.flush_held_for(Some(contact_id), true);
            self.flush_pending_control_for(Some(contact_id));
        }
    }

    /// Issue a **pairing pass** for an invite whose bundle carries one-time key `otk`
    /// (`optional-relay.md` §5): authorize a fresh key on our onion and return its secret for the
    /// QR or short-code payload (`&ck=`). Whoever holds it can fetch our descriptor, which lets
    /// their `Hello` through even once our onion admits only contacts. It gets them that far and no
    /// further: the `Hello` still lands as a request the user must accept. Single-use (revoked when
    /// a `Hello` uses `otk`) and gone after a day. `None` where there is no restricted discovery.
    pub(crate) fn issue_pass(&mut self, otk: &str) -> Option<String> {
        let (public, secret) = self.transport.mint_pass()?.ok()?;
        let name = format!("pass:{otk}");
        self.transport.authorize_client(&name, &public).ok()?;
        self.passes.push((
            name,
            otk.to_string(),
            crate::api::now_secs() + PASS_TTL_SECS,
        ));
        self.dirty = true;
        Some(encode_secret(&secret))
    }

    /// Revoke the pass issued with the invite whose one-time key a `Hello` just used: single-use.
    pub(crate) fn spend_pass(&mut self, otk: &str) {
        let transport = Arc::clone(&self.transport);
        let before = self.passes.len();
        self.passes.retain(|(name, o, _)| {
            let spent = o == otk;
            if spent {
                let _ = transport.revoke_client(name);
                crate::diag!("pair: the invite's pass was used — revoked");
            }
            !spent
        });
        self.dirty |= self.passes.len() != before;
    }

    /// Revoke passes nobody used within [`PASS_TTL_SECS`], so a QR posted online stops letting
    /// strangers knock after a day.
    pub(crate) fn expire_passes(&mut self, now: u64) {
        let transport = Arc::clone(&self.transport);
        let before = self.passes.len();
        self.passes.retain(|(name, _, expires)| {
            let live = *expires > now;
            if !live {
                let _ = transport.revoke_client(name);
            }
            live
        });
        self.dirty |= self.passes.len() != before;
    }

    /// Accepting a request: authorize the joiner's client key on our onion and install their reply
    /// pass as our key for theirs (`optional-relay.md` §5). Until now both were held unused, so a
    /// request had no way to reach us beyond the `Hello` it came in with.
    fn apply_pending_keys(&mut self, contact_id: &str) {
        let Some(chat) = self.chats.get_mut(contact_id) else {
            return;
        };
        if let Some(key) = chat.peer_key_pending.take() {
            let _ = self.transport.authorize_client(contact_id, &key);
        }
        if let Some(pass) = chat.pass_pending.take() {
            if !chat.peer_address.is_empty() {
                let _ = self.transport.insert_client_key(&chat.peer_address, &pass);
            }
            chat.client_key = Some(pass);
        }
        self.dirty = true;
    }

    /// Tests only: whether no removed relay is still being drained.
    #[cfg(test)]
    pub(crate) fn relays_draining_is_empty(&self) -> bool {
        self.relays_draining.is_empty()
    }

    /// Tests only: how many frames are held.
    #[cfg(test)]
    pub(crate) fn held_count(&self) -> usize {
        self.held.len()
    }

    /// Tests only: let every contact's retry backoff run out, as a minute or more passing would.
    #[cfg(test)]
    pub(crate) fn let_backoff_elapse(&mut self) {
        self.held_backoff.clear();
    }

    /// Whether the app is in the background, which lengthens the retry cap.
    pub fn set_background(&mut self, background: bool) {
        self.background = background;
    }

    /// Hold a message that reached neither the peer nor a relay. With no relay route it is
    /// **"waiting"** for the peer to come online; otherwise "queued" until a relay takes it.
    pub(crate) fn hold_message(&mut self, contact_id: &str, msg_id: &str, bytes: Vec<u8>) {
        let outdated = self.peer_outdated(contact_id);
        let waiting = !self.has_relay_route(contact_id);
        if let Some(m) = self.chats.get_mut(contact_id).and_then(|c| {
            c.history
                .iter_mut()
                .find(|m| m.from_me && m.msg_id == msg_id)
        }) {
            m.delivery = if outdated {
                "update"
            } else if waiting {
                "waiting"
            } else {
                "queued"
            }
            .to_string();
        }
        self.held.push(HeldSend {
            contact_id: contact_id.to_string(),
            msg_id: msg_id.to_string(),
            bytes,
            in_flight: false,
        });
        self.dirty = true;
    }

    /// The held-queue index of message `msg_id` to `contact_id`, if it is held and not on its way
    /// right now: one in flight may land, so it is edited or unsent the ordinary way instead.
    pub(crate) fn held_unsent(&self, contact_id: &str, msg_id: &str) -> Option<usize> {
        self.held.iter().position(|h| {
            !h.in_flight && !msg_id.is_empty() && h.contact_id == contact_id && h.msg_id == msg_id
        })
    }

    /// Retry the held queue (`optional-relay.md` §4). Returns the contacts whose messages
    /// changed (synchronous transports; otherwise the poller reports them).
    pub(crate) fn flush_held(&mut self) -> Vec<String> {
        self.flush_held_for(None, false)
    }

    /// [`flush_held`](Self::flush_held), for one contact when `only` is set. A contact with a relay
    /// route gets a relay copy of every entry (`direct_first` tries their device first, for a
    /// contact just heard from). One without gets its **oldest** entry only, directly, when its
    /// backoff allows: held frames go out in send order, and the rest follow as soon as that one
    /// lands, so an offline contact costs one dial per retry, not one per message.
    pub(crate) fn flush_held_for(&mut self, only: Option<&str>, direct_first: bool) -> Vec<String> {
        let now = crate::api::now_secs();
        // A deleted or closed chat's held frames go with it.
        let chats = &self.chats;
        let before = self.held.len();
        self.held
            .retain(|h| chats.get(&h.contact_id).is_some_and(|c| !c.closed));
        if self.held.len() != before {
            self.dirty = true;
        }
        let mut sends: Vec<(usize, Route)> = Vec::new();
        let mut busy: Vec<String> = Vec::new(); // relay-less contacts with one already going
        for (i, h) in self.held.iter().enumerate() {
            if only.is_some_and(|c| c != h.contact_id) {
                continue;
            }
            // Held until they update: their app would take it and discard it.
            if self.peer_outdated(&h.contact_id) {
                continue;
            }
            if self.has_relay_route(&h.contact_id) {
                if !h.in_flight {
                    let route = if direct_first {
                        Route::DirectThenRelay
                    } else {
                        Route::RelayOnly
                    };
                    sends.push((i, route));
                }
                continue;
            }
            if busy.contains(&h.contact_id) {
                continue;
            }
            busy.push(h.contact_id.clone());
            if !h.in_flight && self.may_retry(&h.contact_id, now) {
                sends.push((i, Route::DirectThenRelay));
            }
        }
        let mut affected = Vec::new();
        for (i, route) in sends {
            let (contact_id, bytes) = (self.held[i].contact_id.clone(), self.held[i].bytes.clone());
            self.held[i].in_flight = true;
            let peer_address = self
                .chats
                .get(&contact_id)
                .map(|c| c.peer_address.clone())
                .unwrap_or_default();
            // A fresh connection: a frame that nothing confirms must not vanish into a dead
            // kept-open one (`Transport::send_fresh`).
            let send = self.detach(&peer_address, &contact_id, bytes.clone(), true);
            if self.send_or_queue(send, route, OutboundDone::Held(bytes)) == Some(true)
                && !affected.contains(&contact_id)
            {
                affected.push(contact_id);
            }
        }
        affected
    }

    /// Where a relay copy for someone with relay set `set` goes: the Night Drop relay's client and
    /// the directory's relays when the set includes it, and the set's own onions, once each. There
    /// is no implicit relay: an empty set gets nothing. Neither does anyone while **we** are direct
    /// only, which means this device never talks to a relay, sending included
    /// (`optional-relay.md` §3 rule 2).
    pub(crate) fn post_route(&self, set: &RelaySet) -> (Option<RelayClient>, Vec<String>) {
        if self.relays.is_empty() {
            return (None, Vec::new());
        }
        self.expand_relay_set(set)
    }

    /// `set` as clients to talk to, without the direct-only rule: the Night Drop client when it is
    /// in the set, and the onions besides it, each once. The directory normally lists the Night
    /// Drop relay itself; [`queue_on_relays`] and the drain skip that address beside the client.
    fn expand_relay_set(&self, set: &RelaySet) -> (Option<RelayClient>, Vec<String>) {
        let primary = if set.nightdrop {
            self.relay.clone()
        } else {
            None
        };
        let mut targets: Vec<String> = Vec::new();
        let nightdrop = if set.nightdrop {
            &self.discovered_relays[..]
        } else {
            &[][..]
        };
        // A contact's set is untrusted input: past the cap, extra onions are ignored (§2).
        for addr in set.relays.iter().take(MAX_RELAYS).chain(nightdrop) {
            if !targets.contains(addr) {
                targets.push(addr.clone());
            }
        }
        (primary, targets)
    }

    /// A handle to our transport, so a caller can do bounded network work **after releasing the
    /// core lock**. Network I/O under the lock freezes every other FFI call and the poller for as
    /// long as the dial takes (§6), and the update check is a Tor fetch bounded at
    /// [`update::FETCH_TIMEOUT`](crate::update::FETCH_TIMEOUT) — long enough that holding the lock
    /// across it would be felt as the app hanging.
    pub fn transport_handle(&self) -> std::sync::Arc<dyn Transport> {
        std::sync::Arc::clone(&self.transport)
    }

    /// Reachability of each of **our** advertised extra relays (#17), as observed on the last
    /// relay drain ([`apply_relay_harvest`](Self::apply_relay_harvest)): `(address, reachable)`. A relay not yet probed reports
    /// `true` (optimistic — don't cry wolf before the first poll). Lets the UI warn "your relay is
    /// offline — add a backup" for a self-hosted relay that stops answering.
    pub fn relay_health(&self) -> Vec<(String, bool)> {
        self.my_relays()
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

    /// Whether to send the optional standing announcements (burn support, the v2 mailbox
    /// contribution). Before Tor is up every one of them costs a failed dial and a failed relay post
    /// and is retried anyway, so they wait for the onion to publish — or for
    /// [`ANNOUNCE_FALLBACK`] after start, because `published()` has been measured reading false on
    /// a published service, and gating on it alone would leave a pair on v1 mailboxes for good.
    pub(crate) fn announce_ready(&self) -> bool {
        self.transport.published() || self.started.elapsed() >= ANNOUNCE_FALLBACK
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
                // These are live or derived per-chat state, not part of the stored contact —
                // filled in here so the UI sees one flat object.
                let mut dto = c.contact.clone();
                dto.remote_storage_healthy = c.remote_storage_healthy;
                dto.last_seen_secs = c.last_seen.unwrap_or(0);
                dto.local_name = c.local_name.clone();
                dto.identity_tag = identity_tag(&c.contact.id);
                // Only where a mailbox could exist: a direct-only contact never answers the
                // agreement, which is not evidence of an old app.
                dto.peer_on_old_version =
                    Self::peer_on_old_version(c) && self.relays_meet(&c.contact.id);
                dto.peer_relays = c
                    .relay_set
                    .as_ref()
                    .map(|s| s.relays.clone())
                    .unwrap_or_default();
                dto.peer_relay_state = match &c.relay_set {
                    None => "unknown",
                    Some(set) if set.is_empty() => "direct",
                    Some(_) => "relays",
                }
                .to_string();
                dto.peer_outdated = c.outdated();
                dto.server_storage_available =
                    !self.relays.is_empty() && c.relay_set.as_ref().is_some_and(|s| !s.is_empty());
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
    // Checked arithmetic: on a 32-bit target (armeabi-v7a) a length near 4 GiB would wrap a plain
    // `*p + n` past the bounds check and panic on the slice instead of failing cleanly.
    let corrupt = || anyhow::anyhow!("corrupt media envelope");
    let len_end = p
        .checked_add(4)
        .filter(|&e| e <= buf.len())
        .ok_or_else(corrupt)?;
    let n = u32::from_be_bytes(buf[*p..len_end].try_into().unwrap()) as usize;
    let end = len_end
        .checked_add(n)
        .filter(|&e| e <= buf.len())
        .ok_or_else(corrupt)?;
    let v = buf[len_end..end].to_vec();
    *p = end;
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
    Ok((target, limits::clamp_text(text)))
}

/// Pack a `Burn` envelope: `[burn_secs as ASCII][text...]` (encrypted on the session), so the
/// duration is never visible outside the pair. Mirrors [`pack_edit`]'s shape.
fn pack_burn(burn_secs: u64, text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 8);
    put_field(&mut out, burn_secs.to_string().as_bytes());
    out.extend_from_slice(text.as_bytes()); // trailing
    out
}

/// Inverse of [`pack_burn`]: `(burn_secs, text)`. A duration that does not parse is treated as
/// `0`, which the caller must reject rather than deliver as a normal message — a burn frame whose
/// timer we cannot read is not something to render permanently.
fn unpack_burn(buf: &[u8]) -> Result<(u64, String)> {
    let mut p = 0;
    let secs = String::from_utf8(take_field(buf, &mut p)?)?
        .parse::<u64>()
        .unwrap_or(0);
    let text = String::from_utf8(buf[p..].to_vec())?;
    Ok((secs, limits::clamp_text(text)))
}

/// Pack a `BurnMedia` envelope: `[burn_secs][transfer_id][kind][mime][data...]`.
fn pack_burn_media(
    burn_secs: u64,
    transfer_id: &str,
    kind: &str,
    mime: &str,
    data: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    put_field(&mut out, burn_secs.to_string().as_bytes());
    put_field(&mut out, transfer_id.as_bytes());
    put_field(&mut out, kind.as_bytes());
    put_field(&mut out, mime.as_bytes());
    out.extend_from_slice(data); // trailing
    out
}

/// Inverse of [`pack_burn_media`]: `(burn_secs, transfer_id, kind, mime, data)`.
fn unpack_burn_media(buf: &[u8]) -> Result<(u64, String, String, String, Vec<u8>)> {
    let mut p = 0;
    let secs = String::from_utf8(take_field(buf, &mut p)?)?
        .parse::<u64>()
        .unwrap_or(0);
    let transfer_id = String::from_utf8(take_field(buf, &mut p)?)?;
    anyhow::ensure!(limits::sane_id(&transfer_id), "bad transfer id");
    let kind = String::from_utf8(take_field(buf, &mut p)?)?;
    let mime = String::from_utf8(take_field(buf, &mut p)?)?;
    // The contact chose both: normalise before they are stored (media_kind.rs).
    let (kind, mime) = media_kind::sanitize(kind, mime);
    Ok((secs, transfer_id, kind, mime, buf[p..].to_vec()))
}

/// Pack an `Unsend` envelope: just the target `msg_id` (encrypted on the session).
fn pack_unsend(target_msg_id: &str) -> Vec<u8> {
    target_msg_id.as_bytes().to_vec()
}

/// Inverse of [`pack_unsend`]: the target `msg_id`.
fn unpack_unsend(buf: &[u8]) -> Result<String> {
    Ok(String::from_utf8(buf.to_vec())?)
}

/// Whether opt-in server storage was on for a message stamped `at`: the last switch made at or
/// before it (`Chat::storage_timeline`). The 24h bomb deletes only those messages, so turning
/// storage on never reaches back into the history, and turning it off still expires what was
/// sent while it was on.
fn storage_on_at(timeline: &[(u64, bool)], at: u64) -> bool {
    timeline
        .iter()
        .rev()
        .find(|(changed, _)| *changed <= at)
        .is_some_and(|(_, on)| *on)
}

/// How long after start the optional announcements go out even if the transport never reports
/// itself published ([`Node::announce_ready`]). Tor publishes in ~1–3 minutes.
const ANNOUNCE_FALLBACK: Duration = Duration::from_secs(3 * 60);

/// The disappearing timer that applies to a message stamped `at`: the last change made at or
/// before it, 0 if none (`Chat::disappearing_timeline`). Forward-only: a timer turned on later
/// never reaches back to older messages, and one turned off later still expires what was sent
/// while it was on.
fn timer_at(timeline: &[(u64, u64)], at: u64) -> u64 {
    timeline
        .iter()
        .rev()
        .find(|(changed, _)| *changed <= at)
        .map_or(0, |(_, secs)| *secs)
}

/// A human label for a disappearing-messages timer value (for the in-chat system notice).
fn disappearing_label(secs: u64) -> String {
    let unit = |n: u64, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    match secs {
        0 => "off".to_string(),
        s if s % 604800 == 0 => unit(s / 604800, "week"),
        s if s % 86400 == 0 => unit(s / 86400, "day"),
        s if s % 3600 == 0 => unit(s / 3600, "hour"),
        s if s % 60 == 0 => unit(s / 60, "minute"),
        s => unit(s, "second"),
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
        burn_secs: m.burn_secs,
        viewed_at: m.viewed_at,
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

/// Turn an unopened burn message that expired into a "burn message expired" marker, in place.
/// Keeps only its position and arrival time: the content, the sealed-file ids (the caller deletes
/// the files) and every id that could reveal or match it are cleared, and so is `burn_secs`, so it
/// is neither blurred nor swept again.
fn make_burn_tombstone(msg: &mut ChatMessage) {
    msg.text = String::new();
    msg.kind = "burn_expired".to_string();
    msg.mime = String::new();
    msg.media_id = String::new();
    msg.media_size = 0;
    msg.thumb_id = String::new();
    msg.transfer_id = String::new();
    msg.msg_id = String::new();
    msg.edited = false;
    msg.delivery = String::new();
    msg.burn_secs = 0;
    msg.viewed_at = 0;
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
    anyhow::ensure!(limits::sane_id(&transfer_id), "bad transfer id");
    let kind = String::from_utf8(take_field(buf, &mut p)?)?;
    let mime = String::from_utf8(take_field(buf, &mut p)?)?;
    // The contact chose both: normalise before they are stored (media_kind.rs).
    let (kind, mime) = media_kind::sanitize(kind, mime);
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
    anyhow::ensure!(limits::sane_id(&transfer_id), "bad transfer id");
    let kind = String::from_utf8(take_field(buf, &mut p)?)?;
    let mime = String::from_utf8(take_field(buf, &mut p)?)?;
    // The contact chose both: normalise before they are stored (media_kind.rs).
    let (kind, mime) = media_kind::sanitize(kind, mime);
    if p + 8 > buf.len() {
        anyhow::bail!("corrupt media-incoming envelope");
    }
    let size = u64::from_be_bytes(buf[p..p + 8].try_into().unwrap());
    p += 8;
    Ok((transfer_id, kind, mime, size, buf[p..].to_vec()))
}

// -------- Interactive SPAKE2 short-code pairing (§5b) --------

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
///
/// Two minutes, because that is how long a brand-new Tor state takes to reach the relay at all:
/// measured on a phone (2026-10-09), a fresh identity's first relay round failed outright and the
/// next, two and a half minutes after Tor started, succeeded. Joining straight after creating an
/// identity — what a new user does — fell inside that window, and 45 s gave up on it every time.
#[cfg(not(test))]
const POST_ESTABLISH_TIMEOUT: Duration = Duration::from_secs(120);
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
        anyhow::bail!("short codes need a relay; with direct only, add contacts by QR code");
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
            anyhow::bail!(
                "could not reach the relay yet — Tor may still be starting. Try again in a minute; \
                 the code stays valid for 10 minutes"
            );
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
            // Codes meet only on relays both lists share (`optional-relay.md` §5), so "nobody
            // answered" can also mean "we share none": say so, and point to the QR.
            anyhow::bail!(
                "timed out waiting for the other device — ask for a fresh code, or, if you \
                 don't share a relay with them, use the QR code instead"
            );
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

/// Longest display name a contact may give themselves, in characters.
pub(crate) const MAX_NAME_CHARS: usize = 64;

/// A contact's self-chosen name, made safe to show: control and invisible formatting characters
/// removed (bidi overrides and isolates, zero-width characters, tag characters), runs of
/// whitespace collapsed, and at most [`MAX_NAME_CHARS`] kept. The name arrives from the contact
/// and was stored as sent, so it could reorder the text around it, hide characters, or fill the
/// screen (self-audit 2026-10-13, finding 7). Empty → [`DEFAULT_NAME`].
pub(crate) fn clean_display_name(raw: &str) -> String {
    fn invisible(c: char) -> bool {
        c.is_control()
            || matches!(
                c,
                '\u{00AD}'
                    | '\u{034F}'
                    | '\u{061C}'
                    | '\u{115F}'
                    | '\u{1160}'
                    | '\u{17B4}'
                    | '\u{17B5}'
                    | '\u{180B}'..='\u{180F}'
                    // Not U+200C/U+200D: the non-joiner is ordinary Persian spelling and the
                    // joiner builds composite emoji; neither can reorder text.
                    | '\u{200B}'
                    | '\u{200E}'
                    | '\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{206F}'
                    | '\u{3164}'
                    // Not U+FE0E/U+FE0F: they pick text or emoji presentation.
                    | '\u{FE00}'..='\u{FE0D}'
                    | '\u{FEFF}'
                    | '\u{FFA0}'
                    | '\u{FFF0}'..='\u{FFFB}'
                    | '\u{E0000}'..='\u{E0FFF}'
            )
    }
    let mut out = String::new();
    let mut count = 0usize;
    let mut pending_space = false;
    for c in raw.chars().filter(|&c| !invisible(c)) {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if count >= MAX_NAME_CHARS {
            break;
        }
        if pending_space && count + 1 < MAX_NAME_CHARS {
            out.push(' ');
            count += 1;
        }
        pending_space = false;
        out.push(c);
        count += 1;
    }
    if out.is_empty() {
        DEFAULT_NAME.to_string()
    } else {
        out
    }
}

/// Six characters derived from an identity key, so two contacts who never named themselves are
/// still distinguishable (`docs/design/contact-naming.md` §3).
///
/// Crockford-style alphabet: no I/L/O/U, so it can't be misread or accidentally spell anything.
/// ~30 bits — ample against *accidental* collision across a handful of contacts, and nowhere near
/// enough to resist someone grinding keys for a matching tag. That is why it is a disambiguator and
/// never a verification: the safety number is the thing that answers "is this really them".
pub(crate) fn identity_tag(identity_key: &str) -> String {
    use sha2::{Digest, Sha256};
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let digest = Sha256::digest(identity_key.as_bytes());
    digest
        .iter()
        .take(6)
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
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
mod tests_announce;
#[cfg(test)]
mod tests_b;
#[cfg(test)]
mod tests_directory;
#[cfg(test)]
mod tests_held;
#[cfg(test)]
mod tests_hostile;
#[cfg(test)]
mod tests_mailbox;
#[cfg(test)]
mod tests_outbox;
#[cfg(test)]
mod tests_outdated;
#[cfg(test)]
mod tests_pass;
#[cfg(test)]
mod tests_relay_list;
#[cfg(test)]
mod tests_relay_set;
