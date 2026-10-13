//! The minimal relay (`ARCHITECTURE.md` §5c + §6 + §11): an untrusted box that holds only
//! opaque, already-encrypted blobs under ephemeral handles, with short TTLs — **no keys,
//! no logs, no identities** (callers arrive over Tor). It serves two roles with one store:
//!
//! * **Rendezvous mailbox** — first-contact via short code: post an encrypted blob under a
//!   slot handle, drained once by the joiner (`post` / `take`).
//! * **Store-and-forward** — offline / opt-in 24h storage: queue encrypted message blobs
//!   under an (unlinkable) recipient handle until the peer drains them. Each queued blob has
//!   a random `msg_id` and a `delete_token`; the poster keeps the token so it can **recall**
//!   an as-yet-undelivered message (§11.2). A receiver can **peek** (content-free count, for
//!   the low-data background check) and **fetch** (drain `(msg_id, blob)` pairs).
//!
//! A background **reaper** actively drops blobs past their TTL (the server-side 24h time-bomb;
//! §11.4). Blobs are never inspected here. The optional [`RelayLogger`] feeds the dev flow-log
//! / TUI (§11.9) with metadata only — op, truncated handle, size + short hash, **never bytes**.
//! This module holds the shared protocol, the [`RelayCore`] (store + reaper + dispatch), a
//! blocking [`RelayClient`], and an in-process [`RelayServer`] (the `relay/` binary + tests).

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::Result;

/// How often the reaper sweeps expired blobs.
const REAP_INTERVAL: Duration = Duration::from_secs(60);

/// Minimum wall-clock time between disk flushes of the queue. A write that lands at least this long
/// after the previous flush persists **immediately** (durability for normal traffic — a post is on
/// disk before it's acked, unless a write is already running); writes bursting inside the window are
/// **coalesced** into a single flush by the background flusher. Without this, a flood of posts to a large store forces one full
/// rewrite per post — O(store) disk I/O per request (a DoS amplifier). Half a second keeps the
/// crash-loss window tiny while capping flush frequency regardless of request rate.
const FLUSH_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// Lock the store, **recovering from poisoning**: if a thread ever panicked while holding it, take
/// the guard anyway (`into_inner`) rather than propagating the panic to every later request — one
/// bad request must not brick the relay. Mirrors the core's `Inner::lock` (§1.5.3).
fn lock(store: &Store) -> std::sync::MutexGuard<'_, StoreInner> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

/// Wall-clock time in unix seconds (for persistable blob expiry — `Instant` is monotonic and
/// cannot survive a restart).
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resource limits protecting the (in-memory) relay from floods. All are
/// **reject-new**: a full mailbox refuses further posts rather than evicting queued
/// blobs — otherwise a flooder could silently destroy a victim's undelivered mail.
/// Rejections return an error to the poster (whose direct P2P path still works) and are
/// visible in the dev flow-log.
#[derive(Clone, Copy, Debug)]
pub struct RelayLimits {
    /// Largest single blob. Sized for the app's 100 MB media cap after E2E encryption
    /// and envelope sealing (~1.4x inflation).
    pub max_blob_bytes: usize,
    /// Most queued blobs per mailbox handle.
    pub max_mailbox_blobs: usize,
    /// Most queued bytes per mailbox handle.
    pub max_mailbox_bytes: usize,
    /// Total bytes across all mailboxes (the OOM backstop).
    pub max_total_bytes: usize,
    /// Blobs up to this size are "small": every text message and control frame (padded to at
    /// most 16 KiB, then sealed). Only they may use [`small_reserve_bytes`](Self::small_reserve_bytes).
    pub small_blob_bytes: usize,
    /// The last part of `max_total_bytes`, kept for small blobs. Anyone may post, so a few
    /// anonymous 150 MB junk blobs used to fill the relay and refuse every message for up to a day;
    /// large blobs now stop short of this, so ordinary messages still get through a flood of them
    /// (self-audit 2026-10-13, finding 4). A flood of *small* posts can still fill it — that needs
    /// a cost on posting (proof of work), which is not built.
    pub small_reserve_bytes: usize,
    /// Charged per stored blob on top of its length and handle: the entry's own bookkeeping (ids,
    /// timestamps, map slot) is real memory and real `queue.json`, and without it an empty blob
    /// cost nothing. 512 by default.
    pub entry_overhead_bytes: usize,
    /// Longest accepted TTL; longer requests are clamped, keeping the 24h promise (§6)
    /// even against a hostile client asking for more.
    pub max_ttl: Duration,
    /// Largest accepted request line (bytes). Bounds the per-connection read buffer so a client
    /// streaming endless bytes with no newline can't OOM the relay before any other limit applies.
    /// Sized for the biggest legitimate line — a `Post` carrying a base64 blob — plus JSON headroom.
    pub max_line_bytes: usize,
}

impl Default for RelayLimits {
    fn default() -> Self {
        let max_blob_bytes = MAX_BLOB_BYTES;
        Self {
            max_blob_bytes,
            max_mailbox_blobs: 256,
            max_mailbox_bytes: 256 * 1024 * 1024,
            // usize-safe on 32-bit targets too (the core builds for armeabi-v7a).
            max_total_bytes: 1024 * 1024 * 1024,
            small_blob_bytes: 64 * 1024,
            small_reserve_bytes: 128 * 1024 * 1024,
            entry_overhead_bytes: 512,
            max_ttl: Duration::from_secs(24 * 60 * 60),
            // base64 inflates ~4/3; + headroom for the JSON envelope (handle, ttl, version).
            // (`/ 3 * 4` avoids the intermediate overflow that `* 4 / 3` risks on 32-bit.)
            max_line_bytes: max_blob_bytes / 3 * 4 + 4096,
        }
    }
}

/// The relay protocol version, stamped into every request and response line. Bump it on any
/// incompatible change to [`Request`] / [`Response`] so a mismatched client and relay reject
/// each other with a clear error instead of misparsing. Independent of the peer-to-peer
/// [`wire::WIRE_VERSION`](crate::wire::WIRE_VERSION); the two protocols version separately.
pub const RELAY_VERSION: u8 = 1;

/// The virtual port a relay's onion service is dialed on. It lives here, with the protocol,
/// because both ends need it: the core's Tor transport dials it, and the `relay/` binary
/// self-dials it from its own reachability watchdog. A relay accepts any rendezvous stream, so
/// this is a convention rather than a constraint — but the two ends must agree on the convention.
pub const RELAY_PORT: u16 = 9001;

/// The versioned line that actually goes over the socket: `{"v":1,"req":<request>}`.
/// Serializing borrows the request; deserializing (on the relay) owns it.
#[derive(Serialize)]
struct RequestLineRef<'a> {
    v: u8,
    req: &'a Request,
}

#[derive(Deserialize)]
struct RequestLine {
    v: u8,
    req: Request,
}

/// The versioned response line: `{"v":1,"resp":<response>}`.
#[derive(Serialize)]
struct ResponseLineRef<'a> {
    v: u8,
    resp: &'a Response,
}

#[derive(Deserialize)]
struct ResponseLine {
    v: u8,
    resp: Response,
}

/// Most handles one [`Request::TakeMany`] may name. A fragment holds ~8 (`node::mailbox`); the bound
/// only stops a request from asking a relay for unbounded work.
pub const MAX_TAKE_MANY: usize = 64;

/// Largest blob a relay stores by default ([`RelayLimits::max_blob_bytes`]): the app's 100 MB media
/// cap after E2E encryption and envelope sealing (~1.4x).
pub const MAX_BLOB_BYTES: usize = 150 * 1024 * 1024;

/// Raw blob bytes one draining response (`Take`, `TakeMany`, `Fetch`) carries, at most. The rest
/// stays queued for the next take. A response always carries at least one blob, so the bound on
/// the reply is this plus one [`MAX_BLOB_BYTES`] blob — which is what lets a client cap the line it
/// reads ([`MAX_RESPONSE_LINE_BYTES`]). Without it a reply's size was bounded only by the relay's
/// whole store, and a client had to read whatever arrived: a hostile relay could stream until the
/// phone ran out of memory.
pub const TAKE_BUDGET_BYTES: usize = 32 * 1024 * 1024;

/// Longest response line a client reads from a relay: a full [`TAKE_BUDGET_BYTES`] plus one
/// maximum blob, base64-encoded, plus generous JSON overhead (ids, quotes, commas per item).
/// Anything longer is refused, and the connection dropped, before it is buffered.
pub const MAX_RESPONSE_LINE_BYTES: usize =
    (TAKE_BUDGET_BYTES + MAX_BLOB_BYTES) / 3 * 4 + 8 * 1024 * 1024;

/// What is left of one draining response's [`TAKE_BUDGET_BYTES`].
struct TakeBudget {
    room: usize,
    first: bool,
}

impl Default for TakeBudget {
    fn default() -> Self {
        Self {
            room: TAKE_BUDGET_BYTES,
            first: true,
        }
    }
}

impl TakeBudget {
    /// Whether a blob of `len` bytes goes in this response; counts it if so. The first always does.
    fn admit(&mut self, len: usize) -> bool {
        if !self.first && len > self.room {
            return false;
        }
        self.first = false;
        self.room = self.room.saturating_sub(len);
        true
    }
}

/// A request to the relay (one JSON line per request).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Append an opaque blob under `handle`, expiring after `ttl_secs`. Replies with a
    /// `msg_id` and a `delete_token` (the poster keeps the token to [`Recall`](Request::Recall)).
    Post {
        handle: String,
        blob: String,
        ttl_secs: u64,
    },
    /// Content-free: how many non-expired blobs wait under `handle` (the low-data peek).
    Peek { handle: String },
    /// Remove and return all non-expired `(msg_id, blob)` under `handle` (store-and-forward).
    Fetch { handle: String },
    /// Remove and return all non-expired blobs under `handle` (rendezvous; ids not needed).
    Take { handle: String },
    /// [`Take`](Request::Take) for several handles in one round-trip: one polling fragment
    /// (`mailbox-handles.md` §5c), whose handles already share a circuit, so batching reveals
    /// nothing the circuit did not. At most [`MAX_TAKE_MANY`] handles. A relay from before this
    /// request answers "bad request"; clients then fall back to one `Take` per handle.
    TakeMany { handles: Vec<String> },
    /// Delete one still-queued blob — authorised only by its `delete_token` (sender recall).
    Recall {
        handle: String,
        msg_id: String,
        delete_token: String,
    },
    /// Return the operator-signed relay directory this relay serves, if any (§3.1 rotation). The
    /// client verifies the signature against its baked-in key before trusting it.
    GetDirectory,
}

/// A response from the relay (one JSON line). Unused fields are omitted.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `Take` results (blobs only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blobs: Vec<String>,
    /// `Fetch` results: `(msg_id, blob)` pairs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<(String, String)>,
    /// `Peek` result.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub count: u64,
    /// `Post` results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_token: Option<String>,
    /// `GetDirectory` result: the operator-signed relay list (one-line JSON), if the relay serves one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Response {
    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            ..Default::default()
        }
    }
    fn just_ok() -> Self {
        Self {
            ok: true,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------------------
// Dev flow-log (§11.9) — metadata only, never blob bytes. Off in production.
// ---------------------------------------------------------------------------------------

/// One observed relay operation, for the dev flow-log / TUI. Carries only what the relay can
/// see: op, the (truncated) handle, an id, blob **size + a short non-crypto hash** (never the
/// bytes), ttl, the resulting queue depth, and a short result. No identities (callers are Tor).
#[derive(Clone, Debug)]
pub struct RelayEvent {
    pub op: &'static str,
    pub handle: String,
    pub msg_id: Option<String>,
    pub blob_len: usize,
    pub blob_hash: String,
    pub ttl_secs: u64,
    pub queue_depth: usize,
    pub result: String,
}

/// A sink for [`RelayEvent`]s (the dev flow-log writer and/or the TUI). `None` = production.
pub type RelayLogger = Arc<dyn Fn(RelayEvent) + Send + Sync>;

/// A transport for one relay round-trip: takes a request line (newline-terminated), returns the
/// response line. The Tor build provides one that dials the relay `.onion` via the arti client
/// (`TorTransport::relay_dialer`); tests/local use the built-in TCP path. Carries no identity.
pub type RelayDialer = Arc<dyn Fn(&str) -> Result<String> + Send + Sync>;

impl RelayEvent {
    /// A compact human-readable line (the caller prepends a timestamp + `anonymous(Tor)`).
    pub fn summary(&self) -> String {
        let h = trunc(&self.handle);
        let id = self
            .msg_id
            .as_deref()
            .map(|m| format!(" id={}", trunc(m)))
            .unwrap_or_default();
        match self.op {
            "POST" => format!(
                "POST   {h}{id}  {}B h={}  ttl={}s  depth={}  -> {}",
                self.blob_len, self.blob_hash, self.ttl_secs, self.queue_depth, self.result
            ),
            "PEEK" => format!("PEEK   {h}  -> {}", self.result),
            "FETCH" => format!("FETCH  {h}  -> {}", self.result),
            "TAKE" => format!("TAKE   {h}  -> {}", self.result),
            "RECALL" => format!("RECALL {h}{id}  -> {}", self.result),
            "REAP" => format!("REAP   {h}  -> {}", self.result),
            other => format!("{other:6} {h}  -> {}", self.result),
        }
    }
}

// Char-boundary-safe: handles are client-supplied, so byte-slicing could panic mid-codepoint.
fn trunc(s: &str) -> String {
    match s.char_indices().nth(6) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// A short, non-cryptographic fingerprint of a blob — enough to correlate it across log lines
/// without revealing content. Dev-only; never used for security.
fn short_hash(bytes: &[u8]) -> String {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:04x}", h.finish() & 0xffff)
}

// ---------------------------------------------------------------------------------------
// Core (store + reaper + dispatch)
// ---------------------------------------------------------------------------------------

/// One queued blob: an opaque ciphertext under a handle, with its id, recall token, expiry, and
/// when it was posted (for the dashboard's "oldest age").
#[derive(Clone)]
struct Stored {
    msg_id: String,
    delete_token: String,
    posted: Instant,
    expiry: Instant,
    /// Wall-clock expiry (unix seconds) — the persistable mirror of `expiry` (which is a
    /// monotonic `Instant` and can't survive a restart). Enforced again on load.
    expiry_unix: u64,
    bytes: Vec<u8>,
    /// What this entry counts against `max_total_bytes`: the blob, its handle and
    /// `entry_overhead_bytes` ([`entry_cost`]). Freed exactly, wherever it leaves the store.
    cost: usize,
}

/// Longest handle a post may use. Every handle any version sends is under 64 characters
/// (`mbx:` + base64, `rdv:<slot>:<leg>`, `bkp:` + base64).
pub const MAX_HANDLE_BYTES: usize = 128;

/// Whether `handle` may be posted to: 1..=[`MAX_HANDLE_BYTES`] of `A-Z a-z 0-9 : _ - .`. Handles
/// were unchecked, and neither they nor a stored entry's own bookkeeping counted against the
/// relay's byte cap, so posts with huge handles or empty blobs filled memory and `queue.json`
/// without limit (self-audit 2026-10-13, third round).
pub fn valid_handle(handle: &str) -> bool {
    (1..=MAX_HANDLE_BYTES).contains(&handle.len())
        && handle
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-' | b'.'))
}

/// What one stored blob costs against `max_total_bytes`.
fn entry_cost(handle: &str, blob_len: usize, overhead: usize) -> usize {
    blob_len + handle.len() + overhead
}

/// The mailbox map plus a running byte total (kept in sync by post/drain/recall/reap)
/// so the global cap check is O(1) instead of a full-store walk per post.
#[derive(Default)]
struct StoreInner {
    map: HashMap<String, Vec<Stored>>,
    total_bytes: usize,
    /// Set on any mutation (post/drain/recall/reap); the flusher writes the queue to disk when
    /// set and clears it. Only meaningful when persistence is enabled.
    dirty: bool,
    /// When the store was last written to disk. `None` = never, so the first write flushes at once.
    /// Drives the [`FLUSH_MIN_INTERVAL`] rate limit.
    last_flush: Option<Instant>,
}

impl StoreInner {
    /// Remove and return the oldest live blobs under `handle` that fit in what is left of one
    /// response's [`TakeBudget`], keeping the byte total in sync. What does not fit stays queued
    /// for the next take; expired blobs met on the way are dropped.
    fn drain_handle_bounded(
        &mut self,
        handle: &str,
        budget: &mut TakeBudget,
        now: Instant,
    ) -> Vec<Stored> {
        let Some(queue) = self.map.get_mut(handle) else {
            return Vec::new();
        };
        let mut taken = Vec::new();
        let mut freed = 0;
        while let Some(first) = queue.first() {
            if first.expiry <= now {
                freed += queue.remove(0).cost;
                continue;
            }
            if !budget.admit(first.bytes.len()) {
                break;
            }
            let s = queue.remove(0);
            freed += s.cost;
            taken.push(s);
        }
        if queue.is_empty() {
            self.map.remove(handle);
        }
        self.total_bytes -= freed;
        if freed > 0 {
            self.dirty = true;
        }
        taken
    }
}

type Store = Arc<Mutex<StoreInner>>;

// --- Opt-in persistence: survive a relay restart so queued mail isn't lost. Only the same
// opaque, already-encrypted blobs that live in RAM are written — under their unlinkable handles,
// with their 24h expiry as wall-clock time. Expired entries are dropped on load, so nothing
// outlives its TTL on disk. Enabled by the binary via `RelayCore::with_persistence`.

/// On-disk form of one queued blob (base64 so the JSON stays compact; matches the wire codec).
#[derive(Serialize, Deserialize)]
struct PersistedBlob {
    msg_id: String,
    delete_token: String,
    expiry_unix: u64,
    blob: String,
}

/// On-disk form of the whole mailbox store.
#[derive(Serialize, Deserialize, Default)]
struct PersistedStore {
    mailboxes: HashMap<String, Vec<PersistedBlob>>,
}

/// Serialize the live store to JSON bytes. The caller holds the lock, so keep this cheap and free
/// of I/O — the disk write happens off the lock (see [`do_flush`]).
fn serialize_store(inner: &StoreInner) -> Option<Vec<u8>> {
    let mut persisted = PersistedStore::default();
    for (handle, queue) in &inner.map {
        if queue.is_empty() {
            continue;
        }
        persisted.mailboxes.insert(
            handle.clone(),
            queue
                .iter()
                .map(|s| PersistedBlob {
                    msg_id: s.msg_id.clone(),
                    delete_token: s.delete_token.clone(),
                    expiry_unix: s.expiry_unix,
                    blob: B64.encode(&s.bytes),
                })
                .collect(),
        );
    }
    serde_json::to_vec(&persisted).ok()
}

/// Atomically write already-serialized bytes to `path` (tmp + fsync + rename). **No lock held**, so
/// a large write never blocks request handling.
fn write_store_bytes(json: &[u8], path: &Path) -> bool {
    // The relay binary creates its state directory during Tor setup, but queue persistence is
    // constructed first. Make the queue path self-sufficient so the first post cannot be lost.
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let tmp = path.with_extension("tmp");
    let Ok(mut file) = std::fs::File::create(&tmp) else {
        return false;
    };
    if file.write_all(json).is_err() || file.sync_all().is_err() {
        return false;
    }
    std::fs::rename(&tmp, path).is_ok()
}

/// Load a persisted store, dropping anything already past its wall-clock expiry and rebuilding
/// each blob's monotonic `Instant` expiry from the remaining TTL. Missing/corrupt file → empty.
fn load_store(path: &Path, overhead: usize) -> StoreInner {
    let mut inner = StoreInner::default();
    let Ok(data) = std::fs::read(path) else {
        return inner;
    };
    let Ok(persisted) = serde_json::from_slice::<PersistedStore>(&data) else {
        return inner;
    };
    let now_u = now_unix();
    let now_i = Instant::now();
    for (handle, blobs) in persisted.mailboxes {
        let mut queue = Vec::new();
        for b in blobs {
            if b.expiry_unix <= now_u {
                continue; // already expired — never resurrect past the TTL
            }
            let Ok(bytes) = B64.decode(b.blob.as_bytes()) else {
                continue;
            };
            let cost = entry_cost(&handle, bytes.len(), overhead);
            inner.total_bytes += cost;
            queue.push(Stored {
                cost,
                msg_id: b.msg_id,
                delete_token: b.delete_token,
                posted: now_i, // real post time is lost across a restart (dashboard age only)
                expiry: now_i + Duration::from_secs(b.expiry_unix - now_u),
                expiry_unix: b.expiry_unix,
                bytes,
            });
        }
        if !queue.is_empty() {
            inner.map.insert(handle, queue);
        }
    }
    inner
}

/// Persist the store if it's dirty: serialize under the lock, then write to disk **off** the lock
/// (so a large write never blocks request handling). With `respect_interval` (the per-request
/// path), skip the inline write when the last flush was too recent — coalescing a burst into one
/// write per [`FLUSH_MIN_INTERVAL`], which the background flusher then writes. The flusher calls
/// with `respect_interval = false` so a deferred write always lands within one interval.
fn do_flush(store: &Store, writer: &Mutex<()>, path: &Path, respect_interval: bool) {
    // One writer at a time. Without this the flusher and a request could both be mid-write on the
    // same `tmp` file — a long store written over by a shorter one leaves invalid JSON, which loads
    // as an empty store — and an older snapshot could land after a newer one with `dirty` already
    // cleared. Taken before the store lock, so snapshots reach disk in the order they were taken.
    // Waiting here holds up only this request's reply, never the store.
    let _writing = writer.lock().unwrap_or_else(|e| e.into_inner());
    let json = {
        let mut inner = lock(store);
        if !inner.dirty {
            return;
        }
        if respect_interval {
            if let Some(t) = inner.last_flush {
                if t.elapsed() < FLUSH_MIN_INTERVAL {
                    return; // coalesce; the background flusher will write it
                }
            }
        }
        let Some(json) = serialize_store(&inner) else {
            return;
        };
        // Tentatively clear; a post arriving during the off-lock write re-sets it, so no update is
        // lost. Re-set on write failure so the next attempt retries.
        inner.dirty = false;
        inner.last_flush = Some(Instant::now());
        json
    };
    if !write_store_bytes(&json, path) {
        lock(store).dirty = true;
    }
}

/// Background flusher: every [`FLUSH_MIN_INTERVAL`], write out any changes the per-request rate
/// limiter deferred, so a coalesced burst is never unpersisted for longer than one interval.
fn spawn_flusher(store: Store, writer: Arc<Mutex<()>>, path: PathBuf) {
    std::thread::spawn(move || loop {
        std::thread::sleep(FLUSH_MIN_INTERVAL);
        do_flush(&store, &writer, &path, false);
    });
}

/// A per-mailbox snapshot for the dev dashboard (TUI). Metadata only — never blob bytes.
#[derive(Clone, Debug)]
pub struct MailboxStat {
    pub handle: String,
    pub depth: usize,
    pub oldest_age_secs: u64,
    pub total_bytes: usize,
}

/// The relay's in-memory store, reaper, and request dispatch — shared by the TCP [`RelayServer`]
/// (tests/local) and the onion-service loop in the `relay/` binary. An optional [`RelayLogger`]
/// receives a [`RelayEvent`] per operation (and per reaped blob) for the dev flow-log.
pub struct RelayCore {
    store: Store,
    logger: Option<RelayLogger>,
    limits: RelayLimits,
    /// The operator-signed relay directory this relay serves (one-line JSON), if any. Loaded from
    /// the state dir by the binary; served verbatim on `GetDirectory` (clients verify the signature).
    directory: Option<String>,
    /// Where to durably store the mailbox, if persistence was requested.
    persist_path: Option<PathBuf>,
    /// Serializes writes of `persist_path` between request threads and the flusher ([`do_flush`]).
    writer: Arc<Mutex<()>>,
}

impl RelayCore {
    /// Build a core and start its reaper. Pass `Some(logger)` for the dev flow-log, `None` for
    /// production / tests. Uses the default [`RelayLimits`].
    pub fn new(logger: Option<RelayLogger>) -> Self {
        Self::with_limits(logger, RelayLimits::default())
    }

    /// As [`new`](Self::new) with explicit resource limits (tests use tiny ones).
    pub fn with_limits(logger: Option<RelayLogger>, limits: RelayLimits) -> Self {
        let store: Store = Arc::new(Mutex::new(StoreInner::default()));
        spawn_reaper(Arc::clone(&store), logger.clone());
        Self {
            store,
            logger,
            limits,
            directory: None,
            persist_path: None,
            writer: Arc::default(),
        }
    }

    /// Attach the operator-signed relay directory (one-line JSON) this relay should serve on
    /// `GetDirectory`. `None` = no directory. Builder so the binary can load it from disk at start.
    #[must_use]
    pub fn with_directory(mut self, directory: Option<String>) -> Self {
        self.directory = directory;
        self
    }

    /// As [`with_limits`](Self::with_limits) but ALSO persists the queue to `persist_path` —
    /// loading it on startup and flushing changes on a timer — so store-and-forward survives a
    /// relay restart or crash. Only opaque, already-encrypted, time-boxed blobs under unlinkable
    /// handles are written; entries past their TTL are dropped on load.
    pub fn with_persistence(
        logger: Option<RelayLogger>,
        limits: RelayLimits,
        persist_path: PathBuf,
    ) -> Self {
        let store: Store = Arc::new(Mutex::new(load_store(
            &persist_path,
            limits.entry_overhead_bytes,
        )));
        spawn_reaper(Arc::clone(&store), logger.clone());
        let writer: Arc<Mutex<()>> = Arc::default();
        spawn_flusher(
            Arc::clone(&store),
            Arc::clone(&writer),
            persist_path.clone(),
        );
        Self {
            store,
            logger,
            limits,
            directory: None,
            persist_path: Some(persist_path),
            writer,
        }
    }

    /// Process one request line and return the response line (with trailing newline). Emits a
    /// flow-log event if a logger is attached.
    pub fn handle_line(&self, line: &str) -> String {
        let response = match serde_json::from_str::<RequestLine>(line) {
            Ok(rl) if rl.v != RELAY_VERSION => Response::err(format!(
                "unsupported relay version {} (server speaks v{RELAY_VERSION})",
                rl.v
            )),
            Ok(rl) => {
                let (response, event) =
                    process(rl.req, &self.store, &self.limits, self.directory.as_deref());
                if let Some(logger) = &self.logger {
                    logger(event);
                }
                if let Some(path) = &self.persist_path {
                    do_flush(&self.store, &self.writer, path, true);
                }
                response
            }
            Err(e) => Response::err(format!("bad request: {e}")),
        };
        Self::to_line(&response)
    }

    /// The configured maximum request-line length (see [`RelayLimits::max_line_bytes`]). The
    /// connection read loops use this to cap their per-line buffer.
    pub fn max_line_bytes(&self) -> usize {
        self.limits.max_line_bytes
    }

    /// A serialized error response line for a request rejected *before* it could be parsed (e.g. an
    /// over-length line). Same wire shape as [`handle_line`](Self::handle_line)'s output.
    pub fn error_line(msg: &str) -> String {
        Self::to_line(&Response::err(msg))
    }

    /// Serialize a [`Response`] into the versioned, newline-terminated wire line.
    fn to_line(response: &Response) -> String {
        let mut out = serde_json::to_string(&ResponseLineRef {
            v: RELAY_VERSION,
            resp: response,
        })
        .unwrap_or_else(|_| format!("{{\"v\":{RELAY_VERSION},\"resp\":{{\"ok\":false}}}}"));
        out.push('\n');
        out
    }

    /// A snapshot of non-empty mailboxes (metadata only) for the dev dashboard.
    pub fn snapshot(&self) -> Vec<MailboxStat> {
        let now = Instant::now();
        let inner = lock(&self.store);
        let mut out: Vec<MailboxStat> = inner
            .map
            .iter()
            .filter(|(_, q)| !q.is_empty())
            .map(|(handle, q)| MailboxStat {
                handle: handle.clone(),
                depth: q.len(),
                oldest_age_secs: q
                    .iter()
                    .map(|s| now.saturating_duration_since(s.posted).as_secs())
                    .max()
                    .unwrap_or(0),
                total_bytes: q.iter().map(|s| s.bytes.len()).sum(),
            })
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.depth));
        out
    }
}

/// Periodically drop blobs past their TTL (the server-side 24h time-bomb), emitting a `REAP`
/// event per handle that lost blobs.
fn spawn_reaper(store: Store, logger: Option<RelayLogger>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(REAP_INTERVAL);
        let now = Instant::now();
        let mut inner = lock(&store);
        let mut reaped: Vec<(String, usize, usize)> = Vec::new();
        let mut freed = 0usize;
        for (handle, queue) in inner.map.iter_mut() {
            let before = queue.len();
            queue.retain(|s| {
                let keep = s.expiry > now;
                if !keep {
                    freed += s.cost;
                }
                keep
            });
            let dropped = before - queue.len();
            if dropped > 0 {
                reaped.push((handle.clone(), dropped, queue.len()));
            }
        }
        inner.map.retain(|_, q| !q.is_empty());
        inner.total_bytes -= freed;
        inner.dirty |= freed > 0;
        drop(inner);
        if let Some(logger) = &logger {
            for (handle, dropped, depth) in reaped {
                logger(RelayEvent {
                    op: "REAP",
                    handle,
                    msg_id: None,
                    blob_len: 0,
                    blob_hash: String::new(),
                    ttl_secs: 0,
                    queue_depth: depth,
                    result: format!("dropped={dropped}"),
                });
            }
        }
    });
}

/// Apply a request to the store, returning the response and a flow-log event.
fn process(
    request: Request,
    store: &Store,
    limits: &RelayLimits,
    directory: Option<&str>,
) -> (Response, RelayEvent) {
    // Read-only, storeless: hand back the operator-signed relay list (if this relay serves one).
    if let Request::GetDirectory = request {
        let ev = simple_ev(
            "DIRECTORY",
            "-",
            0,
            if directory.is_some() {
                "served"
            } else {
                "none"
            }
            .into(),
        );
        return (
            Response {
                ok: true,
                directory: directory.map(|s| s.to_string()),
                ..Default::default()
            },
            ev,
        );
    }
    let now = Instant::now();
    let mut inner = lock(store);
    let depth_of = |inner: &StoreInner, h: &str| inner.map.get(h).map(|q| q.len()).unwrap_or(0);
    match request {
        Request::Post {
            handle,
            blob,
            ttl_secs,
        } => {
            let mut ev = RelayEvent {
                op: "POST",
                handle: handle.clone(),
                msg_id: None,
                blob_len: 0,
                blob_hash: String::new(),
                ttl_secs,
                queue_depth: 0,
                result: String::new(),
            };
            let Ok(bytes) = B64.decode(blob.as_bytes()) else {
                ev.result = "bad blob".into();
                return (Response::err("bad blob"), ev);
            };
            ev.blob_len = bytes.len();
            ev.blob_hash = short_hash(&bytes);
            if !valid_handle(&handle) {
                ev.result = "rejected: bad handle".into();
                return (Response::err("bad handle"), ev);
            }
            let cost = entry_cost(&handle, bytes.len(), limits.entry_overhead_bytes);

            // Resource limits (reject-new; see [`RelayLimits`]). Checked before storing
            // so a flood can neither OOM the relay nor evict queued mail.
            let reject = if bytes.len() > limits.max_blob_bytes {
                Some("blob too large")
            } else if inner.total_bytes + cost > limits.max_total_bytes {
                Some("relay full")
            } else if bytes.len() > limits.small_blob_bytes
                && inner.total_bytes + cost
                    > limits
                        .max_total_bytes
                        .saturating_sub(limits.small_reserve_bytes)
            {
                // Large blobs leave the reserve to small ones (see `small_reserve_bytes`).
                Some("relay full")
            } else {
                let q = inner.map.get(&handle);
                let depth = q.map(|q| q.len()).unwrap_or(0);
                let q_bytes: usize = q
                    .map(|q| q.iter().map(|s| s.bytes.len()).sum())
                    .unwrap_or(0);
                if depth >= limits.max_mailbox_blobs
                    || q_bytes + bytes.len() > limits.max_mailbox_bytes
                {
                    Some("mailbox full")
                } else {
                    None
                }
            };
            if let Some(reason) = reject {
                ev.queue_depth = depth_of(&inner, &handle);
                ev.result = format!("rejected: {reason}");
                return (Response::err(reason), ev);
            }
            // Clamp the TTL to the advertised cap (§6: 24h promise, even to hostile clients).
            let ttl = Duration::from_secs(ttl_secs).min(limits.max_ttl);

            let msg_id = random_token();
            let delete_token = random_token();
            inner.total_bytes += cost;
            inner.map.entry(handle.clone()).or_default().push(Stored {
                cost,
                msg_id: msg_id.clone(),
                delete_token: delete_token.clone(),
                posted: now,
                expiry: now + ttl,
                expiry_unix: now_unix() + ttl.as_secs(),
                bytes,
            });
            inner.dirty = true;
            ev.msg_id = Some(msg_id.clone());
            ev.queue_depth = depth_of(&inner, &handle);
            ev.result = "ok".into();
            (
                Response {
                    ok: true,
                    msg_id: Some(msg_id),
                    delete_token: Some(delete_token),
                    ..Default::default()
                },
                ev,
            )
        }
        Request::Peek { handle } => {
            let count = inner
                .map
                .get(&handle)
                .map(|q| q.iter().filter(|s| s.expiry > now).count() as u64)
                .unwrap_or(0);
            let ev = simple_ev(
                "PEEK",
                &handle,
                depth_of(&inner, &handle),
                format!("count={count}"),
            );
            (
                Response {
                    ok: true,
                    count,
                    ..Default::default()
                },
                ev,
            )
        }
        Request::Fetch { handle } => {
            let mut budget = TakeBudget::default();
            let items: Vec<(String, String)> = inner
                .drain_handle_bounded(&handle, &mut budget, now)
                .into_iter()
                .map(|s| (s.msg_id, B64.encode(s.bytes)))
                .collect();
            let ev = simple_ev("FETCH", &handle, 0, format!("{} items", items.len()));
            (
                Response {
                    ok: true,
                    items,
                    ..Default::default()
                },
                ev,
            )
        }
        Request::TakeMany { handles } => {
            if handles.len() > MAX_TAKE_MANY {
                let ev = simple_ev("TAKE_MANY", "-", 0, "refused: too many handles".to_string());
                return (
                    Response::err(format!("at most {MAX_TAKE_MANY} handles")),
                    ev,
                );
            }
            let mut blobs: Vec<String> = Vec::new();
            let mut budget = TakeBudget::default();
            for handle in &handles {
                blobs.extend(
                    inner
                        .drain_handle_bounded(handle, &mut budget, now)
                        .into_iter()
                        .map(|s| B64.encode(s.bytes)),
                );
            }
            let ev = simple_ev(
                "TAKE_MANY",
                handles.first().map_or("-", String::as_str),
                0,
                format!("{} handles, {} blobs", handles.len(), blobs.len()),
            );
            (
                Response {
                    ok: true,
                    blobs,
                    ..Default::default()
                },
                ev,
            )
        }
        Request::Take { handle } => {
            let mut budget = TakeBudget::default();
            let blobs: Vec<String> = inner
                .drain_handle_bounded(&handle, &mut budget, now)
                .into_iter()
                .map(|s| B64.encode(s.bytes))
                .collect();
            let ev = simple_ev("TAKE", &handle, 0, format!("{} blobs", blobs.len()));
            (
                Response {
                    ok: true,
                    blobs,
                    ..Default::default()
                },
                ev,
            )
        }
        Request::Recall {
            handle,
            msg_id,
            delete_token,
        } => {
            let mut removed = false;
            let mut freed = 0usize;
            if let Some(queue) = inner.map.get_mut(&handle) {
                let before = queue.len();
                queue.retain(|s| {
                    let hit = s.msg_id == msg_id && s.delete_token == delete_token;
                    if hit {
                        freed += s.cost;
                    }
                    !hit
                });
                removed = queue.len() != before;
                if queue.is_empty() {
                    inner.map.remove(&handle);
                }
            }
            inner.total_bytes -= freed;
            inner.dirty |= removed;
            let mut ev = simple_ev(
                "RECALL",
                &handle,
                depth_of(&inner, &handle),
                if removed { "removed" } else { "not found" }.into(),
            );
            ev.msg_id = Some(msg_id);
            let response = if removed {
                Response::just_ok()
            } else {
                Response::err("not found")
            };
            (response, ev)
        }
        // Handled before the store lock (it needs no store), so it never reaches here.
        Request::GetDirectory => unreachable!("GetDirectory is served before the store lock"),
    }
}

fn simple_ev(op: &'static str, handle: &str, depth: usize, result: String) -> RelayEvent {
    RelayEvent {
        op,
        handle: handle.to_string(),
        msg_id: None,
        blob_len: 0,
        blob_hash: String::new(),
        ttl_secs: 0,
        queue_depth: depth,
        result,
    }
}

/// A random, unguessable opaque id/token (96 bits, base64). Carries no identity.
fn random_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    B64.encode(b)
}

// ---------------------------------------------------------------------------------------
// TCP server (tests / local; production reaches the core over the onion service)
// ---------------------------------------------------------------------------------------

/// Binds a TCP port and serves the relay protocol via a [`RelayCore`]. Tests bind an ephemeral
/// loopback port; the `relay/` binary serves the same core over its onion service instead.
pub struct RelayServer;

impl RelayServer {
    /// Bind `addr` and serve forever on background threads (no flow-log). Returns the bound
    /// address (useful when `addr` has port 0).
    pub fn spawn(addr: &str) -> Result<std::net::SocketAddr> {
        Self::spawn_with(addr, None)
    }

    /// As [`spawn`](Self::spawn), with an optional dev flow-log logger.
    pub fn spawn_with(addr: &str, logger: Option<RelayLogger>) -> Result<std::net::SocketAddr> {
        Self::spawn_with_limits(addr, logger, RelayLimits::default())
    }

    /// As [`spawn_with`](Self::spawn_with), with explicit [`RelayLimits`] (tests use tiny ones).
    pub fn spawn_with_limits(
        addr: &str,
        logger: Option<RelayLogger>,
        limits: RelayLimits,
    ) -> Result<std::net::SocketAddr> {
        let listener = TcpListener::bind(addr)?;
        let local = listener.local_addr()?;
        let core = Arc::new(RelayCore::with_limits(logger, limits));
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                let core = Arc::clone(&core);
                std::thread::spawn(move || {
                    let _ = handle_connection(stream, core);
                });
            }
        });
        Ok(local)
    }
}

fn handle_connection(stream: TcpStream, core: Arc<RelayCore>) -> Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let max = core.max_line_bytes();
    loop {
        match read_line_capped(&mut reader, max) {
            Ok(None) => break, // clean EOF
            Ok(Some(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                writer.write_all(core.handle_line(line).as_bytes())?;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::InvalidData => {
                // Over-length line: refuse and drop the connection rather than buffer more.
                let _ = writer.write_all(RelayCore::error_line("line too long").as_bytes());
                break;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Read one newline-terminated line, capping the buffer at `max` bytes. Returns `Ok(None)` at a
/// clean EOF and `Err(InvalidData)` if a line exceeds `max` (so an endless no-newline stream can't
/// grow the buffer without bound). The trailing newline is not included.
fn read_line_capped<R: BufRead>(reader: &mut R, max: usize) -> std::io::Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        // Scope the borrow of the reader's buffer so we can `consume` after copying out of it.
        let (consumed, done) = {
            let chunk = reader.fill_buf()?;
            if chunk.is_empty() {
                (0usize, true) // EOF
            } else if let Some(i) = chunk.iter().position(|&b| b == b'\n') {
                buf.extend_from_slice(&chunk[..i]); // exclude the '\n'
                (i + 1, true)
            } else {
                buf.extend_from_slice(chunk);
                (chunk.len(), false)
            }
        };
        reader.consume(consumed);
        if done {
            if buf.is_empty() && consumed == 0 {
                return Ok(None);
            }
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
        if buf.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too long",
            ));
        }
    }
}

// ---------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------

/// A receipt for a posted blob: the relay's id for it plus the secret token needed to recall
/// it while it is still queued (§11.3 sender "unsend").
#[derive(Clone, Debug)]
pub struct PostReceipt {
    pub msg_id: String,
    pub delete_token: String,
}

/// A drained store-and-forward message: its relay id and the opaque blob.
#[derive(Clone, Debug)]
pub struct FetchedBlob {
    pub msg_id: String,
    pub blob: Vec<u8>,
}

/// A blocking client to a relay. Cheap to clone/store. Either dials a TCP address (tests/local)
/// or routes each round-trip through a [`RelayDialer`] (production: the relay `.onion` over the
/// Tor client; see §11.2).
#[derive(Clone)]
pub struct RelayClient {
    inner: RelayInner,
}

#[derive(Clone)]
enum RelayInner {
    Tcp(String),
    /// The dialer, and the address it dials when known — kept so a caller can build a sibling
    /// client for the same relay on an isolated circuit (`Transport::relay_dialer_isolated`).
    Dialer(RelayDialer, Option<String>),
}

impl std::fmt::Debug for RelayClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            RelayInner::Tcp(addr) => write!(f, "RelayClient(tcp:{addr})"),
            RelayInner::Dialer(..) => write!(f, "RelayClient(tor)"),
        }
    }
}

impl RelayClient {
    /// A relay reached over plain TCP (tests / local / a fronting Tor HiddenService).
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            inner: RelayInner::Tcp(addr.into()),
        }
    }

    /// A relay reached through a [`RelayDialer`] — production dials its `.onion` over Tor.
    pub fn with_dialer(dialer: RelayDialer) -> Self {
        Self {
            inner: RelayInner::Dialer(dialer, None),
        }
    }

    /// [`with_dialer`](Self::with_dialer), recording the address `dialer` reaches, so
    /// [`addr`](Self::addr) can report it.
    pub fn with_dialer_for(addr: impl Into<String>, dialer: RelayDialer) -> Self {
        Self {
            inner: RelayInner::Dialer(dialer, Some(addr.into())),
        }
    }

    /// The relay's address, if this client knows it: always for TCP, for a dialer only when built
    /// with [`with_dialer_for`](Self::with_dialer_for).
    pub fn addr(&self) -> Option<&str> {
        match &self.inner {
            RelayInner::Tcp(addr) => Some(addr),
            RelayInner::Dialer(_, addr) => addr.as_deref(),
        }
    }

    /// Post an opaque, already-encrypted blob under `handle`; returns its id + recall token.
    pub fn post(&self, handle: &str, blob: &[u8], ttl: Duration) -> Result<PostReceipt> {
        let response = self.round_trip(&Request::Post {
            handle: handle.to_string(),
            blob: B64.encode(blob),
            ttl_secs: ttl.as_secs(),
        })?;
        if !response.ok {
            anyhow::bail!("relay post failed: {:?}", response.error);
        }
        Ok(PostReceipt {
            msg_id: response.msg_id.unwrap_or_default(),
            delete_token: response.delete_token.unwrap_or_default(),
        })
    }

    /// Content-free count of queued blobs under `handle` (the low-data background peek).
    pub fn peek(&self, handle: &str) -> Result<u64> {
        let response = self.round_trip(&Request::Peek {
            handle: handle.to_string(),
        })?;
        if !response.ok {
            anyhow::bail!("relay peek failed: {:?}", response.error);
        }
        Ok(response.count)
    }

    /// Drain queued store-and-forward messages (id + blob) under `handle`.
    pub fn fetch(&self, handle: &str) -> Result<Vec<FetchedBlob>> {
        let response = self.round_trip(&Request::Fetch {
            handle: handle.to_string(),
        })?;
        if !response.ok {
            anyhow::bail!("relay fetch failed: {:?}", response.error);
        }
        response
            .items
            .into_iter()
            .map(|(msg_id, blob)| {
                Ok(FetchedBlob {
                    msg_id,
                    blob: B64.decode(blob.as_bytes())?,
                })
            })
            .collect()
    }

    /// Drain all queued blobs under `handle` (rendezvous; ids not needed).
    pub fn take(&self, handle: &str) -> Result<Vec<Vec<u8>>> {
        let response = self.round_trip(&Request::Take {
            handle: handle.to_string(),
        })?;
        if !response.ok {
            anyhow::bail!("relay take failed: {:?}", response.error);
        }
        response
            .blobs
            .iter()
            .map(|b| B64.decode(b.as_bytes()).map_err(Into::into))
            .collect()
    }

    /// Drain several handles in one round-trip ([`Request::TakeMany`]), falling back to one
    /// [`take`](Self::take) per handle against a relay that predates it. Partial failure in the
    /// fallback keeps what did arrive: a take that errors removed nothing, so nothing is lost.
    pub fn take_many(&self, handles: &[String]) -> Result<Vec<Vec<u8>>> {
        if handles.len() <= 1 || handles.len() > MAX_TAKE_MANY {
            return self.take_each(handles);
        }
        let response = self.round_trip(&Request::TakeMany {
            handles: handles.to_vec(),
        })?;
        if !response.ok {
            if response
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("bad request"))
            {
                return self.take_each(handles); // an older relay
            }
            anyhow::bail!("relay take_many failed: {:?}", response.error);
        }
        response
            .blobs
            .iter()
            .map(|b| B64.decode(b.as_bytes()).map_err(Into::into))
            .collect()
    }

    fn take_each(&self, handles: &[String]) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        let mut answered = handles.is_empty();
        let mut last_err = None;
        for handle in handles {
            match self.take(handle) {
                Ok(blobs) => {
                    answered = true;
                    out.extend(blobs);
                }
                Err(e) => last_err = Some(e),
            }
        }
        match (answered, last_err) {
            (false, Some(e)) => Err(e),
            _ => Ok(out),
        }
    }

    /// Recall (delete) a still-queued blob using the token from its [`PostReceipt`]. Returns
    /// `Ok(true)` if it was removed, `Ok(false)` if it was already delivered/expired.
    pub fn recall(&self, handle: &str, receipt: &PostReceipt) -> Result<bool> {
        let response = self.round_trip(&Request::Recall {
            handle: handle.to_string(),
            msg_id: receipt.msg_id.clone(),
            delete_token: receipt.delete_token.clone(),
        })?;
        Ok(response.ok)
    }

    /// Fetch this relay's operator-signed relay directory (§3.1), if it serves one. The caller
    /// MUST verify the signature against its baked-in key before trusting the relays inside.
    pub fn get_directory(&self) -> Result<Option<String>> {
        let response = self.round_trip(&Request::GetDirectory)?;
        if !response.ok {
            anyhow::bail!("relay get_directory failed: {:?}", response.error);
        }
        Ok(response.directory)
    }

    fn round_trip(&self, request: &Request) -> Result<Response> {
        let line = request_line(request)?;
        let response_line = match &self.inner {
            RelayInner::Tcp(addr) => tcp_round_trip(addr, &line)?,
            RelayInner::Dialer(dial, _) => dial(&line)?,
        };
        parse_response_line(&response_line)
    }
}

/// Serialize `request` as the newline-terminated line that goes over the socket.
///
/// [`RelayClient`] is blocking, so an async caller (the `relay/` binary's self-dial watchdog)
/// cannot go through it. Rather than have that caller hand-roll the `{"v":1,"req":…}` envelope —
/// which would then be a second definition of the wire format, free to drift from this one — it
/// builds its line here and reads the answer back through [`parse_response_line`].
pub fn request_line(request: &Request) -> Result<String> {
    let mut line = serde_json::to_string(&RequestLineRef {
        v: RELAY_VERSION,
        req: request,
    })?;
    line.push('\n');
    Ok(line)
}

/// Parse one relay response line, rejecting a version this client does not speak.
///
/// The counterpart to [`request_line`]; see it for why this is public.
pub fn parse_response_line(line: &str) -> Result<Response> {
    let rl: ResponseLine = serde_json::from_str(line)?;
    if rl.v != RELAY_VERSION {
        anyhow::bail!(
            "unsupported relay version {} (client speaks v{RELAY_VERSION})",
            rl.v
        );
    }
    Ok(rl.resp)
}

/// One newline-JSON request/response over plain TCP (tests / local).
fn tcp_round_trip(addr: &str, line: &str) -> Result<String> {
    let stream = TcpStream::connect(addr)?;
    let mut writer = stream.try_clone()?;
    writer.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream);
    // Capped: a relay's reply is bounded by `MAX_RESPONSE_LINE_BYTES`, and one that is not
    // honest must not be able to grow this buffer until the app runs out of memory.
    read_line_capped(&mut reader, MAX_RESPONSE_LINE_BYTES)?
        .ok_or_else(|| anyhow::anyhow!("relay closed the connection without answering"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A client on an in-process relay; `old` makes it answer `take_many` as a relay from before
    /// that request does (a genuine "bad request", produced by the relay's own parser).
    fn in_process(old: bool) -> (Arc<RelayCore>, RelayClient) {
        let core = Arc::new(RelayCore::new(None));
        let c = Arc::clone(&core);
        let client = RelayClient::with_dialer(Arc::new(move |line: &str| {
            if old && line.contains("\"take_many\"") {
                return Ok(c.handle_line("{not a request}"));
            }
            Ok(c.handle_line(line))
        }));
        (core, client)
    }

    #[test]
    fn take_many_drains_every_handle_in_one_round_trip() {
        let (_core, relay) = in_process(false);
        let ttl = Duration::from_secs(60);
        relay.post("mbx:a", b"one", ttl).unwrap();
        relay.post("mbx:b", b"two", ttl).unwrap();
        relay.post("mbx:c", b"not asked for", ttl).unwrap();
        let mut got = relay
            .take_many(&["mbx:a".into(), "mbx:b".into(), "mbx:none".into()])
            .unwrap();
        got.sort();
        assert_eq!(got, vec![b"one".to_vec(), b"two".to_vec()]);
        assert!(
            relay
                .take_many(&["mbx:a".into(), "mbx:b".into()])
                .unwrap()
                .is_empty(),
            "drained"
        );
        assert_eq!(
            relay.take("mbx:c").unwrap(),
            vec![b"not asked for".to_vec()],
            "untouched"
        );
    }

    #[test]
    fn take_many_falls_back_on_a_relay_that_predates_it() {
        let (_core, relay) = in_process(true);
        let ttl = Duration::from_secs(60);
        relay.post("mbx:a", b"one", ttl).unwrap();
        relay.post("mbx:b", b"two", ttl).unwrap();
        let mut got = relay.take_many(&["mbx:a".into(), "mbx:b".into()]).unwrap();
        got.sort();
        assert_eq!(
            got,
            vec![b"one".to_vec(), b"two".to_vec()],
            "one take per handle instead"
        );
    }

    /// A draining reply carries at most `TAKE_BUDGET_BYTES` (plus its first blob); the rest waits
    /// for the next take, in order, and nothing is lost.
    #[test]
    fn a_take_leaves_what_does_not_fit_its_budget_for_the_next_one() {
        let (core, relay) = in_process(false);
        let big = vec![7u8; TAKE_BUDGET_BYTES / 2 + 1];
        for _ in 0..3 {
            relay
                .post("mbx:big", &big, Duration::from_secs(60))
                .unwrap();
        }
        let first = relay.take_many(&["mbx:big".into()]).unwrap();
        assert_eq!(first.len(), 1, "two of these would pass the budget");
        let second = relay.take_many(&["mbx:big".into()]).unwrap();
        assert_eq!(second.len(), 1);
        let third = relay.take_many(&["mbx:big".into()]).unwrap();
        assert_eq!(third.len(), 1, "the last one, on the third take");
        assert!(relay.take_many(&["mbx:big".into()]).unwrap().is_empty());
        assert_eq!(
            core.store.lock().unwrap().total_bytes,
            0,
            "byte count back to zero"
        );
    }

    #[test]
    fn a_blob_larger_than_the_budget_still_goes_out_alone() {
        let (_core, relay) = in_process(false);
        relay
            .post("mbx:a", &[1u8; 10], Duration::from_secs(60))
            .unwrap();
        let huge = vec![9u8; TAKE_BUDGET_BYTES + 1];
        relay.post("mbx:b", &huge, Duration::from_secs(60)).unwrap();
        let got = relay.take_many(&["mbx:b".into(), "mbx:a".into()]).unwrap();
        assert_eq!(got.len(), 1, "the oversized blob, first and alone");
        assert_eq!(got[0].len(), huge.len());
        assert_eq!(relay.take_many(&["mbx:a".into()]).unwrap().len(), 1);
    }

    #[test]
    fn take_many_is_bounded() {
        let core = RelayCore::new(None);
        let handles: Vec<String> = (0..=MAX_TAKE_MANY).map(|i| format!("mbx:{i}")).collect();
        let line = serde_json::to_string(&RequestLineRef {
            v: RELAY_VERSION,
            req: &Request::TakeMany { handles },
        })
        .unwrap();
        let reply = core.handle_line(&line);
        assert!(reply.contains("\"ok\":false"), "{reply}");
    }

    #[test]
    fn read_line_capped_splits_lines_and_bounds_length() {
        // Normal lines are returned without the trailing newline; EOF yields None.
        let mut r = Cursor::new(b"hello\nworld\n".to_vec());
        assert_eq!(
            read_line_capped(&mut r, 100).unwrap().as_deref(),
            Some("hello")
        );
        assert_eq!(
            read_line_capped(&mut r, 100).unwrap().as_deref(),
            Some("world")
        );
        assert_eq!(read_line_capped(&mut r, 100).unwrap(), None);

        // A final line without a trailing newline is still returned.
        let mut r = Cursor::new(b"tail".to_vec());
        assert_eq!(
            read_line_capped(&mut r, 100).unwrap().as_deref(),
            Some("tail")
        );

        // An over-length line (no newline within the cap) is rejected instead of buffered without
        // bound — this is the OOM guard against an endless no-newline stream.
        let mut r = Cursor::new(vec![b'a'; 10_000]);
        let err = read_line_capped(&mut r, 1024).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn relay_serves_the_configured_directory_and_none_otherwise() {
        let req = serde_json::to_string(&RequestLineRef {
            v: RELAY_VERSION,
            req: &Request::GetDirectory,
        })
        .unwrap();

        // Configured → served verbatim.
        let signed = r#"{"payload":"abc","sig":"def"}"#;
        let core = RelayCore::new(None).with_directory(Some(signed.to_string()));
        let resp: ResponseLine = serde_json::from_str(&core.handle_line(&req)).unwrap();
        assert!(resp.resp.ok);
        assert_eq!(resp.resp.directory.as_deref(), Some(signed));

        // Not configured → ok with no directory.
        let core2 = RelayCore::new(None);
        let resp2: ResponseLine = serde_json::from_str(&core2.handle_line(&req)).unwrap();
        assert!(resp2.resp.ok && resp2.resp.directory.is_none());
    }

    #[test]
    fn persisted_queue_round_trips_and_drops_expired_on_load() {
        // A relay restart must not lose still-live mail, and must not resurrect expired mail.
        let path =
            std::env::temp_dir().join(format!("nd-relay-persist-{}.json", std::process::id()));
        let now_i = Instant::now();
        let now_u = now_unix();
        let mut inner = StoreInner::default();
        inner.map.insert(
            "mbx:live".into(),
            vec![Stored {
                msg_id: "m1".into(),
                delete_token: "t1".into(),
                posted: now_i,
                expiry: now_i + Duration::from_secs(3600),
                expiry_unix: now_u + 3600,
                bytes: b"live-blob".to_vec(),
                cost: 9,
            }],
        );
        inner.map.insert(
            "mbx:dead".into(),
            vec![Stored {
                msg_id: "m2".into(),
                delete_token: "t2".into(),
                posted: now_i,
                expiry: now_i,
                expiry_unix: now_u.saturating_sub(10), // already expired
                bytes: b"expired".to_vec(),
                cost: 7,
            }],
        );
        inner.total_bytes = 9 + 7;

        write_store_bytes(&serialize_store(&inner).unwrap(), &path);
        let loaded = load_store(&path, 0);

        assert_eq!(
            loaded.map.get("mbx:live").map(|q| q.len()),
            Some(1),
            "live blob must survive a restart"
        );
        assert_eq!(loaded.map["mbx:live"][0].bytes, b"live-blob");
        assert_eq!(loaded.map["mbx:live"][0].msg_id, "m1");
        assert_eq!(loaded.map["mbx:live"][0].delete_token, "t1");
        assert!(
            !loaded.map.contains_key("mbx:dead"),
            "an expired blob must not be resurrected past its TTL"
        );
        assert_eq!(
            loaded.total_bytes,
            entry_cost("mbx:live", 9, 0),
            "byte total counts only the live blob (and its handle)"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("tmp"));
    }

    #[test]
    fn persistent_relay_flushes_before_acknowledging_a_post() {
        // Simulate a process dying immediately after `post` returns: no flusher interval may
        // stand between a successful reply and the mailbox reaching queue.json.
        let path = std::env::temp_dir().join(format!(
            "nd-relay-immediate-persist-{}-{}.json",
            std::process::id(),
            now_unix()
        ));
        let core = RelayCore::with_persistence(None, RelayLimits::default(), path.clone());
        let post = serde_json::to_string(&RequestLineRef {
            v: RELAY_VERSION,
            req: &Request::Post {
                handle: "mbx:crash-recovery".into(),
                blob: B64.encode(b"survive-now"),
                ttl_secs: 60,
            },
        })
        .unwrap();
        let response: ResponseLine = serde_json::from_str(&core.handle_line(&post)).unwrap();
        assert!(response.resp.ok);
        assert!(path.exists(), "queue is written before post succeeds");

        // A fresh core is the recovery path after the simulated crash.
        drop(core);
        let recovered = RelayCore::with_persistence(None, RelayLimits::default(), path.clone());
        let peek = serde_json::to_string(&RequestLineRef {
            v: RELAY_VERSION,
            req: &Request::Peek {
                handle: "mbx:crash-recovery".into(),
            },
        })
        .unwrap();
        let response: ResponseLine = serde_json::from_str(&recovered.handle_line(&peek)).unwrap();
        assert_eq!(response.resp.count, 1, "mail survives immediate restart");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("tmp"));
    }

    #[test]
    fn concurrent_flushes_never_leave_a_torn_queue_file() {
        // The flusher and a request thread both write the queue. Run two writers flat out while
        // the store swings between large and small; every file left on disk must parse, and the
        // last one must hold the final state.
        let path = std::env::temp_dir().join(format!(
            "nd-relay-torn-{}-{}.json",
            std::process::id(),
            now_unix()
        ));
        let store: Store = Arc::new(Mutex::new(StoreInner::default()));
        let writer: Arc<Mutex<()>> = Arc::default();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flushers: Vec<_> = (0..2)
            .map(|_| {
                let (store, writer, path, stop) = (
                    Arc::clone(&store),
                    Arc::clone(&writer),
                    path.clone(),
                    Arc::clone(&stop),
                );
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        do_flush(&store, &writer, &path, false);
                    }
                })
            })
            .collect();
        let mut torn = 0;
        for round in 0..300 {
            {
                let mut inner = lock(&store);
                let size = if round % 2 == 0 { 512 * 1024 } else { 16 };
                inner.map.clear();
                inner.total_bytes = size;
                inner.map.insert(
                    "mbx:t".into(),
                    vec![Stored {
                        msg_id: format!("{round}"),
                        delete_token: String::new(),
                        posted: Instant::now(),
                        expiry: Instant::now() + Duration::from_secs(60),
                        expiry_unix: now_unix() + 60,
                        bytes: vec![b'x'; size],
                        cost: size,
                    }],
                );
                inner.dirty = true;
            }
            for _ in 0..4 {
                if let Ok(data) = std::fs::read(&path) {
                    torn += usize::from(serde_json::from_slice::<PersistedStore>(&data).is_err());
                }
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for f in flushers {
            f.join().unwrap();
        }
        do_flush(&store, &writer, &path, false);
        assert_eq!(torn, 0, "a queue file on disk failed to parse");
        let last = load_store(&path, 0);
        assert_eq!(
            last.map["mbx:t"][0].msg_id, "299",
            "the newest snapshot is on disk"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("tmp"));
    }

    #[test]
    fn coalesced_posts_are_not_lost_by_the_flush_rate_limiter() {
        // A burst of posts within FLUSH_MIN_INTERVAL: the first flushes inline, the rest are
        // deferred (rate-limited). None may be lost — the background flusher persists them within
        // one interval. Guards the DoS fix against silently dropping mail under load.
        let path = std::env::temp_dir().join(format!(
            "nd-relay-coalesce-{}-{}.json",
            std::process::id(),
            now_unix()
        ));
        let core = RelayCore::with_persistence(None, RelayLimits::default(), path.clone());
        for i in 0..5 {
            let post = serde_json::to_string(&RequestLineRef {
                v: RELAY_VERSION,
                req: &Request::Post {
                    handle: format!("mbx:burst-{i}"),
                    blob: B64.encode(format!("blob-{i}").as_bytes()),
                    ttl_secs: 60,
                },
            })
            .unwrap();
            assert!(
                serde_json::from_str::<ResponseLine>(&core.handle_line(&post))
                    .unwrap()
                    .resp
                    .ok
            );
        }
        // Let the background flusher write the deferred posts.
        std::thread::sleep(FLUSH_MIN_INTERVAL + Duration::from_millis(400));
        let recovered = load_store(&path, 0);
        assert_eq!(
            recovered.map.len(),
            5,
            "all coalesced posts persist, none dropped by the rate limiter"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("tmp"));
    }

    #[test]
    fn take_drains_in_order_and_clears() {
        let addr = RelayServer::spawn("127.0.0.1:0").unwrap();
        let client = RelayClient::new(addr.to_string());
        client
            .post("slot-7", b"a", Duration::from_secs(60))
            .unwrap();
        client
            .post("slot-7", b"b", Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            client.take("slot-7").unwrap(),
            vec![b"a".to_vec(), b"b".to_vec()]
        );
        assert!(client.take("slot-7").unwrap().is_empty());
    }

    #[test]
    fn peek_counts_without_draining_then_fetch_drains() {
        let addr = RelayServer::spawn("127.0.0.1:0").unwrap();
        let client = RelayClient::new(addr.to_string());
        client.post("mbx", b"one", Duration::from_secs(60)).unwrap();
        client.post("mbx", b"two", Duration::from_secs(60)).unwrap();

        assert_eq!(client.peek("mbx").unwrap(), 2, "peek counts, doesn't drain");
        assert_eq!(client.peek("mbx").unwrap(), 2, "still there after peek");

        let fetched = client.fetch("mbx").unwrap();
        assert_eq!(fetched.len(), 2);
        assert_eq!(fetched[0].blob, b"one");
        assert!(!fetched[0].msg_id.is_empty());
        assert_eq!(client.peek("mbx").unwrap(), 0, "fetch drained the queue");
    }

    #[test]
    fn recall_removes_only_with_the_right_token() {
        let addr = RelayServer::spawn("127.0.0.1:0").unwrap();
        let client = RelayClient::new(addr.to_string());
        let r = client
            .post("mbx", b"secret", Duration::from_secs(60))
            .unwrap();

        let bad = PostReceipt {
            msg_id: r.msg_id.clone(),
            delete_token: "wrong".into(),
        };
        assert!(!client.recall("mbx", &bad).unwrap());
        assert_eq!(client.peek("mbx").unwrap(), 1);

        assert!(client.recall("mbx", &r).unwrap());
        assert_eq!(client.peek("mbx").unwrap(), 0);
        assert!(!client.recall("mbx", &r).unwrap());
    }

    #[test]
    fn expired_blobs_are_not_returned() {
        let addr = RelayServer::spawn("127.0.0.1:0").unwrap();
        let client = RelayClient::new(addr.to_string());
        client
            .post("mbx", b"too late", Duration::from_secs(0))
            .unwrap();
        assert_eq!(client.peek("mbx").unwrap(), 0, "ttl=0 expires immediately");
        assert!(client.fetch("mbx").unwrap().is_empty());
    }

    #[test]
    fn unknown_handle_is_empty() {
        let addr = RelayServer::spawn("127.0.0.1:0").unwrap();
        let client = RelayClient::new(addr.to_string());
        assert!(client.fetch("nope").unwrap().is_empty());
        assert_eq!(client.peek("nope").unwrap(), 0);
    }

    #[test]
    fn limits_reject_new_without_evicting_queued_mail() {
        let tiny = RelayLimits {
            max_blob_bytes: 8,
            max_mailbox_blobs: 2,
            max_mailbox_bytes: 12,
            max_total_bytes: 30,
            small_blob_bytes: usize::MAX,
            small_reserve_bytes: 0,
            entry_overhead_bytes: 0,
            max_ttl: Duration::from_secs(60),
            max_line_bytes: 4096,
        };
        let addr = RelayServer::spawn_with_limits("127.0.0.1:0", None, tiny).unwrap();
        let client = RelayClient::new(addr.to_string());
        let ttl = Duration::from_secs(30);

        // Oversized blob is refused outright.
        assert!(
            client.post("mbx", &[0u8; 9], ttl).is_err(),
            "blob too large"
        );

        // Mailbox depth cap: the third post to one handle is refused; the first two stay.
        client.post("mbx", b"one", ttl).unwrap();
        client.post("mbx", b"two", ttl).unwrap();
        assert!(client.post("mbx", b"three", ttl).is_err(), "mailbox full");
        assert_eq!(
            client.peek("mbx").unwrap(),
            2,
            "queued mail was NOT evicted"
        );

        // Global cap: filling other mailboxes stops before OOM. Each entry counts its blob and its
        // handle: "mbx" holds 2 × (3 + 3) = 12 of the 30-byte cap.
        client.post("other1", b"12345678", ttl).unwrap(); // + 8 + 6 = 26
        assert!(
            client.post("other2", b"12345678", ttl).is_err(),
            "relay full"
        );

        // Draining frees budget: after a fetch the same post succeeds.
        client.fetch("other1").unwrap();
        client.post("other2", b"12345678", ttl).unwrap();
    }

    #[test]
    fn every_real_handle_is_valid_and_junk_is_not() {
        for ok in [
            "mbx:Zm9vYmFyYmF6cXV4Zm9vYmFyYmF6cXV4Zm9vYmFyYmE",
            "mbx:ab-_CD12",
            "rdv:k3mf9q:joiner",
            "bkp:AAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(valid_handle(ok), "{ok}");
        }
        let long = "m".repeat(MAX_HANDLE_BYTES + 1);
        for bad in [
            "",
            long.as_str(),
            "mbx:a b",
            "mbx:é",
            "mbx:a\n",
            "../x",
            "mbx:a/b",
        ] {
            assert!(!valid_handle(bad), "{bad:?}");
        }
    }

    #[test]
    fn empty_blobs_and_long_handles_cannot_grow_the_store_without_limit() {
        // Self-audit 2026-10-13, third round: an empty blob counted 0 bytes and a handle counted
        // nothing, so posts to fresh handles grew memory and queue.json past every cap.
        let limits = RelayLimits {
            max_total_bytes: 10_000,
            entry_overhead_bytes: 512,
            ..RelayLimits::default()
        };
        let addr = RelayServer::spawn_with_limits("127.0.0.1:0", None, limits).unwrap();
        let client = RelayClient::new(addr.to_string());
        let ttl = Duration::from_secs(30);
        let mut stored = 0;
        for i in 0..1000 {
            if client.post(&format!("mbx:junk{i}"), b"", ttl).is_ok() {
                stored += 1;
            }
        }
        assert!(stored < 20, "{stored} empty blobs stored under a 10 kB cap");
        assert!(client
            .post(&"m".repeat(MAX_HANDLE_BYTES + 1), b"x", ttl)
            .is_err());
    }

    #[test]
    fn a_flood_of_large_blobs_cannot_block_small_messages() {
        // Self-audit 2026-10-13, finding 4: anyone can post, so large junk blobs filled the relay
        // and every message after them was refused as "relay full" for up to a day.
        let limits = RelayLimits {
            max_blob_bytes: 100,
            max_mailbox_blobs: 100,
            max_mailbox_bytes: 1000,
            max_total_bytes: 480,
            small_blob_bytes: 10,
            small_reserve_bytes: 160,
            entry_overhead_bytes: 0,
            max_ttl: Duration::from_secs(60),
            max_line_bytes: 4096,
        };
        let addr = RelayServer::spawn_with_limits("127.0.0.1:0", None, limits).unwrap();
        let client = RelayClient::new(addr.to_string());
        let ttl = Duration::from_secs(30);
        // The flood: large blobs on throwaway handles until the relay refuses them.
        let mut posted = 0;
        for i in 0..10 {
            if client.post(&format!("junk{i}"), &[0u8; 100], ttl).is_ok() {
                posted += 1;
            }
        }
        assert_eq!(
            posted, 3,
            "large blobs stop at total - reserve (3 × (100 + 5) of 320)"
        );
        // Ordinary messages still get through, into the reserve.
        for i in 0..10 {
            client
                .post(&format!("mbx{i}"), b"0123456789", ttl)
                .expect("a small message after a large-blob flood");
        }
        // And the reserve is still bounded by the total.
        let mut small = 0;
        for i in 0..20 {
            if client.post(&format!("more{i}"), b"0123456789", ttl).is_ok() {
                small += 1;
            }
        }
        assert!(small < 20, "small posts still stop at max_total_bytes");
    }

    #[test]
    fn ttl_is_clamped_to_the_advertised_cap() {
        let tiny = RelayLimits {
            max_ttl: Duration::from_secs(0), // everything expires immediately
            ..RelayLimits::default()
        };
        let addr = RelayServer::spawn_with_limits("127.0.0.1:0", None, tiny).unwrap();
        let client = RelayClient::new(addr.to_string());
        // The client asks for a year; the relay clamps to its cap (here: instant expiry).
        client
            .post("mbx", b"forever?", Duration::from_secs(365 * 24 * 3600))
            .unwrap();
        assert_eq!(
            client.peek("mbx").unwrap(),
            0,
            "hostile TTL clamped to the cap"
        );
    }

    #[test]
    fn request_and_response_lines_carry_the_version() {
        let addr = RelayServer::spawn("127.0.0.1:0").unwrap();
        // Hand-build a raw versioned request line and read the raw response line, so we
        // assert the on-the-wire shape (not just the round-trip through the client).
        let req = serde_json::json!({
            "v": RELAY_VERSION,
            "req": { "op": "peek", "handle": "h" },
        });
        let mut line = req.to_string();
        line.push('\n');
        let raw = tcp_round_trip(&addr.to_string(), &line).unwrap();
        let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(resp["v"], RELAY_VERSION);
        assert_eq!(resp["resp"]["ok"], true);
    }

    #[test]
    fn the_exported_wire_helpers_match_what_the_client_sends() {
        // `request_line` / `parse_response_line` exist so an async caller (the relay binary's
        // self-dial watchdog) speaks the protocol without a second copy of the envelope. They are
        // only worth having while they stay byte-identical to what `RelayClient` puts on the wire.
        let line = request_line(&Request::Peek { handle: "h".into() }).unwrap();
        assert_eq!(
            line,
            "{\"v\":1,\"req\":{\"op\":\"peek\",\"handle\":\"h\"}}\n"
        );

        let addr = RelayServer::spawn("127.0.0.1:0").unwrap();
        let raw = tcp_round_trip(&addr.to_string(), &line).unwrap();
        assert!(parse_response_line(&raw).unwrap().ok);

        // And a relay speaking a version we don't is rejected rather than misparsed.
        let future = format!("{{\"v\":{},\"resp\":{{\"ok\":true}}}}", RELAY_VERSION + 1);
        let err = parse_response_line(&future).unwrap_err().to_string();
        assert!(err.contains("unsupported relay version"), "{err}");
    }

    #[test]
    fn a_wrong_version_request_is_refused() {
        let addr = RelayServer::spawn("127.0.0.1:0").unwrap();
        let bad = serde_json::json!({
            "v": RELAY_VERSION + 1,
            "req": { "op": "peek", "handle": "h" },
        });
        let mut line = bad.to_string();
        line.push('\n');
        let raw = tcp_round_trip(&addr.to_string(), &line).unwrap();
        let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(resp["resp"]["ok"], false);
        assert!(
            resp["resp"]["error"]
                .as_str()
                .unwrap()
                .contains("unsupported relay version"),
            "{resp}"
        );
    }

    #[test]
    fn logger_observes_ops_with_metadata_only() {
        use std::sync::Mutex as StdMutex;
        let seen: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let logger: RelayLogger = Arc::new(move |ev: RelayEvent| {
            sink.lock().unwrap().push(ev.summary());
        });
        let addr = RelayServer::spawn_with("127.0.0.1:0", Some(logger)).unwrap();
        let client = RelayClient::new(addr.to_string());
        client
            .post("handle-xyz", b"ciphertext", Duration::from_secs(60))
            .unwrap();
        client.peek("handle-xyz").unwrap();

        let lines = seen.lock().unwrap();
        assert!(lines.iter().any(|l| l.starts_with("POST")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("PEEK")), "{lines:?}");
        // The flow-log never contains the blob bytes.
        assert!(!lines.iter().any(|l| l.contains("ciphertext")), "{lines:?}");
    }
}
