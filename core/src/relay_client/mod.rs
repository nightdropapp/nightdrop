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

/// Wall-clock time in unix seconds (for persistable blob expiry — `Instant` is monotonic and
/// cannot survive a restart).
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resource limits protecting the (in-memory) relay from floods (`TODO.md` #1). All are
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
        let max_blob_bytes = 150 * 1024 * 1024;
        Self {
            max_blob_bytes,
            max_mailbox_blobs: 256,
            max_mailbox_bytes: 256 * 1024 * 1024,
            // usize-safe on 32-bit targets too (the core builds for armeabi-v7a).
            max_total_bytes: 1024 * 1024 * 1024,
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
/// See `TODO.md` #2.
pub const RELAY_VERSION: u8 = 1;

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
}

impl StoreInner {
    /// Remove a whole mailbox, keeping the byte total in sync.
    fn drain_handle(&mut self, handle: &str) -> Vec<Stored> {
        let entries = self.map.remove(handle).unwrap_or_default();
        self.total_bytes -= entries.iter().map(|s| s.bytes.len()).sum::<usize>();
        entries
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

/// Serialize the live store and write it atomically (`tmp` + rename) to `path`.
fn save_store(inner: &StoreInner, path: &Path) -> bool {
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
    let Ok(json) = serde_json::to_vec(&persisted) else {
        return false;
    };
    // The relay binary creates its state directory during Tor setup, but queue persistence is
    // constructed first. Make the queue path self-sufficient so the first post cannot be lost
    // merely because that setup has not completed yet.
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let tmp = path.with_extension("tmp");
    let Ok(mut file) = std::fs::File::create(&tmp) else {
        return false;
    };
    if file.write_all(&json).is_err() || file.sync_all().is_err() {
        return false;
    }
    std::fs::rename(&tmp, path).is_ok()
}

/// Load a persisted store, dropping anything already past its wall-clock expiry and rebuilding
/// each blob's monotonic `Instant` expiry from the remaining TTL. Missing/corrupt file → empty.
fn load_store(path: &Path) -> StoreInner {
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
            inner.total_bytes += bytes.len();
            queue.push(Stored {
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

/// Flush a changed queue atomically. This is called at the end of every mutating relay request,
/// rather than on a timer: once `post` reports success the opaque mailbox is crash-recoverable.
fn flush_store(store: &Store, path: &Path) {
    let mut inner = store.lock().unwrap();
    if inner.dirty {
        // Retain dirty on an I/O failure so the next relay operation retries persistence.
        inner.dirty = !save_store(&inner, path);
    }
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
        let store: Store = Arc::new(Mutex::new(load_store(&persist_path)));
        spawn_reaper(Arc::clone(&store), logger.clone());
        Self {
            store,
            logger,
            limits,
            directory: None,
            persist_path: Some(persist_path),
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
                    flush_store(&self.store, path);
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
        let inner = self.store.lock().unwrap();
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
        let mut inner = store.lock().unwrap();
        let mut reaped: Vec<(String, usize, usize)> = Vec::new();
        let mut freed = 0usize;
        for (handle, queue) in inner.map.iter_mut() {
            let before = queue.len();
            queue.retain(|s| {
                let keep = s.expiry > now;
                if !keep {
                    freed += s.bytes.len();
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
    let mut inner = store.lock().unwrap();
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

            // Resource limits (reject-new; see [`RelayLimits`]). Checked before storing
            // so a flood can neither OOM the relay nor evict queued mail.
            let reject = if bytes.len() > limits.max_blob_bytes {
                Some("blob too large")
            } else if inner.total_bytes + bytes.len() > limits.max_total_bytes {
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
            inner.total_bytes += bytes.len();
            inner.map.entry(handle.clone()).or_default().push(Stored {
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
            let entries = inner.drain_handle(&handle);
            inner.dirty |= !entries.is_empty();
            let items: Vec<(String, String)> = entries
                .into_iter()
                .filter(|s| s.expiry > now)
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
        Request::Take { handle } => {
            let entries = inner.drain_handle(&handle);
            inner.dirty |= !entries.is_empty();
            let blobs: Vec<String> = entries
                .into_iter()
                .filter(|s| s.expiry > now)
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
                        freed += s.bytes.len();
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
    Dialer(RelayDialer),
}

impl std::fmt::Debug for RelayClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            RelayInner::Tcp(addr) => write!(f, "RelayClient(tcp:{addr})"),
            RelayInner::Dialer(_) => write!(f, "RelayClient(tor)"),
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
            inner: RelayInner::Dialer(dialer),
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
        let mut line = serde_json::to_string(&RequestLineRef {
            v: RELAY_VERSION,
            req: request,
        })?;
        line.push('\n');
        let response_line = match &self.inner {
            RelayInner::Tcp(addr) => tcp_round_trip(addr, &line)?,
            RelayInner::Dialer(dial) => dial(&line)?,
        };
        let rl: ResponseLine = serde_json::from_str(&response_line)?;
        if rl.v != RELAY_VERSION {
            anyhow::bail!(
                "unsupported relay version {} (client speaks v{RELAY_VERSION})",
                rl.v
            );
        }
        Ok(rl.resp)
    }
}

/// One newline-JSON request/response over plain TCP (tests / local).
fn tcp_round_trip(addr: &str, line: &str) -> Result<String> {
    let stream = TcpStream::connect(addr)?;
    let mut writer = stream.try_clone()?;
    writer.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader.read_line(&mut response_line)?;
    Ok(response_line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
            }],
        );
        inner.total_bytes = 9 + 7;

        save_store(&inner, &path);
        let loaded = load_store(&path);

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
            loaded.total_bytes, 9,
            "byte total counts only the live blob"
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
            max_total_bytes: 20,
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

        // Global cap: filling other mailboxes stops before OOM (total is 6B, cap 20B).
        client.post("other1", b"12345678", ttl).unwrap(); // total 14
        assert!(
            client.post("other2", b"12345678", ttl).is_err(),
            "relay full"
        );

        // Draining frees budget: after a fetch the same post succeeds.
        client.fetch("other1").unwrap();
        client.post("other2", b"12345678", ttl).unwrap();
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
