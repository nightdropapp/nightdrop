//! The in-process demo peer — a zero-config, no-network fallback used by the app's dev
//! path and by the `api` unit tests, kept **out** of the production [`Inner`](super::Inner)
//! so the shipped core carries no demo branches beyond a single `Option<Demo>`.
//!
//! A [`Demo`] owns an in-memory [`MemoryNetwork`] and a set of auto-replying [`Node`] peers.
//! When present, the core stands up a genuine second `Node` per chat over that network: the
//! two really pair and exchange ratcheted messages, and the peer echoes each message back so
//! the UI feels alive. A real (Tor/LAN/TCP) core holds `demo: None` and never touches any of
//! this. Swapping the transport for Tor + relay does not change the [`NightdropCore`](super::NightdropCore)
//! surface — only whether a `Demo` is attached.

use std::collections::HashMap;

use crate::identity::PreKeyBundle;
use crate::node::Node;
use crate::transport::{MemoryNetwork, Transport};
use crate::Result;

/// The in-process demo harness: an in-memory network plus its auto-replying peers.
pub(super) struct Demo {
    net: MemoryNetwork,
    peers: HashMap<String, Node>,
    counter: usize,
}

impl Demo {
    /// Stand up a demo network and hand back the local ("me") endpoint to build the core's
    /// own [`Node`] on, so the core and its demo peers share one in-memory network.
    pub(super) fn new() -> (Self, Box<dyn Transport>) {
        let net = MemoryNetwork::new();
        let me: Box<dyn Transport> = Box::new(net.endpoint("me"));
        (
            Self {
                net,
                peers: HashMap::new(),
                counter: 0,
            },
            me,
        )
    }

    /// Simulate a peer joining via an invite: a fresh peer connects to `me` with the published
    /// bundle, producing a pending request for the user to approve.
    pub(super) fn simulate_join(&mut self, me: &mut Node, bundle: &PreKeyBundle) -> Result<()> {
        self.counter += 1;
        let peer_address = format!("demo-peer-{}", self.counter);
        let mut peer = Node::new(Box::new(self.net.endpoint(&peer_address)));
        let my_address = me.address();
        peer.connect_with_bundle(&my_address, bundle)?;
        me.pump()?;
        self.peers.insert(peer.identity_key(), peer);
        Ok(())
    }

    /// Open a chat against a fresh in-process peer (the genuine bundle handshake). Returns the
    /// new contact id.
    pub(super) fn open_chat(&mut self, me: &mut Node) -> Result<String> {
        self.counter += 1;
        let peer_address = format!("demo-peer-{}", self.counter);
        let mut peer = Node::new(Box::new(self.net.endpoint(&peer_address)));
        let bundle = peer.publish_bundle();
        let contact_id = me.connect_with_bundle(&peer_address, &bundle)?;
        peer.pump()?;
        self.peers.insert(contact_id.clone(), peer);
        Ok(contact_id)
    }

    /// One echo tick: let each peer reply "(echo) …" to anything it received, then pump `me`
    /// to pick the echoes up. Returns whether `me` gained anything new.
    pub(super) fn echo_tick(&mut self, me: &mut Node) -> Result<bool> {
        if self.peers.is_empty() {
            return Ok(false);
        }
        for peer in self.peers.values_mut() {
            for (peer_contact, received) in peer.pump()? {
                let _ = peer.send(&peer_contact, &format!("(echo) {received}"));
            }
        }
        Ok(!me.pump()?.is_empty())
    }

    /// Drop the peer backing a chat (on decline / delete).
    pub(super) fn drop_peer(&mut self, contact_id: &str) {
        self.peers.remove(contact_id);
    }
}
