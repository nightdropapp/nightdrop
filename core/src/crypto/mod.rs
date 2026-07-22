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
    Ok(local.account().create_outbound_session(
        SessionConfig::version_2(),
        identity_key,
        one_time_key,
    ))
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
    let result = local
        .account_mut()
        .create_inbound_session(their_key, pre_key)
        .context("create inbound session")?;
    Ok(Accepted {
        session: result.session,
        first_plaintext: result.plaintext,
    })
}

/// Encrypt one message on an established session (advances the ratchet).
pub fn encrypt(session: &mut Session, plaintext: &[u8]) -> OlmMessage {
    session.encrypt(plaintext)
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
        let first = encrypt(&mut alice_session, b"hi bob");

        // Bob accepts it, deriving his session and the first plaintext.
        let alice_identity = alice.curve25519().to_base64();
        let accepted = accept_inbound(&mut bob, &alice_identity, &first).unwrap();
        let mut bob_session = accepted.session;
        assert_eq!(accepted.first_plaintext, b"hi bob");

        // Bob replies; Alice decrypts. Then several more rounds to exercise the ratchet.
        let reply = encrypt(&mut bob_session, b"hi alice");
        assert_eq!(decrypt(&mut alice_session, &reply).unwrap(), b"hi alice");

        for i in 0..5 {
            let a = format!("from alice #{i}");
            let m = encrypt(&mut alice_session, a.as_bytes());
            assert_eq!(decrypt(&mut bob_session, &m).unwrap(), a.as_bytes());

            let b = format!("from bob #{i}");
            let m = encrypt(&mut bob_session, b.as_bytes());
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
        let first = encrypt(&mut s, b"once");
        assert!(accept_inbound(&mut bob, &alice_identity, &first).is_ok());

        // Replaying against the same consumed one-time key must fail.
        let mut s2 = open_outbound(&alice, &bundle).unwrap();
        let replay = encrypt(&mut s2, b"again");
        assert!(accept_inbound(&mut bob, &alice_identity, &replay).is_err());
    }
}
