//! End-to-end encryption (`ARCHITECTURE.md` §2): X3DH-style initial agreement plus the
//! Signal Double Ratchet, via vodozemac's Olm [`Session`]. Only sender and receiver can
//! read messages; each message advances the ratchet (forward secrecy + post-compromise
//! security).
//!
//! A session is opened once, after authorization (QR bundle §5a, or a successful PAKE
//! §5b). The first ciphertext is a `PreKey` message that lets the recipient derive the
//! matching session; subsequent messages are `Normal`.

use anyhow::Context as _;
use vodozemac::olm::{OlmMessage, Session, SessionConfig};
use vodozemac::Curve25519PublicKey;

use crate::identity::{LocalIdentity, PreKeyBundle};
use crate::Result;

/// Open an **outbound** session to a peer from their pre-key bundle. Use this on the
/// side that initiated contact (scanned the QR, or completed the PAKE as the joiner).
pub fn open_outbound(local: &LocalIdentity, bundle: &PreKeyBundle) -> Result<Session> {
    let identity_key =
        Curve25519PublicKey::from_base64(&bundle.identity_key).context("bundle identity key")?;
    let one_time_key =
        Curve25519PublicKey::from_base64(&bundle.one_time_key).context("bundle one-time key")?;
    // Fails if the bundle's keys are not contributory (low-order points): a key no honest
    // client generates, which would give an insecure shared secret (vodozemac >= 0.10).
    local
        .account()
        .create_outbound_session(SessionConfig::version_2(), identity_key, one_time_key)
        .context("create outbound session")
}

/// The result of accepting a peer's first (pre-key) message: the established session and
/// the decrypted first plaintext.
pub struct Accepted {
    pub session: Session,
    pub first_plaintext: Vec<u8>,
}

/// Accept an **inbound** session from a peer's first message. `their_identity_key` is the
/// sender's long-term Curve25519 key (base64), authenticated by the handshake. Consumes
/// the matching one-time key, so it can succeed only once per pre-key.
pub fn accept_inbound(
    local: &mut LocalIdentity,
    their_identity_key: &str,
    first_message: &OlmMessage,
) -> Result<Accepted> {
    let their_key =
        Curve25519PublicKey::from_base64(their_identity_key).context("peer identity key")?;
    let pre_key = match first_message {
        OlmMessage::PreKey(m) => m,
        OlmMessage::Normal(_) => {
            anyhow::bail!("first message must be a pre-key message to open a session")
        }
    };
    // Every Night Drop build opens sessions as version 2 (`open_outbound`), and vodozemac no
    // longer infers it, so it is stated here; a version-1 peer is not one of ours.
    let result = local
        .account_mut()
        .create_inbound_session(SessionConfig::version_2(), their_key, pre_key)
        .context("create inbound session")?;
    Ok(Accepted {
        session: result.session,
        first_plaintext: result.plaintext,
    })
}

/// Encrypt one message on an established session (advances the ratchet). Fails only if the
/// peer's ratchet key is not contributory (a low-order point): no honest client sends one, so the
/// session is unusable and nothing must be sent on it.
pub fn encrypt(session: &mut Session, plaintext: &[u8]) -> Result<OlmMessage> {
    session
        .encrypt(plaintext)
        .context("encrypt: the peer's ratchet key is unusable")
}

/// Decrypt one message on an established session (advances the ratchet).
pub fn decrypt(session: &mut Session, message: &OlmMessage) -> Result<Vec<u8>> {
    session.decrypt(message).context("decrypt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_1to1_handshake_and_bidirectional_ratchet() {
        // Alice wants to message Bob. Bob has published a pre-key bundle (e.g. via QR or
        // the rendezvous after a PAKE).
        let alice = LocalIdentity::generate();
        let mut bob = LocalIdentity::generate();
        let bob_bundle = bob.publish_prekey_bundle();

        // Alice opens an outbound session and sends the first (pre-key) message.
        let mut alice_session = open_outbound(&alice, &bob_bundle).unwrap();
        let first = encrypt(&mut alice_session, b"hi bob").unwrap();

        // Bob accepts it, deriving his session and the first plaintext.
        let alice_identity = alice.curve25519().to_base64();
        let accepted = accept_inbound(&mut bob, &alice_identity, &first).unwrap();
        let mut bob_session = accepted.session;
        assert_eq!(accepted.first_plaintext, b"hi bob");

        // Bob replies; Alice decrypts. Then several more rounds to exercise the ratchet.
        let reply = encrypt(&mut bob_session, b"hi alice").unwrap();
        assert_eq!(decrypt(&mut alice_session, &reply).unwrap(), b"hi alice");

        for i in 0..5 {
            let a = format!("from alice #{i}");
            let m = encrypt(&mut alice_session, a.as_bytes()).unwrap();
            assert_eq!(decrypt(&mut bob_session, &m).unwrap(), a.as_bytes());

            let b = format!("from bob #{i}");
            let m = encrypt(&mut bob_session, b.as_bytes()).unwrap();
            assert_eq!(decrypt(&mut alice_session, &m).unwrap(), b.as_bytes());
        }
    }

    #[test]
    fn a_one_time_key_cannot_be_reused() {
        let alice = LocalIdentity::generate();
        let mut bob = LocalIdentity::generate();
        let bundle = bob.publish_prekey_bundle();
        let alice_identity = alice.curve25519().to_base64();

        let mut s = open_outbound(&alice, &bundle).unwrap();
        let first = encrypt(&mut s, b"once").unwrap();
        assert!(accept_inbound(&mut bob, &alice_identity, &first).is_ok());

        // Replaying against the same consumed one-time key must fail.
        let mut s2 = open_outbound(&alice, &bundle).unwrap();
        let replay = encrypt(&mut s2, b"again").unwrap();
        assert!(accept_inbound(&mut bob, &alice_identity, &replay).is_err());
    }

    /// vodozemac >= 0.10 refuses keys without contributory behaviour (low-order points), which
    /// would give a shared secret an attacker can predict. A bundle carrying one must not open a
    /// session. (The all-zero point is the simplest low-order point.)
    #[test]
    fn a_bundle_with_a_low_order_key_is_refused() {
        let alice = LocalIdentity::generate();
        let mut bob = LocalIdentity::generate();
        let good = bob.publish_prekey_bundle();
        let zero = Curve25519PublicKey::from_bytes([0u8; 32]).to_base64();

        let bad_identity = PreKeyBundle {
            identity_key: zero.clone(),
            ..good.clone()
        };
        assert!(open_outbound(&alice, &bad_identity).is_err());
        let bad_one_time = PreKeyBundle {
            one_time_key: zero,
            ..good.clone()
        };
        assert!(open_outbound(&alice, &bad_one_time).is_err());
        assert!(
            open_outbound(&alice, &good).is_ok(),
            "an honest bundle still works"
        );
    }
}
