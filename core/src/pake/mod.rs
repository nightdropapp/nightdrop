//! The "bouncer": a symmetric PAKE (SPAKE2 over Ed25519) keyed on the short-code secret
//! words (`ARCHITECTURE.md` §5b). Each side proves it knows the secret without ever
//! sending it; if they agree, both derive the same key. A wrong secret yields a
//! different key and the handshake is rejected. This authorizes the stranger AND blocks
//! a man-in-the-middle, so the rendezvous mailbox is never trusted.
//!
//! Protocol: each side calls [`start`] to get its `(state, outbound_msg)`, ships the
//! outbound message to the peer, then calls [`Bouncer::finish`] with the peer's message
//! to obtain the shared key. The two sides must agree on a fixed `context` string.

use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::Result;

/// Domain-separation context bound into every Night Drop PAKE run.
pub const CONTEXT: &[u8] = b"night-drop/pake/v1";

/// In-progress PAKE state. Hold it only until [`finish`](Bouncer::finish); the secret
/// words must not be persisted (the app passes them in transiently — see §5b/§7 wiping).
pub struct Bouncer {
    inner: Spake2<Ed25519Group>,
}

/// Begin a symmetric PAKE from the shared secret words. Returns the state plus the
/// message to send to the peer.
pub fn start(secret_words: &[u8]) -> (Bouncer, Vec<u8>) {
    let (inner, outbound) = Spake2::<Ed25519Group>::start_symmetric(
        &Password::new(secret_words),
        &Identity::new(CONTEXT),
    );
    (Bouncer { inner }, outbound)
}

impl Bouncer {
    /// Complete the handshake with the peer's message. On success both sides hold the
    /// same key iff they used the same secret words.
    pub fn finish(self, peer_message: &[u8]) -> Result<Vec<u8>> {
        self.inner
            .finish(peer_message)
            .map_err(|e| anyhow::anyhow!("PAKE failed: {e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_secret_yields_matching_key() {
        let secret = b"4-cedar-lantern-river";
        let (a, a_msg) = start(secret);
        let (b, b_msg) = start(secret);
        let ka = a.finish(&b_msg).unwrap();
        let kb = b.finish(&a_msg).unwrap();
        assert_eq!(ka, kb);
        assert!(!ka.is_empty());
    }

    #[test]
    fn mismatched_secret_yields_different_key() {
        // SPAKE2 still "succeeds" cryptographically, but the derived keys differ, so a
        // subsequent key-confirmation step fails — the bouncer turns the imposter away.
        let (a, a_msg) = start(b"correct horse");
        let (b, b_msg) = start(b"wrong battery");
        let ka = a.finish(&b_msg).unwrap();
        let kb = b.finish(&a_msg).unwrap();
        assert_ne!(ka, kb);
    }
}
