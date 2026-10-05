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
//! conversations going in both directions. Regenerate the fixture only on purpose, with the old
//! version: `NIGHTDROP_WRITE_FIXTURE=1 cargo test -p nightdrop --test vodozemac_compat -- --ignored`.

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

fn first_otk(account: &mut Account) -> Curve25519PublicKey {
    account.generate_one_time_keys(1);
    let key = *account
        .one_time_keys()
        .values()
        .next()
        .expect("one-time key");
    account.mark_keys_as_published();
    key
}

#[test]
#[ignore = "writes the fixture; run only with the OLD vodozemac, on purpose"]
fn generate_fixture() {
    if std::env::var("NIGHTDROP_WRITE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let alice = Account::new();
    let mut bob = Account::new();
    let carol = Account::new();

    // Alice opens a session to Bob, as after a QR pairing; they talk a little.
    let bob_otk = first_otk(&mut bob);
    let mut a =
        alice.create_outbound_session(SessionConfig::version_2(), bob.curve25519_key(), bob_otk);
    let first = a.encrypt(b"hello bob");
    let OlmMessage::PreKey(pre) = &first else {
        panic!("first message is pre-key")
    };
    let mut b = bob
        .create_inbound_session(alice.curve25519_key(), pre)
        .unwrap()
        .session;
    for i in 0..3 {
        let m = b.encrypt(format!("bob {i}").as_bytes());
        assert_eq!(a.decrypt(&m).unwrap(), format!("bob {i}").as_bytes());
        let m = a.encrypt(format!("alice {i}").as_bytes());
        assert_eq!(b.decrypt(&m).unwrap(), format!("alice {i}").as_bytes());
    }

    // Bob publishes another one-time key; Carol (still on the old app) starts a chat with it.
    let bob_otk2 = first_otk(&mut bob);
    let mut c =
        carol.create_outbound_session(SessionConfig::version_2(), bob.curve25519_key(), bob_otk2);
    let carol_first = c.encrypt(b"hi bob, it's carol");

    // Each side sends one more message that the other has not received; the app saves its state
    // after sending, so the pickles below already include these sends.
    let pending_to_bob = a.encrypt(b"sent before bob updated");
    let pending_to_alice = b.encrypt(b"sent before alice updated");

    let fixture = Fixture {
        vodozemac: "0.8.1".into(),
        alice_account: alice.pickle().encrypt(&KEY),
        alice_session: a.pickle().encrypt(&KEY),
        bob_account: bob.pickle().encrypt(&KEY),
        bob_session: b.pickle().encrypt(&KEY),
        alice_identity_key: alice.curve25519_key().to_base64(),
        bob_identity_key: bob.curve25519_key().to_base64(),
        carol_identity_key: carol.curve25519_key().to_base64(),
        pending_to_bob: (&pending_to_bob).into(),
        pending_to_alice: (&pending_to_alice).into(),
        carol_prekey_to_bob: (&carol_first).into(),
    };
    std::fs::create_dir_all(std::path::Path::new(FIXTURE).parent().unwrap()).unwrap();
    std::fs::write(FIXTURE, serde_json::to_string_pretty(&fixture).unwrap()).unwrap();
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
        let m = a.encrypt(format!("after {i}").as_bytes());
        assert_eq!(b.decrypt(&m).unwrap(), format!("after {i}").as_bytes());
        let m = b.encrypt(format!("reply {i}").as_bytes());
        assert_eq!(a.decrypt(&m).unwrap(), format!("reply {i}").as_bytes());
    }

    // A first contact from an old-version peer, to a one-time key published before the upgrade.
    let OlmMessage::PreKey(pre) = f.carol_prekey_to_bob.olm() else {
        panic!("pre-key message")
    };
    let carol = Curve25519PublicKey::from_base64(&f.carol_identity_key).unwrap();
    let accepted = nightdrop_accept(&mut bob, carol, &pre);
    assert_eq!(accepted, b"hi bob, it's carol");

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
    bob.create_inbound_session(carol, pre)
        .expect("inbound session")
        .plaintext
}
