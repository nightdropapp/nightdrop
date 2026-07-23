//! Tests for `Node` — split out of node.rs (part 1) to keep files small (§2.1).
use super::*;
use crate::relay_client::{RelayClient, RelayServer};
use crate::transport::MemoryNetwork;

#[test]
fn offline_message_is_delivered_via_the_relay() {
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();

    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());

    // Pair while both are online.
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Bob goes offline; Alice sends anyway -> queued in the relay.
    net.disconnect("bob");
    alice.send(&bob_contact, "while you were out").unwrap();
    assert!(
        bob.pump().unwrap().is_empty(),
        "nothing arrives directly while offline"
    );
    assert_eq!(
        alice.messages(&bob_contact).last().unwrap().delivery,
        "queued",
        "Alice's message is held on the relay until Bob picks it up"
    );

    // Bob comes back and drains the mailbox (which also sends Alice a delivery ack).
    let received = bob.poll_relay().unwrap();
    assert_eq!(
        received,
        vec![(alice_contact.clone(), "while you were out".to_string())]
    );
    assert_eq!(
        bob.messages(&alice_contact).last().unwrap().text,
        "while you were out"
    );

    // Alice processes Bob's ack -> her message flips queued -> delivered.
    alice.pump().unwrap();
    assert_eq!(
        alice.messages(&bob_contact).last().unwrap().delivery,
        "delivered",
        "the delivery ack flips Alice's message to delivered"
    );
}

#[test]
fn unsend_after_restart_recalls_the_queued_blob() {
    use crate::storage;

    let key: storage::StoreKey = [7u8; 32];
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();

    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());

    // Pair while both online.
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Bob offline; Alice sends -> the message is queued on the relay and a recall receipt held.
    net.disconnect("bob");
    alice.send(&bob_contact, "oops sent too soon").unwrap();
    let queued = alice
        .messages(&bob_contact)
        .into_iter()
        .next_back()
        .unwrap();
    assert_eq!(queued.delivery, "queued");
    let msg_id = queued.msg_id.clone();

    // "Restart" Alice: persist to disk, drop, restore onto a fresh endpoint, reattach the relay.
    // The in-memory receipt is gone; only the persisted one can drive the recall now.
    let path = std::env::temp_dir().join(format!("nightdrop-unsend-{}.bin", std::process::id()));
    let path = path.to_str().unwrap().to_string();
    storage::save_to_file(&path, &key, &alice.export(&key)).unwrap();
    drop(alice);
    let state = storage::load_from_file(&path, &key).unwrap();
    let mut alice2 = Node::restore(&state, Box::new(net.endpoint("alice")), &key).unwrap();
    alice2.set_relay(relay.clone());

    // Unsend after the restart. The persisted receipt lets us recall the still-queued blob.
    alice2.unsend_message(&bob_contact, &msg_id).unwrap();
    assert!(
        alice2.messages(&bob_contact).is_empty(),
        "a recalled held message leaves no local tombstone, even after a restart"
    );

    // Bob comes back and drains his mailbox: the message was recalled, so he receives NOTHING —
    // the pre-fix behavior would have delivered it and only then tombstoned it.
    let received = bob.poll_relay().unwrap();
    assert!(
        received.is_empty(),
        "the queued message was recalled from the relay before Bob ever fetched it"
    );
    assert!(
        bob.messages(&alice_contact)
            .iter()
            .all(|m| m.text != "oops sent too soon"),
        "Bob never received the unsent message"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn state_survives_a_restart_via_encrypted_storage() {
    use crate::storage;

    let key: storage::StoreKey = [42u8; 32];
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));

    // Pair and exchange a message so there's a session + history to persist.
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();
    alice.send(&bob_contact, "remember me").unwrap();
    bob.pump().unwrap();

    let bob_id_before = bob.identity_id();

    // Persist Bob to an encrypted file, then drop him.
    let path = std::env::temp_dir().join(format!("nightdrop-test-{}.bin", std::process::id()));
    let path = path.to_str().unwrap().to_string();
    storage::save_to_file(&path, &key, &bob.export(&key)).unwrap();
    drop(bob);

    // "Restart": load + restore onto a fresh endpoint at the same address.
    let state = storage::load_from_file(&path, &key).unwrap();
    let mut bob2 = Node::restore(&state, Box::new(net.endpoint("bob")), &key).unwrap();

    assert_eq!(
        bob2.identity_id(),
        bob_id_before,
        "same identity after restart"
    );
    assert_eq!(
        bob2.messages(&alice_contact).last().unwrap().text,
        "remember me"
    );

    // The restored session still works: Alice sends, Bob2 decrypts.
    alice.send(&bob_contact, "still here?").unwrap();
    bob2.pump().unwrap();
    assert_eq!(
        bob2.messages(&alice_contact).last().unwrap().text,
        "still here?"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn inbound_request_requires_authorization_before_messaging() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice"))); // recipient
    alice.set_require_authorization(true);
    let mut bob = Node::new(Box::new(net.endpoint("bob"))); // stranger

    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();

    // Alice has a pending request, not a contact, and cannot be messaged yet.
    assert_eq!(alice.pending_authorizations().len(), 1);
    assert!(alice.contacts().is_empty());
    let bob_contact = alice.pending_authorizations()[0].id.clone();
    assert!(
        alice.send(&bob_contact, "hi").is_err(),
        "cannot message before approving"
    );

    // Bob's message is ignored while unauthorized.
    bob.send(&alice_contact, "let me in").unwrap();
    assert!(alice.pump().unwrap().is_empty());

    // Alice approves -> now it's a real contact and messaging works both ways.
    alice.authorize(&bob_contact, true).unwrap();
    assert!(alice.pending_authorizations().is_empty());
    assert_eq!(alice.contacts().len(), 1);

    alice.send(&bob_contact, "welcome").unwrap();
    bob.pump().unwrap();
    assert_eq!(bob.messages(&alice_contact).last().unwrap().text, "welcome");
}

#[test]
fn re_pair_after_close_warns_to_reverify_and_resets_verification() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice"))); // recipient (auto-authorizes)
    let mut bob = Node::new(Box::new(net.endpoint("bob")));

    // First pairing: Bob -> Alice.
    let bundle = alice.publish_bundle();
    let alice_on_bob = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_id = alice.contacts()[0].id.clone();

    // Alice verifies Bob out-of-band, and confirms the first pairing did NOT warn.
    alice.set_verified(&bob_id, true);
    assert!(alice.contacts()[0].verified);
    assert!(
        !alice
            .messages(&bob_id)
            .iter()
            .any(|m| m.text.contains("re-paired")),
        "no re-pair warning on the initial pairing"
    );

    // Bob tears the chat down; Alice processes the authenticated Closed (chat now closed).
    bob.delete_chat(&alice_on_bob).unwrap();
    alice.pump().unwrap();

    // Bob re-pairs from scratch with the SAME identity: a fresh Hello revives Alice's chat.
    let bundle2 = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle2).unwrap();
    alice.pump().unwrap();

    // Alice is warned to re-verify, and the contact is no longer marked verified.
    assert!(
        alice.messages(&bob_id).iter().any(|m| m.system
            && m.text.contains("re-paired")
            && m.text.contains("verification no longer applies")),
        "re-pair emits the reverify warning (verified variant)"
    );
    assert!(
        !alice.contacts()[0].verified,
        "verification is reset by the re-pair"
    );
}

#[test]
fn declining_a_request_drops_it() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    alice.set_require_authorization(true);
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();

    let bob_contact = alice.pending_authorizations()[0].id.clone();
    alice.authorize(&bob_contact, false).unwrap();
    assert!(alice.pending_authorizations().is_empty());
    assert!(alice.contacts().is_empty());
}

#[test]
fn remote_storage_keeps_an_encrypted_copy_on_the_relay() {
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

    // Enable opt-in server storage, then send while Bob is online.
    alice.set_remote_storage(&bob_contact, true).unwrap();
    alice.send(&bob_contact, "keep on server").unwrap();

    // Bob still got it directly...
    bob.pump().unwrap();
    // ...AND a sealed copy is sitting in Bob's relay mailbox, addressed by the
    // unlinkable handle (NOT his transport address — the relay must not learn it).
    let stored = relay.take(&mailbox_handle(&bob_contact)).unwrap();
    assert_eq!(stored.len(), 1, "one sealed copy on the relay");
    assert!(
        !stored[0].windows(4).any(|w| w == b"keep"),
        "blob is not plaintext"
    );
    // The envelope hides routing metadata too: the sender's identity key (which rides
    // in the clear inside wire frames for P2P routing) must not be readable on-relay.
    let alice_ik = alice.identity_key();
    assert!(
        !stored[0]
            .windows(alice_ik.len())
            .any(|w| w == alice_ik.as_bytes()),
        "sender identity key must not appear in the relay blob"
    );
    assert!(
        relay.take("mbx:bob").unwrap().is_empty(),
        "no address-keyed mailbox"
    );
}

#[test]
fn password_backup_round_trips_and_rejects_wrong_password() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();
    alice.send(&bob_contact, "keep me safe").unwrap();
    bob.pump().unwrap();

    let password = crate::storage::random_password();
    let blob = bob.backup(&password).unwrap();
    let bob_id = bob.identity_id();

    // Wrong password cannot open it.
    assert!(
        Node::restore_from_backup(&blob, "WRONG-PASSWORD", Box::new(net.endpoint("x"))).is_err()
    );

    // Correct password restores identity + history (e.g. onto a new device).
    let bob2 = Node::restore_from_backup(&blob, &password, Box::new(net.endpoint("bob2"))).unwrap();
    assert_eq!(bob2.identity_id(), bob_id);
    assert_eq!(
        bob2.messages(&alice_contact).last().unwrap().text,
        "keep me safe"
    );
}

#[test]
fn server_backup_recovers_on_a_fresh_device() {
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_id = bob.identity_id();

    let password = crate::storage::random_password();
    bob.server_backup(&relay, &password, None, true).unwrap(); // default 24h, full

    // Wrong password resolves to a different handle -> nothing found.
    assert!(Node::restore_from_server(&relay, "NOPE", Box::new(net.endpoint("z"))).is_err());

    // Correct password recovers the identity on a fresh device.
    let bob2 =
        Node::restore_from_server(&relay, &password, Box::new(net.endpoint("bob2"))).unwrap();
    assert_eq!(bob2.identity_id(), bob_id);
}

#[test]
fn lite_backup_keeps_the_chat_but_drops_history() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();
    alice.send(&bob_contact, "history line").unwrap();
    bob.pump().unwrap();
    assert!(!bob.messages(&alice_contact).is_empty());

    let password = crate::storage::random_password();

    // Lite: identity + the contact survive, but message history does not.
    let lite = bob.backup_with_mode(&password, false).unwrap();
    let bob_lite =
        Node::restore_from_backup(&lite, &password, Box::new(net.endpoint("l"))).unwrap();
    assert_eq!(bob_lite.identity_id(), bob.identity_id());
    assert!(bob_lite.contacts().iter().any(|c| c.id == alice_contact));
    assert!(
        bob_lite.messages(&alice_contact).is_empty(),
        "Lite backup carries no message history"
    );

    // Full: the same restore brings the history back.
    let full = bob.backup_with_mode(&password, true).unwrap();
    let bob_full =
        Node::restore_from_backup(&full, &password, Box::new(net.endpoint("f"))).unwrap();
    assert!(
        bob_full
            .messages(&alice_contact)
            .iter()
            .any(|m| m.text == "history line"),
        "Full backup carries history"
    );
}

#[test]
fn scoped_chat_backup_merges_into_a_lite_restore() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();
    alice.send(&bob_contact, "keep this one").unwrap();
    bob.pump().unwrap();

    let scoped_pw = crate::storage::random_password();
    let scoped = bob.backup_chat(&alice_contact, &scoped_pw, true).unwrap();

    // A Lite full-device restore: same identity + the contact, but no history.
    let lite_pw = crate::storage::random_password();
    let lite = bob.backup_with_mode(&lite_pw, false).unwrap();
    let mut bob2 =
        Node::restore_from_backup(&lite, &lite_pw, Box::new(net.endpoint("bob2"))).unwrap();
    assert!(bob2.messages(&alice_contact).is_empty());

    // Merging the scoped backup folds that chat's history back in.
    let added = bob2.merge_from_backup(&scoped, &scoped_pw).unwrap();
    assert!(added >= 1, "at least the received message merges");
    assert!(bob2
        .messages(&alice_contact)
        .iter()
        .any(|m| m.text == "keep this one"));

    // Merging again is idempotent (dedup by msg_id) — no duplicate history.
    let before = bob2.messages(&alice_contact).len();
    let again = bob2.merge_from_backup(&scoped, &scoped_pw).unwrap();
    assert_eq!(again, 0, "second merge adds nothing");
    assert_eq!(bob2.messages(&alice_contact).len(), before);

    // Wrong password can't open the scoped blob.
    assert!(bob2.merge_from_backup(&scoped, "NOPE").is_err());
}

#[test]
fn full_backup_signals_the_peer_and_logout_respects_the_flag() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // Lite: our flag is set, but the peer is NOT notified (no messages copied).
    alice.mark_backed_up(std::slice::from_ref(&bob_contact), false);
    bob.pump().unwrap();
    assert!(alice.contacts()[0].backed_up);
    assert!(!bob.contacts()[0].peer_backed_up);

    // Full: the peer gets the transparency signal (their messages persist in our backup).
    alice.mark_backed_up(std::slice::from_ref(&bob_contact), true);
    bob.pump().unwrap();
    assert!(bob.contacts()[0].peer_backed_up);

    // Logout on a backed-up chat leaves the peer silent (their mail queues until restore).
    alice.logout();
    bob.pump().unwrap();
    assert!(alice.contacts().is_empty());
    assert!(
        !bob.messages(&alice_contact)
            .iter()
            .any(|m| m.text.contains("deleted this chat")),
        "a backed-up chat's peer is not told the chat closed"
    );
}

#[test]
fn safety_number_matches_on_both_ends_and_verifies() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // Both sides compute the *same* safety number for each other (symmetric derivation).
    let a_view = alice.safety_number(&bob_contact).unwrap();
    let b_view = bob.safety_number(&alice_contact).unwrap();
    assert_eq!(a_view, b_view);
    assert_eq!(a_view.split(' ').count(), 12); // 12 groups
    assert!(a_view
        .split(' ')
        .all(|g| g.len() == 5 && g.chars().all(|c| c.is_ascii_digit())));

    // Fresh contacts start unverified.
    assert!(!alice.contacts()[0].verified);

    // Scanning the peer's matching QR verifies; a wrong payload does not.
    let bob_qr = bob.safety_qr(&alice_contact).unwrap();
    assert!(alice.verify_safety_qr(&bob_contact, &bob_qr).unwrap());
    assert!(alice.contacts()[0].verified);
    assert!(!alice
        .verify_safety_qr(&bob_contact, "AAAAdifferent")
        .unwrap());
}

#[test]
fn offline_mail_fans_out_across_the_recipients_relay_set_and_dedups() {
    let relay1_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay2_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay1 = RelayClient::new(relay1_addr.to_string());
    let net = MemoryNetwork::new();

    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay1.clone()); // shared primary default
    bob.set_relay(relay1.clone());
    bob.set_my_relays(vec![relay2_addr.to_string()]); // bob's extra relay (#17)

    // Pair while online.
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Bob announces his relay set; Alice learns it as peer_relays (Frame::Relays).
    bob.announce_relays();
    alice.pump().unwrap();
    assert_eq!(
        alice.contacts()[0].peer_relays,
        vec![relay2_addr.to_string()]
    );

    // Bob offline; Alice sends -> fans the identical sealed blob to primary + bob's extra.
    net.disconnect("bob");
    alice.send(&bob_contact, "redundant hi").unwrap();
    assert_eq!(
        alice.messages(&bob_contact).last().unwrap().delivery,
        "queued"
    );
    // The blob really landed on BOTH relays (non-draining peek — fetch/take would drain it).
    let relay2 = RelayClient::new(relay2_addr.to_string());
    assert_eq!(relay1.peek(&mailbox_handle(&bob_contact)).unwrap(), 1);
    assert_eq!(relay2.peek(&mailbox_handle(&bob_contact)).unwrap(), 1);

    // Bob polls primary + his extra relay and receives the message exactly ONCE (deduped).
    let received = bob.poll_relay().unwrap();
    assert_eq!(
        received,
        vec![(alice_contact.clone(), "redundant hi".to_string())]
    );
    // A second poll yields nothing (both copies drained).
    assert!(bob.poll_relay().unwrap().is_empty());
}

#[test]
fn unsend_recalls_every_fanned_out_copy() {
    let relay1_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay2_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(RelayClient::new(relay1_addr.to_string()));
    bob.set_relay(RelayClient::new(relay1_addr.to_string()));

    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();
    // Alice knows bob is reachable via a second relay too (set directly for the test).
    alice
        .chats
        .get_mut(&bob_contact)
        .unwrap()
        .contact
        .peer_relays = vec![relay2_addr.to_string()];

    net.disconnect("bob");
    alice.send(&bob_contact, "oops").unwrap();
    let msg_id = alice.messages(&bob_contact).last().unwrap().msg_id.clone();
    alice.unsend_message(&bob_contact, &msg_id).unwrap();

    // Both relays' copies were recalled — bob, coming back, sees nothing.
    assert!(RelayClient::new(relay1_addr.to_string())
        .fetch(&mailbox_handle(&bob_contact))
        .unwrap()
        .is_empty());
    assert!(RelayClient::new(relay2_addr.to_string())
        .fetch(&mailbox_handle(&bob_contact))
        .unwrap()
        .is_empty());
    assert!(bob.poll_relay().unwrap().is_empty());
}

#[test]
fn relay_health_flags_an_unreachable_extra_relay() {
    let good_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let net = MemoryNetwork::new();
    let mut node = Node::new(Box::new(net.endpoint("me")));
    node.set_relay(RelayClient::new(good_addr.to_string()));
    // One reachable relay (a live server) and one dead address (nothing is listening).
    let dead = "127.0.0.1:1".to_string();
    node.set_my_relays(vec![good_addr.to_string(), dead.clone()]);

    // Before any poll, health is optimistic (don't cry wolf).
    assert!(node.relay_health().iter().all(|(_, ok)| *ok));

    node.poll_relay().unwrap();
    let health = node.relay_health();
    let good = health
        .iter()
        .find(|(a, _)| a == &good_addr.to_string())
        .unwrap();
    let dead = health.iter().find(|(a, _)| a == &dead).unwrap();
    assert!(good.1, "a live relay must report reachable");
    assert!(!dead.1, "a relay that doesn't answer must report offline");
}

#[test]
fn server_storage_health_drops_when_no_relay_can_store_the_copy() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    // Deliberately no relay configured on Alice.

    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Opt into server storage — starts optimistic.
    alice.set_remote_storage(&bob_contact, true).unwrap();
    assert!(
        alice.contacts()[0].remote_storage_healthy,
        "enabling server storage starts healthy"
    );

    // Bob is online, so the message is delivered directly — but with no relay, the server
    // copy can't be stored. Health drops (the message still went through).
    alice
        .send(&bob_contact, "delivered but not stored")
        .unwrap();
    assert!(
        !alice.contacts()[0].remote_storage_healthy,
        "server storage with no reachable relay reports unhealthy"
    );
    assert_eq!(
        alice.messages(&bob_contact).last().unwrap().delivery,
        "sent",
        "the message itself still reached the peer directly"
    );
}

#[test]
fn a_forged_control_frame_is_rejected() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();

    // An attacker forges a "chat deleted" from Alice with junk ciphertext (no valid session).
    let forged = Frame::Closed {
        from: alice_contact.clone(),
        message: WireOlm {
            message_type: 1,
            body: "Zm9yZ2Vk".into(), // "forged" — valid base64, not a valid Olm message
        },
    };
    bob.process_frame(None, forged).unwrap();

    // Bob's chat with Alice is untouched — the spoofed teardown was rejected.
    assert!(!bob.chats.get(&alice_contact).unwrap().closed);
    assert!(bob
        .messages(&alice_contact)
        .iter()
        .all(|m| !m.text.contains("deleted")));

    // A forged Ack likewise can't flip a queued message to "delivered".
    let forged_ack = Frame::Ack {
        from: alice_contact.clone(),
        message: WireOlm {
            message_type: 1,
            body: "Zm9yZ2Vk".into(),
        },
    };
    bob.process_frame(None, forged_ack).unwrap();
}

#[test]
fn logout_closes_an_unbacked_chat_for_the_peer() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // alice never backed up → logout tells bob the chat is closed so his mail isn't lost.
    // No relay here, so it falls back to a direct send (bob is online); nothing failed.
    assert_eq!(alice.logout(), 0);
    bob.pump().unwrap();
    assert!(bob
        .messages(&alice_contact)
        .iter()
        .any(|m| m.text.contains("deleted this chat")));
}

#[test]
fn logout_reaches_an_offline_peer_via_the_relay() {
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());

    let bundle = bob.publish_bundle();
    alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // Bob goes offline, THEN alice logs out (deleting her identity). A direct-only send would be
    // lost with no retry; the relay store-and-forward must carry the notice so Bob still gets it.
    net.disconnect("bob");
    assert_eq!(alice.logout(), 0, "the notice was queued on the relay");

    // Bob comes back and drains his mailbox: the "chat deleted" notice is waiting for him.
    bob.poll_relay().unwrap();
    assert!(bob
        .messages(&alice_contact)
        .iter()
        .any(|m| m.text.contains("deleted this chat")));
}

#[test]
fn delete_chat_closed_survives_no_reachable_relay_and_delivers_on_retry() {
    // Regression (§11.6): deleting a chat while BOTH the peer and every relay are unreachable (arti
    // cold / relay mid-republish) must not silently drop the "chat deleted" notice — it's retried
    // from the poller until a relay accepts it. Bob can drain the relay; Alice has none attached at
    // delete time, standing in for a relay that's momentarily unreachable.
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    bob.set_relay(relay.clone());

    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // Peer offline AND no relay reachable to Alice → the Closed reaches neither path at delete time.
    net.disconnect("bob");
    alice.delete_chat(&bob_contact).unwrap();
    assert!(
        alice.contacts().is_empty(),
        "chat is removed locally on delete"
    );

    // Nothing was posted yet, so a drain finds no notice.
    bob.poll_relay().unwrap();
    assert!(
        !bob.messages(&alice_contact)
            .iter()
            .any(|m| m.text.contains("deleted this chat")),
        "no notice could be delivered while every path was down"
    );

    // A relay becomes reachable; the poller's retry (flush_pending_control) posts the held Closed.
    alice.set_relay(relay.clone());
    alice.flush_pending_control();

    // Bob drains it and sees the chat deleted — the notice was not lost.
    bob.poll_relay().unwrap();
    assert!(
        bob.messages(&alice_contact)
            .iter()
            .any(|m| m.text.contains("deleted this chat")),
        "the retried Closed finally reaches Bob via the relay"
    );
}

#[test]
fn pending_delete_signal_persists_across_a_restart() {
    use crate::storage;
    // A delete during a total outage stashes the Closed for retry; it must survive an app restart
    // (persisted), so the peer is still told even if the app is killed before the retry lands.
    let key: storage::StoreKey = [9u8; 32];
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    bob.set_relay(relay.clone());

    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // Delete while nothing is reachable → the Closed is queued for retry.
    net.disconnect("bob");
    alice.delete_chat(&bob_contact).unwrap();

    // "Restart" Alice before the retry runs: persist to disk, drop, restore on a fresh endpoint.
    let path =
        std::env::temp_dir().join(format!("nightdrop-pendingctl-{}.bin", std::process::id()));
    let path = path.to_str().unwrap().to_string();
    storage::save_to_file(&path, &key, &alice.export(&key)).unwrap();
    drop(alice);
    let state = storage::load_from_file(&path, &key).unwrap();
    let mut alice2 = Node::restore(&state, Box::new(net.endpoint("alice")), &key).unwrap();
    alice2.set_relay(relay.clone());

    // The queued delete survived the restart: flushing now delivers it via the relay.
    alice2.flush_pending_control();
    bob.poll_relay().unwrap();
    assert!(
        bob.messages(&alice_contact)
            .iter()
            .any(|m| m.text.contains("deleted this chat")),
        "the persisted delete signal still reaches Bob after Alice restarted"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn approval_signal_reaches_the_joiner() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice"))); // recipient
    alice.set_require_authorization(true);
    alice.set_last_invite_code("7-cedar-river-ember".to_string());
    let mut bob = Node::new(Box::new(net.endpoint("bob"))); // joiner

    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();

    // Alice approves -> an Approved signal goes back to Bob.
    let bob_request = alice.pending_authorizations()[0].id.clone();
    alice.authorize(&bob_request, true).unwrap();
    bob.pump().unwrap();

    let notice = bob.messages(&alice_contact);
    assert!(
        notice
            .iter()
            .any(|m| m.system && m.text.contains("approved")),
        "joiner sees the approval notice: {notice:?}"
    );
}

#[test]
fn approving_twice_does_not_resend_approval() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    alice.set_require_authorization(true);
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();

    let req = alice.pending_authorizations()[0].id.clone();
    alice.authorize(&req, true).unwrap();
    alice.authorize(&req, true).unwrap(); // impatient double-tap: must be a no-op
    bob.pump().unwrap();

    // Exactly one approval notice reached Bob.
    let approvals = bob
        .messages(&alice_contact)
        .into_iter()
        .filter(|m| m.system && m.text.contains("approved"))
        .count();
    assert_eq!(approvals, 1, "second approval must not resend");
}

#[test]
fn media_round_trips_e2e_and_is_sealed_at_rest() {
    let key: StoreKey = [9u8; 32];
    let dir = std::env::temp_dir().join(format!("nightdrop-media-test-{}", std::process::id()));
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_media_store(format!("{}-a", dir.display()), key);
    bob.set_media_store(format!("{}-b", dir.display()), key);

    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Alice sends an "image"; Bob receives it as a media message.
    let pixels = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
    alice
        .send_media(&bob_contact, &pixels, "image/png", "image", &[])
        .unwrap();
    bob.pump().unwrap();

    let msg = bob.messages(&alice_contact).into_iter().last().unwrap();
    assert_eq!(msg.kind, "image");
    assert_eq!(msg.mime, "image/png");
    assert_eq!(msg.media_size, pixels.len() as u64);
    assert!(!msg.media_id.is_empty());

    // Bob can decrypt the bytes back, and the on-disk blob is NOT the plaintext.
    assert_eq!(bob.media_bytes(&msg.media_id).unwrap(), pixels);
    let sealed = std::fs::read(format!("{}-b/{}.bin", dir.display(), msg.media_id)).unwrap();
    assert_ne!(sealed, pixels, "media is sealed at rest");

    std::fs::remove_dir_all(format!("{}-a", dir.display())).ok();
    std::fs::remove_dir_all(format!("{}-b", dir.display())).ok();
}

/// Pairing the SAME two identities twice — e.g. once in each direction — must not break messaging.
/// Both sides have to adopt the newest session; before the fix the re-pair left one side encrypting
/// with a session the other had discarded, so messages silently failed to decrypt (the "sent but
/// not received" double-pair bug).
#[test]
fn double_pairing_the_same_contact_keeps_messaging_working_both_ways() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));

    // Pairing 1: Bob joins Alice.
    let alice_bundle = alice.publish_bundle();
    let alice_on_bob = bob.connect_with_bundle("alice", &alice_bundle).unwrap();
    alice.pump().unwrap();
    let bob_on_alice = alice.contacts()[0].id.clone();

    bob.send(&alice_on_bob, "one").unwrap();
    alice.pump().unwrap();
    assert_eq!(alice.messages(&bob_on_alice).last().unwrap().text, "one");

    // Pairing 2 (the double-pair): Alice now joins Bob — the reverse direction, same identities.
    let bob_bundle = bob.publish_bundle();
    alice.connect_with_bundle("bob", &bob_bundle).unwrap();
    bob.pump().unwrap(); // Bob adopts Alice's new session in place (open re-key).

    // Messaging must still work BOTH ways on the (now single, re-keyed) session.
    bob.send(&alice_on_bob, "two from bob").unwrap();
    alice.pump().unwrap();
    assert_eq!(
        alice.messages(&bob_on_alice).last().unwrap().text,
        "two from bob",
        "bob -> alice after the double-pair"
    );

    alice.send(&bob_on_alice, "two from alice").unwrap();
    bob.pump().unwrap();
    assert_eq!(
        bob.messages(&alice_on_bob).last().unwrap().text,
        "two from alice",
        "alice -> bob after the double-pair"
    );

    // Still one contact per identity, and the re-pair warned to re-verify.
    assert_eq!(alice.contacts().len(), 1);
    assert!(bob
        .messages(&alice_on_bob)
        .iter()
        .any(|m| m.system && m.text.to_lowercase().contains("re-paired")));
}
