//! Pluggable anonymity transport (`ARCHITECTURE.md` §6). Tor (`arti`, embedded — works
//! on iOS without a daemon) is the intended default; this trait leaves room for
//! I2P/Snowflake/etc. and, crucially, an in-memory implementation for tests.
//!
//! The trait is deliberately small and **synchronous/poll-based** so the security core
//! stays sync and testable. A networked implementation (e.g. Tor) runs its own async I/O
//! on a background thread and bridges to this interface via channels — async stays
//! isolated to the transport, never leaking into the ratchet/session logic.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::Result;

pub mod client_auth;
pub mod lan;
pub mod tcp;

#[cfg(feature = "tor")]
pub mod tor;

// Note: the trait is `Send + Sync` so a `Node`/`NightdropCore` holding a boxed transport can
// cross the flutter_rust_bridge opaque boundary (which requires `Sync`).

/// A peer address. For Tor this is a `.onion`; for the in-memory transport it is an
/// arbitrary unique string.
pub type Address = String;

/// One endpoint on an anonymity network. Frames are opaque, already-encrypted bytes
/// (see [`crate::wire`]); the transport never inspects them.
pub trait Transport: Send + Sync {
    /// The address peers use to reach us.
    fn address(&self) -> Address;

    /// Send a framed message to `peer`. Errors if the peer is unreachable (e.g. offline),
    /// which the caller can treat as "fall back to the relay" (§6).
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()>;

    /// Non-blocking receive of the next inbound `(sender_address, frame)`, if any.
    fn try_recv(&self) -> Option<(Address, Vec<u8>)>;

    /// Whether our address is published/reachable so peers can actually reach us. For Tor this
    /// is false for the ~1–3 min after launch while the onion descriptor (re)publishes; other
    /// transports are reachable immediately (default `true`).
    ///
    /// This is a **UI-facing** question ("can others pair with me yet") and nothing more. It is
    /// deliberately not used to decide that anything is broken: it was measured reading false on a
    /// fully published service, and a guard heal keyed off it destroyed healthy guard sets.
    fn published(&self) -> bool {
        true
    }

    /// Whether `send` completes instantly and locally, so it's safe to run inline while the core
    /// lock is held. True for the in-memory transport (tests/demo); false for any real network
    /// transport, where a dial can block for seconds. A `false` here lets the node deliver
    /// **off the hot path**: it stores the message immediately and hands delivery to the
    /// background poller, so composing a message never blocks the UI on a Tor round-trip (§6).
    fn is_synchronous(&self) -> bool {
        false
    }

    /// Build a relay round-trip dialer for an **arbitrary** relay address, if this transport can
    /// reach relays by address over its anonymized path (Tor does). Returns `None` for transports
    /// that don't (tests/TCP), so the node falls back to a direct [`RelayClient::new`]. This is
    /// what lets a sender post to a *recipient-chosen* relay set (multi-relay mailboxes, #17).
    fn relay_dialer(&self, _addr: &str) -> Option<crate::relay_client::RelayDialer> {
        None
    }

    /// Fetch a small static file from an onion service over this transport's anonymized path,
    /// for the update check (`crate::update`). `Some(bytes)` on success; `None` means **this
    /// transport cannot fetch anonymously**, and the caller must then do nothing at all.
    ///
    /// `None` deliberately does NOT mean "fall back to something else". A closed transport once
    /// returned `None` from [`relay_dialer`](Transport::relay_dialer) and the node read that as
    /// "use a direct TCP client", which handed an `.onion` hostname to the system resolver — a
    /// clearnet leak from a path that was supposed to be anonymous. The update check exists to
    /// tell users about security fixes; it must never become the thing that deanonymizes them, so
    /// there is no non-Tor path here by construction and the only correct handling of `None` is to
    /// skip the check.
    fn onion_get(&self, onion: &str, port: u16, path: &str) -> Option<Result<Vec<u8>>> {
        self.onion_get_capped(onion, port, path, crate::update::MAX_MANIFEST_BYTES)
    }

    /// As [`onion_get`](Transport::onion_get) but with an explicit size cap, for the one caller
    /// that fetches something big (a build, `crate::update::download`). Split out so the manifest
    /// path keeps its tiny bound by default — a shared cap large enough for an APK would silently
    /// make the every-24h fetch unbounded too.
    fn onion_get_capped(
        &self,
        _onion: &str,
        _port: u16,
        _path: &str,
        _max_bytes: usize,
    ) -> Option<Result<Vec<u8>>> {
        None
    }

    /// As [`onion_get_capped`](Transport::onion_get_capped) but **streamed to a file**, returning
    /// the number of body bytes written. Same `None` contract: no anonymized path, do nothing.
    ///
    /// Exists because a build is tens of megabytes and returning it as a `Vec<u8>` means holding
    /// all of it in memory for the minutes the transfer takes — which on Android is precisely when
    /// the process is backgrounded and the low-memory killer is choosing a victim. Streaming keeps
    /// resident memory at one small buffer.
    ///
    /// `dest` is written as the bytes arrive, so it is **unverified while in flight**. Callers must
    /// hand it a scratch path and only move the result somewhere the user can reach it after the
    /// hash matches — see [`crate::update::download`].
    fn onion_get_to_file(
        &self,
        _onion: &str,
        _port: u16,
        _path: &str,
        _dest: &std::path::Path,
        _max_bytes: u64,
    ) -> Option<Result<u64>> {
        None
    }

    /// Onion client authorization (#22, Tor only). Generate (and store in arti's keymgr) *our*
    /// client descriptor-encryption keypair for connecting to `peer_onion`'s (possibly restricted)
    /// onion, returning the **public** key string (`descriptor:x25519:…`) to hand the peer so they
    /// can authorize us. arti then uses the stored keypair automatically on future connects to that
    /// onion. `None` for transports without restricted discovery (everything but Tor), so the node
    /// simply skips the client-key exchange.
    /// Mint our client descriptor-encryption key for `peer_onion`'s restricted service (#22),
    /// returning the **public** half to hand the peer and the **secret** for us to keep.
    ///
    /// The secret comes back because we persist it ourselves now, sealed in the store, instead of
    /// leaving it in arti's on-disk keystore — where it sat unencrypted in a directory named after
    /// the peer's onion address (`docs/design/onion-key-at-rest.md`).
    fn make_client_key(&self, _peer_onion: &str) -> Option<Result<(String, [u8; 32])>> {
        None
    }

    /// Put a previously-saved client secret back into the keystore at startup. With the keystore
    /// in memory this is what keeps a restricted peer reachable across restarts; without it every
    /// chat would silently fall back to relay-only after each launch.
    fn insert_client_key(&self, _peer_onion: &str, _secret: &[u8; 32]) -> Result<()> {
        Ok(())
    }

    /// Authorize `contact_id` to reach our onion by writing their client public `key` into our
    /// watched authorized-keys directory ([`client_auth`], #22). No-op `Ok(())` where client auth
    /// isn't configured (non-Tor transports, or Tor launched without an auth dir).
    fn authorize_client(&self, _contact_id: &str, _key: &str) -> Result<()> {
        Ok(())
    }

    /// Revoke `contact_id`'s reachability by removing their authorized-key file (#22). No-op where
    /// client auth isn't configured.
    fn revoke_client(&self, _contact_id: &str) -> Result<()> {
        Ok(())
    }

    /// Forget the client key we hold for reaching `peer_onion`'s restricted service (#22) — the
    /// *other* direction from [`revoke_client`](Self::revoke_client), which only drops their
    /// permission to reach us.
    ///
    /// This matters beyond tidiness: arti stores that key in a directory **named after the peer's
    /// onion address**, so leaving it behind means a deleted chat's address stays on disk, and a
    /// wiped identity leaves a recoverable contact list. The key is re-derivable by re-pairing, so
    /// there is nothing to preserve and nothing to back up.
    ///
    /// No-op where client auth isn't configured.
    fn forget_peer_key(&self, _peer_onion: &str) -> Result<()> {
        Ok(())
    }
}

/// The inert transport a node is left with after [`crate::node::Node::close_transport`]: it
/// reaches no one. Its purpose is to let the *real* transport be dropped — and with it the OS
/// resources it holds — while the node itself stays alive and readable. For Tor that resource
/// is arti's on-disk state lock, which only one instance may hold per state directory (§6).
/// The old address is kept so the UI can still display who we were.
pub struct ClosedTransport {
    address: Address,
}

impl ClosedTransport {
    pub fn new(address: Address) -> Self {
        Self { address }
    }
}

impl Transport for ClosedTransport {
    fn address(&self) -> Address {
        self.address.clone()
    }

    fn send(&self, _peer: &str, _frame: &[u8]) -> Result<()> {
        anyhow::bail!("transport is closed")
    }

    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        None
    }

    /// Never reachable — a closed transport publishes nothing.
    fn published(&self) -> bool {
        false
    }

    /// A dialer that always fails — deliberately **not** `None`.
    ///
    /// `None` means "this transport has no relay dialer of its own", and `node::build_relay` reads
    /// that as permission to fall back to a plain **TCP** relay client. For the `.onion` addresses
    /// this app actually uses, that client would
    /// hand the hostname to `TcpStream::connect`, which resolves it through the **system DNS
    /// resolver** — announcing to the resolver, and to anyone watching it, exactly which hidden
    /// service this device is trying to reach. Off Tor entirely.
    ///
    /// Reachable because a closed transport is still consulted: the poller can build a drain plan,
    /// and a send can fall back to the relay, in the window between `close_transport` and the
    /// poller's exit — and any FFI call made against a core that has been shut down.
    ///
    /// A closed Tor transport must **fail**, never downgrade. See `CLAUDE.md`: never a
    /// non-anonymized network path.
    fn relay_dialer(&self, _addr: &str) -> Option<crate::relay_client::RelayDialer> {
        Some(Arc::new(|_request: &str| {
            anyhow::bail!("transport is closed")
        }))
    }
}

type Inbox = Sender<(Address, Vec<u8>)>;

/// A shared in-memory network for tests: endpoints register by address and deliver frames
/// to each other directly. Stands in for Tor so the whole messaging stack is exercisable
/// without any real networking.
#[derive(Clone, Default)]
pub struct MemoryNetwork {
    endpoints: Arc<Mutex<HashMap<Address, Inbox>>>,
}

impl MemoryNetwork {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create (and register) an endpoint at `address`.
    pub fn endpoint(&self, address: &str) -> MemoryTransport {
        let (tx, rx) = channel();
        self.endpoints
            .lock()
            .unwrap()
            .insert(address.to_string(), tx);
        MemoryTransport {
            address: address.to_string(),
            endpoints: Arc::clone(&self.endpoints),
            rx: Mutex::new(rx),
        }
    }

    /// Drop an endpoint to simulate going offline (sends to it then fail).
    pub fn disconnect(&self, address: &str) {
        self.endpoints.lock().unwrap().remove(address);
    }
}

/// An in-memory [`Transport`] endpoint produced by [`MemoryNetwork::endpoint`].
pub struct MemoryTransport {
    address: Address,
    endpoints: Arc<Mutex<HashMap<Address, Inbox>>>,
    // Mutex-wrapped so the endpoint is `Sync` (a bare `Receiver` is not).
    rx: Mutex<Receiver<(Address, Vec<u8>)>>,
}

impl Transport for MemoryTransport {
    fn address(&self) -> Address {
        self.address.clone()
    }

    /// In-memory delivery is a channel send — instant and non-blocking, so the node delivers
    /// inline and tests see synchronous send/receive without running a poller.
    fn is_synchronous(&self) -> bool {
        true
    }

    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        let endpoints = self.endpoints.lock().unwrap();
        let inbox = endpoints
            .get(peer)
            .ok_or_else(|| anyhow::anyhow!("peer {peer} is unreachable"))?;
        inbox
            .send((self.address.clone(), frame.to_vec()))
            .map_err(|_| anyhow::anyhow!("peer {peer} closed"))
    }

    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        self.rx.lock().unwrap().try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_exchange_frames() {
        let net = MemoryNetwork::new();
        let alice = net.endpoint("alice.onion");
        let bob = net.endpoint("bob.onion");

        alice.send("bob.onion", b"hello").unwrap();
        let (from, frame) = bob.try_recv().expect("bob receives");
        assert_eq!(from, "alice.onion");
        assert_eq!(frame, b"hello");

        bob.send("alice.onion", b"hi back").unwrap();
        assert_eq!(alice.try_recv().unwrap().1, b"hi back");

        // Nothing left to read.
        assert!(alice.try_recv().is_none());
    }

    #[test]
    fn sending_to_offline_peer_errors() {
        let net = MemoryNetwork::new();
        let alice = net.endpoint("alice.onion");
        net.endpoint("bob.onion");
        net.disconnect("bob.onion");
        assert!(alice.send("bob.onion", b"anyone?").is_err());
    }

    /// A closed transport must hand back a **failing** relay dialer, not `None`.
    ///
    /// `None` sends `build_relay` down its plain-TCP fallback, and a TCP connect to a `.onion`
    /// resolves the name through the system DNS resolver — telling it which hidden service this
    /// device wants. The closed transport is still consulted (a poller tick or a relay-fallback
    /// send between `close_transport` and the poller's exit, or any call against a shut-down
    /// core), so "no dialer" here is a silent way off Tor.
    #[test]
    fn a_closed_transport_fails_relay_dials_rather_than_falling_back_off_tor() {
        let closed = ClosedTransport::new("me.onion".to_string());
        let dialer = closed
            .relay_dialer("somerelay.onion")
            .expect("a closed transport must not report 'no dialer' — that means plain TCP");
        assert!(
            dialer("{\"op\":\"peek\"}").is_err(),
            "a closed transport's dialer must fail rather than reach the network"
        );
    }

    /// `published()` answers a UI question and nothing else.
    ///
    /// It used to double as the guard-heal trigger, which cost a healthy guard set on every launch
    /// (TODO.txt item 00): arti's aggregate onion-service state is bootstrap *progress*, so it read
    /// false on a service sitting on 8/8 HSDirs. The trigger that replaced it — asking arti whether
    /// its client was stuck — was then removed too, once a router-level block of every confirmed
    /// guard showed arti cold-bootstrapping back to `Running` in ~80 s on its own. Nothing in the
    /// transport layer should grow a "therefore the guards are bad" inference again.
    struct Quiet;
    impl Transport for Quiet {
        fn address(&self) -> Address {
            Address::new()
        }
        fn send(&self, _peer: &str, _frame: &[u8]) -> Result<()> {
            Ok(())
        }
        fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
            None
        }
        fn published(&self) -> bool {
            false
        }
    }

    #[test]
    fn an_unpublished_transport_exposes_no_brokenness_signal() {
        // The only thing an unpublished transport says is "not published yet". If some future
        // health check wants to act on it, that has to be a deliberate new decision with its own
        // evidence — not something silently inherited from this bit.
        let t = Quiet;
        assert!(!t.published());
        assert!(!ClosedTransport::new("me.onion".to_string()).published());
        assert!(MemoryNetwork::new().endpoint("alice").published());
    }
}
