//! Tests for `Node` — split out of node.rs (part 2) to keep files small (§2.1).
use super::*;
use crate::relay_client::{RelayClient, RelayServer};
use crate::transport::MemoryNetwork;

#[test]
fn backup_bundles_media_and_restores_it_on_a_fresh_device() {
    let key: StoreKey = [9u8; 32];
    let dir = std::env::temp_dir().join(format!("nightdrop-bkmedia-{}", std::process::id()));
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_media_store(format!("{}-a", dir.display()), key);
    bob.set_media_store(format!("{}-b", dir.display()), key);
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    let pixels = vec![10u8, 20, 30, 40, 50];
    alice
        .send_media(&bob_contact, &pixels, "image/png", "image", &[])
        .unwrap();
    bob.pump().unwrap();
    let media_id = bob
        .messages(&alice_contact)
        .into_iter()
        .last()
        .unwrap()
        .media_id;
    assert!(!media_id.is_empty());

    // Back Bob up and restore onto a fresh device with a NEW media-store key/dir.
    let blob = bob.backup("PASS-WORD-PASS-WORD-").unwrap();
    let net2 = MemoryNetwork::new();
    let mut bob2 = Node::restore_from_backup(
        &blob,
        "PASS-WORD-PASS-WORD-",
        Box::new(net2.endpoint("bob2")),
    )
    .unwrap();
    let key2: StoreKey = [7u8; 32]; // different device key
    bob2.set_media_store(format!("{}-c", dir.display()), key2);

    // The attachment came across in the backup and decrypts to the original bytes.
    assert_eq!(bob2.media_bytes(&media_id).unwrap(), pixels);

    for s in ["a", "b", "c"] {
        std::fs::remove_dir_all(format!("{}-{s}", dir.display())).ok();
    }
}

#[test]
fn backup_bundles_the_onion_keystore_for_a_stable_address() {
    use std::fs;
    let net = MemoryNetwork::new();
    let base = std::env::temp_dir().join(format!("nightdrop-onionbk-{}", std::process::id()));
    let ks = base
        .join("arti-state")
        .join("keystore")
        .join("hss")
        .join("nightdrop");
    fs::create_dir_all(&ks).unwrap();
    fs::write(
        ks.join("ks_hs_id.ed25519_expanded_private"),
        b"FAKE-ONION-SECRET",
    )
    .unwrap();

    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    alice.set_tor_state_dir(base.to_string_lossy().into_owned());
    let blob = alice.backup("PASS-WORD-PASS-WORD-").unwrap();

    // The keystore travelled in the backup and restores byte-for-byte into a fresh base.
    let (state, _key) = Node::open_backup(&blob, "PASS-WORD-PASS-WORD-").unwrap();
    assert!(
        !state.onion_keys.is_empty(),
        "backup carries the onion keystore"
    );
    let dest = std::env::temp_dir().join(format!("nightdrop-onionrs-{}", std::process::id()));
    Node::write_onion_keys(&state.onion_keys, &dest.to_string_lossy()).unwrap();
    let restored = fs::read(
        dest.join("arti-state")
            .join("keystore")
            .join("hss")
            .join("nightdrop")
            .join("ks_hs_id.ed25519_expanded_private"),
    )
    .unwrap();
    assert_eq!(restored, b"FAKE-ONION-SECRET");

    fs::remove_dir_all(&base).ok();
    fs::remove_dir_all(&dest).ok();
}

#[test]
fn renaming_yourself_propagates_to_the_peer() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Default name to start.
    assert_eq!(bob.contacts()[0].their_name, DEFAULT_NAME);

    // Alice renames herself; Bob learns it (E2E, relabels her messages).
    alice.set_my_name(&bob_contact, "Spectre").unwrap();
    bob.pump().unwrap();
    assert_eq!(bob.contacts()[0].their_name, "Spectre");
    assert_eq!(alice.contacts()[0].my_name, "Spectre");

    // Messages still flow after the rename (ratchet stayed consistent).
    alice.send(&bob_contact, "still me").unwrap();
    bob.pump().unwrap();
    assert_eq!(
        bob.messages(&alice_contact).last().unwrap().text,
        "still me"
    );
}

#[test]
fn deleting_a_chat_signals_the_peer() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Alice deletes the chat: gone on her side, Bob is told.
    alice.delete_chat(&bob_contact).unwrap();
    assert!(alice.contacts().is_empty(), "deleted locally");

    bob.pump().unwrap();
    let history = bob.messages(&alice_contact);
    assert!(
        history
            .iter()
            .any(|m| m.system && m.text.contains("deleted this chat")),
        "peer sees the deletion notice: {history:?}"
    );
    // Bob can no longer send on the closed chat.
    assert!(bob.send(&alice_contact, "still there?").is_err());
}

#[test]
fn deleted_chat_stays_closed_across_restart_then_revives_on_repair() {
    use crate::storage;
    let key: storage::StoreKey = [7u8; 32];
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Alice deletes -> Bob receives the Closed signal and the chat closes.
    alice.delete_chat(&bob_contact).unwrap();
    bob.pump().unwrap();
    assert!(
        bob.send(&alice_contact, "x").is_err(),
        "closed right after delete"
    );

    // Persist + restore Bob: the deleted chat STAYS closed (no resurrection).
    let path = std::env::temp_dir()
        .join(format!("nightdrop-closed-{}.bin", std::process::id()))
        .to_string_lossy()
        .into_owned();
    storage::save_to_file(&path, &key, &bob.export(&key)).unwrap();
    let state = storage::load_from_file(&path, &key).unwrap();
    let mut bob2 = Node::restore(&state, Box::new(net.endpoint("bob")), &key).unwrap();
    assert!(
        bob2.send(&alice_contact, "x").is_err(),
        "deleted chat must stay closed across restart"
    );

    // A fresh Hello (re-pair) revives it: Alice dials Bob's new bundle.
    let bob_bundle = bob2.publish_bundle();
    alice.connect_with_bundle("bob", &bob_bundle).unwrap();
    bob2.pump().unwrap();
    bob2.send(&alice_contact, "revived").unwrap(); // no longer closed
    assert!(bob2.contacts().iter().any(|c| c.id == alice_contact));

    std::fs::remove_file(&path).ok();
}

#[test]
fn reused_code_is_reported_to_the_joiner() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    alice.set_require_authorization(true);
    alice.set_last_invite_code("9-cobalt-marble".to_string());

    // Two strangers join with the same code; Alice approves the first.
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let mut carol = Node::new(Box::new(net.endpoint("carol")));
    let bundle_b = alice.publish_bundle();
    let alice_contact_for_bob = bob.connect_with_bundle("alice", &bundle_b).unwrap();
    let bundle_c = alice.publish_bundle();
    let alice_contact_for_carol = carol.connect_with_bundle("alice", &bundle_c).unwrap();
    alice.pump().unwrap();

    let requests = alice.pending_authorizations();
    assert_eq!(requests.len(), 2);
    for r in requests {
        alice.authorize(&r.id, true).unwrap();
    }
    // The second approval hit the in-use code: only one active contact remains.
    assert_eq!(alice.contacts().len(), 1);

    // Exactly one joiner (the refused one — which one depends on approval order) is told
    // the code was already used.
    bob.pump().unwrap();
    carol.pump().unwrap();
    let told = |n: &Node, id: &str| {
        n.messages(id)
            .iter()
            .any(|m| m.system && m.text.contains("already been used"))
    };
    let bob_refused = told(&bob, &alice_contact_for_bob);
    let carol_refused = told(&carol, &alice_contact_for_carol);
    assert!(
        bob_refused ^ carol_refused,
        "exactly one joiner sees the code-in-use notice (bob={bob_refused}, carol={carol_refused})"
    );
}

#[test]
fn editing_a_delivered_message_updates_the_peer() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    alice.send(&bob_contact, "helo bob").unwrap();
    bob.pump().unwrap();
    let msg_id = alice.messages(&bob_contact).last().unwrap().msg_id.clone();
    assert!(!msg_id.is_empty());

    // Alice fixes the typo within the window; both sides converge and show "edited".
    alice
        .edit_message(&bob_contact, &msg_id, "hello bob")
        .unwrap();
    bob.pump().unwrap();
    let mine = alice.messages(&bob_contact).into_iter().last().unwrap();
    assert_eq!(mine.text, "hello bob");
    assert!(mine.edited);
    let theirs = bob.messages(&alice_contact).into_iter().last().unwrap();
    assert_eq!(theirs.text, "hello bob");
    assert!(theirs.edited, "receiver shows the edited tag");
}

#[test]
fn unsending_a_delivered_message_tombstones_it_on_both_sides() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    alice.send(&bob_contact, "oops secret").unwrap();
    bob.pump().unwrap();
    let msg_id = alice.messages(&bob_contact).last().unwrap().msg_id.clone();

    alice.unsend_message(&bob_contact, &msg_id).unwrap();
    bob.pump().unwrap();

    let mine = alice.messages(&bob_contact).into_iter().last().unwrap();
    assert_eq!(mine.kind, "deleted");
    assert_eq!(mine.text, "");
    let theirs = bob.messages(&alice_contact).into_iter().last().unwrap();
    assert_eq!(theirs.kind, "deleted", "receiver's copy is tombstoned");
    assert_eq!(theirs.text, "");
}

#[test]
fn unsending_a_queued_message_recalls_it_so_the_peer_never_sees_it() {
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Bob offline: the message is queued, then unsent before he ever drains it.
    net.disconnect("bob");
    alice.send(&bob_contact, "recall me").unwrap();
    let msg_id = alice.messages(&bob_contact).last().unwrap().msg_id.clone();
    alice.unsend_message(&bob_contact, &msg_id).unwrap();
    assert!(
        alice.messages(&bob_contact).is_empty(),
        "an unseen held message disappears locally rather than leaving a tombstone"
    );

    // Bob drains the mailbox and finds nothing — the blob was recalled.
    let received = bob.poll_relay().unwrap();
    assert!(
        received.is_empty(),
        "recalled message never arrives: {received:?}"
    );
    assert!(bob.messages(&alice_contact).is_empty());
}

#[test]
fn editing_a_queued_message_replaces_it_on_the_relay() {
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Bob goes offline; Alice's message is queued on the relay, then edited.
    net.disconnect("bob");
    alice.send(&bob_contact, "draft text").unwrap();
    let msg_id = alice.messages(&bob_contact).last().unwrap().msg_id.clone();
    alice
        .edit_message(&bob_contact, &msg_id, "final text")
        .unwrap();
    assert_eq!(
        alice.messages(&bob_contact).last().unwrap().delivery,
        "queued"
    );
    assert!(alice.messages(&bob_contact).last().unwrap().edited);

    // Bob drains the mailbox: he sees ONLY the final text — exactly one message,
    // never the draft (the queued blob was recalled and replaced).
    let received = bob.poll_relay().unwrap();
    assert_eq!(
        received,
        vec![(alice_contact.clone(), "final text".to_string())]
    );
    let history = bob.messages(&alice_contact);
    assert_eq!(
        history.len(),
        1,
        "the draft was never delivered: {history:?}"
    );
    assert_eq!(history[0].text, "final text");

    // Alice's badge flips to delivered via the ack, edit intact.
    alice.pump().unwrap();
    let mine = alice.messages(&bob_contact).into_iter().last().unwrap();
    assert_eq!(mine.delivery, "delivered");
    assert_eq!(mine.text, "final text");
}

#[test]
fn edit_window_is_enforced_but_queued_messages_stay_editable() {
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // A delivered message older than the window cannot be edited...
    alice.send(&bob_contact, "old news").unwrap();
    let msg_id = alice.messages(&bob_contact).last().unwrap().msg_id.clone();
    {
        let chat = alice.chats.get_mut(&bob_contact).unwrap();
        let m = chat.history.last_mut().unwrap();
        m.at -= EDIT_WINDOW.as_secs() + 60; // backdate past the window
    }
    assert!(alice
        .edit_message(&bob_contact, &msg_id, "too late")
        .is_err());

    // ...but a QUEUED one that old is still editable (the peer never saw it).
    net.disconnect("bob");
    alice.send(&bob_contact, "stuck in the mailbox").unwrap();
    let queued_id = alice.messages(&bob_contact).last().unwrap().msg_id.clone();
    {
        let chat = alice.chats.get_mut(&bob_contact).unwrap();
        let m = chat.history.last_mut().unwrap();
        m.at -= EDIT_WINDOW.as_secs() + 60;
    }
    alice
        .edit_message(&bob_contact, &queued_id, "still fixable")
        .unwrap();
    assert_eq!(
        alice.messages(&bob_contact).last().unwrap().text,
        "still fixable"
    );
}

#[test]
fn remote_storage_toggle_reaches_both_sides() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Alice opts in: Bob's side mirrors the flag (both see the warning banner, §6)
    // and gets an explicit notice.
    alice.set_remote_storage(&bob_contact, true).unwrap();
    bob.pump().unwrap();
    assert!(
        bob.contacts()[0].remote_storage,
        "peer sees server storage is ON"
    );
    assert!(bob
        .messages(&alice_contact)
        .iter()
        .any(|m| m.system && m.text.contains("enabled 24h server storage")));

    // And off again.
    alice.set_remote_storage(&bob_contact, false).unwrap();
    bob.pump().unwrap();
    assert!(!bob.contacts()[0].remote_storage);
}

#[test]
fn disappearing_timer_syncs_and_sweeps_both_sides() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Alice sets a 1-hour timer; Bob's side mirrors it and both see a notice.
    alice.set_disappearing(&bob_contact, 3600).unwrap();
    bob.pump().unwrap();
    assert_eq!(alice.contacts()[0].disappearing_secs, 3600);
    assert_eq!(bob.contacts()[0].disappearing_secs, 3600);
    assert!(bob
        .messages(&alice_contact)
        .iter()
        .any(|m| m.system && m.text.contains("disappearing messages")));

    // A message older than the timer is swept from both devices; a fresh one survives.
    alice.send(&bob_contact, "old secret").unwrap();
    bob.pump().unwrap();
    alice.send(&bob_contact, "recent").unwrap();
    bob.pump().unwrap();

    // Backdate the "old secret" copy past the 1h horizon on each device.
    let backdate = |node: &mut Node, cid: &str| {
        let chat = node.chats.get_mut(cid).unwrap();
        for m in chat.history.iter_mut() {
            if m.text == "old secret" {
                m.at -= 3700;
            }
        }
    };
    backdate(&mut alice, &bob_contact);
    backdate(&mut bob, &alice_contact);
    alice.sweep_time();
    bob.sweep_time();

    for (node, cid) in [(&alice, &bob_contact), (&bob, &alice_contact)] {
        let texts: Vec<String> = node.messages(cid).into_iter().map(|m| m.text).collect();
        assert!(
            !texts.contains(&"old secret".to_string()),
            "swept: {texts:?}"
        );
        assert!(texts.contains(&"recent".to_string()), "kept: {texts:?}");
    }
}

#[test]
fn announcing_a_new_address_updates_the_contact() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();

    // Simulate Bob holding a stale address for Alice (as if Alice's onion had rotated).
    bob.chats.get_mut(&alice_contact).unwrap().peer_address = "stale.onion".to_string();

    // Alice announces her current address; Bob receives it and updates his record.
    let notified = alice.announce_address();
    assert_eq!(notified, 1);
    bob.pump().unwrap();
    assert_eq!(
        bob.chats.get(&alice_contact).unwrap().peer_address,
        alice.address()
    );
    assert!(bob
        .messages(&alice_contact)
        .iter()
        .any(|m| m.system && m.text.contains("address changed")));
}

#[test]
fn address_is_only_announced_when_it_changes() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    // Brand-new node: nothing to compare against → no announcement, baseline learned.
    assert!(!alice.announce_address_if_changed());
    // Same address on the next "startup" → still no announcement.
    assert!(!alice.announce_address_if_changed());
    // A persisted-then-changed onion: prior address differs from the live one → announce.
    alice.restored_address = "old.onion".to_string();
    assert!(
        alice.announce_address_if_changed(),
        "changed onion announces"
    );
}

#[test]
fn interactive_spake2_seals_the_invite_only_to_the_right_code() {
    // The crypto core of TODO #3, exercised without the relay: the inviter's response
    // opens iff the joiner used the same short code. A wrong code yields a different
    // SPAKE2 key, so the AEAD tag (our key-confirmation) rejects it — and nothing the
    // relay could offline-attack is ever produced. The seal key is the §4.1 HYBRID of the
    // SPAKE2 secret and the ML-KEM secret the joiner ships in its opener.
    let payload = "nightdrop://pair?addr=abc.onion&ik=IKEY&otk=OTKEY";
    let code = "7-cedar-river-ember";

    // Right code: joiner opener (SPAKE2 msg + ML-KEM pubkey) -> inviter response -> joiner finishes
    // SPAKE2, decapsulates, and opens under the hybrid key.
    let (joiner, msg_j) = crate::pake::start(code.as_bytes());
    let kem = crate::pqkem::generate();
    let mut opener = Vec::new();
    put_field(&mut opener, &msg_j);
    put_field(&mut opener, &kem.public);
    let response = build_invite_response(code, payload, &opener).unwrap();
    let (msg_i, kem_ct, sealed) = three_fields(&response).expect("well-formed response");
    let key = joiner.finish(&msg_i).unwrap();
    let kem_ss = kem.decapsulate(&kem_ct).unwrap();
    let opened =
        crate::storage::open(&crate::pqkem::hybrid_seal_key(&key, &kem_ss), &sealed).unwrap();
    assert_eq!(opened, payload.as_bytes());

    // Wrong code on the joiner side: the ML-KEM half still agrees, but the SPAKE2 half differs, so
    // the hybrid key differs and `open` fails — a wrong code alone still turns the imposter away.
    let (joiner2, msg_j2) = crate::pake::start(b"7-wrong-words-xyz");
    let kem2 = crate::pqkem::generate();
    let mut opener2 = Vec::new();
    put_field(&mut opener2, &msg_j2);
    put_field(&mut opener2, &kem2.public);
    let response2 = build_invite_response(code, payload, &opener2).unwrap();
    let (msg_i2, kem_ct2, sealed2) = three_fields(&response2).unwrap();
    let key2 = joiner2.finish(&msg_i2).unwrap();
    let kem_ss2 = kem2.decapsulate(&kem_ct2).unwrap();
    assert!(
        crate::storage::open(&crate::pqkem::hybrid_seal_key(&key2, &kem_ss2), &sealed2).is_err(),
        "a wrong short code must not open the sealed invite"
    );
}

#[test]
fn sweep_expires_stale_queued_messages_and_time_bombs_ephemeral_chats() {
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // A queued message older than 24h flips to "expired" (§11.3).
    net.disconnect("bob");
    alice.send(&bob_contact, "never picked up").unwrap();
    {
        let chat = alice.chats.get_mut(&bob_contact).unwrap();
        chat.history.last_mut().unwrap().at -= RELAY_TTL.as_secs() + 60;
    }
    alice.sweep_time();
    assert_eq!(
        alice.messages(&bob_contact).last().unwrap().delivery,
        "expired"
    );

    // In an ephemeral (server-storage) chat, >24h-old device copies are destroyed
    // (§11.4); fresh ones and normal chats are untouched.
    alice.set_remote_storage(&bob_contact, true).unwrap();
    alice.send(&bob_contact, "doomed").unwrap();
    {
        let chat = alice.chats.get_mut(&bob_contact).unwrap();
        chat.history.last_mut().unwrap().at -= RELAY_TTL.as_secs() + 60;
    }
    alice.send(&bob_contact, "fresh").unwrap();
    let before = alice.messages(&bob_contact).len();
    alice.sweep_time();
    let after = alice.messages(&bob_contact);
    assert_eq!(
        after.len(),
        before - 2,
        "expired msg + doomed msg removed: {after:?}"
    );
    assert!(after.iter().any(|m| m.text == "fresh"));
    assert!(!after.iter().any(|m| m.text == "doomed"));
}

#[test]
fn two_nodes_pair_and_converse_for_real() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice"))); // inviter
    let mut bob = Node::new(Box::new(net.endpoint("bob"))); // joiner

    // Alice advertises a bundle (e.g. via QR); Bob joins against it.
    let bundle = alice.publish_bundle();
    let alice_contact_for_bob = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap(); // Alice accepts the Hello and creates her chat.
    let bob_contact_for_alice = alice.contacts()[0].id.clone();

    // Bob -> Alice
    bob.send(&alice_contact_for_bob, "hi Alice").unwrap();
    let got = alice.pump().unwrap();
    assert_eq!(
        got,
        vec![(bob_contact_for_alice.clone(), "hi Alice".to_string())]
    );

    // Alice -> Bob
    alice.send(&bob_contact_for_alice, "hi Bob").unwrap();
    bob.pump().unwrap();

    let bob_view = bob.messages(&alice_contact_for_bob);
    assert_eq!(bob_view.len(), 2);
    assert!(bob_view[0].from_me && bob_view[0].text == "hi Alice");
    assert!(!bob_view[1].from_me && bob_view[1].text == "hi Bob");

    // Several more rounds to exercise the ratchet over the wire.
    for i in 0..4 {
        bob.send(&alice_contact_for_bob, &format!("b{i}")).unwrap();
        alice.pump().unwrap();
        alice
            .send(&bob_contact_for_alice, &format!("a{i}"))
            .unwrap();
        bob.pump().unwrap();
    }
    // "hi Alice" (recv) + "hi Bob" (sent) + 4*(recv + sent) = 10.
    assert_eq!(alice.messages(&bob_contact_for_alice).len(), 10);
}

/// A [`Transport`] that behaves like a [`MemoryTransport`] but implements the onion
/// client-authorization hooks (#22) against a real on-disk key directory — so we can assert the
/// node drives `authorize_client`/`revoke_client` through the actual pairing flow without Tor.
struct AuthTransport {
    inner: crate::transport::MemoryTransport,
    auth_dir: std::path::PathBuf,
}

impl AuthTransport {
    /// A well-formed `descriptor:x25519:<base32>` client key deterministically derived from a
    /// peer address (stands in for arti's `generate_service_discovery_key`).
    fn fake_key(peer: &str) -> String {
        use sha2::{Digest as _, Sha256};
        use std::fmt::Write as _;
        let digest = Sha256::digest(peer.as_bytes());
        let mut s = String::from("descriptor:x25519:");
        for b in &digest {
            let _ = write!(s, "{b:02X}");
        }
        s
    }
}

impl Transport for AuthTransport {
    fn address(&self) -> Address {
        self.inner.address()
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        self.inner.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        self.inner.try_recv()
    }
    fn make_client_key(&self, peer_onion: &str) -> Option<Result<(String, [u8; 32])>> {
        // The secret is what the node persists; a fixed one is enough for the auth-dir assertions
        // this fake exists for.
        Some(Ok((Self::fake_key(peer_onion), [7u8; 32])))
    }
    fn authorize_client(&self, contact_id: &str, key: &str) -> Result<()> {
        crate::transport::client_auth::authorize(&self.auth_dir, contact_id, key)
    }
    fn revoke_client(&self, contact_id: &str) -> Result<()> {
        crate::transport::client_auth::revoke(&self.auth_dir, contact_id)
    }
}

#[test]
fn pairing_exchanges_onion_client_keys_and_delete_revokes_them() {
    use crate::transport::client_auth;

    // Fresh per-run auth dirs (no tempfile dev-dep).
    let base = std::env::temp_dir().join(format!("nightdrop-nodeauth-{}", std::process::id()));
    let alice_auth = base.join("alice");
    let bob_auth = base.join("bob");
    let _ = std::fs::remove_dir_all(&base);

    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(AuthTransport {
        inner: net.endpoint("alice"),
        auth_dir: alice_auth.clone(),
    }));
    let mut bob = Node::new(Box::new(AuthTransport {
        inner: net.endpoint("bob"),
        auth_dir: bob_auth.clone(),
    }));
    let alice_ik = alice.identity_key();
    let bob_ik = bob.identity_key();

    // Pair: Bob joins Alice's bundle. Bob sends Hello + his ClientKey (for Alice's onion);
    // Alice, on Hello, sends back her ClientKey (for Bob's onion).
    let bundle = alice.publish_bundle();
    let bob_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    assert_eq!(bob_contact, alice_ik);
    alice.pump().unwrap(); // Alice authorizes Bob + announces her key back
    bob.pump().unwrap(); // Bob authorizes Alice

    // Each side authorized the *other* to reach its onion.
    assert!(
        client_auth::is_authorized(&alice_auth, &bob_ik),
        "Alice authorized Bob's client key"
    );
    assert!(
        client_auth::is_authorized(&bob_auth, &alice_ik),
        "Bob authorized Alice's client key"
    );

    // Deleting the chat revokes the peer's reachability to our onion.
    alice.delete_chat(&bob_ik).unwrap();
    assert!(
        !client_auth::is_authorized(&alice_auth, &bob_ik),
        "delete_chat revoked Bob's authorization"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn media_to_file_writes_owner_only_plaintext_into_the_app_private_scratch() {
    // §1.4: decrypted attachments must land in an app-private, owner-only sibling of the sealed
    // store — never the world-readable system temp under a predictable name.
    let key: StoreKey = [7u8; 32];
    let base = std::env::temp_dir().join(format!("nightdrop-14-{}", std::process::id()));
    let media_dir = base.join("nightdrop-media");
    let net = MemoryNetwork::new();
    let mut node = Node::new(Box::new(net.endpoint("me")));
    node.set_media_store(media_dir.to_string_lossy().into_owned(), key);

    let id = node.store_media(b"decrypted video bytes").unwrap();
    let path = node.media_to_file(&id, "mp4").unwrap();
    let p = std::path::Path::new(&path);

    // Lands in the app-private scratch (sibling `nightdrop-open`), keyed by the unguessable media id.
    assert!(
        p.starts_with(base.join("nightdrop-open")),
        "decrypted media escaped the app-private scratch: {path}"
    );
    // The old predictable, world-readable system-temp path must NOT be used.
    let legacy = std::env::temp_dir().join(format!("nightdrop-media-{id}.mp4"));
    assert!(
        !legacy.exists(),
        "still wrote to the shared temp dir: {legacy:?}"
    );
    assert_eq!(std::fs::read(p).unwrap(), b"decrypted video bytes");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let file_mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "plaintext file is not owner-only");
        let dir_mode = std::fs::metadata(p.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "scratch dir is not owner-only");
    }

    // Logout wipes the scratch; a fresh media store sweeps any leftovers on next launch.
    node.logout();
    assert!(!p.exists(), "logout left decrypted media on disk: {path}");

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn short_code_pairing_survives_a_dead_primary_relay() {
    // §3.1: with the baked-in primary relay down, short-code pairing still completes as long as
    // both sides share one live relay — the rendezvous broadcasts across primary + configured
    // extras (#17), so no single relay is a pairing chokepoint.
    let live = RelayServer::spawn("127.0.0.1:0").unwrap().to_string();
    let dead = "127.0.0.1:1".to_string(); // nothing listening -> connect refused
    let net = MemoryNetwork::new();
    let slot = "42";
    let secret = "cedar-lantern-river-ember";

    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    alice.set_relay(RelayClient::new(dead.clone()));
    alice.set_my_relays(vec![live.clone()]); // shared live fallback
    alice
        .stage_short_code_invite(slot, secret, Duration::from_secs(60))
        .unwrap();

    // The joiner drives the (blocking) broadcast handshake on another thread.
    let joiner_relays = {
        let mut bob = Node::new(Box::new(net.endpoint("bob")));
        bob.set_relay(RelayClient::new(dead.clone()));
        bob.set_my_relays(vec![live.clone()]);
        bob.rendezvous_relays()
    };
    let (slot_s, secret_s) = (slot.to_string(), secret.to_string());
    let joiner = std::thread::spawn(move || {
        run_join_handshake(&joiner_relays, &slot_s, &secret_s, Duration::from_secs(10))
    });

    // The inviter services the rendezvous until the joiner completes.
    let payload = loop {
        alice.service_pending_invites();
        if joiner.is_finished() {
            break joiner.join().unwrap().unwrap();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        payload.contains("addr=alice"),
        "joiner recovered the inviter's payload over the live fallback relay: {payload}"
    );
}

/// A transport that reports when it is dropped, so a test can prove the *live* transport was
/// released rather than merely stopped being used.
struct DropWatched {
    inner: crate::transport::MemoryTransport,
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for DropWatched {
    fn drop(&mut self) {
        self.dropped
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl crate::transport::Transport for DropWatched {
    fn address(&self) -> crate::transport::Address {
        self.inner.address()
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        self.inner.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(crate::transport::Address, Vec<u8>)> {
        self.inner.try_recv()
    }
}

/// `close_transport` must actually **drop** the live transport and the relay, not just stop
/// using them. Tor's on-disk state lock is released only when the last handle goes away, so a
/// half-measure here reintroduces the bug where restoring a backup failed to launch its onion
/// service with "State already locked" while the old core was still alive.
#[test]
fn close_transport_drops_the_live_transport_so_its_resources_are_released() {
    use std::sync::atomic::Ordering;

    let net = MemoryNetwork::new();
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut alice = Node::new(Box::new(DropWatched {
        inner: net.endpoint("alice"),
        dropped: std::sync::Arc::clone(&dropped),
    }));
    alice.set_relay(RelayClient::new("relay-addr"));

    assert!(!dropped.load(Ordering::Relaxed), "live before close");

    alice.close_transport();

    assert!(
        dropped.load(Ordering::Relaxed),
        "the real transport must be dropped by close_transport — Tor's state lock is only \
         released once every handle is gone"
    );
    // The address survives so the UI can still show who we were...
    assert_eq!(alice.address(), "alice");
    // ...but the node is inert: never reachable again.
    assert!(
        !alice.onion_published(),
        "a closed transport publishes nothing"
    );
}

/// The inviter's read of the joiner leg is **destructive**, so an opener can be consumed without
/// an answer ever being posted back — the answer's post fails, or the inviter is backgrounded or
/// killed mid-handshake. The joiner must not sit out its whole timeout waiting for a reply that
/// can never arrive (TODO #7: "issues using the secret"); it re-posts, so an inviter that comes
/// back still finds an opener and pairing completes.
#[test]
fn a_consumed_opener_does_not_strand_the_joiner() {
    let live = RelayServer::spawn("127.0.0.1:0").unwrap().to_string();
    let net = MemoryNetwork::new();
    let slot = "43";
    let secret = "cedar-lantern-river-ember";

    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    alice.set_relay(RelayClient::new(live.clone()));
    alice
        .stage_short_code_invite(slot, secret, Duration::from_secs(60))
        .unwrap();

    let joiner_relays = {
        let mut bob = Node::new(Box::new(net.endpoint("bob")));
        bob.set_relay(RelayClient::new(live.clone()));
        bob.rendezvous_relays()
    };
    let (slot_s, secret_s) = (slot.to_string(), secret.to_string());
    let joiner = std::thread::spawn(move || {
        run_join_handshake(&joiner_relays, &slot_s, &secret_s, Duration::from_secs(30))
    });

    // Stand in for the broken inviter: swallow the first opener and answer nothing at all.
    let thief = RelayClient::new(live.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "joiner never posted an opener to steal"
        );
        let stolen = thief.take(&rendezvous_handle(slot, RDV_JOINER)).unwrap();
        if !stolen.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // Only now does a working inviter start answering. Pre-fix there was nothing left for it to
    // answer, and the joiner ran out its timeout.
    let payload = loop {
        alice.service_pending_invites();
        if joiner.is_finished() {
            break joiner
                .join()
                .unwrap()
                .expect("pairing recovered after a lost opener");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        payload.contains("addr=alice"),
        "joiner recovered the inviter's payload after its first opener was consumed: {payload}"
    );
}

/// A non-synchronous transport (stands in for Tor): delivery must be **deferred** to the poller,
/// not done inline, so composing a message never blocks the UI on a dial. Wraps the in-memory
/// transport but reports `is_synchronous() == false`.
struct AsyncMemory(crate::transport::MemoryTransport);

impl crate::transport::Transport for AsyncMemory {
    fn address(&self) -> crate::transport::Address {
        self.0.address()
    }
    fn is_synchronous(&self) -> bool {
        false
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        self.0.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(crate::transport::Address, Vec<u8>)> {
        self.0.try_recv()
    }
}

#[test]
fn a_non_synchronous_transport_defers_delivery_to_the_poller() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(AsyncMemory(net.endpoint("alice"))));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Send returns immediately having only sealed + stored the message — no delivery yet.
    alice.send(&bob_contact, "deferred hi").unwrap();
    let stored = alice.messages(&bob_contact);
    assert_eq!(stored.last().unwrap().text, "deferred hi");
    assert_eq!(
        stored.last().unwrap().delivery,
        "queued",
        "a deferred send is stored queued, not yet sent"
    );
    bob.pump().unwrap();
    assert!(
        bob.messages(&alice_contact)
            .iter()
            .all(|m| m.text != "deferred hi"),
        "the peer must NOT have the message before the poller delivers it"
    );

    // The poller runs: now it delivers, and the peer receives it.
    let affected = alice.flush_pending_sends();
    assert_eq!(affected, vec![bob_contact.clone()]);
    assert_eq!(
        alice.messages(&bob_contact).last().unwrap().delivery,
        "sent",
        "delivery flips the stored message to sent"
    );
    bob.pump().unwrap();
    assert_eq!(
        bob.messages(&alice_contact).last().unwrap().text,
        "deferred hi",
        "the peer receives the message once the poller has delivered it"
    );
}
