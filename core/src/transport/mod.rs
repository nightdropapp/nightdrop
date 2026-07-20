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

    /// Onion client authorization (#22, Tor only). Generate (and store in arti's keymgr) *our*
    /// client descriptor-encryption keypair for connecting to `peer_onion`'s (possibly restricted)
    /// onion, returning the **public** key string (`descriptor:x25519:…`) to hand the peer so they
    /// can authorize us. arti then uses the stored keypair automatically on future connects to that
    /// onion. `None` for transports without restricted discovery (everything but Tor), so the node
    /// simply skips the client-key exchange.
    fn make_client_key(&self, _peer_onion: &str) -> Option<Result<String>> {
        None
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
}
