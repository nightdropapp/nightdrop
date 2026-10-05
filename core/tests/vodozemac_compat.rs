//! Installs keep their identity and chats across vodozemac upgrades.
//!
//! `fixtures/vodozemac-0.8.json` was written by vodozemac 0.8.1 (Night Drop up to 0.1.27) with
//! `generate_fixture` below. It holds what an install has on disk, encrypted the way the core
//! stores it (`pickle().encrypt(key)`, `node/backup.rs`), plus messages still in flight from
//! peers on that version:
//!
//! * Alice's and Bob's accounts and their session with each other, mid-conversation;
//! * a message from each side that the other has not received yet (waiting in a mailbox);
//! * a first-contact (pre-key) message from Carol to a one-time key Bob published before the
//!   upgrade, which Bob's restored account must still accept.
//!
//! The test restores all of it with the vodozemac this crate builds with and keeps the
//! conversations going in both directions. The generator was the `generate_fixture` test in commit
//! c031723, written against the 0.8 API, which no longer compiles; the fixture itself must never be
//! regenerated with a newer vodozemac, or it stops testing anything.

use nightdrop::wire::WireOlm;
use serde::{Deserialize, Serialize};
use vodozemac::olm::{Account, AccountPickle, OlmMessage, Session, SessionConfig, SessionPickle};
use vodozemac::Curve25519PublicKey;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/vodozemac-0.8.json"
);
/// The at-rest pickle key in the fixture. A test value, not one any install uses.
const KEY: [u8; 32] = *b"nightdrop-vodozemac-compat-key!!";

#[derive(Serialize, Deserialize)]
struct Fixture {
    vodozemac: String,
    alice_account: String,
    alice_session: String,
    bob_account: String,
    bob_session: String,
    alice_identity_key: String,
    bob_identity_key: String,
    carol_identity_key: String,
    /// Alice -> Bob, sent (and saved as sent) but not yet received by Bob.
    pending_to_bob: WireOlmJson,
    /// Bob -> Alice, likewise.
    pending_to_alice: WireOlmJson,
    /// Carol's first message to Bob, to a one-time key Bob had published.
    carol_prekey_to_bob: WireOlmJson,
}

#[derive(Serialize, Deserialize, Clone)]
struct WireOlmJson {
    message_type: u8,
    body: String,
}

impl From<&OlmMessage> for WireOlmJson {
    fn from(m: &OlmMessage) -> Self {
        let w = WireOlm::from_olm(m);
        Self {
            message_type: w.message_type,
            body: w.body,
        }
    }
}

impl WireOlmJson {
    fn olm(&self) -> OlmMessage {
        WireOlm {
            message_type: self.message_type,
            body: self.body.clone(),
        }
        .to_olm()
        .expect("fixture message decodes")
    }
}

#[test]
fn state_saved_by_vodozemac_0_8_keeps_working() {
    let f: Fixture = serde_json::from_str(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
    assert_eq!(f.vodozemac, "0.8.1");

    let restore_account = |s: &str| {
        Account::from_pickle(AccountPickle::from_encrypted(s, &KEY).expect("account unpickles"))
    };
    let restore_session = |s: &str| {
        Session::from_pickle(SessionPickle::from_encrypted(s, &KEY).expect("session unpickles"))
    };

    let alice = restore_account(&f.alice_account);
    let mut bob = restore_account(&f.bob_account);
    let mut a = restore_session(&f.alice_session);
    let mut b = restore_session(&f.bob_session);

    // Sessions keep the version they were made with. Night Drop uses version 2 everywhere, and
    // vodozemac >= 0.10 defaults to version 1: a session restored as version 1 would stop talking
    // to every peer still on the old app.
    assert_eq!(a.session_config(), SessionConfig::version_2());
    assert_eq!(b.session_config(), SessionConfig::version_2());

    // Identities survive: the same long-term keys contacts know them by.
    assert_eq!(alice.curve25519_key().to_base64(), f.alice_identity_key);
    assert_eq!(bob.curve25519_key().to_base64(), f.bob_identity_key);

    // Mail that waited in a mailbox across the upgrade still opens.
    assert_eq!(
        b.decrypt(&f.pending_to_bob.olm()).unwrap(),
        b"sent before bob updated"
    );
    assert_eq!(
        a.decrypt(&f.pending_to_alice.olm()).unwrap(),
        b"sent before alice updated"
    );

    // The conversation goes on, both ways, ratchet and all.
    for i in 0..5 {
        let m = a.encrypt(format!("after {i}").as_bytes()).unwrap();
        assert_eq!(b.decrypt(&m).unwrap(), format!("after {i}").as_bytes());
        let m = b.encrypt(format!("reply {i}").as_bytes()).unwrap();
        assert_eq!(a.decrypt(&m).unwrap(), format!("reply {i}").as_bytes());
    }

    // A first contact from an old-version peer, to a one-time key published before the upgrade.
    let OlmMessage::PreKey(pre) = f.carol_prekey_to_bob.olm() else {
        panic!("pre-key message")
    };
    let carol = Curve25519PublicKey::from_base64(&f.carol_identity_key).unwrap();
    let accepted = nightdrop_accept(&mut bob, carol, &pre);
    assert_eq!(accepted, b"hi bob, it's carol");

    // For the opposite direction (this version -> a peer still on 0.8), write messages sealed by
    // the restored, upgraded sessions; an out-of-tree 0.8 program decrypts them with the fixture's
    // original sessions. Only when asked: NIGHTDROP_INTEROP_OUT=<file>.
    if let Ok(out) = std::env::var("NIGHTDROP_INTEROP_OUT") {
        let mut a2 = restore_session(&f.alice_session);
        let mut b2 = restore_session(&f.bob_session);
        let msgs: Vec<WireOlmJson> = vec![
            (&b2.encrypt(b"from bob on the new version").unwrap()).into(),
            (&a2.encrypt(b"from alice on the new version").unwrap()).into(),
        ];
        std::fs::write(out, serde_json::to_string(&msgs).unwrap()).unwrap();
    }

    // And the restored sessions still save and load again.
    let again = Session::from_pickle(
        SessionPickle::from_encrypted(&a.pickle().encrypt(&KEY), &KEY).unwrap(),
    );
    assert_eq!(again.session_id(), a.session_id());
}

/// Accept an inbound pre-key message the way this version of the core does.
fn nightdrop_accept(
    bob: &mut Account,
    carol: Curve25519PublicKey,
    pre: &vodozemac::olm::PreKeyMessage,
) -> Vec<u8> {
    bob.create_inbound_session(SessionConfig::version_2(), carol, pre)
        .expect("inbound session")
        .plaintext
}
