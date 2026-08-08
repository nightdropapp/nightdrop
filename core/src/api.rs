//! High-level surface — the only boundary the Flutter app sees (`ARCHITECTURE.md` §3).
//! This is what `flutter_rust_bridge` mirrors into Dart; keep it small and coarse, and
//! keep the DTOs in sync with `app/lib/src/core/models.dart`.
//!
//! [`NightdropCore`] wraps a real [`Node`](crate::node::Node) (genuine identity, Olm
//! sessions, framed messaging over a [`Transport`](crate::transport)). The cryptography
//! and the wire protocol are real. Production cores run on a real transport (Tor/LAN/TCP)
//! and hold `demo: None`. For the app's zero-config dev path and the unit tests there is an
//! optional in-process fallback — see [`demo::Demo`] — that stands up auto-replying peers
//! over a [`MemoryNetwork`]; it lives entirely in that module, so the shipped core carries no
//! demo branches beyond a single `Option<Demo>`. Swapping the transport for Tor + relay does
//! not change this surface.

mod demo;
use demo::Demo;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rand::seq::SliceRandom;
use rand::Rng;

use crate::frb_generated::StreamSink;
use crate::identity::{LocalIdentity, PreKeyBundle};
use crate::lifecycle::{ExitGuard, StopSignal};
use crate::node::{drain_relay_mailboxes, Node, RelayHarvest};
use crate::relay_client::{RelayClient, RelayServer};
use crate::transport::{MemoryNetwork, Transport};
use crate::Result;

/// A push event from the core to the UI (flutter_rust_bridge stream). The real transport
/// uses this to surface unsolicited messages and incoming requests; the UI reacts by
/// re-reading state. `kind` is one of "request", "message", "contacts".
///
/// `contacts` names the chats whose **message history** actually changed, so the UI can re-pull
/// only those instead of every conversation (§1.5.5). Empty means "unknown / refresh broadly" —
/// e.g. roster changes (the contact/request lists are cheap to re-read) or control-frame churn
/// whose affected chat isn't individually tracked.
#[derive(Clone, Debug)]
pub struct AppEvent {
    pub kind: String,
    pub contacts: Vec<String>,
    /// Set only on `update_progress`. `None` on every other event, which is most of them.
    pub progress: Option<TransferProgress>,
}

/// Bytes moved so far by a long-running transfer, for a progress indicator.
///
/// `total` is what the server claimed and may be absent, so the UI must be able to show progress
/// without it. It is advisory in the strong sense: nothing decides a transfer is finished or
/// correct from it — the published SHA-256 does that.
#[derive(Clone, Debug)]
pub struct TransferProgress {
    pub done: u64,
    pub total: Option<u64>,
}

/// The UI event sink, kept OUTSIDE the `NightdropCore` opaque on purpose: a streaming
/// function holds the opaque's lock for the stream's whole lifetime, which would deadlock
/// every other `NightdropCore` call. A module-global keeps `subscribe` cheap and lock-free
/// w.r.t. the core.
static EVENTS: Mutex<Option<StreamSink<AppEvent>>> = Mutex::new(None);

/// When true, the background poller slows down (app is backgrounded). Set via
/// [`NightdropCore::set_background`].
static BACKGROUND: AtomicBool = AtomicBool::new(false);

/// One-shot "poll the relay on the next tick" signal, set when the app returns to the
/// foreground so queued offline messages appear immediately (fetch-on-open, §11.3)
/// without keeping the steady-state relay cadence hot.
static RELAY_POLL_NOW: AtomicBool = AtomicBool::new(true);

/// How often the poller does a relay round-trip (a full Tor circuit exchange — the
/// expensive part of the loop; the transport pump itself is a cheap local check).
/// Online messages arrive over the direct Tor stream regardless, so the relay poll only
/// bounds how quickly *offline/queued* mail shows up.
const RELAY_POLL_FOREGROUND: Duration = Duration::from_secs(15);
const RELAY_POLL_BACKGROUND: Duration = Duration::from_secs(60);

/// While a short-code invite is outstanding, the inviter polls the rendezvous this often so
/// answering a joiner's SPAKE2 opener feels near-instant (pairing is a brief, attended flow).
const RELAY_POLL_PAIRING: Duration = Duration::from_secs(2);

/// How long [`NightdropCore::shutdown`] waits for the poller thread to actually exit before giving
/// up and returning anyway.
///
/// Sized from measurement, not from the poll loop: waking the poller and getting it out of a relay
/// round-trip takes milliseconds, but the exit then drops the **last** `Arc<Runtime>`, and shutting
/// that runtime down unwinds every arti task on it — which is precisely the work that releases the
/// on-disk state lock. `tor_smoke`'s guard-heal test measures ~3.2 s end to end (debug build,
/// desktop, right after a bootstrap); a phone can be slower. An earlier 3 s bound expired 200 ms
/// short of the real thing, which reported a false failure and, in the field, would have let a
/// rebuild race the lock — so this leaves real headroom.
///
/// It is *not* sized to the duress wipe. That path caps its own wait on the Dart side, because the
/// part the wipe needs (the poller stopped, its last save done) is over long before the arti
/// teardown finishes, and a coerced user must not watch a spinner for it.
const POLLER_EXIT_TIMEOUT: Duration = Duration::from_secs(8);

/// How long [`NightdropCore::shutdown`] will try for the core lock before proceeding without it.
///
/// Short on purpose. The lock is held for the length of a poller tick, and a tick that is dialling
/// a dead peer or a dead relay can run for tens of seconds — waiting that out is exactly the hang
/// this bound exists to prevent. Missing the early close only costs a late transport teardown; the
/// second attempt, after the poller has exited, almost always gets it.
const LOCK_TRY_TIMEOUT: Duration = Duration::from_millis(750);

/// Cover traffic (#4), opt-in and off by default. A process-wide flag like `BACKGROUND`: the poller
/// reads it every tick, so toggling it takes effect without restarting anything.
static COVER_TRAFFIC: AtomicBool = AtomicBool::new(false);

/// Mean gap between cover posts. Intervals are drawn from an **exponential** distribution around
/// this, not spaced evenly: a fixed cadence is its own fingerprint, and an observer can subtract
/// every post that lands on a 30-minute boundary and read what is left. Exponential is memoryless,
/// so the next post says nothing about the last.
const COVER_MEAN: Duration = Duration::from_secs(30 * 60);

/// Floor on the sampled interval. The tail of an exponential draws arbitrarily short gaps, and a
/// burst of them costs the user battery and the relay operator — who is usually a volunteer —
/// bandwidth, for no extra concealment.
const COVER_MIN: Duration = Duration::from_secs(5 * 60);

/// Draw the next cover interval: exponential with mean [`COVER_MEAN`], floored at [`COVER_MIN`].
fn next_cover_delay() -> Duration {
    // Inverse-transform sampling: -mean * ln(U), U in (0,1].
    let u: f64 = 1.0 - rand::random::<f64>(); // (0,1], never 0 -> never infinite
    let secs = -(COVER_MEAN.as_secs_f64()) * u.ln();
    Duration::from_secs_f64(secs.max(COVER_MIN.as_secs_f64()))
}

/// How long a staged short-code invite is honored, and how long a joiner waits for the
/// inviter to answer before giving up (§5b). Both sides are attended during pairing.
const SHORT_CODE_TTL: Duration = Duration::from_secs(600);
const SHORT_CODE_JOIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Subscribe to push events from the core (flutter_rust_bridge stream). Call once at
/// startup; the UI refreshes its view whenever an event arrives.
pub fn subscribe(sink: StreamSink<AppEvent>) {
    *EVENTS.lock().unwrap() = Some(sink);
}

/// Drop the event sink (closes the stream). Call on app/teardown so no port lingers.
pub fn unsubscribe() {
    *EVENTS.lock().unwrap() = None;
}

/// Turn on opt-in operational diagnostics (`crate::diag`) — off unless a debugging build asks
/// for it. These lines record protocol outcomes (which leg ran, how many relays answered), never
/// identity keys, onion addresses, invite codes, or names; the identity-linked `devlog!` lines
/// stay compiled out of release builds either way.
pub fn set_diagnostics(enabled: bool) {
    crate::diag::set_enabled(enabled);
    crate::diag!("diagnostics enabled");
}

/// Write one line to the diagnostics channel from the **app layer**.
///
/// Without this the Dart side is invisible in a field log: the core narrates what it does, while
/// the decisions *around* it — a heal declining to fire, a menu action taking an early return —
/// leave no trace at all, so a feature that silently does nothing looks identical to one that ran
/// and didn't help. Same rules as any other diagnostic: outcomes only, never keys, codes, names or
/// addresses (onion addresses are redacted here regardless), and silent unless diagnostics are on.
pub fn diag_note(line: String) {
    crate::diag!("{line}");
}

/// Delete arti's entry-guard state under `state_dir` — but NOT the onion keystore, so the device
/// keeps its stable `.onion`. The next Tor bootstrap then picks fresh entry guards. This is the
/// recovery for a **wedged guard set** (guards that have churned out of the network): a client
/// stuck on them can neither publish its own onion nor reach the relay, and a plain re-bootstrap
/// reuses the same guards, so it can't recover on its own (§6). Call this with the core shut down,
/// then build a fresh core. No-op if the file is absent.
///
/// **`circuit_timeouts.json` is deliberately left alone.** It holds arti's *learned* circuit-build
/// time distribution, which has nothing to do with which guards we use; deleting it dropped the
/// replacement client onto conservative defaults until it re-measured, so a reset made the next
/// several minutes slower — the opposite of the intent, and precisely when the network was already
/// bad. It was being deleted only because it sits in the same directory.
///
/// This is a **last resort**. Entry guards exist to bound the chance that a hostile relay ever
/// becomes our entry, so they are meant to be sticky for weeks; C-tor keeps one for months.
/// Rotating them spends real anonymity margin, and anything that can cause unreachability can
/// force rotation. arti already recovers from a single unreachable guard on its own — measured
/// 2026-08-03: it added a fresh guard to the sample 0.7 s after the failure and had it usable 79 s
/// later, without discarding the persisted set. Only reach for this when the client is stuck in a
/// way that persists (see [`NightdropCore::tor_client_wedged`]).
pub fn reset_tor_guards(state_dir: String) {
    let dir = std::path::Path::new(&state_dir)
        .join("arti-state")
        .join("state");
    let _ = std::fs::remove_file(dir.join("guards.json"));
    crate::diag!("tor: reset entry-guard state (kept onion identity and circuit timings)");
}

fn emit(kind: &str) {
    emit_chats(kind, Vec::new());
}

/// Emit an event naming the chats whose message history changed (§1.5.5), so the UI re-pulls
/// only those. An empty `contacts` means "refresh broadly" (see [`AppEvent`]).
fn emit_chats(kind: &str, contacts: Vec<String>) {
    // Recover from poisoning (§1.5.3): a panicked emit must not wedge every future one.
    let guard = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(sink) = guard.as_ref() {
        let _ = sink.add(AppEvent {
            kind: kind.to_string(),
            contacts,
            progress: None,
        });
    }
}

/// Report how far a running download has got, so the UI can show a real progress bar rather than
/// a spinner that says nothing for two minutes.
///
/// Deliberately carries no contacts: the Dart side must route this **away** from its roster/history
/// refresh, which reloads every chat and would otherwise run on every tick of a download.
fn emit_progress(done: u64, total: Option<u64>) {
    let guard = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(sink) = guard.as_ref() {
        let _ = sink.add(AppEvent {
            kind: "update_progress".to_string(),
            contacts: Vec::new(),
            progress: Some(TransferProgress { done, total }),
        });
    }
}

/// An anonymous, device-held identity handle (just its public id for the UI).
#[derive(Clone, Debug)]
pub struct Identity {
    pub id: String,
}

/// A pairing invite shown to the other person (§5). `short_code` is the Wormhole-style
/// `slot-secret-words`; `qr_payload` embeds the inviter's address + a real pre-key bundle.
#[derive(Clone, Debug)]
pub struct PairingInvite {
    pub short_code: String,
    pub qr_payload: String,
}

/// The result of enabling an opt-in server backup (§7c). `password` is the randomly generated
/// recovery secret — shown **once**, never persisted; it is the only way to restore, and the
/// relay never receives it (it holds an opaque blob addressed by a password-derived handle).
/// `expires_at_secs` is the exact Unix time the server copy is dropped, for the acknowledgment
/// screen the invariant requires.
#[derive(Clone, Debug)]
pub struct ServerBackupInfo {
    pub password: String,
    pub expires_at_secs: u64,
}

/// A 1:1 conversation partner. Names default to "Anon" and are per-chat (§4).
#[derive(Clone, Debug)]
pub struct Contact {
    pub id: String,
    pub their_name: String,
    pub my_name: String,
    pub remote_storage: bool,
    /// Per-chat disappearing-messages timer in seconds (0 = off). A shared setting: changing
    /// it is synced to the peer so both devices delete messages older than this horizon.
    pub disappearing_secs: u64,
    /// Whether **we** have this chat in a backup (#7). Drives the logout signal: an un-backed
    /// chat's peer is told the chat is closed on identity deletion (§11.6). UI can badge it.
    pub backed_up: bool,
    /// Whether the **peer** told us they keep a (Full) backup of this chat (#7) — a transparency
    /// signal; the UI shows a persistent warning that our messages may persist in their backup.
    pub peer_backed_up: bool,
    /// Whether the user has **verified the safety number** for this contact out-of-band
    /// (key-verification design). Per-identity-key, so a re-paired contact starts unverified.
    pub verified: bool,
    /// Whether the **peer** told us *they* verified this chat's safety number — **informational
    /// only**: the UI shows "the other person marked this verified", but it never sets our own
    /// `verified`. Each side must still confirm the number itself, so a compromised device can't
    /// forge a verified badge on the other. Resets on a re-pair (new session), like `verified`.
    pub peer_verified: bool,
    /// Whether the **peer's** device cannot tell them about screenshots (#1): `Some(true)` means a
    /// capture on their side raises no notice, so their silence proves nothing about whether what
    /// you send has been captured.
    ///
    /// `None` means they have not said — an older build, or a chat that predates the signal. The UI
    /// must render that as unknown and NOT as "captures are visible": inferring the reassuring
    /// answer from silence is precisely the false guarantee this exists to remove.
    pub peer_captures_silent: Option<bool>,
    /// The peer's advertised **extra** relay addresses (#17): where their mailbox also lives, so
    /// our offline mail to them is fanned out redundantly. The shared primary relay is implicit.
    pub peer_relays: Vec<String>,
    /// Whether opt-in server storage (§6) is actually working: `false` when it is enabled but the
    /// last send couldn't reach any relay to store the copy (the message still reached the peer
    /// directly). Lets the UI downgrade the storage banner to "not currently stored" instead of
    /// implying a server copy exists. Always `true` when server storage is off. Ephemeral.
    pub remote_storage_healthy: bool,
    /// Unix seconds of the last **authenticated** contact from this peer — a message we decrypted
    /// or a control frame that verified on their ratchet, including the silent delivery `Ack` that
    /// says their device drained our mailbox. `0` when we have no reading yet.
    ///
    /// A nickname **you** gave this contact, or empty. Local only: it is never sent, never
    /// announced, and never leaves the device — only you know that this key is the person you met,
    /// and the peer cannot supply that knowledge. Takes precedence over `their_name` in the UI.
    pub local_name: String,
    /// Six characters derived from this contact's identity key, to tell two unnamed contacts apart
    /// (`docs/design/contact-naming.md`). **Not verification** — it is short enough to grind, so a
    /// matching tag proves nothing and the UI must never let it look like the safety number does.
    /// Derived rather than random so it *changes* when the identity does.
    pub identity_tag: String,
    /// Drives the "no sign of them" notice. It reports **silence, not a cause**: a wiped identity,
    /// a seized phone, a lost phone and a flat battery all look the same from here, and the UI must
    /// not imply otherwise. That ambiguity is deliberate — see `docs/design/silence-detection.md`.
    pub last_seen_secs: u64,
}

/// Reachability of one of our advertised extra relays (#17), for the UI's relay-status surface.
#[derive(Clone, Debug)]
pub struct RelayHealth {
    /// The relay address we advertise (as entered by the user; `.onion` or host:port).
    pub address: String,
    /// Whether it answered our mailbox drain on the last poll. `false` = likely offline.
    pub reachable: bool,
}

/// Result of the update check (`crate::update`), for the UI's "a newer release exists" notice.
#[derive(Clone, Debug)]
pub struct AppUpdate {
    /// The version this build reports itself as.
    pub current: String,
    /// The version our onion site publishes.
    pub latest: String,
    /// Whether `latest` is strictly newer. `false` means say nothing at all.
    pub update_available: bool,
}

/// One message in a conversation (UI-facing).
#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub from_me: bool,
    pub text: String,
    /// A local system notice (deletion/approval) rather than an exchanged message. The UI
    /// renders these centered, without a sender name or bubble side.
    pub system: bool,
    /// Message kind: "text" (default), "image", or "video". For media, `text` is empty and
    /// the bytes live in a sealed file fetched via [`media_bytes`](NightdropCore::media_bytes).
    pub kind: String,
    /// MIME type for a media message (e.g. "image/png", "video/mp4"); empty for text.
    pub mime: String,
    /// Opaque id of the sealed media file for this message; empty for text. For a video
    /// still in transit this is empty (the bytes haven't arrived yet) — see `thumb_id`.
    pub media_id: String,
    /// Size of the media in bytes (for display); 0 for text.
    pub media_size: u64,
    /// Correlates a `MediaIncoming` placeholder with its later `Media` payload.
    pub transfer_id: String,
    /// Sealed-file id of a small preview thumbnail (videos); empty if none. While a video is
    /// in transit, the UI shows this with a spinner until `media_id` is filled.
    pub thumb_id: String,
    /// Delivery state of an outgoing (`from_me`) message:
    ///
    /// * `""` — n/a (incoming or system).
    /// * `"sent"` — handed to the peer's onion, which answered. **Not** a claim that their device
    ///   has it: the frame can still be lost there. Transient — if no receipt names it within
    ///   `RECEIPT_TIMEOUT` the core puts a relay copy behind it and it becomes `"queued"`.
    /// * `"queued"` — held on a relay until they collect it.
    /// * `"delivered"` — **only** state that means arrival, and only ever set by a
    ///   [`Frame::Delivered`](crate::wire::Frame::Delivered) naming this message. A dial
    ///   succeeding, a message arriving from them, and their relay `Ack` all used to set it; each
    ///   means "they are alive", which is not the same thing and was wrong often enough to lose a
    ///   message in the field (2026-08-02).
    /// * `"expired"` — sat queued past the relay's 24h TTL uncollected (§11.3).
    pub delivery: String,
    /// Random per-message token shared by both sides (rides inside the wire frame), so an
    /// [`edit_message`](NightdropCore::edit_message) can name its target. Empty for system
    /// notices and messages that predate editing.
    pub msg_id: String,
    /// True once the text was replaced by a sender edit — the UI shows an "edited" tag.
    pub edited: bool,
    /// Unix timestamp (seconds) when this message was created locally (sent) or received.
    /// Drives the edit window, the "expired" badge, and the ephemeral-chat time-bomb.
    /// 0 for messages persisted before timestamps existed (they never expire/edit).
    pub at: u64,
}

/// Unix time in seconds (message timestamps).
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl ChatMessage {
    /// A plain text message with a fresh timestamp; `msg_id` correlates edits.
    pub(crate) fn text(from_me: bool, text: String, msg_id: String) -> Self {
        Self {
            from_me,
            text,
            system: false,
            kind: "text".to_string(),
            mime: String::new(),
            media_id: String::new(),
            media_size: 0,
            transfer_id: String::new(),
            thumb_id: String::new(),
            delivery: String::new(),
            msg_id,
            edited: false,
            at: now_secs(),
        }
    }

    /// A local system notice (deletion/approval/etc), rendered centered.
    pub(crate) fn system(text: String) -> Self {
        Self {
            from_me: false,
            text,
            system: true,
            kind: "text".to_string(),
            mime: String::new(),
            media_id: String::new(),
            media_size: 0,
            transfer_id: String::new(),
            thumb_id: String::new(),
            delivery: String::new(),
            msg_id: String::new(),
            edited: false,
            at: now_secs(),
        }
    }

    /// A system notice tagged with a `kind` marker so it can be located and removed later —
    /// e.g. the "awaiting approval" hint (cleared on approval or the first received message) and
    /// the "approved" notice (cleared on the first received message). The UI renders any `system`
    /// message the same way (centered, via `_SystemNotice`), ignoring `kind`.
    pub(crate) fn system_tagged(text: String, kind: &str) -> Self {
        let mut m = Self::system(text);
        m.kind = kind.to_string();
        m
    }

    /// A media (image/video) message; bytes live in sealed files named `media_id`/`thumb_id`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn media(
        from_me: bool,
        kind: String,
        mime: String,
        media_id: String,
        media_size: u64,
        transfer_id: String,
        thumb_id: String,
    ) -> Self {
        Self {
            from_me,
            text: String::new(),
            system: false,
            kind,
            mime,
            media_id,
            media_size,
            transfer_id,
            thumb_id,
            delivery: String::new(),
            msg_id: String::new(),
            edited: false,
            at: now_secs(),
        }
    }
}

/// Internal mutable state, shared with the background poller in real mode.
struct Inner {
    me: Node,
    /// The in-process demo harness (auto-replying peers over an in-memory network), or `None`
    /// for a real-transport core. All demo behaviour is gated on this being `Some`.
    demo: Option<Demo>,
    /// A backup blob created by [`create_backup`](NightdropCore::create_backup) and waiting for
    /// the user to choose a save location (so the password is acknowledged first, §7).
    pending_backup: Option<Vec<u8>>,
    /// When set, the device state is auto-saved to this encrypted file after every change,
    /// so identity + chats survive an app restart. The key is held by the OS secure store
    /// on the Dart side; here it only encrypts the at-rest blob.
    persist: Option<Persist>,
}

/// Encrypted at-rest persistence target plus the debounce bookkeeping (§1.5.4). Each write
/// re-serializes the whole state (`Node::export`) and atomically replaces the file (§1.5.1), so
/// it is O(total history). User-initiated mutations still write synchronously (durability), but the
/// high-frequency **background** churn — delivery acks, "delivered" flips, renames, sweeps arriving
/// on the poller — is coalesced: at most one write per [`PERSIST_DEBOUNCE`], with a `pending` flag
/// the poller flushes once the window elapses. New *messages* and roster changes bypass the debounce.
struct Persist {
    path: String,
    /// The at-rest key (from the OS keystore). Held for the process lifetime so the state file can
    /// be re-sealed on every change; [zeroized on drop](Persist::drop) so it doesn't linger in freed
    /// memory after logout/shutdown. (Transient by-value copies of this `Copy` array — e.g. the one
    /// [`save`](Inner::save) hands to `save_to_file` — are short-lived stack values we can't all
    /// wipe; this covers the long-lived holder, which is the meaningful exposure.)
    key: crate::storage::StoreKey,
    /// When we last actually wrote to disk (initialized in the past so the first save writes now).
    last_write: std::time::Instant,
    /// A debounced background change is waiting to be flushed once the window elapses.
    pending: bool,
}

impl Persist {
    fn new(path: String, key: crate::storage::StoreKey) -> Self {
        Self {
            path,
            key,
            last_write: std::time::Instant::now() - PERSIST_DEBOUNCE,
            pending: false,
        }
    }
}

impl Drop for Persist {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.key.zeroize();
    }
}

/// Coalescing window for high-frequency background persistence (§1.5.4).
const PERSIST_DEBOUNCE: Duration = Duration::from_secs(3);

impl Inner {
    /// Persist the full device state to the encrypted file **now**, if persistence is enabled.
    /// Best-effort: a write failure is logged, never propagated into a UI action. Used for
    /// user-initiated mutations and message/roster changes (durability); background churn goes
    /// through [`save_soon`](Self::save_soon).
    fn save(&mut self) {
        let Some((path, key)) = self.persist.as_ref().map(|p| (p.path.clone(), p.key)) else {
            return;
        };
        let state = self.me.export(&key);
        if let Err(e) = crate::storage::save_to_file(&path, &key, &state) {
            eprintln!("[nightdrop] persist: save to {path} failed: {e}");
        }
        if let Some(p) = self.persist.as_mut() {
            p.last_write = std::time::Instant::now();
            p.pending = false;
        }
    }

    /// Debounced persistence for high-frequency **background** changes (§1.5.4): write immediately
    /// if the last write was more than [`PERSIST_DEBOUNCE`] ago, otherwise just mark a pending write
    /// for the poller to flush later ([`maybe_flush`](Self::maybe_flush)). Coalesces a burst of acks
    /// / delivery flips into a single write instead of one O(history) rewrite each.
    fn save_soon(&mut self) {
        let Some(p) = self.persist.as_ref() else {
            return;
        };
        if p.last_write.elapsed() >= PERSIST_DEBOUNCE {
            self.save();
        } else if let Some(p) = self.persist.as_mut() {
            p.pending = true;
        }
    }

    /// Flush a pending debounced write once its window has elapsed (called by the poller each tick).
    fn maybe_flush(&mut self) {
        let due = self
            .persist
            .as_ref()
            .is_some_and(|p| p.pending && p.last_write.elapsed() >= PERSIST_DEBOUNCE);
        if due {
            self.save();
        }
    }

    /// One poll cycle: deliver inbound transport (and optionally relay) messages, let demo
    /// peers echo, and emit a "message" event if anything new arrived. This is the same
    /// code path whether driven synchronously (demo) or by the background poller (real).
    fn drive(&mut self, poll_relay: bool) -> Result<()> {
        // Synchronous callers (tests, the demo path) already hold the lock and do no Tor I/O, so
        // draining here is fine. The background poller instead drains **off** the lock and hands
        // us the harvest via `apply_tick` (§1.5.2).
        let harvest = if poll_relay {
            self.me
                .relay_drain_plan()
                .map(|plan| crate::node::drain_relay_mailboxes(&plan))
        } else {
            None
        };
        let sent = self
            .me
            .plan_pending_sends()
            .map(|plan| crate::node::execute_sends(&plan));
        self.apply_tick(poll_relay, harvest, sent)
    }

    /// The state-mutating half of a poll cycle: pump the transport, apply any relay blobs already
    /// drained (off-lock) by the poller, let demo peers echo, and emit/persist on change. Runs
    /// under the core lock; the blocking relay **reads** happened before this (§1.5.2). When
    /// `relay_due` is set, also service short-code pairing and the time-sweep.
    fn apply_tick(
        &mut self,
        relay_due: bool,
        harvest: Option<RelayHarvest>,
        sent: Option<crate::node::SendOutcomes>,
    ) -> Result<()> {
        // Counts only (no contact clones): this runs on every poller tick (~80ms).
        let before_requests = self.me.pending_count();
        let before_contacts = self.me.contact_count();

        // Track whether *durability-critical* content arrived — new messages, or a roster change
        // (a new pending request / contact). These bypass the persistence debounce (§1.5.4); the
        // rest of the churn (acks, "delivered" flips, renames, sweeps) is coalesced. Also collect
        // the chats that gained messages so the UI re-pulls only those (§1.5.5).
        let mut affected: Vec<String> = Vec::new();
        for (id, _) in self.me.pump()? {
            if !affected.contains(&id) {
                affected.push(id);
            }
        }
        if let Some(harvest) = harvest {
            for (id, _) in self.me.apply_relay_harvest(harvest)? {
                if !affected.contains(&id) {
                    affected.push(id);
                }
            }
        }
        // Messages composed since the last tick (§6). The dialling already happened **without this
        // lock** (see `plan_pending_sends`/`execute_sends`); all that is left here is recording
        // what it found. Doing the dial inline is what used to hold the lock for minutes on a
        // device whose circuits were timing out.
        if let Some(sent) = sent {
            for id in self.me.apply_send_outcomes(sent) {
                if !affected.contains(&id) {
                    affected.push(id);
                }
            }
        }
        let mut messages_arrived = !affected.is_empty();
        if relay_due {
            // Inviter side of short-code pairing: answer any joiner's SPAKE2 opener (§5b).
            self.me.service_pending_invites();
            // Retry messages that couldn't reach the peer or any relay when first sent (arti was
            // cold): now that Tor has had time to warm, re-queue them so they finally deliver.
            for id in self.me.flush_pending_relay() {
                if !affected.contains(&id) {
                    affected.push(id);
                }
                messages_arrived = true;
            }
            // Retry any chat-delete Closed signal (§11.6) that couldn't reach the peer or a relay
            // when the chat was deleted (arti cold / relay briefly down), so it isn't lost.
            self.me.flush_pending_control();
            // Adopt a newer operator-signed relay directory if any relay serves one (§3.1 —
            // relay rotation without an app update). Its dirty flag drives persistence below.
            self.me.refresh_directory();
            // Put a relay copy behind any message the peer's onion accepted but never receipted:
            // a successful dial is not delivery, and a frame lost to a torn-down core would
            // otherwise never be asked for again.
            for id in self.me.sweep_unconfirmed() {
                if !affected.contains(&id) {
                    affected.push(id);
                }
                messages_arrived = true;
            }
            // Same cadence: flip long-queued messages to "expired" and run the ephemeral
            // time-bomb (§11.3/§11.4). Reported via the dirty flag below.
            self.me.sweep_time();
        }
        let mut changed = messages_arrived;
        // Demo mode only: let the in-process peers echo, then pump again to pick the
        // echoes up. In real mode there is no demo harness and the first pump drained
        // everything (anything arriving mid-drive is caught by the next tick).
        if let Some(demo) = self.demo.as_mut() {
            let echoed = demo.echo_tick(&mut self.me)?;
            messages_arrived |= echoed;
            changed |= echoed;
        }
        // Silent control frames (delivery acks, renames, deletions) mutate state without
        // producing a "received message"; refresh on those too.
        changed |= self.me.take_dirty();
        // An inbound `Hello` creates a pending request (no "received message"), so also
        // refresh the UI when the request/contact counts change.
        let roster_changed = self.me.pending_count() != before_requests
            || self.me.contact_count() != before_contacts;
        changed |= roster_changed;

        if changed {
            // New messages / roster changes persist immediately; background-only churn debounces.
            if messages_arrived || roster_changed {
                self.save();
            } else {
                self.save_soon();
            }
            // Name the changed chats so the UI re-pulls only those (§1.5.5). Empty when only
            // control-frame churn / roster changed → the UI refreshes broadly (cheap list re-read).
            emit_chats("message", std::mem::take(&mut affected));
        }
        Ok(())
    }
}

/// The application core. State lives behind a mutex so a background poller can drive the
/// live (real-transport) flow concurrently with UI calls.
///
/// Explicitly opaque: flutter_rust_bridge exposes its methods but never inspects fields.
#[flutter_rust_bridge::frb(opaque)]
pub struct NightdropCore {
    inner: Arc<Mutex<Inner>>,
    /// Set while a background poller thread is running (real mode). Stopping it is not enough —
    /// [`shutdown`](NightdropCore::shutdown) waits for it to actually exit.
    poller: Option<Arc<StopSignal>>,
}

impl Default for NightdropCore {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NightdropCore {
    fn drop(&mut self) {
        // Ask the poller to stop, but never *wait* here: a drop can run on any thread (Dart frees
        // the core from a finalizer) and must not block. Anything that needs the poller to be gone
        // before it continues — anything rebuilding a core over the same Tor state dir — calls
        // `shutdown()`, which does wait.
        if let Some(poller) = &self.poller {
            poller.stop();
        }
    }
}

impl NightdropCore {
    /// Demo core: a fresh identity on an in-memory network with auto-replying peers and
    /// **no** background poller (so messaging is synchronous and flutter_rust_bridge
    /// stream tests don't see continuous events). This is what the app uses today.
    pub fn new() -> Self {
        let (demo, transport) = Demo::new();
        let mut me = Node::new(transport);
        me.set_require_authorization(true); // approve strangers first (§5)
                                            // Local relay so the opt-in 24h server-storage toggle works in the demo.
        if let Ok(addr) = RelayServer::spawn("127.0.0.1:0") {
            me.set_relay(RelayClient::new(addr.to_string()));
        }
        let inner = Inner {
            me,
            demo: Some(demo),
            pending_backup: None,
            persist: None,
        };
        Self {
            inner: Arc::new(Mutex::new(inner)),
            poller: None,
        }
    }

    /// Real core: run on an injected transport (e.g. Tor) with an optional relay. Inbound
    /// messages arrive asynchronously, delivered by the poll loop and surfaced via the
    /// event stream. `background` spawns the poller thread; pass `false` in tests and call
    /// [`poll_once`](Self::poll_once) deterministically.
    #[flutter_rust_bridge::frb(ignore)]
    pub fn new_with_transport(
        transport: Box<dyn Transport>,
        relay: Option<RelayClient>,
        background: bool,
    ) -> Self {
        let mut me = Node::with_identity(LocalIdentity::generate(), transport);
        me.set_require_authorization(true);
        if let Some(relay) = relay {
            me.set_relay(relay);
        }
        let inner = Arc::new(Mutex::new(Inner {
            me,
            demo: None,
            pending_backup: None,
            persist: None,
        }));
        // `Some` only when a thread actually exists to signal its exit. Setting it regardless made
        // `shutdown()` wait out its whole timeout on a `background: false` core — nothing would
        // ever mark the exit — and then report the poller as stuck, which is a false sighting of
        // the very bug that wait exists to catch.
        let poller = background.then(|| {
            let signal = StopSignal::new();
            spawn_poller(Arc::clone(&inner), Arc::clone(&signal));
            signal
        });
        Self { inner, poller }
    }

    /// Restore a core from a password-encrypted backup file (§7, TODO #5). `listen_addr` +
    /// `relay_addr` select the real networked transport (as in [`new_networked`]); omit
    /// both for the in-process demo. Runs the background poll loop.
    pub fn restore_backup(
        path: String,
        password: String,
        listen_addr: Option<String>,
        relay_addr: Option<String>,
    ) -> Result<NightdropCore> {
        let blob = std::fs::read(&path)?;
        let (transport, relay): (Box<dyn Transport>, Option<RelayClient>) =
            match (listen_addr, relay_addr) {
                (Some(listen), Some(relay)) => (
                    Box::new(crate::transport::tcp::TcpTransport::bind(&listen)?),
                    Some(RelayClient::new(relay)),
                ),
                _ => (Box::new(MemoryNetwork::new().endpoint("me")), None),
            };
        let mut me = Node::restore_from_backup(&blob, &password, transport)?;
        me.set_require_authorization(true);
        if let Some(relay) = relay {
            me.set_relay(relay);
        }
        let inner = Arc::new(Mutex::new(Inner {
            me,
            demo: None,
            pending_backup: None,
            persist: None,
        }));
        let poller = StopSignal::new();
        spawn_poller(Arc::clone(&inner), Arc::clone(&poller));
        Ok(Self {
            inner,
            poller: Some(poller),
        })
    }

    /// Real networked core over plain TCP (for the desktop two-client demo / LAN). Binds a
    /// listener at `listen_addr` (e.g. `127.0.0.1:7001`) and uses the relay at `relay_addr`
    /// for rendezvous + offline delivery. Runs the background poll loop. (Production would
    /// use a Tor variant of this constructor.)
    pub fn new_networked(listen_addr: String, relay_addr: String) -> Result<NightdropCore> {
        let transport = crate::transport::tcp::TcpTransport::bind(&listen_addr)?;
        let relay = RelayClient::new(relay_addr);
        Ok(Self::new_with_transport(
            Box::new(transport),
            Some(relay),
            true,
        ))
    }

    /// Real core over the **local-network (LAN / Wi-Fi) transport** — a Briar-style offline path
    /// for when there is no internet, or Tor is blocked, but paired devices share a network (§6).
    /// Binds a listener on `port` (`0` = an ephemeral port) across all interfaces and advertises
    /// this machine's detected LAN IP, so a peer on the same network reaches us with no public
    /// address or port-forwarding. The canonical flow is QR pairing in the same room; frames then
    /// flow directly, end-to-end encrypted and fixed-size padded, over the LAN.
    ///
    /// `relay_addr` is optional (offline LANs usually have none). Persistence (`persist_path` +
    /// base64 `persist_key`) behaves exactly as in [`new_tor`](Self::new_tor): restore if the file
    /// exists, else create + save a fresh identity. Because a LAN address changes between runs, on
    /// restore we announce the new address to contacts in-band (§5c) so they can still reach us —
    /// pass a **fixed** `port` for a more stable address.
    ///
    /// Anonymity note: LAN traffic is **not** anonymized (a local observer sees two IPs talking);
    /// only the content is encrypted. This is a censorship/blackout fallback, not a Tor replacement.
    pub fn new_lan(
        port: u16,
        relay_addr: Option<String>,
        persist_path: Option<String>,
        persist_key: Option<String>,
    ) -> Result<NightdropCore> {
        let transport = crate::transport::lan::LanTransport::bind(port)?;
        let relay = relay_addr.map(RelayClient::new);

        let persist = match (persist_path, persist_key) {
            (Some(path), Some(key)) => Some((path, decode_store_key(&key)?)),
            _ => None,
        };
        let restore = persist
            .as_ref()
            .filter(|(path, _)| std::path::Path::new(path).exists());
        let mut me = match restore {
            Some((path, key)) => {
                let state = crate::storage::load_from_file(path, key)?;
                Node::restore(&state, Box::new(transport), key)?
            }
            None => Node::with_identity(LocalIdentity::generate(), Box::new(transport)),
        };
        me.set_require_authorization(true);
        if let Some(relay) = relay {
            me.set_relay(relay);
        }
        if let Some((path, key)) = &persist {
            let dir = std::path::Path::new(path)
                .parent()
                .map(|p| p.join("nightdrop-media"))
                .unwrap_or_else(|| std::path::PathBuf::from("nightdrop-media"));
            me.set_media_store(dir.to_string_lossy().into_owned(), *key);
        }
        me.announce_address_if_changed();

        let inner = Arc::new(Mutex::new(Inner {
            me,
            demo: None,
            pending_backup: None,
            persist: persist.map(|(p, k)| Persist::new(p, k)),
        }));
        inner.lock().unwrap().save();
        let poller = StopSignal::new();
        spawn_poller(Arc::clone(&inner), Arc::clone(&poller));
        Ok(Self {
            inner,
            poller: Some(poller),
        })
    }

    /// Real core over the **embedded Tor transport** (the production WAN path, §6): this
    /// device gets a reachable `.onion`, so two peers pair and converse over any network
    /// (LTE included) with no public IP or port-forwarding. Requires the crate built with
    /// `--features tor`; bootstrapping a circuit can take tens of seconds. QR pairing needs
    /// no relay (the QR carries the inviter's `.onion`); an optional `relay_addr` (itself
    /// reachable over Tor) would enable short-code rendezvous + offline delivery.
    /// `state_dir` is a writable base directory for Tor's state/cache — required on Android
    /// (pass the app's support directory); pass `None` on desktop for arti's defaults.
    ///
    /// When `persist_path` + `persist_key` (base64 32-byte key) are given, the device state
    /// is persisted to that encrypted file: if the file already exists it is **restored**
    /// (same identity + chats survive a restart); otherwise a fresh identity is created and
    /// saved. The key is held by the OS secure store on the Dart side. The Tor onion address
    /// itself persists separately via arti's `state_dir`.
    pub fn new_tor(
        state_dir: Option<String>,
        relay_addr: Option<String>,
        persist_path: Option<String>,
        persist_key: Option<String>,
    ) -> Result<NightdropCore> {
        #[cfg(feature = "tor")]
        {
            // The store key is needed BEFORE Tor starts now: the onion identity is sealed under it
            // and has to be in the keystore before the service launches, or arti generates a new
            // one and our address changes (`onion-key-at-rest.md` §4).
            let persist = match (persist_path, persist_key) {
                (Some(path), Some(key)) => Some((path, decode_store_key(&key)?)),
                _ => None,
            };
            // Only when an identity is actually being restored. Creating a NEW identity means a new
            // address, so a sealed key left on disk belongs to an identity being abandoned — and
            // reading it there is not merely pointless, it is a trap: it will not unseal under the
            // new store key, and `read_onion_key` fails hard on that (rightly — see its doc). The
            // failure then lands on the one path the user has left, so "set up a new identity" on
            // the load-error screen could not recover the app from inside. Found on a device,
            // 2026-08-02, after a wipe left the file behind. The stale file is overwritten below,
            // once the fresh identity has a key of its own to seal.
            // Restoring an identity, or creating one? The state file is the only signal — the same
            // one the node itself uses below — and it decides both whether a sealed onion key may
            // be read and whether arti's on-disk keystore may still speak for us.
            let restoring = persist
                .as_ref()
                .is_some_and(|(path, _)| std::path::Path::new(path).exists());
            let saved_onion_key = match (state_dir.as_deref(), persist.as_ref()) {
                (Some(dir), Some((_, key))) => onion_key_for_start(dir, key, restoring)?,
                _ => None,
            };
            if let Some(dir) = state_dir.as_deref() {
                drop_superseded_keystore(dir, saved_onion_key.is_some(), restoring);
            }
            let transport = crate::transport::tor::TorTransport::bootstrap(
                "nightdrop",
                state_dir.as_deref(),
                // Authorized-client keys for onion client authorization (#22) live under the
                // Tor state dir; empty/absent → a normal public onion.
                state_dir
                    .as_deref()
                    .map(|s| format!("{s}/client-auth"))
                    .as_deref(),
                saved_onion_key,
            )?;
            // First run: arti generated the identity, so capture it into our sealed store. Without
            // this the in-memory keystore forgets it and the next start is a different address.
            if saved_onion_key.is_none() {
                if let (Some(dir), Some((_, key))) = (state_dir.as_deref(), persist.as_ref()) {
                    let material = transport.onion_key_material().ok_or_else(|| {
                        anyhow::anyhow!("onion service started without an identity key")
                    })?;
                    write_onion_key(dir, key, &material)?;
                }
            }
            // Reach the relay over Tor: build a dialer from the transport's arti client before it
            // is moved into the node (a relay `.onion` can't be reached over plain TCP).
            let relay = relay_addr
                .map(|onion| RelayClient::with_dialer(transport.make_relay_dialer(onion)));
            // Restore from the existing file, or start a fresh identity.
            let restore = persist
                .as_ref()
                .filter(|(path, _)| std::path::Path::new(path).exists());
            let mut me = match restore {
                Some((path, key)) => {
                    let state = crate::storage::load_from_file(path, key)?;
                    Node::restore(&state, Box::new(transport), key)?
                }
                None => Node::with_identity(LocalIdentity::generate(), Box::new(transport)),
            };
            me.set_require_authorization(true);
            if let Some(sd) = &state_dir {
                me.set_tor_state_dir(sd.clone()); // so backups can fold in the onion keystore
            }
            if let Some(relay) = relay {
                me.set_relay(relay);
            }
            // Store attachments as sealed files in a sibling dir of the state file.
            if let Some((path, key)) = &persist {
                let dir = std::path::Path::new(path)
                    .parent()
                    .map(|p| p.join("nightdrop-media"))
                    .unwrap_or_else(|| std::path::PathBuf::from("nightdrop-media"));
                me.set_media_store(dir.to_string_lossy().into_owned(), *key);
            }
            // If our onion changed since last run (rebuilt keystore), tell contacts the new
            // address in-band so they can still reach us (#11). No-op on first run / no change.
            me.announce_address_if_changed();
            // The keystore is in memory now, so the per-peer client keys have to be put back
            // before any connection is attempted (`onion-key-at-rest.md`).
            me.restore_client_keys();

            let inner = Arc::new(Mutex::new(Inner {
                me,
                demo: None,
                pending_backup: None,
                persist: persist.map(|(p, k)| Persist::new(p, k)),
            }));
            inner.lock().unwrap().save(); // create the file on first run
            let poller = StopSignal::new();
            spawn_poller(Arc::clone(&inner), Arc::clone(&poller));
            Ok(Self {
                inner,
                poller: Some(poller),
            })
        }
        #[cfg(not(feature = "tor"))]
        {
            let _ = (state_dir, relay_addr, persist_path, persist_key);
            anyhow::bail!(
                "this build was compiled without Tor support (rebuild with --features tor)"
            )
        }
    }

    /// Import a password-encrypted backup onto the **Tor transport** with persistence
    /// (§7 / logout-recovery): decrypt the backup, re-encrypt it as the at-rest state file
    /// under `persist_key`, then bootstrap Tor and restore from it (so it also survives
    /// future restarts). This is the Tor counterpart of [`restore_backup`](Self::restore_backup).
    pub fn restore_backup_tor(
        backup_path: String,
        password: String,
        state_dir: Option<String>,
        relay_addr: Option<String>,
        persist_path: String,
        persist_key: String,
    ) -> Result<NightdropCore> {
        #[cfg(feature = "tor")]
        {
            let blob = std::fs::read(&backup_path)?;
            // Decrypt the backup FIRST (password-derived key). This also lets us restore the
            // onion keystore onto disk *before* Tor bootstraps, so the device comes back up on
            // the SAME `.onion` — otherwise the peer's stored address is stale and unreachable.
            let (state, bkey) = Node::open_backup(&blob, &password)?;
            if let Some(sd) = &state_dir {
                Node::write_onion_keys(&state.onion_keys, sd)?;
            }
            let transport = crate::transport::tor::TorTransport::bootstrap(
                "nightdrop",
                state_dir.as_deref(),
                // Authorized-client keys for onion client authorization (#22) live under the
                // Tor state dir; empty/absent → a normal public onion.
                state_dir
                    .as_deref()
                    .map(|s| format!("{s}/client-auth"))
                    .as_deref(),
                // Restore paths write the backup's keystore files to disk just above, so the
                // identity is read from there for this run and sealed afterwards.
                None,
            )?;
            // Build the relay dialer over Tor before the transport is moved into the node.
            let relay = relay_addr
                .map(|onion| RelayClient::with_dialer(transport.make_relay_dialer(onion)));
            // Rebuild the node from the same decrypted state (pickles decrypt with bkey).
            let mut me = Node::restore(&state, Box::new(transport), &bkey)?;
            me.set_require_authorization(true);
            if let Some(sd) = &state_dir {
                me.set_tor_state_dir(sd.clone());
            }
            if let Some(relay) = relay {
                me.set_relay(relay);
            }
            let key = decode_store_key(&persist_key)?;
            // Sealed-file media store beside the state file.
            let dir = std::path::Path::new(&persist_path)
                .parent()
                .map(|p| p.join("nightdrop-media"))
                .unwrap_or_else(|| std::path::PathBuf::from("nightdrop-media"));
            me.set_media_store(dir.to_string_lossy().into_owned(), key);
            let inner = Arc::new(Mutex::new(Inner {
                me,
                demo: None,
                pending_backup: None,
                persist: Some(Persist::new(persist_path, key)),
            }));
            // Re-export under the store key: the at-rest file now uses the device key (not the
            // backup password), so the restored identity survives future restarts.
            inner.lock().unwrap().save();
            let poller = StopSignal::new();
            spawn_poller(Arc::clone(&inner), Arc::clone(&poller));
            Ok(Self {
                inner,
                poller: Some(poller),
            })
        }
        #[cfg(not(feature = "tor"))]
        {
            let _ = (
                backup_path,
                password,
                state_dir,
                relay_addr,
                persist_path,
                persist_key,
            );
            anyhow::bail!(
                "this build was compiled without Tor support (rebuild with --features tor)"
            )
        }
    }

    /// Recover from an opt-in **server backup** on a fresh device (§7c / #9): bootstrap Tor,
    /// fetch the opaque blob from the relay by its password-derived handle, decrypt it with the
    /// user's recovery `password`, and rebuild identity + chats — persisted under `persist_key`
    /// so it survives future restarts. The relay counterpart of [`restore_backup_tor`].
    ///
    /// The backup blob does not carry into a *pre-bootstrap* onion keystore here (the relay is
    /// only reachable once Tor is up), so this device comes back on a **new** `.onion`; the
    /// startup address-rotation announcement (#11) then tells contacts the new address. Note: the
    /// relay copy is drained on fetch, so an unsuccessful attempt (e.g. wrong password) consumes
    /// it — the same "lose the password, lose the backup" contract as create.
    pub fn restore_server_backup_tor(
        password: String,
        state_dir: Option<String>,
        relay_addr: String,
        persist_path: String,
        persist_key: String,
    ) -> Result<NightdropCore> {
        #[cfg(feature = "tor")]
        {
            let transport = crate::transport::tor::TorTransport::bootstrap(
                "nightdrop",
                state_dir.as_deref(),
                // Authorized-client keys for onion client authorization (#22) live under the
                // Tor state dir; empty/absent → a normal public onion.
                state_dir
                    .as_deref()
                    .map(|s| format!("{s}/client-auth"))
                    .as_deref(),
                // Restore paths write the backup's keystore files to disk just above, so the
                // identity is read from there for this run and sealed afterwards.
                None,
            )?;
            // Build the relay dialer before the transport is moved into the node; it both
            // fetches the backup and stays attached for store-and-forward afterwards.
            let relay = RelayClient::with_dialer(transport.make_relay_dialer(relay_addr));
            let mut me = Node::restore_from_server(&relay, &password, Box::new(transport))?;
            me.set_require_authorization(true);
            if let Some(sd) = &state_dir {
                me.set_tor_state_dir(sd.clone());
            }
            me.set_relay(relay);
            let key = decode_store_key(&persist_key)?;
            let dir = std::path::Path::new(&persist_path)
                .parent()
                .map(|p| p.join("nightdrop-media"))
                .unwrap_or_else(|| std::path::PathBuf::from("nightdrop-media"));
            me.set_media_store(dir.to_string_lossy().into_owned(), key);
            // We came up on a new onion (see above) — announce it so contacts can still reach us.
            me.announce_address_if_changed();
            // The keystore is in memory now, so the per-peer client keys have to be put back
            // before any connection is attempted (`onion-key-at-rest.md`).
            me.restore_client_keys();
            let inner = Arc::new(Mutex::new(Inner {
                me,
                demo: None,
                pending_backup: None,
                persist: Some(Persist::new(persist_path, key)),
            }));
            // Re-encrypt at rest under the device key so the restored identity persists.
            inner.lock().unwrap().save();
            let poller = StopSignal::new();
            spawn_poller(Arc::clone(&inner), Arc::clone(&poller));
            Ok(Self {
                inner,
                poller: Some(poller),
            })
        }
        #[cfg(not(feature = "tor"))]
        {
            let _ = (password, state_dir, relay_addr, persist_path, persist_key);
            anyhow::bail!(
                "this build was compiled without Tor support (rebuild with --features tor)"
            )
        }
    }

    /// Stop the background poller and tear down the network side (see
    /// [`crate::node::Node::close_transport`]), releasing Tor's on-disk state lock. Idempotent;
    /// the core stays readable afterwards but can no longer send or receive.
    ///
    /// Call this before building a second core over the same `state_dir` — restoring a backup and
    /// the guard heal both do exactly that, and arti refuses to launch a second onion service while
    /// the first instance still holds the lock. Dropping the core is **not** enough on its own: the
    /// poller thread holds handles on the same state, so the lock would still be held when the new
    /// instance tried to start.
    ///
    /// Nor is *asking* the poller to stop enough, which is what this used to do. The poller
    /// snapshots [`RelayClient`]s and drains them off the core lock (§1.5.2), and on Tor each clone
    /// carries an `Arc<TorClient>` + the tokio runtime — so a poller still inside a drain keeps
    /// arti's lock alive past the teardown. The replacement client then logs "Another process has
    /// the lock on our state files" and runs **read-only**: it cannot persist the fresh guards a
    /// heal just picked, so the next heal inherits the same wedged set and heals again, forever.
    /// Seen looping all afternoon on a desktop, 2026-08-02.
    ///
    /// So: stop the poller, tear the transport down (which also aborts an in-flight relay dial —
    /// see [`TorTransport::make_relay_dialer`]), then **wait, bounded**, for the poller to actually
    /// exit. The wait is capped at [`POLLER_EXIT_TIMEOUT`] because this same path runs the duress
    /// wipe, where hanging the app is worse than the bug being fixed; on expiry it says so in the
    /// diagnostics and carries on.
    ///
    /// Idempotent; the core stays readable afterwards but can no longer send or receive.
    ///
    /// [`RelayClient`]: crate::relay_client::RelayClient
    /// [`TorTransport::make_relay_dialer`]: crate::transport::tor::TorTransport::make_relay_dialer
    pub fn shutdown(&self) {
        if let Some(poller) = &self.poller {
            poller.stop();
        }
        // Close the transport early if we can — that flips the Tor transport's closing flag, which
        // cuts short a relay dial the poller may be sitting in.
        //
        // **Try**, never block. This was `self.lock()`, and that made the whole "bounded" promise
        // a lie: the poller holds the core lock across a tick, and a tick contains peer dials
        // (PEER_DIAL_TIMEOUT) and relay round-trips (RELAY_DIAL_TIMEOUT), so on a device whose
        // circuits are timing out the lock is held for minutes. Measured on a phone, 2026-08-03:
        // the user tapped "Reset Tor connection", `shutdown` blocked here, and nothing happened at
        // all — no teardown, no reset, no rebuild, no error. The wait below was bounded and
        // irrelevant, because control never reached it.
        let closed_early = self.try_close_transport(LOCK_TRY_TIMEOUT);
        if let Some(poller) = &self.poller {
            if !poller.wait_for_exit(POLLER_EXIT_TIMEOUT) {
                crate::diag!(
                    "shutdown: the poller was still running {}s after being stopped — it is \
                     probably inside a relay round-trip; whatever it holds (on Tor, arti's state \
                     lock) is released late, so a core rebuilt now may come up read-only",
                    POLLER_EXIT_TIMEOUT.as_secs()
                );
            }
        }
        // Once the poller is gone the lock is uncontended, so this is where a close that lost the
        // race above still happens. Bounded too: if the poller never exited, the lock may still be
        // held, and hanging here would be the same bug in a different place.
        if !closed_early && !self.try_close_transport(LOCK_TRY_TIMEOUT) {
            crate::diag!(
                "shutdown: could not take the core lock to close the transport — it is still held \
                 by work that has not finished; the transport closes when that work drops it"
            );
        }
    }

    /// Close the transport if the core lock can be taken within `bound`. `false` = it could not.
    ///
    /// Deliberately never blocks: see [`shutdown`](Self::shutdown) for what blocking here cost.
    fn try_close_transport(&self, bound: Duration) -> bool {
        let deadline = std::time::Instant::now() + bound;
        loop {
            match self.inner.try_lock() {
                Ok(mut g) => {
                    g.me.close_transport();
                    return true;
                }
                // Poisoned: some thread panicked holding it. Recover rather than give up — the
                // whole point of §1.5.3 is that a panic must not brick the core.
                Err(std::sync::TryLockError::Poisoned(e)) => {
                    e.into_inner().me.close_transport();
                    return true;
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        return false;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }

    /// Acquire the inner lock, **recovering from poisoning** (§1.5.3). If some thread panicked
    /// while holding the guard, the mutex is poisoned; without recovery every subsequent FFI call
    /// would `unwrap`-panic, bricking the core for the rest of the session. We take the guard
    /// anyway (`into_inner`): the worst case is one operation left partial state behind, which is
    /// far better than an app that is permanently unresponsive until restart.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run one poll cycle. Used by tests of the real flow; harmless otherwise.
    #[flutter_rust_bridge::frb(ignore)]
    pub fn poll_once(&self) -> Result<()> {
        self.lock().drive(true)
    }

    /// This device's public identity handle.
    pub fn identity(&self) -> Identity {
        Identity {
            id: self.lock().me.identity_id(),
        }
    }

    /// Whether this device's address is published/reachable yet. On Tor it is `false` for the
    /// ~1–3 min after launch while the onion descriptor (re)publishes — the UI shows a "still
    /// publishing, others can't pair yet" banner until this is `true`.
    pub fn onion_ready(&self) -> bool {
        self.lock().me.onion_published()
    }

    /// Whether the **direct** onion-to-onion path looks wedged: several sends in a row have failed
    /// to reach a peer and none has ever succeeded this run.
    ///
    /// The companion to [`onion_ready`](Self::onion_ready), and the half that was missing. That one
    /// only asks whether *our* service published; a device can do that perfectly while being unable
    /// to reach anybody, because every circuit it builds dies in its guard set. Seen on a phone
    /// (2026-08-03): descriptor uploaded 8/8, and simultaneously 417 circuit builds with 61 hard
    /// timeouts and `Unable to build circuit to introduction point`. By the old health check that
    /// device was fine, so nothing healed and every message silently went by relay instead.
    pub fn direct_path_wedged(&self) -> bool {
        self.lock().me.direct_path_wedged()
    }

    /// This device's address others dial (its `.onion` on a Tor transport).
    #[flutter_rust_bridge::frb(ignore)]
    pub fn address(&self) -> String {
        self.lock().me.address()
    }

    /// All current contacts.
    pub fn contacts(&self) -> Vec<Contact> {
        self.lock().me.contacts()
    }

    /// Messages for a contact, oldest first.
    pub fn messages(&self, contact_id: &str) -> Vec<ChatMessage> {
        self.lock().me.messages(contact_id)
    }

    /// The human-comparable **safety number** for a contact (12×5 digits, identical on both
    /// devices) — compare it out-of-band to confirm no MITM on pairing (key-verification design).
    pub fn safety_number(&self, contact_id: &str) -> Result<String> {
        self.lock().me.safety_number(contact_id)
    }

    /// The raw safety fingerprint (base64url) to render as a QR for scan-to-verify.
    pub fn safety_qr(&self, contact_id: &str) -> Result<String> {
        self.lock().me.safety_qr(contact_id)
    }

    /// Compare a scanned safety-QR payload against this contact; on a match, mark verified and
    /// persist. Returns whether it matched.
    pub fn verify_safety_qr(&self, contact_id: &str, scanned: String) -> Result<bool> {
        let mut g = self.lock();
        let matched = g.me.verify_safety_qr(contact_id, &scanned)?;
        if matched {
            g.save();
            emit("contacts");
        }
        Ok(matched)
    }

    /// Set the contact's verified flag (after comparing the safety number by hand) and persist.
    pub fn set_verified(&self, contact_id: &str, verified: bool) {
        let mut g = self.lock();
        g.me.set_verified(contact_id, verified);
        g.save();
        emit("contacts");
    }

    /// Our advertised **extra** relay addresses (#17) — relays that also host our mailbox, on top
    /// of the built-in default. Empty by default.
    pub fn my_relays(&self) -> Vec<String> {
        self.lock().me.my_relays()
    }

    /// Replace our advertised extra relay set (#17): peers will fan out our offline mail to these
    /// too, and we poll them alongside the default. Announces the change to existing contacts
    /// (E2E) and persists. Addresses are validated as non-empty; reachability is the user's call.
    pub fn set_my_relays(&self, relays: Vec<String>) {
        let cleaned: Vec<String> = relays
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // Persist the user's intent synchronously so the edit is durable immediately and the
        // Save button returns at once. Empty set = advertise nothing extra → fall back to the
        // baked-in default relay only.
        {
            let mut g = self.lock();
            g.me.set_my_relays(cleaned);
            g.save();
        }
        emit("contacts");
        // Notify existing contacts (E2E `Relays` frame) in the BACKGROUND. A slow/cold arti
        // (Tor) would otherwise block the FFI call for tens of seconds; the edit is already
        // saved, so this is best-effort and must never hold up the UI.
        let inner = std::sync::Arc::clone(&self.inner);
        std::thread::spawn(move || {
            let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
            g.me.announce_relays();
            g.save(); // persist any relay receipts created while announcing
        });
    }

    /// Generate this device's **access key** for a PRIVATE relay (restricted discovery, §3.2).
    /// Returns the public `descriptor:x25519:…` string to give the relay operator, who runs
    /// `nightdrop-relay authorize-client <name> <key>`. arti stores the private half and presents it
    /// automatically whenever this device dials that relay, so after authorization the private relay
    /// becomes reachable. Only meaningful on the Tor transport; errors otherwise. Generating a key
    /// for a public relay is harmless but unnecessary.
    pub fn create_relay_access_key(&self, relay_onion: String) -> Result<String> {
        let onion = relay_onion.trim();
        let g = self.lock();
        match g.me.relay_access_key(onion) {
            Some(result) => result,
            None => anyhow::bail!("relay access keys require the Tor transport"),
        }
    }

    /// Health of each of our advertised extra relays (#17), as of the last relay poll: `(address,
    /// reachable)`. A relay that stops answering our mailbox drain (e.g. a self-hosted one that
    /// went down) reports `reachable = false`, so the UI can warn the user and suggest adding a
    /// backup relay. A not-yet-polled relay reports `true` (optimistic).
    /// Ask our onion site whether a newer release exists (`crate::update`).
    ///
    /// `None` means **no answer, say nothing**: either this transport has no anonymized path, or
    /// the site did not respond. Both are silence, never a warning — a user who is taught to
    /// dismiss update notices will dismiss the one that matters.
    ///
    /// Pass the app's own version (the pubspec string, `"0.1.17+403"`, is fine — the build suffix
    /// is ignored). Call it at most daily; it is a network round trip, not a getter.
    pub fn check_for_update(&self, current_version: String) -> Option<AppUpdate> {
        // Take a transport handle under the lock, then DROP the lock before any network I/O. A
        // Tor fetch bounded at 30s would otherwise pin the core lock for 30s and freeze every
        // other FFI call and the poller behind it (§6).
        let transport = self.lock().me.transport_handle();
        match crate::update::check(transport.as_ref(), &current_version) {
            Ok(Some(s)) => Some(AppUpdate {
                current: s.current,
                latest: s.latest,
                update_available: s.update_available,
            }),
            // Site down, Tor slow, malformed JSON, or a transport that cannot fetch anonymously.
            // All the same to the user: nothing to report.
            Ok(None) => None,
            Err(e) => {
                crate::diag!("update check failed (ignored): {e:#}");
                None
            }
        }
    }

    /// Download the published build **for this device** over Tor and write it to `dest_path`,
    /// verifying its SHA-256 against the manifest first. Returns the byte count.
    ///
    /// The ABI is not a parameter on purpose: it comes from
    /// [`update::native_abi`](crate::update::native_abi), which reads the architecture this core
    /// was compiled for. The caller cannot know better, and getting it wrong produces a build
    /// Android will refuse to install after the user has waited out the whole download.
    ///
    /// Nothing is installed: the file is handed to the user, who chooses. Android verifies the
    /// signature itself and refuses to replace Night Drop with anything not signed by our release
    /// key, so the app never becomes the thing that decides what code runs.
    ///
    /// Slow by nature — tens of megabytes over Tor — so call it off the UI path and expect it to
    /// take minutes on a poor circuit.
    pub fn download_update(&self, dest_path: String) -> Result<u64> {
        // Same rule as check_for_update, and it matters far more here: this can run for minutes.
        // Holding the core lock across it would freeze the entire app for the whole download.
        let transport = self.lock().me.transport_handle();
        let Some(fetched) = transport.onion_get(
            crate::update::UPDATE_ONION,
            crate::update::UPDATE_PORT,
            crate::update::MANIFEST_PATH,
        ) else {
            anyhow::bail!("this transport cannot fetch anonymously");
        };
        let manifest: crate::update::UpdateManifest = serde_json::from_slice(&fetched?)?;
        crate::update::download(
            transport.as_ref(),
            &manifest,
            crate::update::native_abi(),
            std::path::Path::new(&dest_path),
            &emit_progress,
        )
    }

    pub fn relay_health(&self) -> Vec<RelayHealth> {
        self.lock()
            .me
            .relay_health()
            .into_iter()
            .map(|(address, reachable)| RelayHealth { address, reachable })
            .collect()
    }

    /// Contacts whose inbound chat request awaits the user's approval (§5).
    pub fn incoming_requests(&self) -> Vec<Contact> {
        self.lock().me.pending_authorizations()
    }

    /// Create a pairing invite: a `slot-secret-words` short code plus a QR payload that
    /// embeds our address and a real pre-key bundle (§5a). In demo mode it also simulates
    /// a peer joining, producing a request to approve.
    pub fn create_invite(&self) -> Result<PairingInvite> {
        let mut g = self.lock();
        let bundle = g.me.publish_bundle();
        let qr_payload = format!(
            "nightdrop://pair?addr={}&ik={}&otk={}",
            g.me.address(),
            bundle.identity_key,
            bundle.one_time_key
        );
        let short_code = random_short_code();
        // Remember the code so an approval can echo it back to the joiner (§5).
        g.me.set_last_invite_code(short_code.clone());
        {
            let inner = &mut *g;
            if let Some(demo) = inner.demo.as_mut() {
                demo.simulate_join(&mut inner.me, &bundle)?;
                emit("request");
            }
        }
        g.save(); // publishing a bundle consumed a one-time key — persist the account
        Ok(PairingInvite {
            short_code,
            qr_payload,
        })
    }

    /// Join from a scanned QR payload (pre-authorized, §5a):
    /// `nightdrop://pair?addr=...&ik=...&otk=...`. Opens a session and sends the Hello. This
    /// is the relay-free pairing path used over Tor (the QR carries the inviter's `.onion`).
    pub fn connect_via_qr(&self, payload: &str) -> Result<Contact> {
        let (addr, bundle) = parse_invite(payload)?;
        let mut g = self.lock();
        let contact_id = g.me.connect_with_bundle(&addr, &bundle)?;
        // The recipient must accept before anything is delivered (they always require approval, §5),
        // just like a short-code join — show the waiting notice until we hear back (cleared on the
        // approval signal or the first received message).
        g.me.note_awaiting_approval(&contact_id);
        g.save(); // new chat (and a consumed session) — persist
        g.me.contacts()
            .into_iter()
            .find(|c| c.id == contact_id)
            .ok_or_else(|| anyhow::anyhow!("chat not created"))
    }

    /// Create a short-code invite via the rendezvous mailbox (§5b/§5c). Returns the full
    /// `slot-secret-words` code to read out; the secret never reaches the relay. Returns
    /// immediately — the background poller completes the interactive SPAKE2 handshake with a
    /// joiner (`Node::service_pending_invites`), so this device must stay reachable meanwhile.
    pub fn create_short_code_invite(&self) -> Result<String> {
        let slot = random_slot();
        let secret = random_secret_words();
        let code = format!("{slot}-{secret}");
        let mut g = self.lock();
        g.me.stage_short_code_invite(&slot, &secret, SHORT_CODE_TTL)?;
        // Remember the code so the approval signal can echo it back to the joiner (§5).
        g.me.set_last_invite_code(code.clone());
        g.save();
        RELAY_POLL_NOW.store(true, std::sync::atomic::Ordering::Relaxed); // answer joiners promptly
        Ok(code)
    }

    /// Join a chat from a `slot-secret-words` short code (§5b): run the interactive SPAKE2
    /// handshake through the rendezvous, then open a session toward the inviter. The handshake
    /// blocks on relay round-trips, so it runs **without** the core lock held; only the brief
    /// identity-mutating steps (relay lookup, session open) take the lock.
    pub fn join_via_short_code(&self, code: &str) -> Result<Contact> {
        let (slot, secret) = code
            .split_once('-')
            .ok_or_else(|| anyhow::anyhow!("malformed short code"))?;
        // Snapshot the rendezvous relay set (primary + our extras, §3.1) under a brief lock, then
        // release it for the blocking handshake, which broadcasts across the whole set.
        let relays = self.lock().me.rendezvous_relays();
        if relays.is_empty() {
            anyhow::bail!("no relay configured");
        }
        let payload =
            crate::node::run_join_handshake(&relays, slot, secret, SHORT_CODE_JOIN_TIMEOUT)?;
        let mut g = self.lock();
        let contact = g.me.connect_from_invite_payload(&payload)?;
        // Short-code pairing needs the inviter to accept before anything is delivered — show a
        // notice until we hear back (cleared on approval or the first received message).
        g.me.note_awaiting_approval(&contact.id);
        g.save();
        Ok(contact)
    }

    /// Approve or decline a pending inbound request. On approval it becomes a contact.
    pub fn authorize(&self, contact_id: &str, accept: bool) -> Result<()> {
        let mut g = self.lock();
        g.me.authorize(contact_id, accept)?;
        if !accept {
            if let Some(demo) = g.demo.as_mut() {
                demo.drop_peer(contact_id);
            }
        }
        g.save();
        emit("contacts");
        Ok(())
    }

    /// Open a new chat against a fresh in-process peer (demo only): stands up a real `Node`
    /// over the in-memory network and performs the genuine bundle handshake. Errors on a real
    /// core, which has no demo harness — real chats are opened by pairing (QR / short code).
    pub fn open_chat(&self, _code: Option<String>) -> Result<Contact> {
        let mut g = self.lock();
        let inner = &mut *g;
        let demo = inner
            .demo
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("open_chat is only available in the in-process demo"))?;
        let contact_id = demo.open_chat(&mut inner.me)?;
        inner
            .me
            .contacts()
            .into_iter()
            .find(|c| c.id == contact_id)
            .ok_or_else(|| anyhow::anyhow!("chat not created"))
    }

    /// Send a message. In demo mode the reply is produced synchronously and returned; in
    /// real mode the reply arrives asynchronously via the poll loop + event stream.
    pub fn send_message(&self, contact_id: &str, text: &str) -> Result<Vec<ChatMessage>> {
        let mut g = self.lock();
        g.me.send(contact_id, text)?;
        if g.demo.is_some() {
            g.drive(false)?;
        }
        g.save();
        Ok(g.me.messages(contact_id))
    }

    /// Send an image/video attachment (E2E-encrypted, sealed at rest). `kind` is
    /// "image"/"video", `mime` like "image/jpeg". Capped at 100 MB.
    pub fn send_media(
        &self,
        contact_id: &str,
        data: Vec<u8>,
        mime: String,
        kind: String,
        thumb: Vec<u8>,
    ) -> Result<()> {
        let mut g = self.lock();
        g.me.send_media(contact_id, &data, &mime, &kind, &thumb)?;
        g.save();
        Ok(())
    }

    /// Decrypt and return an attachment's bytes (for inline image display).
    pub fn media_bytes(&self, media_id: &str) -> Result<Vec<u8>> {
        self.lock().me.media_bytes(media_id)
    }

    /// Decrypt an attachment to a temp file and return its path (to open a video/file in the
    /// system player without copying the bytes back through the bridge).
    pub fn media_to_file(&self, media_id: &str, ext: String) -> Result<String> {
        self.lock().me.media_to_file(media_id, &ext)
    }

    /// Begin an encrypted backup: build the blob, hold it pending, and return the
    /// **recovery password to show once** (§7). Randomly generated, never persisted; losing
    /// it loses the backup. `full` picks the content matrix (#7): `true` = Full (history +
    /// media included), `false` = Lite (identity + onion + contacts + sessions only). The UI
    /// shows the password for acknowledgment, then calls [`save_backup`](Self::save_backup).
    pub fn create_backup(&self, full: bool) -> Result<String> {
        let mut g = self.lock();
        let password = crate::storage::random_password();
        let blob = g.me.backup_with_mode(&password, full)?;
        g.pending_backup = Some(blob);
        // Mark every chat backed up (#7) and, for a Full backup, tell each peer so they see the
        // transparency warning. Persist the flags so they survive a restart.
        let ids: Vec<String> = g.me.contacts().into_iter().map(|c| c.id).collect();
        g.me.mark_backed_up(&ids, full);
        g.save();
        Ok(password)
    }

    /// Write the backup prepared by [`create_backup`](Self::create_backup) to `path` (the
    /// location chosen by the user after acknowledging the password, §7 / TODO #4).
    pub fn save_backup(&self, path: String) -> Result<()> {
        let mut g = self.lock();
        let blob = g
            .pending_backup
            .take()
            .ok_or_else(|| anyhow::anyhow!("no backup prepared; create one first"))?;
        std::fs::write(&path, &blob)?;
        Ok(())
    }

    /// Return the prepared backup blob (already password-encrypted) so the UI can hand it to
    /// the OS file picker — needed on Android, where the app can only write to a user-chosen
    /// location (Documents/Downloads) via the Storage Access Framework, not a private path.
    /// Leaves the pending blob in place so a cancelled save can be retried.
    pub fn backup_bytes(&self) -> Result<Vec<u8>> {
        self.lock()
            .pending_backup
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no backup prepared; create one first"))
    }

    /// Begin a **single-chat scoped backup** (#8): prepare a blob carrying only `contact_id`'s
    /// chat (history + media if `full`) and return the one-time recovery password. Held pending
    /// exactly like [`create_backup`](Self::create_backup) — acknowledge the password, then
    /// [`save_backup`](Self::save_backup) / [`backup_bytes`](Self::backup_bytes) to write it.
    pub fn create_chat_backup(&self, contact_id: &str, full: bool) -> Result<String> {
        let mut g = self.lock();
        let password = crate::storage::random_password();
        let blob = g.me.backup_chat(contact_id, &password, full)?;
        g.pending_backup = Some(blob);
        g.me.mark_backed_up(&[contact_id.to_string()], full);
        g.save();
        Ok(password)
    }

    /// Report a **screenshot** of this chat (#1) — log it locally and tell the peer.
    ///
    /// Screenshots stay allowed: blocking them just moves people to photographing the screen, which
    /// is undetectable. Making them visible is the honest trade. Fire-and-forget from the UI's point
    /// of view; a chat that no longer exists is silently ignored.
    ///
    /// Only Android 14+ can detect a screenshot, so a peer's silence proves nothing — the UI must
    /// not imply otherwise.
    pub fn report_screenshot(&self, contact_id: String) -> Result<()> {
        let mut g = self.lock();
        g.me.report_screenshot(&contact_id);
        g.save();
        Ok(())
    }

    /// Tell peers whether this device can report screenshots at all (#1).
    ///
    /// `visible` is what the platform can actually do — Android 14+ only. It goes to the PEER, not
    /// to us: we already know when we take a screenshot. The party who needs it is the one deciding
    /// what to send, because on a device that cannot report captures the peer's silence means
    /// nothing, and silence reading as "nothing happened" is a guarantee this app does not make.
    ///
    /// Safe to call on every launch: only a change is put on the wire.
    pub fn set_capture_reporting(&self, visible: bool) -> Result<()> {
        let mut g = self.lock();
        g.me.announce_captures(visible);
        g.save();
        Ok(())
    }

    /// Merge a scoped backup file at `path` into the **current** identity (#8): add the chat(s)
    /// it carries without disturbing our identity or any active session (existing chats only
    /// gain missing history). Returns how many messages were merged.
    pub fn merge_backup(&self, path: String, password: String) -> Result<u32> {
        let blob = std::fs::read(&path)?;
        let mut g = self.lock();
        let added = g.me.merge_from_backup(&blob, &password)?;
        g.save();
        emit("contacts");
        Ok(added as u32)
    }

    /// Enable an **opt-in server backup** (§7c invariant): post an opaque, password-encrypted
    /// backup blob to the relay for `ttl_hours` (clamped to 1..=36; default 24). The recovery
    /// `password` is randomly generated here, returned to be shown **once**, and never
    /// persisted — the relay never sees it. The returned exact expiry drives the acknowledgment
    /// screen the invariant requires. Losing the password loses the backup, by design.
    pub fn create_server_backup(&self, ttl_hours: u64, full: bool) -> Result<ServerBackupInfo> {
        let mut g = self.lock();
        let relay =
            g.me.relay_client()
                .ok_or_else(|| anyhow::anyhow!("server backup needs a relay (Tor mode)"))?;
        let hours = ttl_hours.clamp(1, 36);
        let ttl = Duration::from_secs(hours * 3600);
        let password = crate::storage::random_password();
        g.me.server_backup(&relay, &password, Some(ttl), full)?;
        // The blob is posted, so mark the chats backed up (#7) + notify peers of a Full backup.
        let ids: Vec<String> = g.me.contacts().into_iter().map(|c| c.id).collect();
        g.me.mark_backed_up(&ids, full);
        g.save();
        Ok(ServerBackupInfo {
            password,
            expires_at_secs: now_secs() + hours * 3600,
        })
    }

    /// Peer-facing logout (#7 / §11.6): tell the peer of every **un-backed** chat that it's
    /// closed (so their mail isn't lost to a since-deleted identity), leave backed-up chats
    /// silent (their mail queues until we restore within 24h), and clear all chats. The app
    /// then wipes the local state file + secure key. Call **before** deleting local files.
    ///
    /// Returns the number of un-backed chats whose "chat deleted" notice we could **not** get out
    /// (neither queued on a relay nor sent directly), so the UI can warn "some contacts may not
    /// have been notified" (§1.3). `0` means every notice was queued/sent.
    pub fn logout(&self) -> u32 {
        let mut g = self.lock();
        let failed = g.me.logout();
        emit("contacts");
        failed as u32
    }

    /// [`logout`](Self::logout) for the **duress wipe** (#3): same teardown, but *every* live chat
    /// is told, not just un-backed ones, since no restore is coming. The notice is the ordinary
    /// "chat deleted" — never anything that identifies this as a duress event.
    ///
    /// Best-effort by design. The caller must wipe regardless of the count returned: a wipe that
    /// can be prevented by taking the phone off the network is not a wipe.
    pub fn duress_logout(&self) -> u32 {
        let mut g = self.lock();
        let failed = g.me.duress_logout();
        emit("contacts");
        failed as u32
    }

    /// Delete a chat (TODO #1): signal the peer (who then sees a "chat deleted" notice) and
    /// remove it locally. Creating a new chat is required to talk again.
    pub fn delete_chat(&self, contact_id: &str) -> Result<()> {
        let mut g = self.lock();
        g.me.delete_chat(contact_id)?;
        if let Some(demo) = g.demo.as_mut() {
            demo.drop_peer(contact_id);
        }
        g.save();
        emit("contacts");
        Ok(())
    }

    /// Edit the text of one of our own messages (`msg_id` from [`ChatMessage`]). Allowed
    /// within 15 minutes of sending, or at any time while the message is still queued on
    /// the relay (the peer never saw it — the queued blob is recalled and replaced).
    /// The message is marked `edited`; the peer's copy updates via an E2E edit frame.
    pub fn edit_message(&self, contact_id: &str, msg_id: &str, text: &str) -> Result<()> {
        let mut g = self.lock();
        g.me.edit_message(contact_id, msg_id, text)?;
        g.save();
        emit_chats("message", vec![contact_id.to_string()]);
        Ok(())
    }

    /// Unsend ("delete for both") one of our own messages (`msg_id` from [`ChatMessage`]).
    /// Same eligibility as [`edit_message`](Self::edit_message): within 15 minutes, or while
    /// still queued (then the relay blob is recalled so the peer never receives it). The
    /// message becomes a "deleted" tombstone (`kind == "deleted"`) on both sides.
    pub fn unsend_message(&self, contact_id: &str, msg_id: &str) -> Result<()> {
        let mut g = self.lock();
        g.me.unsend_message(contact_id, msg_id)?;
        g.save();
        emit_chats("message", vec![contact_id.to_string()]);
        Ok(())
    }

    /// Set the local user's display name within one chat (§4).
    pub fn set_my_name(&self, contact_id: &str, name: &str) -> Result<()> {
        let mut g = self.lock();
        g.me.set_my_name(contact_id, name)?;
        g.save();
        Ok(())
    }

    /// Give a contact a nickname that only you see (`contact-naming.md`). Never sent, so a peer
    /// can neither read it nor set it; empty clears it. This is the answer to a contact list of
    /// identical "Anon"s — the peer's own name is their choice, and may be missing or duplicated.
    pub fn set_local_name(&self, contact_id: &str, name: &str) -> Result<()> {
        let mut g = self.lock();
        g.me.set_local_name(contact_id, name)?;
        g.save();
        Ok(())
    }

    /// Toggle opt-in 24h server storage for a chat (§6).
    pub fn set_remote_storage(&self, contact_id: &str, enabled: bool) -> Result<()> {
        let mut g = self.lock();
        g.me.set_remote_storage(contact_id, enabled)?;
        g.save();
        Ok(())
    }

    /// Set a chat's disappearing-messages timer (`secs`, 0 = off). A shared setting: messages
    /// older than the timer are deleted on both devices, and the new value is synced to the
    /// peer. Common values: 3600 (1h), 86400 (1d), 604800 (1w).
    pub fn set_disappearing(&self, contact_id: &str, secs: u64) -> Result<()> {
        let mut g = self.lock();
        g.me.set_disappearing(contact_id, secs)?;
        g.save();
        emit_chats("message", vec![contact_id.to_string()]);
        Ok(())
    }

    /// Tell the core the app moved to/from the background. Backgrounded, the poller slows
    /// down to conserve battery/data; foregrounded, it resumes snappy polling and does an
    /// immediate relay catch-up so queued offline mail appears right away.
    pub fn set_background(&self, background: bool) {
        BACKGROUND.store(background, std::sync::atomic::Ordering::Relaxed);
        if !background {
            RELAY_POLL_NOW.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// A fresh random 32-byte at-rest key (base64) for the persisted state file. Generated once
/// per device, stored in the OS secure store on the Dart side, and passed back to
/// [`new_tor`](NightdropCore::new_tor) on later launches to restore the saved state.
pub fn random_store_key() -> String {
    use base64::Engine as _;
    use zeroize::Zeroize as _;
    let mut key = [0u8; 32];
    rand::Rng::fill(&mut rand::thread_rng(), &mut key);
    let encoded = base64::engine::general_purpose::STANDARD.encode(key);
    key.zeroize(); // don't leave the raw key on the stack after it's been handed off as base64
    encoded
}

/// Whether this store's key is protected by a passphrase rather than sitting in the platform
/// keystore. `dir` is the same directory the state blob lives in.
pub fn store_is_locked(dir: String) -> bool {
    crate::storage::lock::is_locked(&dir)
}

/// Put the store key behind `passphrase`. The caller **must** then delete its keystore copy of
/// `key_b64`; leaving it there defeats the entire purpose, since the key would still be readable
/// without the passphrase.
///
/// A short PIN cannot be made safe here: an attacker holding the lock file tries every 4-6 digit
/// value offline regardless of the derivation cost. Only a passphrase with real entropy protects
/// an imaged device, and the UI must say so rather than implying otherwise.
pub fn set_store_passphrase(dir: String, key_b64: String, passphrase: String) -> Result<()> {
    let key = decode_store_key(&key_b64)?;
    crate::storage::lock::set_passphrase(&dir, &key, &passphrase)
}

/// What a secret presented at the lock screen turned out to be.
pub struct StoreUnlock {
    /// The **duress** secret (#3) was presented: the caller must wipe the identity, and `key_b64`
    /// is empty. Never true and non-empty at once — the duress secret yields no key, ever.
    pub duress: bool,
    /// The store key, base64, when this was the normal secret.
    pub key_b64: String,
}

/// Recover the store key from `secret`, base64-encoded for the Dart side to hand straight back to a
/// core constructor. Errors on a wrong secret without saying which part was wrong.
///
/// A **duress** secret (#3) returns `duress: true` instead of a key; the caller must then wipe (see
/// `docs/design/duress-wipe.md`). Both slots are always derived, so the two outcomes are
/// indistinguishable by timing to anyone watching the user unlock.
pub fn unlock_store_key(dir: String, secret: String) -> Result<StoreUnlock> {
    use base64::Engine as _;
    use zeroize::Zeroize as _;
    match crate::storage::lock::unlock(&dir, &secret)? {
        crate::storage::lock::Opened::Normal(mut key) => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(key);
            key.zeroize();
            Ok(StoreUnlock {
                duress: false,
                key_b64: encoded,
            })
        }
        crate::storage::lock::Opened::Duress => Ok(StoreUnlock {
            duress: true,
            key_b64: String::new(),
        }),
    }
}

/// Arm (or replace) the **duress** secret (#3) — the second secret that wipes instead of opening.
/// Requires the normal secret, so an adversary who coerced one unlock cannot re-arm or disarm it.
///
/// The lock file is written so that an armed duress slot is **indistinguishable** from an unarmed
/// one, and the UI must never display which it is: showing it would mean persisting it, which is
/// the tell the design removes. Warn the user at this moment and nowhere else.
pub fn set_duress_secret(dir: String, passphrase: String, duress: String) -> Result<()> {
    crate::storage::lock::set_duress(&dir, &passphrase, &duress)
}

/// Disarm duress. Requires the normal secret. Succeeds whether or not anything was armed, so a
/// caller who has not proven they know the state cannot infer it from the outcome.
pub fn clear_duress_secret(dir: String, passphrase: String) -> Result<()> {
    crate::storage::lock::clear_duress(&dir, &passphrase)
}

/// Turn **cover traffic** (#4) on or off. Off by default, and deliberately opt-in: it costs the
/// user battery and bandwidth continuously, and costs whoever runs the relay — usually a
/// volunteer — the load of carrying dummy mail.
///
/// What it buys: the relay's per-mailbox timing profile stops being a clean read of when you are
/// active. What it does **not** buy, and the UI must say so: this is chaff, not constant-rate
/// transmission. Real messages still post *in addition* to the cover, so a patient observer can
/// still see aggregate volume rise when you are genuinely busy. It raises the cost of traffic
/// analysis; it does not end it. See `docs/design/cover-traffic.md` §4.
pub fn set_cover_traffic(enabled: bool) {
    COVER_TRAFFIC.store(enabled, Ordering::Relaxed);
}

/// Whether cover traffic is currently on.
pub fn cover_traffic_enabled() -> bool {
    COVER_TRAFFIC.load(Ordering::Relaxed)
}

/// Whether a wipe code is armed. Needs the **store key**, which is precisely what makes this safe
/// to expose: the unlocked app can tell the user where they stand, while someone holding only an
/// image of the device still cannot — reading it requires the key the lock protects.
///
/// Offering no readout at all was worse. A user who cannot see whether their wipe code is armed can
/// believe they have one when they don't, and find out while being coerced.
pub fn duress_is_armed(dir: String, key_b64: String) -> bool {
    let Ok(key) = decode_store_key(&key_b64) else {
        return false;
    };
    crate::storage::lock::duress_armed(&dir, &key)
}

/// Whether `secret` is the **normal** secret — so a settings flow can reject a wrong one up front
/// rather than after the user has filled in everything that follows. A duress secret answers
/// `false` and wipes nothing: its contract is the lock screen.
pub fn store_secret_is_correct(dir: String, secret: String) -> bool {
    crate::storage::lock::is_normal_secret(&dir, &secret)
}

/// Delete the lock file without any secret — **only** for the duress wipe, where the store it
/// protects is destroyed in the same breath. Leaving it behind would show a lock screen for a store
/// that no longer exists.
pub fn destroy_store_lock(dir: String) -> Result<()> {
    crate::storage::lock::destroy(&dir)
}

/// Drop the passphrase lock, returning the store key so the caller can restore its keystore copy.
/// Without that the store would be unopenable — the lock file was the only way in.
pub fn clear_store_passphrase(dir: String, passphrase: String) -> Result<String> {
    use base64::Engine as _;
    use zeroize::Zeroize as _;
    let mut key = crate::storage::lock::clear(&dir, &passphrase)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(key);
    key.zeroize();
    Ok(encoded)
}

/// Decode a base64 32-byte at-rest key.
/// Where the onion identity lives now: sealed under the store key, beside the state file, instead
/// of unencrypted in arti's keystore (`docs/design/onion-key-at-rest.md`).
// Not gated on the `tor` feature, unlike its caller: sealing is plain `storage` work with no arti
// in it, and keeping it buildable without Tor is what lets the tests below cover the start-up
// decision — the part that had a device-visible bug — in the default test run.
#[cfg_attr(not(feature = "tor"), allow(dead_code))]
const ONION_KEY_FILE: &str = "onion-key.sealed";

/// Read the saved onion identity, or `None` on a first run.
///
/// An unreadable file is **not** treated as absent: returning `None` there would let the caller
/// start with a fresh identity and silently change our address, stranding every contact. That case
/// is an error, and callers must fail on it.
#[cfg_attr(not(feature = "tor"), allow(dead_code))]
fn read_onion_key(dir: &str, key: &crate::storage::StoreKey) -> Result<Option<[u8; 64]>> {
    let path = format!("{dir}/{ONION_KEY_FILE}");
    let Ok(blob) = std::fs::read(&path) else {
        return Ok(None); // genuinely absent — first run
    };
    let plain = crate::storage::open(key, &blob)
        .map_err(|_| anyhow::anyhow!("onion identity is unreadable"))?;
    let bytes: [u8; 64] = plain
        .try_into()
        .map_err(|_| anyhow::anyhow!("onion identity is the wrong length"))?;
    Ok(Some(bytes))
}

/// Which onion identity a start-up should use: the saved one when an identity is being restored,
/// and none at all when a new one is being created.
///
/// The `restoring` distinction is the whole point. A new identity gets a new address, so a sealed
/// key left on disk belongs to an identity being abandoned; it will not unseal under the new store
/// key, and [`read_onion_key`] fails hard on that by design. Consulting it there turned a stale
/// file into an unrecoverable app: the failure landed on "set up a new identity", the one path out
/// of the load-error screen. The caller overwrites the file once the fresh identity has its own key.
#[cfg_attr(not(feature = "tor"), allow(dead_code))]
fn onion_key_for_start(
    dir: &str,
    key: &crate::storage::StoreKey,
    restoring: bool,
) -> Result<Option<[u8; 64]>> {
    if !restoring {
        return Ok(None);
    }
    read_onion_key(dir, key)
}

/// Seal the onion identity beside the state file. Called once, after a first-run bootstrap.
#[cfg_attr(not(feature = "tor"), allow(dead_code))]
/// Remove arti's **on-disk** keystore unless it is still the rightful source of our identity.
///
/// It may speak for us in exactly one case: we are restoring an identity and have no sealed key of
/// our own yet — the migration run for an install from before `onion-key-at-rest.md`, and the run
/// after a backup restore (which writes the backup's keystore to disk on purpose). Then arti reads
/// the identity from there and the caller seals it immediately afterwards.
///
/// Every other case it is leftovers, and leaving it is not merely untidy:
///
/// * **Restoring, with a sealed key** — it holds a superseded copy of the identity plus one
///   directory *named after each contact's onion address*, which is a recoverable contact list.
/// * **Creating a new identity** — arti would find that keystore and launch on the OLD identity,
///   so a "new" identity would come up on the previous `.onion`, be reachable by everyone who knew
///   it, and be trivially linkable to the identity the user believed they had left. The caller then
///   seals that old key as if it were the new one, making it permanent. Reachable two ways: a
///   pre-`onion-key-at-rest` install whose user picks "set up a new identity" before the migration
///   ever runs (the load-error screen is exactly where they land), and any wipe that fails to
///   delete `arti-state` — which has been observed on Android. Anonymous identities are the
///   product; one that silently inherits its predecessor's address is not one.
///
/// Best-effort: the keys are superseded either way, so a failure to remove is logged, not fatal.
fn drop_superseded_keystore(state_dir: &str, have_sealed_key: bool, restoring: bool) {
    if restoring && !have_sealed_key {
        return; // the migration / post-restore run: arti must read the identity from disk
    }
    let keystore = std::path::Path::new(state_dir).join("arti-state/keystore");
    if !keystore.exists() {
        return;
    }
    match std::fs::remove_dir_all(&keystore) {
        Ok(()) if restoring => crate::diag!(
            "keys: removed the on-disk keystore — identity and contact keys now live only in the \
             sealed store"
        ),
        Ok(()) => crate::diag!(
            "keys: removed a leftover on-disk keystore before creating a new identity — it would \
             have been adopted as ours, putting the new identity on the old address"
        ),
        // Not fatal: the keys in it are superseded either way, and failing the launch over a
        // leftover directory would be worse than leaving it.
        Err(e) => crate::diag!("keys: could not remove the on-disk keystore ({e})"),
    }
}

/// Only the Tor path seals an onion identity (and the tests that pin its rules). Gated so the
/// default `cargo build`/`make clippy` — which is how the repo's own gate runs — doesn't carry a
/// standing dead-code warning, under which a *new* warning goes unnoticed.
#[cfg(any(feature = "tor", test))]
fn write_onion_key(dir: &str, key: &crate::storage::StoreKey, bytes: &[u8; 64]) -> Result<()> {
    let sealed = crate::storage::seal(key, bytes)?;
    std::fs::create_dir_all(dir).ok();
    let path = format!("{dir}/{ONION_KEY_FILE}");
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, &sealed)?;
    std::fs::rename(&tmp, &path)?; // atomic: a torn write here would lose the identity
    Ok(())
}

fn decode_store_key(b64: &str) -> Result<crate::storage::StoreKey> {
    use base64::Engine as _;
    use zeroize::Zeroize as _;
    let mut bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| anyhow::anyhow!("bad store key: {e}"))?;
    let key = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("store key must be 32 bytes"));
    bytes.zeroize(); // wipe the intermediate copy of the key material
    key
}

/// Background poll loop (real mode): pump the transport often (cheap, local), hit the
/// relay on a timed cadence (expensive: one Tor round-trip per poll).
///
/// The thread holds an [`ExitGuard`] for its whole life, so [`NightdropCore::shutdown`] can wait
/// for it to *finish* rather than merely asking it to — see that method for why the difference
/// matters (arti's state lock, and the guard heal that healed forever).
fn spawn_poller(inner: Arc<Mutex<Inner>>, stop: Arc<StopSignal>) {
    thread::spawn(move || {
        let _exit = ExitGuard::new(Arc::clone(&stop));
        // Fire immediately on startup so offline mail is fetched as the app opens.
        let mut last_relay: Option<std::time::Instant> = None;
        let mut next_cover: Option<std::time::Instant> = None;
        let mut cover_delay = next_cover_delay();
        while !stop.stopped() {
            // Foreground: pump often for snappy delivery. Backgrounded: poll far less often
            // to cut battery/CPU and avoid chatty background work (the warm Tor stream still
            // pushes anything that arrives; we just check for it less frequently). Sleeping on the
            // stop signal (rather than `thread::sleep`) means a shutdown doesn't have to wait out
            // the current interval — at no extra cost while idle.
            let background = BACKGROUND.load(Ordering::Relaxed);
            if !stop.sleep(Duration::from_millis(if background { 2000 } else { 80 })) {
                break;
            }
            // While hosting a short-code invite, poll the rendezvous briskly so we answer a
            // joiner's handshake promptly (pairing is a short, attended flow), overriding the
            // usual battery-sparing cadence in both foreground and background.
            let pairing = {
                let g = inner.lock().unwrap_or_else(|e| e.into_inner());
                g.me.has_pending_invites()
            };
            let interval = if pairing {
                RELAY_POLL_PAIRING
            } else if background {
                RELAY_POLL_BACKGROUND
            } else {
                RELAY_POLL_FOREGROUND
            };
            let relay_due = RELAY_POLL_NOW.swap(false, Ordering::Relaxed)
                || last_relay.is_none_or(|t| t.elapsed() >= interval);
            // §1.5.2: run the blocking relay reads OFF the lock. Snapshot the relay clients under a
            // brief lock, drain the mailboxes lock-free (seconds over Tor), then re-acquire only to
            // apply the results — so UI calls (send/contacts/messages) aren't stalled behind an
            // in-flight relay poll.
            let mut harvest = if relay_due {
                let plan = {
                    let g = inner.lock().unwrap_or_else(|e| e.into_inner());
                    g.me.relay_drain_plan()
                };
                plan.map(|plan| drain_relay_mailboxes(&plan))
            } else {
                None
            };
            // Same three phases for OUTBOUND messages, and for the same reason — more so, in fact.
            // A send is a peer dial (up to PEER_DIAL_TIMEOUT) plus, on failure, a relay post per
            // target (up to RELAY_DIAL_TIMEOUT each), and it used to run inside `apply_tick` with
            // the core lock held. On a device whose circuits time out that pins the lock for
            // minutes, which stalls every UI read *and* the teardown itself — so the app could not
            // be reset precisely when it was most broken (phone, 2026-08-03).
            let mut sent = {
                let plan = {
                    let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
                    g.me.plan_pending_sends()
                };
                plan.map(|plan| crate::node::execute_sends(&plan))
            };
            // Apply even if a stop landed during the drain: a relay `take` is destructive, so the
            // blobs in hand exist nowhere else and dropping them here would lose messages. The
            // loop exits at the top instead, and `shutdown` waits for that.
            //
            // Recover from poisoning, and run the apply inside `catch_unwind` (§1.5.3): a panic in
            // one tick (e.g. a malformed frame) must not kill the background poller for good — and,
            // because the unwind is caught before the guard drops, it also can't *poison* the mutex.
            {
                let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = g.apply_tick(relay_due, harvest.take(), sent.take());
                    // Flush any debounced background write whose window has elapsed (§1.5.4).
                    g.maybe_flush();
                }));
            }
            if relay_due {
                last_relay = Some(std::time::Instant::now());
            }
            // Cover traffic (#4) runs on its own randomised clock, deliberately NOT tied to the
            // relay poll: posting cover exactly when we drain would pair every dummy with a take,
            // which is a pattern in itself.
            if COVER_TRAFFIC.load(Ordering::Relaxed) {
                if next_cover.is_none_or(|t: std::time::Instant| t.elapsed() >= cover_delay) {
                    {
                        let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            g.me.send_cover_traffic();
                        }));
                    }
                    next_cover = Some(std::time::Instant::now());
                    cover_delay = next_cover_delay();
                }
            } else {
                next_cover = None; // turning it off resets the clock; on again re-samples
            }
        }
        // Drop our handle on the core — and through it the transport, if we are the last holder —
        // *before* the exit guard fires. `wait_for_exit` promises the caller that this thread is
        // holding nothing, and a closure's captures otherwise outlive its body.
        drop(inner);
    });
}

/// Parse `nightdrop://pair?addr=...&ik=...&otk=...` into (address, pre-key bundle).
pub(crate) fn parse_invite(payload: &str) -> Result<(String, PreKeyBundle)> {
    let query = payload
        .split_once("://pair?")
        .map(|(_, q)| q)
        .ok_or_else(|| anyhow::anyhow!("not a Night Drop invite"))?;
    let (mut addr, mut ik, mut otk) = (None, None, None);
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            match k {
                "addr" => addr = Some(v.to_string()),
                "ik" => ik = Some(v.to_string()),
                "otk" => otk = Some(v.to_string()),
                _ => {}
            }
        }
    }
    Ok((
        addr.ok_or_else(|| anyhow::anyhow!("invite missing addr"))?,
        PreKeyBundle {
            identity_key: ik.ok_or_else(|| anyhow::anyhow!("invite missing ik"))?,
            one_time_key: otk.ok_or_else(|| anyhow::anyhow!("invite missing otk"))?,
        },
    ))
}

const WORDS: &[&str] = &[
    "cedar", "lantern", "river", "ember", "willow", "cobalt", "harbor", "thistle", "quartz",
    "meadow", "cinder", "aurora", "marble", "nimbus", "raven", "saffron",
];

/// Generate a `slot-secret-words` short code for display in the QR/Invite tab (§5b).
fn random_short_code() -> String {
    let mut rng = rand::thread_rng();
    let slot = rng.gen_range(1..=99);
    let words: Vec<&str> = WORDS.choose_multiple(&mut rng, 3).cloned().collect();
    format!("{slot}-{}", words.join("-"))
}

/// A non-secret rendezvous slot (unguessable handle).
fn random_slot() -> String {
    const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..6)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

/// The shared secret words for a short code. NOTE: this is a moderate-entropy secret used
/// to password-encrypt the rendezvous blob (Argon2). For low-entropy secrets, production
/// should add the interactive SPAKE2 bouncer (the `pake` module) to defeat offline
/// dictionary attacks by the relay — see `ARCHITECTURE.md` §5b.
fn random_secret_words() -> String {
    let mut rng = rand::thread_rng();
    let words: Vec<&str> = WORDS.choose_multiple(&mut rng, 4).cloned().collect();
    words.join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `shutdown` must not return until the background poller has actually exited. Stopping it and
    /// returning — which is what this did — leaves the poller holding whatever it snapshotted, and
    /// on Tor that includes arti's exclusive state lock: the replacement client then comes up
    /// read-only and cannot persist the guards a heal just picked, so the heal repeats forever.
    #[test]
    fn shutdown_does_not_return_until_the_poller_has_exited() {
        let net = MemoryNetwork::new();
        let core = NightdropCore::new_with_transport(Box::new(net.endpoint("me")), None, true);
        let poller = core
            .poller
            .clone()
            .expect("a background poller was requested");

        assert!(!poller.exited(), "the poller runs until it is shut down");
        core.shutdown();
        assert!(
            poller.exited(),
            "shutdown returned with the poller still alive"
        );
    }

    /// The whole delivery lifecycle through the real poll cycle, not by calling the pieces.
    ///
    /// The unit tests drive `sweep_unconfirmed` directly; this proves it is actually wired into
    /// `apply_tick`, which is the only thing that makes any of it run on a device. Also checks the
    /// honest intermediate: a message is *not* confirmed the moment the dial works.
    #[test]
    fn the_poll_cycle_confirms_a_message_or_falls_back_to_the_relay() {
        let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap().to_string();
        let relay = RelayClient::new(relay_addr);
        let net = MemoryNetwork::new();
        let a = NightdropCore::new_with_transport(
            Box::new(net.endpoint("a.onion")),
            Some(relay.clone()),
            false,
        );
        let b = NightdropCore::new_with_transport(
            Box::new(net.endpoint("b.onion")),
            Some(relay.clone()),
            false,
        );

        let invite = a.create_invite().unwrap();
        let b_contact = b.connect_via_qr(&invite.qr_payload).unwrap().id;
        a.poll_once().unwrap();
        let a_contact = a.incoming_requests()[0].id.clone();
        a.authorize(&a_contact, true).unwrap();
        b.poll_once().unwrap();

        // A message whose dial succeeds is NOT confirmed by that alone.
        a.send_message(&a_contact, "confirm me").unwrap();
        let sent_id = a.messages(&a_contact).last().unwrap().msg_id.clone();
        let status = |c: &NightdropCore, id: &str| {
            c.messages(&a_contact)
                .into_iter()
                .find(|m| m.msg_id == id)
                .unwrap()
                .delivery
        };
        assert_eq!(
            status(&a, &sent_id),
            "sent",
            "a successful dial is not a confirmation"
        );

        // B polls: it receipts what it took, and A's next poll settles the message.
        b.poll_once().unwrap();
        a.poll_once().unwrap();
        assert_eq!(
            status(&a, &sent_id),
            "delivered",
            "the receipt B sent on its poll is what confirms it"
        );

        // Now the case the fallback exists for: the dial works, but B never processes it — the
        // shape of a core torn down with the frame still buffered.
        a.send_message(&a_contact, "did this survive?").unwrap();
        let lost_id = a.messages(&a_contact).last().unwrap().msg_id.clone();
        assert_eq!(status(&a, &lost_id), "sent");

        // Age it past the receipt window and run an ordinary poll cycle — nothing bespoke.
        a.lock()
            .me
            .backdate_unconfirmed(crate::node::RECEIPT_TIMEOUT.as_secs() + 1);
        a.poll_once().unwrap();
        assert_eq!(
            status(&a, &lost_id),
            "queued",
            "the poll cycle must put a relay copy behind an unconfirmed message — if this fails, \
             sweep_unconfirmed is not wired into apply_tick and none of it runs on a device"
        );

        // B collects it from the relay and it settles, having never been processed directly.
        b.poll_once().unwrap();
        assert!(
            b.messages(&b_contact)
                .iter()
                .any(|m| !m.from_me && m.text == "did this survive?"),
            "the relay copy is what actually delivers it"
        );
        a.poll_once().unwrap();
        assert_eq!(status(&a, &lost_id), "delivered");
    }

    /// A slow send must not hold the core lock. This is the property the whole plan/execute/apply
    /// split exists for, so it is asserted directly rather than inferred from where the code sits.
    ///
    /// The dial used to run inside `apply_tick`, under the lock. A dial that hangs — a dead peer,
    /// a dead relay, a device whose circuits are timing out — therefore froze every other caller
    /// for as long as it took: UI reads, and the teardown itself, which is how "Reset Tor
    /// connection" came to do nothing at all on a wedged phone (2026-08-03).
    #[test]
    fn a_slow_send_does_not_hold_the_core_lock() {
        // A transport whose send blocks until released, standing in for a dial into a black hole.
        struct BlockingTransport {
            inner: crate::transport::MemoryTransport,
            /// Only blocks once armed, so pairing and approval (which send control frames on this
            /// same transport) still complete normally.
            armed: Arc<AtomicBool>,
            entered: Arc<AtomicBool>,
            release: Arc<AtomicBool>,
        }
        impl Transport for BlockingTransport {
            fn address(&self) -> String {
                self.inner.address()
            }
            fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
                if !self.armed.load(Ordering::Relaxed) {
                    return self.inner.send(peer, frame);
                }
                self.entered.store(true, Ordering::Relaxed);
                while !self.release.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(5));
                }
                anyhow::bail!("never got there")
            }
            fn try_recv(&self) -> Option<(String, Vec<u8>)> {
                self.inner.try_recv()
            }
        }

        let net = MemoryNetwork::new();
        let armed = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let core = NightdropCore::new_with_transport(
            Box::new(BlockingTransport {
                inner: net.endpoint("me.onion"),
                armed: Arc::clone(&armed),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
            None,
            false,
        );

        // Pair with a peer so there is somewhere to send.
        let invite = core.create_invite().unwrap();
        let mut peer = crate::node::Node::new(Box::new(net.endpoint("peer.onion")));
        let (addr, bundle) = parse_invite(&invite.qr_payload).unwrap();
        peer.connect_with_bundle(&addr, &bundle).unwrap();
        core.poll_once().unwrap();
        let contact = core.incoming_requests()[0].id.clone();
        core.authorize(&contact, true).unwrap();

        // From here on, dialling hangs. Queue a send and drive a tick on another thread.
        armed.store(true, Ordering::Relaxed);
        core.send_message(&contact, "into the void").unwrap();
        let driver = {
            let inner = Arc::clone(&core.inner);
            thread::spawn(move || {
                let plan = {
                    let mut g = inner.lock().unwrap();
                    g.me.plan_pending_sends()
                };
                plan.map(|p| crate::node::execute_sends(&p))
            })
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !entered.load(Ordering::Relaxed) {
            assert!(std::time::Instant::now() < deadline, "send never started");
            thread::sleep(Duration::from_millis(5));
        }

        // THE ASSERTION: the send is mid-dial, and the lock is free.
        assert!(
            core.inner.try_lock().is_ok(),
            "a send in flight is holding the core lock — every other caller, including the \
             teardown, is stuck behind it"
        );

        release.store(true, Ordering::Relaxed);
        let outcomes = driver.join().unwrap().expect("a send was planned");
        core.lock().me.apply_send_outcomes(outcomes);
    }

    /// `shutdown` must not block on the core lock, however long someone else holds it.
    ///
    /// It used to take the lock outright, and the poller holds that lock for a whole tick —
    /// including peer dials and relay round-trips. On a phone whose circuits were timing out
    /// (2026-08-03) the user tapped "Reset Tor connection" and *nothing happened*: shutdown was
    /// parked on the lock, so the teardown, the guard reset and the rebuild never ran, and no
    /// error was reported. The bounded wait that followed was irrelevant — control never got there.
    #[test]
    fn shutdown_does_not_block_on_a_held_core_lock() {
        let net = MemoryNetwork::new();
        let core = NightdropCore::new_with_transport(Box::new(net.endpoint("me")), None, false);

        // Hold the core lock the way a poller tick does, for far longer than shutdown may wait.
        let inner = Arc::clone(&core.inner);
        let release = Arc::new(AtomicBool::new(false));
        let holder = {
            let release = Arc::clone(&release);
            thread::spawn(move || {
                let _guard = inner.lock().unwrap();
                while !release.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(10));
                }
            })
        };
        // Make sure the holder really has it before shutting down.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while core.inner.try_lock().is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "holder never took the lock"
            );
            thread::sleep(Duration::from_millis(5));
        }

        let started = std::time::Instant::now();
        core.shutdown();
        let waited = started.elapsed();
        release.store(true, Ordering::Relaxed);
        holder.join().unwrap();

        assert!(
            waited < Duration::from_secs(4),
            "shutdown blocked for {waited:?} on a lock someone else was holding — that is the hang \
             that made a menu action do nothing at all"
        );
    }

    /// A core with no poller thread must not make `shutdown` wait for one. It used to hold a stop
    /// signal either way, so a `background: false` core sat out the entire timeout waiting for an
    /// exit nobody would ever signal, then blamed the poller for it.
    #[test]
    fn shutdown_returns_at_once_when_there_is_no_poller_thread() {
        let net = MemoryNetwork::new();
        let core = NightdropCore::new_with_transport(Box::new(net.endpoint("me")), None, false);

        let started = std::time::Instant::now();
        core.shutdown();
        assert!(
            started.elapsed() < POLLER_EXIT_TIMEOUT / 4,
            "shutdown waited {:?} for a poller that was never started",
            started.elapsed()
        );
    }

    /// The case that actually bit: the poller is *inside* a relay round-trip (off the core lock,
    /// §1.5.2, holding a RelayClient clone — on Tor, an `Arc<TorClient>` + the runtime) when the
    /// teardown starts. Waiting has to cover that, not just an idle poller between ticks.
    #[test]
    fn shutdown_waits_out_a_relay_poll_that_is_already_in_flight() {
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let dialer: crate::relay_client::RelayDialer = {
            let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
            // Stands in for a Tor relay round-trip: blocks until let go, like a dial that hasn't
            // timed out yet.
            Arc::new(move |_request: &str| -> Result<String> {
                entered.store(true, Ordering::Relaxed);
                while !release.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(5));
                }
                anyhow::bail!("relay unreachable")
            })
        };
        let net = MemoryNetwork::new();
        let core = NightdropCore::new_with_transport(
            Box::new(net.endpoint("me")),
            Some(RelayClient::with_dialer(dialer)),
            true,
        );
        let poller = core
            .poller
            .clone()
            .expect("a background poller was requested");

        // Wait for the poller to be inside the "round-trip" before tearing anything down.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !entered.load(Ordering::Relaxed) {
            assert!(std::time::Instant::now() < deadline, "poller never polled");
            thread::sleep(Duration::from_millis(5));
        }

        let held = Duration::from_millis(200);
        let releaser = {
            let release = Arc::clone(&release);
            thread::spawn(move || {
                thread::sleep(held);
                release.store(true, Ordering::Relaxed);
            })
        };
        let started = std::time::Instant::now();
        core.shutdown();

        let waited = started.elapsed();
        assert!(
            waited >= held / 2,
            "shutdown returned while the poller was still in a relay round-trip"
        );
        // And it got there by *observing* the exit, not by timing out: on a memory transport there
        // is no arti teardown to pay for, so anything near POLLER_EXIT_TIMEOUT means the wait
        // expired and the guarantee is hollow. (The Tor-level equivalent is in `tor_smoke`.)
        assert!(
            waited < POLLER_EXIT_TIMEOUT / 2,
            "shutdown took {waited:?} — it hit its own timeout rather than seeing the poller exit"
        );
        assert!(poller.exited(), "the poller outlived shutdown");
        releaser.join().unwrap();
    }

    // Device bug, 2026-08-02. A wipe left `onion-key.sealed` behind; the next identity had a new
    // store key, so the file would not unseal; and because start-up read it unconditionally, the
    // hard failure hit *every* path — including "set up a new identity", the only way off the
    // load-error screen. The app could not be recovered from inside.
    //
    // The restore path must still fail loudly (silently minting a new address strands every
    // contact), so this pins both halves, not just the one that broke.
    #[test]
    fn a_stale_onion_key_blocks_a_restore_but_never_a_new_identity() {
        let dir = std::env::temp_dir().join(format!("nd-onion-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_str().unwrap();
        let key: crate::storage::StoreKey = [7u8; 32];
        let other: crate::storage::StoreKey = [9u8; 32];

        write_onion_key(path, &key, &[42u8; 64]).unwrap();

        // Restoring, and the key matches: the identity comes back, so the address is kept.
        assert_eq!(
            onion_key_for_start(path, &key, true).unwrap(),
            Some([42u8; 64])
        );
        // Restoring, but the file will not unseal: fail, rather than start on a new address.
        assert!(onion_key_for_start(path, &other, true).is_err());
        // Creating a new identity: the leftover file is simply not consulted. This is the one
        // that was broken — it returned the error above, and nothing could get past it.
        assert_eq!(onion_key_for_start(path, &other, false).unwrap(), None);

        // And the new identity's own key overwrites it, so the leftover does not linger.
        write_onion_key(path, &other, &[1u8; 64]).unwrap();
        assert_eq!(
            onion_key_for_start(path, &other, true).unwrap(),
            Some([1u8; 64])
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// arti's on-disk keystore must survive only the one run that still needs it, and in
    /// particular must never be inherited by a NEW identity — which would launch it on the old
    /// `.onion`, reachable by everyone who knew the address the user was walking away from.
    #[test]
    fn a_leftover_arti_keystore_is_never_inherited_by_a_new_identity() {
        let dir = std::env::temp_dir().join(format!("nd-keystore-{}", rand::random::<u64>()));
        let keystore = dir.join("arti-state/keystore");
        let identity = keystore.join("hss/nightdrop/ks_hs_id.ed25519_expanded_private");
        let plant = || {
            std::fs::create_dir_all(identity.parent().unwrap()).unwrap();
            std::fs::write(&identity, b"the previous identity").unwrap();
            assert!(keystore.exists());
        };
        let path = dir.to_str().unwrap();

        // Restoring, no sealed key yet: the migration run (and the run after a backup restore).
        // This is the ONE case arti is still allowed to read it.
        plant();
        drop_superseded_keystore(path, false, true);
        assert!(
            keystore.exists(),
            "the migration run needs the on-disk keystore — removing it here mints a new address \
             and strands every contact"
        );

        // Restoring, identity already sealed: leftovers, including a directory per contact named
        // after their onion address.
        drop_superseded_keystore(path, true, true);
        assert!(!keystore.exists(), "a superseded keystore must not linger");

        // Creating a new identity, with a keystore left behind by a failed wipe or an install that
        // never migrated. This is the case that used to silently reuse the old address.
        plant();
        drop_superseded_keystore(path, false, false);
        assert!(
            !keystore.exists(),
            "a new identity must not inherit the previous one's onion — that is the linkage the \
             whole design exists to prevent"
        );

        // Nothing there at all: no panic, no error.
        drop_superseded_keystore(path, false, false);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn store_key_round_trips_and_rejects_bad_input() {
        // The zeroization hardening must not change behavior: a generated key still decodes back to
        // its 32 bytes, and malformed input is still rejected.
        let b64 = random_store_key();
        let key = decode_store_key(&b64).expect("a freshly generated key decodes");
        assert_eq!(key.len(), 32);
        assert_eq!(
            decode_store_key(&b64).unwrap(),
            key,
            "decode is deterministic"
        );
        assert!(decode_store_key("not base64!!").is_err());
        // Valid base64 but the wrong length (16 bytes) must be rejected.
        use base64::Engine as _;
        let short = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
        assert!(decode_store_key(&short).is_err());
    }

    #[test]
    fn a_poisoned_lock_is_recovered_not_fatal() {
        let core = NightdropCore::new();
        core.open_chat(None).unwrap();
        assert_eq!(core.contacts().len(), 1);

        // Poison the inner mutex: a thread panics while holding the guard (join swallows it).
        let inner = Arc::clone(&core.inner);
        let joined = std::thread::spawn(move || {
            let _g = inner.lock().unwrap();
            panic!("boom while holding the core lock");
        })
        .join();
        assert!(joined.is_err(), "the helper thread panicked as intended");
        assert!(core.inner.lock().is_err(), "the mutex is now poisoned");

        // Despite the poison, the core keeps working: reads and mutations both recover the guard.
        assert_eq!(core.contacts().len(), 1);
        core.open_chat(None).unwrap();
        assert_eq!(core.contacts().len(), 2);
        core.send_message(&core.contacts()[0].id, "still alive")
            .unwrap();
    }

    #[test]
    fn background_saves_coalesce_while_user_saves_write_immediately() {
        // §1.5.4: user-initiated saves write now; high-frequency background churn is debounced.
        let dir = std::env::temp_dir().join(format!("nightdrop-154-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state").to_string_lossy().into_owned();
        let key: crate::storage::StoreKey = [3u8; 32];
        let net = MemoryNetwork::new();
        let me = Node::with_identity(LocalIdentity::generate(), Box::new(net.endpoint("me")));
        let mut inner = Inner {
            me,
            demo: None,
            pending_backup: None,
            persist: Some(Persist::new(path.clone(), key)),
        };

        // A synchronous save writes immediately and clears any pending flag.
        inner.save();
        assert!(
            std::path::Path::new(&path).exists(),
            "user save must be immediate"
        );
        let written_at = inner.persist.as_ref().unwrap().last_write;
        assert!(!inner.persist.as_ref().unwrap().pending);

        // A background save inside the window coalesces: no new write, just a pending marker.
        inner.save_soon();
        assert_eq!(
            inner.persist.as_ref().unwrap().last_write,
            written_at,
            "background save within the window must not rewrite"
        );
        assert!(
            inner.persist.as_ref().unwrap().pending,
            "the coalesced change is marked pending"
        );

        // maybe_flush before the window elapses is a no-op...
        inner.maybe_flush();
        assert!(inner.persist.as_ref().unwrap().pending);
        // ...but once the window has passed, the pending change is flushed.
        inner.persist.as_mut().unwrap().last_write =
            std::time::Instant::now() - PERSIST_DEBOUNCE - Duration::from_secs(1);
        inner.maybe_flush();
        assert!(
            !inner.persist.as_ref().unwrap().pending,
            "pending change flushed after the debounce window"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_slow_relay_poll_does_not_block_ui_calls() {
        use std::time::{Duration, Instant};
        // A relay whose round-trip is deliberately slow (stands in for a laggy Tor circuit): the
        // dialer sleeps, then returns a valid empty-mailbox `Take` response.
        let slow = RelayClient::with_dialer(Arc::new(|_line: &str| {
            std::thread::sleep(Duration::from_millis(1500));
            Ok(r#"{"v":1,"resp":{"ok":true}}"#.to_string())
        }));
        let net = MemoryNetwork::new();
        // Background poller ON: it fires a relay poll immediately on startup.
        let core =
            NightdropCore::new_with_transport(Box::new(net.endpoint("me.onion")), Some(slow), true);

        // Give the poller time to enter the (slow, lock-free) relay drain.
        std::thread::sleep(Duration::from_millis(300));
        // A UI call must return promptly even with that 1.5s relay poll in flight. Before §1.5.2
        // the poller held the core lock across `take`, so this would block ~1.2s.
        let start = Instant::now();
        let _ = core.contacts();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "UI call blocked {elapsed:?} on an in-flight relay poll (lock held across Tor I/O?)"
        );
    }

    #[test]
    fn loopback_chat_round_trips_through_real_encryption() {
        let core = NightdropCore::new();
        assert!(core.contacts().is_empty());

        let contact = core.open_chat(None).unwrap();
        assert_eq!(contact.their_name, "Anon");
        assert_eq!(core.contacts().len(), 1);

        let history = core.send_message(&contact.id, "hello").unwrap();
        assert_eq!(history.len(), 2);
        assert!(history[0].from_me && history[0].text == "hello");
        assert!(!history[1].from_me && history[1].text == "(echo) hello");

        let history = core.send_message(&contact.id, "again").unwrap();
        assert_eq!(history.len(), 4);
        assert_eq!(history[3].text, "(echo) again");
    }

    #[test]
    fn invite_creates_a_request_that_must_be_authorized() {
        let core = NightdropCore::new();
        // Creating an invite simulates a peer joining -> a pending request appears.
        let _invite = core.create_invite().unwrap();
        assert_eq!(core.incoming_requests().len(), 1);
        assert!(core.contacts().is_empty(), "not a contact until approved");

        let request_id = core.incoming_requests()[0].id.clone();
        core.authorize(&request_id, true).unwrap();
        assert!(core.incoming_requests().is_empty());
        assert_eq!(core.contacts().len(), 1);

        // After approval, messaging works (and the demo peer echoes).
        let history = core.send_message(&request_id, "hi").unwrap();
        assert_eq!(history.last().unwrap().text, "(echo) hi");
    }

    #[test]
    fn declining_an_invite_request_leaves_no_contact() {
        let core = NightdropCore::new();
        core.create_invite().unwrap();
        let request_id = core.incoming_requests()[0].id.clone();
        core.authorize(&request_id, false).unwrap();
        assert!(core.incoming_requests().is_empty());
        assert!(core.contacts().is_empty());
    }

    #[test]
    fn per_chat_name_and_remote_storage_toggle() {
        let core = NightdropCore::new();
        let contact = core.open_chat(None).unwrap();

        core.set_my_name(&contact.id, "Spectre").unwrap();
        core.set_remote_storage(&contact.id, true).unwrap();
        let c = core.contacts().into_iter().next().unwrap();
        assert_eq!(c.my_name, "Spectre");
        assert!(c.remote_storage);

        core.set_my_name(&contact.id, "   ").unwrap();
        let c = core.contacts().into_iter().next().unwrap();
        assert_eq!(c.my_name, "Anon");
    }

    #[test]
    fn live_flow_two_real_cores_over_an_injected_transport() {
        let net = MemoryNetwork::new();
        let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
        let relay = RelayClient::new(relay_addr.to_string());

        // Two real cores (no demo peers, no background thread — we drive poll_once()).
        let a = NightdropCore::new_with_transport(
            Box::new(net.endpoint("a.onion")),
            Some(relay.clone()),
            false,
        );
        let b = NightdropCore::new_with_transport(
            Box::new(net.endpoint("b.onion")),
            Some(relay.clone()),
            false,
        );

        // A invites; B joins by scanning the QR payload.
        let invite = a.create_invite().unwrap();
        assert!(
            a.incoming_requests().is_empty(),
            "no demo peer in real mode"
        );
        let b_contact = b.connect_via_qr(&invite.qr_payload).unwrap().id;
        // The recipient must approve first (they always require authorization, §5), so the joiner
        // shows a "waiting for approval" notice right after connecting — even on the QR/invite-link
        // path, not just short codes.
        assert!(
            b.messages(&b_contact)
                .iter()
                .any(|m| m.system && m.kind == "await_approval"),
            "joiner sees the awaiting-approval notice after connecting via QR"
        );

        // Sending before approval is refused outright. Otherwise the message would sit in the
        // sender's history looking delivered while the peer's core drops it as unauthorized.
        let refused = b
            .send_message(&b_contact, "too early")
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("accept the chat"),
            "send before approval is refused, got: {refused}"
        );
        assert!(
            !b.messages(&b_contact).iter().any(|m| m.text == "too early"),
            "a refused send leaves nothing behind in the chat"
        );

        // A polls and sees the pending request; approves it.
        a.poll_once().unwrap();
        assert_eq!(a.incoming_requests().len(), 1);
        let a_contact = a.incoming_requests()[0].id.clone();
        a.authorize(&a_contact, true).unwrap();

        // B receives the approval signal, which clears the waiting notice.
        b.poll_once().unwrap();
        assert!(
            !b.messages(&b_contact)
                .iter()
                .any(|m| m.system && m.kind == "await_approval"),
            "the approval signal clears the joiner's waiting notice"
        );

        // B -> A, delivered by A's poll loop.
        b.send_message(&b_contact, "live hello").unwrap();
        a.poll_once().unwrap();
        assert!(a
            .messages(&a_contact)
            .iter()
            .any(|m| !m.from_me && m.text == "live hello"));

        // A -> B.
        a.send_message(&a_contact, "live reply").unwrap();
        b.poll_once().unwrap();
        assert!(b
            .messages(&b_contact)
            .iter()
            .any(|m| !m.from_me && m.text == "live reply"));

        // Offline delivery: B drops off the network; A's send falls back to the relay; B
        // picks it up from its mailbox on the next poll.
        net.disconnect("b.onion");
        a.send_message(&a_contact, "while you were gone").unwrap();
        b.poll_once().unwrap();
        assert_eq!(
            b.messages(&b_contact).last().unwrap().text,
            "while you were gone"
        );
    }

    #[test]
    fn two_networked_clients_pair_and_chat_over_tcp() {
        use std::time::{Duration, Instant};

        let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap().to_string();

        // Two independent clients on their own TCP listeners + a shared relay (background
        // pollers deliver inbound asynchronously, just like the running app).
        let a = NightdropCore::new_networked("127.0.0.1:0".into(), relay_addr.clone()).unwrap();
        let b = NightdropCore::new_networked("127.0.0.1:0".into(), relay_addr.clone()).unwrap();

        // A makes a short code; B joins by fetching the rendezvous blob and dialing A.
        let code = a.create_short_code_invite().unwrap();
        let b_contact = b.join_via_short_code(&code).unwrap().id;

        // A's poller sees the request; approve it.
        let a_contact = wait_for(Duration::from_secs(5), || {
            a.incoming_requests().first().map(|c| c.id.clone())
        })
        .expect("A receives the pairing request over TCP");
        a.authorize(&a_contact, true).unwrap();

        // Wait until B has actually received the approval: sending before that is refused, since
        // the message would only be dropped by A as unauthorized.
        wait_for(Duration::from_secs(5), || {
            (!b.messages(&b_contact)
                .iter()
                .any(|m| m.system && m.kind == "await_approval"))
            .then_some(())
        })
        .expect("B learns it was approved");

        // B -> A over real TCP sockets.
        b.send_message(&b_contact, "hello from B").unwrap();
        let ok = wait_for(Duration::from_secs(5), || {
            a.messages(&a_contact)
                .iter()
                .any(|m| !m.from_me && m.text == "hello from B")
                .then_some(())
        });
        assert!(ok.is_some(), "A received B's message over TCP");

        // A -> B.
        a.send_message(&a_contact, "hi back from A").unwrap();
        let ok = wait_for(Duration::from_secs(5), || {
            b.messages(&b_contact)
                .iter()
                .any(|m| !m.from_me && m.text == "hi back from A")
                .then_some(())
        });
        assert!(ok.is_some(), "B received A's reply over TCP");

        // Helper: poll a closure until it yields Some or the deadline passes.
        fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if let Some(v) = f() {
                    return Some(v);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            None
        }
    }

    #[test]
    fn short_code_rendezvous_pairing_over_two_cores() {
        use std::time::Duration;

        let net = MemoryNetwork::new();
        let relay = RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string());
        let a = NightdropCore::new_with_transport(
            Box::new(net.endpoint("a")),
            Some(relay.clone()),
            false,
        );
        let b = NightdropCore::new_with_transport(
            Box::new(net.endpoint("b")),
            Some(relay.clone()),
            false,
        );

        // Drive the joiner's interactive SPAKE2 (which blocks on relay round-trips) on a thread
        // while manually pumping the inviter so it answers the opener — these cores have no
        // background poller. Returns the joiner's result.
        let join = |b: &NightdropCore, a: &NightdropCore, code: &str| -> Result<Contact> {
            std::thread::scope(|s| {
                let joiner = s.spawn(|| b.join_via_short_code(code));
                while !joiner.is_finished() {
                    a.poll_once().unwrap(); // services the pending invite (answers SPAKE2)
                    std::thread::sleep(Duration::from_millis(20));
                }
                joiner.join().unwrap()
            })
        };

        // A publishes a short code; B joins via the interactive SPAKE2 handshake.
        let code = a.create_short_code_invite().unwrap();
        let b_contact = join(&b, &a, &code).unwrap().id;

        a.poll_once().unwrap();
        let a_contact = a.incoming_requests()[0].id.clone();
        a.authorize(&a_contact, true).unwrap();

        // B must hear the approval before it may send (see the guard in Node::send).
        b.poll_once().unwrap();
        b.send_message(&b_contact, "short code works").unwrap();
        a.poll_once().unwrap();
        assert!(a
            .messages(&a_contact)
            .iter()
            .any(|m| m.text == "short code works"));

        // A wrong secret derives a different SPAKE2 key, so opening the sealed payload fails —
        // and, crucially, the relay never saw anything it could dictionary-attack offline.
        let code2 = a.create_short_code_invite().unwrap();
        let slot2 = code2.split_once('-').unwrap().0;
        assert!(join(&b, &a, &format!("{slot2}-wrong-secret-words-x")).is_err());
    }

    #[test]
    fn short_code_has_slot_and_words() {
        let code = random_short_code();
        let parts: Vec<&str> = code.split('-').collect();
        assert_eq!(parts.len(), 4, "slot + 3 words: {code}");
        assert!(parts[0].parse::<u32>().is_ok());
    }

    #[test]
    fn cover_traffic_intervals_are_random_and_floored() {
        // #4: a fixed cadence is its own fingerprint — an observer subtracts every post on a
        // 30-minute boundary and reads what's left — so intervals are drawn from an exponential
        // distribution. Assert they actually vary, and that the floor holds against its long tail
        // (which would otherwise burst, costing the user battery and the relay operator bandwidth).
        let draws: Vec<Duration> = (0..200).map(|_| next_cover_delay()).collect();
        let unique: std::collections::HashSet<u64> =
            draws.iter().map(|d| d.as_millis() as u64).collect();
        assert!(unique.len() > 100, "intervals must not be a fixed period");
        assert!(
            draws.iter().all(|d| *d >= COVER_MIN),
            "no draw may fall below the floor"
        );
        // Sanity on the shape: an exponential around COVER_MEAN should not have every draw pinned
        // at the floor, or the randomness is doing nothing.
        assert!(
            draws.iter().any(|d| *d > COVER_MEAN),
            "the distribution should reach past its mean"
        );
    }

    #[test]
    fn server_backup_opt_in_returns_a_one_time_password_and_exact_expiry() {
        let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap().to_string();
        let a = NightdropCore::new_networked("127.0.0.1:0".into(), relay_addr).unwrap();

        let info = a.create_server_backup(24, true).unwrap();
        assert!(
            !info.password.is_empty(),
            "a recovery password is returned once"
        );
        assert!(info.expires_at_secs > now_secs(), "expiry is in the future");
        assert!(
            info.expires_at_secs <= now_secs() + 24 * 3600 + 5,
            "24h backup expires ~24h out"
        );

        // The TTL is clamped to the 36h cap even if a longer window is requested.
        let capped = a.create_server_backup(999, true).unwrap();
        assert!(
            capped.expires_at_secs <= now_secs() + 36 * 3600 + 5,
            "clamped to 36h"
        );

        // Two backups mint independent one-time passwords (never reused/persisted).
        assert_ne!(info.password, capped.password);
    }

    #[test]
    fn server_backup_requires_a_relay() {
        // With no relay attached, server backup is unavailable (nowhere to store the blob).
        let net = MemoryNetwork::new();
        let a = NightdropCore::new_with_transport(Box::new(net.endpoint("a")), None, false);
        assert!(a.create_server_backup(24, true).is_err());
    }
}
