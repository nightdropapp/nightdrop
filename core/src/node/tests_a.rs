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
    bob.pump().unwrap();
    // Asserted on the history rather than on pump's return: pairing also queues a capability
    // announce (`Frame::Burns`), which is sent while Bob is still online and names his contact
    // without being a message. The claim under test is that no MESSAGE crossed directly.
    assert!(
        !bob.messages(&alice_contact)
            .iter()
            .any(|m| !m.from_me && !m.system),
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

// The invariant above, across a restart — which is where it was being lost. Found on a device
// (2026-08-02): a request arrived on the desktop, the app restarted minutes later, and the chat
// came back as an ordinary approved contact. Nobody ever approved it. `PersistedChat` had no
// `authorized` field and restore hardcoded `true`, on a comment's assumption that only approved
// chats were ever saved — but a pending request is a chat, and is saved like any other.
//
// So a stranger's Hello plus any restart was enough to become a contact, and the peer stayed
// stuck on "waiting for the other person to accept" while their messages were being accepted.
#[test]
fn a_pending_request_is_still_pending_after_a_restart() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    alice.set_require_authorization(true);
    let mut bob = Node::new(Box::new(net.endpoint("bob"))); // stranger

    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    assert_eq!(alice.pending_authorizations().len(), 1);
    let bob_contact = alice.pending_authorizations()[0].id.clone();

    let key: StoreKey = [3u8; 32];
    let state = alice.export(&key);
    drop(alice);
    let mut alice2 = Node::restore(&state, Box::new(net.endpoint("alice")), &key).unwrap();

    assert_eq!(
        alice2.pending_authorizations().len(),
        1,
        "a restart is not an approval"
    );
    assert!(
        alice2.contacts().is_empty(),
        "an unapproved stranger must not be a contact"
    );
    assert!(
        alice2.send(&bob_contact, "hi").is_err(),
        "and must not be messageable"
    );

    // The other half: approval survives too, so this doesn't just make everyone pending forever.
    alice2.authorize(&bob_contact, true).unwrap();
    let state = alice2.export(&key);
    drop(alice2);
    let alice3 = Node::restore(&state, Box::new(net.endpoint("alice")), &key).unwrap();
    assert_eq!(alice3.contacts().len(), 1, "an approval is not forgotten");
    assert!(alice3.pending_authorizations().is_empty());
}

// A state file written before `authorized` existed has no such field. It must read as APPROVED:
// defaulting to false would turn every real contact on an existing install into a request the user
// has to re-approve, with no way to tell which were genuine.
#[test]
fn a_state_file_without_the_authorized_field_restores_as_approved() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    assert_eq!(alice.contacts().len(), 1);

    let key: StoreKey = [4u8; 32];
    let state = alice.export(&key);
    // Exactly what an older file looks like: the field simply is not in the JSON.
    let mut json = serde_json::to_value(&state).unwrap();
    for chat in json["chats"].as_array_mut().unwrap() {
        chat.as_object_mut().unwrap().remove("authorized");
    }
    let old: crate::storage::PersistedState = serde_json::from_value(json).unwrap();

    let alice2 = Node::restore(&old, Box::new(net.endpoint("alice")), &key).unwrap();
    assert_eq!(
        alice2.contacts().len(),
        1,
        "an upgrade must not demote existing contacts to requests"
    );
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
    let stored = relay
        .take(&alice.post_handle(&bob_contact).unwrap())
        .unwrap();
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

/// A restore that fails after `take` has removed the relay's copy must put it back: otherwise one
/// failed attempt destroys the only server backup (self-audit 2026-10-17, R8-3).
#[test]
fn a_failed_server_restore_leaves_the_backup_on_the_relay() {
    let relay = RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string());
    let net = MemoryNetwork::new();
    let password = crate::storage::random_password();
    let handle = super::backup_handle(&password).unwrap();
    // Something under the right handle that will not open: damaged, or from a newer app.
    relay
        .post(&handle, b"not a backup", Duration::from_secs(3600))
        .unwrap();
    assert!(Node::restore_from_server(&relay, &password, Box::new(net.endpoint("z"))).is_err());
    assert_eq!(relay.peek(&handle).unwrap(), 1, "the backup is still there");
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
fn a_chat_backup_merges_only_into_its_own_identity_and_within_the_contact_cap() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let alice_contact = bob
        .connect_with_bundle("alice", &alice.publish_bundle())
        .unwrap();
    alice.pump().unwrap();
    let pw = crate::storage::random_password();
    let scoped = bob.backup_chat(&alice_contact, &pw, true).unwrap();

    // Someone else's chat would go out under a key its contact never paired with.
    let mut stranger = Node::new(Box::new(net.endpoint("stranger")));
    let err = stranger
        .merge_from_backup(&scoped, &pw)
        .unwrap_err()
        .to_string();
    assert!(err.contains("different identity"), "{err}");
    assert!(stranger.contacts().is_empty());

    // The same identity, already at the cap, cannot grow past it by merging.
    let lite_pw = crate::storage::random_password();
    let lite = bob.backup_with_mode(&lite_pw, false).unwrap();
    let mut bob2 =
        Node::restore_from_backup(&lite, &lite_pw, Box::new(net.endpoint("bob2"))).unwrap();
    bob2.delete_chat(&alice_contact).unwrap();
    let mut peers = Vec::new();
    for i in 0..super::mailbox::MAX_CONTACTS {
        let mut peer = Node::new(Box::new(net.endpoint(&format!("p{i}"))));
        bob2.connect_with_bundle(&format!("p{i}"), &peer.publish_bundle())
            .unwrap();
        peers.push(peer);
    }
    let err = bob2
        .merge_from_backup(&scoped, &pw)
        .unwrap_err()
        .to_string();
    assert!(err.contains("50 contacts"), "{err}");
    assert!(bob2.messages(&alice_contact).is_empty());
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
fn verifying_signals_the_peer_informationally_without_auto_trust() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // Alice marks the safety number verified. Bob learns *she* did — but it must not flip Bob's
    // own verified flag (informational only), and Alice's own peer_verified stays false.
    alice.set_verified(&bob_contact, true);
    bob.pump().unwrap();
    assert!(alice.contacts()[0].verified);
    assert!(!alice.contacts()[0].peer_verified);
    assert!(
        bob.contacts()[0].peer_verified,
        "Bob sees that Alice verified"
    );
    assert!(
        !bob.contacts()[0].verified,
        "Bob is NOT auto-trusted — he must still verify himself"
    );
    assert!(
        bob.messages(&alice_contact)
            .iter()
            .any(|m| m.system && m.text.contains("marked this chat's safety number verified")),
        "Bob gets an informational note"
    );

    // Clearing verification propagates the same way, and doesn't spam a duplicate note.
    alice.set_verified(&bob_contact, false);
    bob.pump().unwrap();
    assert!(!bob.contacts()[0].peer_verified);

    // Re-pairing resets the peer_verified hint (a new session invalidates the old claim).
    alice.set_verified(&bob_contact, true);
    bob.pump().unwrap();
    assert!(bob.contacts()[0].peer_verified);
    let bundle2 = bob.publish_bundle();
    alice.connect_with_bundle("bob", &bundle2).unwrap();
    bob.pump().unwrap();
    assert!(
        !bob.contacts()[0].peer_verified,
        "re-pair clears the peer's stale verification signal"
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

    // Bob's set rode in his Hello; Alice knows it at once.
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
    assert_eq!(
        relay1
            .peek(&alice.post_handle(&bob_contact).unwrap())
            .unwrap(),
        1
    );
    assert_eq!(
        relay2
            .peek(&alice.post_handle(&bob_contact).unwrap())
            .unwrap(),
        1
    );

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
        .fetch(&alice.post_handle(&bob_contact).unwrap())
        .unwrap()
        .is_empty());
    assert!(RelayClient::new(relay2_addr.to_string())
        .fetch(&alice.post_handle(&bob_contact).unwrap())
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
    // Alice reads Bob's first frame: his relay set, without which her copies to him are held.
    alice.pump().unwrap();
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
    // Alice reads Bob's first frame: his relay set, without which her copies to him are held.
    alice.pump().unwrap();
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
    // Alice reads Bob's first frame: his relay set, without which her copies to him are held.
    alice.pump().unwrap();
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
    bob.note_awaiting_approval(&alice_contact); // as the API's join paths do
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
    bob.note_awaiting_approval(&alice_contact); // as the API's join paths do
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

/// Seen on the S23, 2026-10-11: the approval came twice (a direct copy that looked lost, then the
/// relay copy put behind it) and the joiner showed "Your chat request was approved" twice. Only
/// the approval that ends the wait is shown; a repeat changes nothing.
#[test]
fn an_approval_that_arrives_twice_is_shown_once() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    alice.set_require_authorization(true);
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    bob.note_awaiting_approval(&alice_contact);
    alice.pump().unwrap();

    let approved = Frame::Approved {
        from: alice_contact.clone(),
        code: String::new(),
    };
    bob.process_frame(None, approved.clone()).unwrap();
    bob.process_frame(None, approved).unwrap();

    let approvals = bob
        .messages(&alice_contact)
        .into_iter()
        .filter(|m| m.system && m.text.contains("approved"))
        .count();
    assert_eq!(approvals, 1, "one approval notice for one approval");
}

/// A memory transport that reports itself asynchronous, as Tor does, so sends take the deferred,
/// off-lock path.
struct AsyncMem(crate::transport::MemoryTransport);

impl crate::transport::Transport for AsyncMem {
    fn address(&self) -> String {
        self.0.address()
    }
    fn is_synchronous(&self) -> bool {
        false
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        self.0.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(String, Vec<u8>)> {
        self.0.try_recv()
    }
}

/// An attachment over a real network goes through the poller's off-lock send path, as text does:
/// sent inline it held the core lock — every other call and the poller — for a whole Tor upload.
#[test]
fn media_on_a_real_network_is_deferred_off_the_lock_and_delivered_in_order() {
    let key: StoreKey = [5u8; 32];
    let dir = std::env::temp_dir().join(format!("nightdrop-media-defer-{}", std::process::id()));
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(AsyncMem(net.endpoint("alice"))));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_media_store(format!("{}-a", dir.display()), key);
    bob.set_media_store(format!("{}-b", dir.display()), key);
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    alice
        .send_media(
            &bob_contact,
            &super::media_scrub::test_mp4(),
            "video/mp4",
            "video",
            super::media_scrub::TEST_JPEG,
        )
        .unwrap();
    let mine = |n: &Node| {
        n.messages(&bob_contact)
            .into_iter()
            .rfind(|m| m.from_me && !m.system)
            .unwrap()
    };
    assert_eq!(
        mine(&alice).delivery,
        "queued",
        "stored at once, not sent inline"
    );
    assert_eq!(
        alice.pending_sends.len(),
        2,
        "the pre-signal and the payload, in that order"
    );
    let early = bob.pump().unwrap();
    let early_msgs: Vec<(bool, String, String)> = bob
        .messages(&alice_contact)
        .into_iter()
        .map(|m| (m.system, m.kind, m.text))
        .collect();
    assert!(
        early_msgs
            .iter()
            .all(|(system, kind, _)| *system || kind != "video"),
        "nothing went out inline: {early:?} {early_msgs:?}"
    );

    let plan = alice.plan_pending_sends().unwrap();
    let outcomes = crate::node::execute_sends(&plan);
    alice.apply_send_outcomes(outcomes);
    assert_eq!(
        mine(&alice).delivery,
        "sent",
        "the outcome found the attachment by its transfer id"
    );

    bob.pump().unwrap();
    let got = bob
        .messages(&alice_contact)
        .into_iter()
        .find(|m| !m.from_me && m.kind == "video" && !m.media_id.is_empty())
        .expect("the attachment arrived");
    assert_eq!(
        bob.media_plain(&got.media_id).unwrap(),
        super::media_scrub::test_mp4()
    );
    let _ = std::fs::remove_dir_all(format!("{}-a", dir.display()));
    let _ = std::fs::remove_dir_all(format!("{}-b", dir.display()));
}

/// The text-send gates apply to every attachment — they used to apply to videos only.
#[test]
fn an_image_cannot_be_sent_into_a_deleted_chat() {
    let key: StoreKey = [6u8; 32];
    let dir = std::env::temp_dir().join(format!("nightdrop-media-gate-{}", std::process::id()));
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_media_store(dir.display().to_string(), key);
    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();
    alice.chats.get_mut(&bob_contact).unwrap().closed = true;

    let err = alice
        .send_media(&bob_contact, b"img", "image/png", "image", b"")
        .unwrap_err()
        .to_string();
    assert!(err.contains("deleted"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
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
    let pixels = super::media_scrub::TEST_JPEG.to_vec();
    alice
        .send_media(&bob_contact, &pixels, "image/jpeg", "image", &[])
        .unwrap();
    bob.pump().unwrap();

    let msg = bob.messages(&alice_contact).into_iter().last().unwrap();
    assert_eq!(msg.kind, "image");
    assert_eq!(msg.mime, "image/jpeg");
    assert_eq!(msg.media_size, pixels.len() as u64);
    assert!(!msg.media_id.is_empty());

    // Bob can decrypt the bytes back, and the on-disk blob is NOT the plaintext.
    assert_eq!(bob.media_plain(&msg.media_id).unwrap(), pixels);
    let sealed = std::fs::read(format!("{}-b/{}.bin", dir.display(), msg.media_id)).unwrap();
    assert_ne!(sealed, pixels, "media is sealed at rest");

    std::fs::remove_dir_all(format!("{}-a", dir.display())).ok();
    std::fs::remove_dir_all(format!("{}-b", dir.display())).ok();
}

/// What the app draws is decoded in-process, so a contact's image reaches it only as a plain JPEG:
/// a PNG (or anything else) sent as an "image" is stored but never handed over for drawing, while
/// the same chat's real JPEG is (self-audit 2026-10-17, P2).
#[test]
fn only_a_plain_jpeg_is_handed_to_the_app_to_draw() {
    let key: StoreKey = [9u8; 32];
    let dir = std::env::temp_dir().join(format!("nightdrop-draw-test-{}", std::process::id()));
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_media_store(format!("{}-a", dir.display()), key);
    bob.set_media_store(format!("{}-b", dir.display()), key);
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    let png = include_bytes!("../../tests/fixtures/small.png");
    let jpeg = include_bytes!("../../tests/fixtures/small-baseline.jpg");
    for (bytes, mime) in [(&png[..], "image/png"), (&jpeg[..], "image/jpeg")] {
        alice
            .send_media_unclean(&bob_contact, bytes, mime, "image")
            .unwrap();
    }
    bob.pump().unwrap();
    let got: Vec<_> = bob
        .messages(&alice_contact)
        .into_iter()
        .filter(|m| !m.media_id.is_empty())
        .collect();
    assert_eq!(got.len(), 2);
    assert!(
        bob.media_bytes(&got[0].media_id).is_err(),
        "the PNG is not drawn"
    );
    assert_eq!(bob.media_bytes(&got[1].media_id).unwrap(), jpeg);

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

#[test]
fn a_peer_that_cannot_report_screenshots_says_so_and_silence_never_means_yes() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // Nobody has said anything yet, and that must stay UNKNOWN rather than collapsing to the
    // reassuring answer. Reading silence as "captures are visible" is the false guarantee this
    // whole signal exists to remove.
    assert_eq!(
        bob.contacts()[0].peer_captures_silent,
        None,
        "an unannounced peer is unknown, never 'captures are visible'"
    );

    // Alice is on a device that cannot report captures, and tells Bob — who is the one deciding
    // what to send her.
    alice.announce_captures(false);
    bob.pump().unwrap();
    assert_eq!(
        bob.contacts()[0].peer_captures_silent,
        Some(true),
        "Bob must learn that a screenshot on Alice's device raises no notice"
    );
    // It is told to the peer, not to the person who already knows what they did.
    assert_eq!(alice.contacts()[0].peer_captures_silent, None);

    // Announced only on a change: re-stating the same value within a session puts nothing on the
    // wire, so a caller may hand it the OS answer as often as it likes.
    let before = bob.messages(&alice_contact).len();
    alice.announce_captures(false);
    bob.pump().unwrap();
    assert_eq!(bob.messages(&alice_contact).len(), before);

    // Upgrading across the boundary flips it the other way.
    alice.announce_captures(true);
    bob.pump().unwrap();
    assert_eq!(bob.contacts()[0].peer_captures_silent, Some(false));

    // And it is not a history event either way — it is a standing property of their device, so it
    // belongs in the chat header, not as a line posted into every existing chat on rollout.
    assert!(
        !bob.messages(&alice_contact)
            .iter()
            .any(|m| m.system && m.text.to_lowercase().contains("screenshot")),
        "a capability announcement must not post system messages"
    );
    let _ = bob_contact;
}

#[test]
fn an_unparseable_frame_costs_that_frame_and_nothing_else() {
    // Version skew is the ordinary way to meet a frame we cannot decode: every control frame added
    // to `Frame` is undecodable to everyone who has not updated yet, and `Captures` is announced to
    // every open chat at launch, so an older peer meets one on the direct path routinely.
    //
    // `pump` used to `?` on the decode, aborting the whole call — and with it the caller's relay
    // harvest and send bookkeeping for that tick — over a single frame it did not recognise. The
    // relay drain has always skipped and continued; the direct path now matches it.
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();

    // Something Bob's build cannot parse, then a perfectly good message behind it.
    net.endpoint("garbage")
        .send("bob", b"not a frame at all")
        .unwrap();
    alice.send(&contact, "still here").unwrap();

    let got = bob.pump().expect("one bad frame must not fail the pump");
    assert!(
        got.iter().any(|(_, text)| text == "still here"),
        "the message queued behind an unparseable frame must still arrive"
    );
}

#[test]
fn a_contact_paired_after_the_announcement_still_learns_the_capability() {
    // The broadcast only walks the chats that exist when it runs, and the value then never changes
    // again — so without a per-chat announce on pairing, everyone met later would read our silence
    // as "a screenshot would be reported", which is exactly backwards.
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let mut carol = Node::new(Box::new(net.endpoint("carol")));

    // Alice settles her capability with nobody to tell.
    alice.announce_captures(false);

    // Bob pairs with her afterwards (Alice is the joiner).
    let bundle = bob.publish_bundle();
    alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    assert_eq!(
        bob.contacts()[0].peer_captures_silent,
        Some(true),
        "a peer paired after the broadcast must still be told"
    );

    // …and the reverse direction: Carol pairs *to* Alice, so Alice is the one receiving the Hello.
    let alice_bundle = alice.publish_bundle();
    carol.connect_with_bundle("alice", &alice_bundle).unwrap();
    alice.pump().unwrap();
    carol.pump().unwrap();
    assert_eq!(
        carol.contacts()[0].peer_captures_silent,
        Some(true),
        "the inviter side must announce to an inbound pairing too"
    );
}

#[test]
fn a_contact_that_blocks_captures_says_so_and_it_survives_a_restore() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let _bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();
    assert_eq!(
        bob.contacts()[0].peer_blocks_capture,
        None,
        "unknown until said"
    );

    // Alice turns "Block screenshots" on: Bob's app learns it, so it blocks captures in her chat.
    alice.announce_block(true);
    bob.pump().unwrap();
    assert_eq!(bob.contacts()[0].peer_blocks_capture, Some(true));
    assert_eq!(
        alice.contacts()[0].peer_blocks_capture,
        None,
        "told to the peer only"
    );
    // Rides the same frame as the capability signal without disturbing it.
    alice.announce_captures(true);
    bob.pump().unwrap();
    assert_eq!(bob.contacts()[0].peer_captures_silent, Some(false));
    assert_eq!(bob.contacts()[0].peer_blocks_capture, Some(true));

    // Off again, and no history line either way.
    alice.announce_block(false);
    bob.pump().unwrap();
    assert_eq!(bob.contacts()[0].peer_blocks_capture, Some(false));
    assert!(!bob.messages(&alice_contact).iter().any(|m| m.system));

    // Persisted: a backup restore keeps it.
    alice.announce_block(true);
    bob.pump().unwrap();
    let password = "correct horse battery staple";
    let backup = bob.backup(password).unwrap();
    let restored =
        Node::restore_from_backup(&backup, password, Box::new(net.endpoint("bob2"))).unwrap();
    assert_eq!(restored.contacts()[0].peer_blocks_capture, Some(true));
}

#[test]
fn a_contact_who_updates_is_told_the_capture_block_again() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    bob.set_app_version("0.2.0+416");
    bob.announce_version();
    alice.pump().unwrap();

    alice.announce_block(true);
    bob.pump().unwrap();
    alice.pump().unwrap(); // delivery confirmed: told_block = Some(true)

    // Bob's (older) build ignored it: model that by forgetting it on his side — the value and the
    // sequence, since a build that does not know the marker never read either.
    let chat = bob.chats.values_mut().next().unwrap();
    chat.contact.peer_blocks_capture = None;
    chat.peer_block_seq = 0;

    // Bob updates; his new version reaches Alice, who tells him again on the next tick.
    bob.set_app_version("0.2.1+417");
    bob.announce_version();
    alice.pump().unwrap();
    alice.reannounce_block();
    bob.pump().unwrap();
    assert_eq!(bob.contacts()[0].peer_blocks_capture, Some(true));
    let _ = bob_contact;
}

#[test]
fn screenshot_notifies_both_sides_every_time_and_cannot_be_forged() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // Both sides learn about it: the capturer sees what their peer was told, the peer sees the
    // event. Screenshots are allowed (#1) precisely because they're made visible instead.
    alice.report_screenshot(&bob_contact);
    bob.pump().unwrap();
    let mine = alice.messages(&bob_contact);
    assert!(
        mine.iter()
            .any(|m| m.system && m.text.contains("You took a screenshot")),
        "the capturer must see that the peer was told"
    );
    let theirs = bob.messages(&alice_contact);
    assert_eq!(
        theirs
            .iter()
            .filter(|m| m.system && m.text.contains("took a screenshot"))
            .count(),
        1
    );

    // An event, not a state flag: a second screenshot is reported again. Timing and count are the
    // informative part, so `BackedUp`-style once-only dedupe would lose real information.
    alice.report_screenshot(&bob_contact);
    bob.pump().unwrap();
    assert_eq!(
        bob.messages(&alice_contact)
            .iter()
            .filter(|m| m.system && m.text.contains("took a screenshot"))
            .count(),
        2,
        "each screenshot is its own notice"
    );

    // Forgery: a genuine peer's ciphertext must not be re-labellable as a screenshot. Mallory has
    // a real session with Bob, so she can produce a valid control frame — but the marker inside is
    // domain-separated per frame type, so splicing it into a `Screenshot` fails to verify.
    let mut mallory = Node::new(Box::new(net.endpoint("mallory")));
    // A fresh bundle: one-time pre-keys are single-use, and Alice already consumed the first.
    let bundle2 = bob.publish_bundle();
    let victim = mallory.connect_with_bundle("bob", &bundle2).unwrap();
    bob.pump().unwrap();
    let before = bob
        .messages(&victim_view(&bob, &victim))
        .iter()
        .filter(|m| m.text.contains("took a screenshot"))
        .count();
    if let Some((addr, frame)) = mallory.authed_control(&victim, MARK_BACKEDUP, |from, message| {
        Frame::BackedUp { from, message }
    }) {
        let spliced = match frame {
            Frame::BackedUp { from, message } => Frame::Screenshot { from, message },
            other => other,
        };
        let _ = mallory.deliver(&addr, &victim, &spliced);
    }
    bob.pump().unwrap();
    assert_eq!(
        bob.messages(&victim_view(&bob, &victim))
            .iter()
            .filter(|m| m.text.contains("took a screenshot"))
            .count(),
        before,
        "a cross-type splice must not produce a screenshot notice"
    );
}

#[test]
fn a_screen_recording_is_reported_to_both_sides_as_its_own_kind() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    alice.report_screen_recording(&bob_contact);
    bob.pump().unwrap();
    assert!(alice
        .messages(&bob_contact)
        .iter()
        .any(|m| m.system && m.text.contains("screen is being recorded")));
    let theirs = bob.messages(&alice_contact);
    assert_eq!(
        theirs
            .iter()
            .filter(|m| m.system && m.text.contains("recording their screen"))
            .count(),
        1
    );
    assert!(
        !theirs.iter().any(|m| m.text.contains("took a screenshot")),
        "a recording is not reported as a screenshot"
    );

    // The session still works afterwards: the one decrypt per frame kept the ratchets in step.
    alice.send(&bob_contact, "after").unwrap();
    bob.pump().unwrap();
    assert!(bob
        .messages(&alice_contact)
        .iter()
        .any(|m| !m.system && m.text == "after"));
}

#[test]
fn re_pairing_an_approved_chat_echoes_the_approval() {
    // Field report (2026-08-01): pairing again with a contact who had already approved us left the
    // joiner stuck on "waiting for the other person to accept", while the inviter showed a live
    // chat and no request — because the inviter's chat was *already* authorized, so `authorize()`
    // (the only thing that sends the approval echo) never ran.
    let net = MemoryNetwork::new();
    let mut inviter = Node::new(Box::new(net.endpoint("inviter")));
    inviter.set_require_authorization(true);
    let mut joiner = Node::new(Box::new(net.endpoint("joiner")));

    // First pairing, approved the normal way.
    let bundle = inviter.publish_bundle();
    let inviter_contact = joiner.connect_with_bundle("inviter", &bundle).unwrap();
    inviter.pump().unwrap();
    let joiner_contact = inviter.pending_authorizations()[0].id.clone();
    inviter.authorize(&joiner_contact, true).unwrap();
    joiner.pump().unwrap();

    // Pair again — the reverse direction, or simply a second scan. The joiner posts the same
    // "waiting to be accepted" notice the api layer adds on every join (`connect_via_qr` /
    // `join_via_short_code`), which is the thing that must get cleared.
    let bundle2 = inviter.publish_bundle();
    joiner.connect_with_bundle("inviter", &bundle2).unwrap();
    joiner.note_awaiting_approval(&inviter_contact);
    assert!(
        awaiting_approval(&joiner, &inviter_contact),
        "precondition: the joiner is waiting"
    );

    inviter.pump().unwrap();
    assert!(
        inviter.pending_authorizations().is_empty(),
        "a known, approved contact must not resurface as a request"
    );
    joiner.pump().unwrap();
    assert!(
        !awaiting_approval(&joiner, &inviter_contact),
        "the joiner must be told the chat is already approved, not left waiting forever"
    );
}

/// Whether `node`'s chat with `contact` is showing the "waiting to be accepted" notice.
fn awaiting_approval(node: &Node, contact: &str) -> bool {
    node.messages(contact)
        .iter()
        .any(|m| m.system && m.kind == "await_approval")
}

#[test]
fn last_seen_tracks_authenticated_contact_including_silent_acks() {
    // Silence detection (#3 follow-up): the peer-side signal that replaces a duress "chat deleted"
    // notice, which cannot exist (`duress-wipe.md` §5). It must count *any* proof of life, not just
    // typed messages — otherwise a peer who reads but doesn't reply looks gone.
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // Pairing itself is contact, so a brand-new chat is never reported as silent.
    let paired_at = alice.contacts()[0].last_seen_secs;
    assert!(paired_at > 0, "pairing starts the clock");

    // A control frame that verifies on their ratchet is proof of life even though the user never
    // typed: this is the case that matters, since a peer can be alive and simply not reply.
    alice.chats.get_mut(&bob_contact).unwrap().last_seen = Some(1);
    bob.report_screenshot(&alice_contact);
    alice.pump().unwrap();
    assert!(
        alice.contacts()[0].last_seen_secs > 1,
        "an authenticated control frame counts as proof of life"
    );

    // A forged frame must NOT refresh it — otherwise anyone could make a seized phone look alive.
    alice.chats.get_mut(&bob_contact).unwrap().last_seen = Some(1);
    let forged = Frame::Screenshot {
        from: bob_contact.clone(),
        message: WireOlm {
            message_type: 1,
            body: "not a real ciphertext".to_string(),
        },
    };
    let _ = alice.process_frame(None, forged);
    assert_eq!(
        alice.contacts()[0].last_seen_secs,
        1,
        "a frame that fails to verify must not count as proof of life"
    );
}

#[test]
fn screenshot_notice_survives_an_offline_peer_via_the_relay() {
    // Field report (2026-08-01): a screenshot taken while the peer's client was closed never
    // reached them once they reopened it. The notice is only useful if it survives the peer being
    // offline — that is the normal case, not an edge case, since the capturer has no idea whether
    // the other side is running.
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());

    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    // Alice reads Bob's first frame: his relay set, without which her copies to him are held.
    alice.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // Bob's client is closed when Alice screenshots the chat.
    net.disconnect("bob");
    alice.report_screenshot(&bob_contact);

    // Bob reopens and drains his mailbox: the notice was held for him.
    bob.poll_relay().unwrap();
    assert!(
        bob.messages(&alice_contact)
            .iter()
            .any(|m| m.system && m.text.contains("took a screenshot")),
        "a screenshot taken while the peer was offline must still reach them"
    );
}

#[test]
fn screenshot_notice_survives_a_total_outage_and_delivers_on_retry() {
    // The capturing device is itself offline — no peer, no relay. Taking a screenshot on a phone
    // with no signal is ordinary, and the local side has already told the user "the other person
    // was told", so the notice is held and retried rather than dropped.
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    bob.set_relay(relay.clone());

    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    // Alice reads Bob's first frame: his relay set, without which her copies to him are held.
    alice.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    // Peer unreachable AND Alice has no relay: both delivery paths are down at capture time.
    net.disconnect("bob");
    alice.report_screenshot(&bob_contact);
    bob.poll_relay().unwrap();
    assert!(
        !bob.messages(&alice_contact)
            .iter()
            .any(|m| m.text.contains("took a screenshot")),
        "nothing could be delivered while every path was down"
    );

    // Alice gets connectivity back; the poller's retry posts the held notice.
    alice.set_relay(relay.clone());
    alice.flush_pending_control();
    bob.poll_relay().unwrap();
    assert!(
        bob.messages(&alice_contact)
            .iter()
            .any(|m| m.system && m.text.contains("took a screenshot")),
        "the retried screenshot notice finally reaches the peer"
    );
}

/// Bob's contact id for whoever most recently paired with him (Mallory, above).
fn victim_view(bob: &Node, _victim: &str) -> String {
    bob.contacts().last().unwrap().id.clone()
}

#[test]
fn a_local_nickname_stays_local_and_survives_a_restart() {
    use crate::storage;
    // Everyone defaults to "Anon", so a contact list is unreadable until you can label people
    // yourself (`docs/design/contact-naming.md`). The label is *yours*: it must never reach the
    // peer, and must outlive a restart or it is worthless.
    let key: storage::StoreKey = [17u8; 32];
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_contact = bob.contacts()[0].id.clone();

    alice
        .set_local_name(&bob_contact, "  Dana from the shop  ")
        .unwrap();
    assert_eq!(alice.contacts()[0].local_name, "Dana from the shop");

    // Nothing went on the wire: Bob's view of himself, and of Alice, is untouched.
    bob.pump().unwrap();
    assert_eq!(bob.contacts()[0].local_name, "");
    assert_eq!(
        bob.messages(&alice_contact).len(),
        0,
        "naming sends nothing"
    );

    // Two unnamed contacts are still told apart by their identity tag, which is derived from the
    // key — so it changes if the identity does, unlike a random label.
    let tag = alice.contacts()[0].identity_tag.clone();
    assert_eq!(tag.len(), 6);
    assert_eq!(tag, crate::node::identity_tag(&bob_contact));
    assert_ne!(tag, crate::node::identity_tag(&alice_contact));

    // Survives a restart.
    let path = std::env::temp_dir().join(format!("nightdrop-nick-{}.bin", std::process::id()));
    let path = path.to_str().unwrap().to_string();
    storage::save_to_file(&path, &key, &alice.export(&key)).unwrap();
    let state = storage::load_from_file(&path, &key).unwrap();
    let alice2 = Node::restore(&state, Box::new(net.endpoint("alice")), &key).unwrap();
    assert_eq!(alice2.contacts()[0].local_name, "Dana from the shop");
    std::fs::remove_file(&path).ok();
}

#[test]
fn cover_traffic_is_indistinguishable_mail_that_never_surfaces() {
    // #4: the relay's only per-identity signal is "mailbox X was posted to at time T". Cover
    // traffic muddies it with self-addressed dummies — which must look like ordinary mail to the
    // relay and be invisible everywhere else. See `docs/design/cover-traffic.md`.
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    // Alice reads Bob's first frame: his relay set, without which her copies to him are held.
    alice.pump().unwrap();

    // A real message and a cover post both land as one sealed blob in a mailbox: from the relay's
    // side they are the same event, which is the entire point.
    net.disconnect("bob");
    alice.send(&bob_contact, "a real one").unwrap();
    alice.send_cover_traffic();

    // Cover is addressed to Alice herself, so Bob's mailbox holds only the real message.
    let received = bob.poll_relay().unwrap();
    assert_eq!(received.len(), 1, "cover must not reach a peer's mailbox");
    assert_eq!(received[0].1, "a real one");

    // Alice drains her own cover and it vanishes: no history, no message, nothing to see.
    let drained = alice.poll_relay().unwrap();
    assert!(drained.is_empty(), "cover produces no received message");
    assert!(
        alice
            .messages(&bob_contact)
            .iter()
            .all(|m| m.text == "a real one"),
        "cover must never appear in a conversation"
    );
    assert_eq!(
        alice.contacts().len(),
        1,
        "cover creates no phantom contact"
    );
}

/// A transport that records the client-auth calls, so the *cleanup* can be asserted rather than
/// assumed. Wraps a [`MemoryTransport`] so pairing and delivery still work normally; the recorders
/// are shared with the test rather than reached through the node, which would need an accessor that
/// exists only for tests.
struct AuthSpyTransport {
    inner: crate::transport::MemoryTransport,
    revoked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    forgotten: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl crate::transport::Transport for AuthSpyTransport {
    fn address(&self) -> String {
        self.inner.address()
    }
    fn is_synchronous(&self) -> bool {
        self.inner.is_synchronous()
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        self.inner.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(String, Vec<u8>)> {
        self.inner.try_recv()
    }
    fn revoke_client(&self, contact_id: &str) -> Result<()> {
        self.revoked.lock().unwrap().push(contact_id.to_string());
        Ok(())
    }
    fn forget_peer_key(&self, peer_onion: &str) -> Result<()> {
        self.forgotten.lock().unwrap().push(peer_onion.to_string());
        Ok(())
    }
}

#[test]
fn deleting_a_chat_and_logging_out_forget_the_peer_in_both_directions() {
    // Field finding (2026-08-02): arti stores our client key for a restricted onion in a directory
    // *named after the peer's onion address*. Nothing removed it, so deleted chats — and wiped
    // identities — left a recoverable contact list on disk. Nine such directories had accumulated
    // on the dev desktop, four of them from a single day's re-pairings.
    let net = MemoryNetwork::new();
    let revoked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let forgotten = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut alice = Node::new(Box::new(AuthSpyTransport {
        inner: net.endpoint("alice"),
        revoked: std::sync::Arc::clone(&revoked),
        forgotten: std::sync::Arc::clone(&forgotten),
    }));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();

    alice.delete_chat(&bob_contact).unwrap();

    // Both directions: their permission to reach us, and our key for reaching them. The second is
    // the one that was missing, and it is keyed by the ADDRESS, not the contact id.
    assert_eq!(
        revoked.lock().unwrap().as_slice(),
        std::slice::from_ref(&bob_contact)
    );
    assert_eq!(
        forgotten.lock().unwrap().as_slice(),
        &["bob".to_string()],
        "our client key for the peer's onion must be dropped too"
    );
}

#[test]
fn logout_forgets_every_peer_key() {
    // The wipe path matters most: a duress wipe that left a contact list behind would undo the
    // point of it. logout() drives both, so covering it covers duress_logout too.
    let net = MemoryNetwork::new();
    let revoked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let forgotten = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut alice = Node::new(Box::new(AuthSpyTransport {
        inner: net.endpoint("alice"),
        revoked: std::sync::Arc::clone(&revoked),
        forgotten: std::sync::Arc::clone(&forgotten),
    }));
    for peer in ["bob", "carol"] {
        let mut p = Node::new(Box::new(net.endpoint(peer)));
        let bundle = p.publish_bundle();
        alice.connect_with_bundle(peer, &bundle).unwrap();
        p.pump().unwrap();
    }
    assert_eq!(alice.contacts().len(), 2);

    alice.logout();

    let mut got = forgotten.lock().unwrap().clone();
    got.sort();
    assert_eq!(
        got,
        vec!["bob".to_string(), "carol".to_string()],
        "every peer's client key goes, not just the last one"
    );
    assert_eq!(revoked.lock().unwrap().len(), 2);
}

/// Shared recorder of `(peer_onion, secret)` pairs put back into the keystore.
type InsertLog = std::sync::Arc<std::sync::Mutex<Vec<(String, [u8; 32])>>>;

/// Records what the node puts back into the keystore at startup.
struct KeyRestoreSpy {
    inner: crate::transport::MemoryTransport,
    inserted: InsertLog,
    minted: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl crate::transport::Transport for KeyRestoreSpy {
    fn address(&self) -> String {
        self.inner.address()
    }
    fn is_synchronous(&self) -> bool {
        self.inner.is_synchronous()
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        self.inner.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(String, Vec<u8>)> {
        self.inner.try_recv()
    }
    fn make_client_key(&self, peer_onion: &str) -> Option<Result<(String, [u8; 32])>> {
        self.minted.lock().unwrap().push(peer_onion.to_string());
        // Distinct per call, so a re-mint is visibly different from a restore.
        let n = self.minted.lock().unwrap().len() as u8;
        Some(Ok((format!("descriptor:x25519:{peer_onion}"), [n; 32])))
    }
    fn insert_client_key(&self, peer_onion: &str, secret: &[u8; 32]) -> Result<()> {
        self.inserted
            .lock()
            .unwrap()
            .push((peer_onion.to_string(), *secret));
        Ok(())
    }
}

#[test]
fn per_peer_client_keys_survive_a_restart() {
    use crate::storage;
    // The keystore is in memory now (`docs/design/onion-key-at-rest.md`), so these keys exist only
    // in our sealed store. If they were not put back at startup, every restricted peer would
    // quietly drop to relay-only after each launch — working, but slower and more observable, with
    // nothing surfaced to say why.
    let key: storage::StoreKey = [21u8; 32];
    let net = MemoryNetwork::new();
    let inserted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let minted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut alice = Node::new(Box::new(KeyRestoreSpy {
        inner: net.endpoint("alice"),
        inserted: std::sync::Arc::clone(&inserted),
        minted: std::sync::Arc::clone(&minted),
    }));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();

    // Pairing minted one and kept the secret.
    assert_eq!(minted.lock().unwrap().len(), 1);
    let saved = alice.chats.get(&bob_contact).unwrap().client_key;
    assert!(
        saved.is_some(),
        "the secret must be kept, not just handed to arti"
    );

    // Restart: persist, restore onto a fresh spy, and restore the keystore contents.
    let path = std::env::temp_dir().join(format!("nightdrop-ckey-{}.bin", std::process::id()));
    let path = path.to_str().unwrap().to_string();
    storage::save_to_file(&path, &key, &alice.export(&key)).unwrap();
    let state = storage::load_from_file(&path, &key).unwrap();
    let inserted2 = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let minted2 = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut alice2 = Node::restore(
        &state,
        Box::new(KeyRestoreSpy {
            inner: net.endpoint("alice"),
            inserted: std::sync::Arc::clone(&inserted2),
            minted: std::sync::Arc::clone(&minted2),
        }),
        &key,
    )
    .unwrap();
    alice2.restore_client_keys();

    // Put back, not re-minted: a re-mint would need the peer to authorize a new key first.
    assert_eq!(inserted2.lock().unwrap().len(), 1);
    assert_eq!(inserted2.lock().unwrap()[0].1, saved.unwrap());
    assert!(
        minted2.lock().unwrap().is_empty(),
        "a saved key must not be replaced on restart"
    );

    // A chat with no saved key (paired before this existed) mints and re-announces instead, so it
    // heals itself rather than needing a manual re-pair.
    alice2.chats.get_mut(&bob_contact).unwrap().client_key = None;
    assert_eq!(alice2.restore_client_keys(), 1, "one re-announced");
    assert_eq!(minted2.lock().unwrap().len(), 1);
    assert!(alice2.chats.get(&bob_contact).unwrap().client_key.is_some());

    std::fs::remove_file(&path).ok();
}

/// "Sent" must mean the peer actually has it — the point of `Frame::Delivered`.
///
/// Before this, a direct send stopped at "sent" forever: the only ack was the coarse `Ack`, sent
/// only on a relay drain, and nothing promoted a directly-delivered message at all. The UI drew no
/// badge for "sent", so a message that had merely been *dialled* looked exactly like one that had
/// arrived. On 2026-08-02 one was lost when the core was torn down mid-flight and read as sent.
#[test]
fn a_direct_message_is_only_delivered_once_the_peer_receipts_it() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = bob.publish_bundle();
    let bob_on_alice = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();

    alice.send(&bob_on_alice, "did this land?").unwrap();
    let sent = alice.messages(&bob_on_alice).last().unwrap().clone();
    assert_eq!(sent.delivery, "sent", "dialled, but not yet acknowledged");

    // Bob receives it and receipts it; Alice picks the receipt up.
    bob.pump().unwrap();
    alice.pump().unwrap();
    assert_eq!(
        alice.messages(&bob_on_alice).last().unwrap().delivery,
        "delivered",
        "the peer confirmed this exact message"
    );

    // The case a coarse "everything up to now" ack gets wrong. Alice sends two more; the FIRST is
    // lost outright (as it was on the device, when the core was torn down with it still buffered)
    // while the second arrives normally.
    alice.send(&bob_on_alice, "the lost one").unwrap();
    let lost_id = alice.messages(&bob_on_alice).last().unwrap().msg_id.clone();
    // Bob's core is torn down with that frame still buffered in his transport, and rebuilt from
    // the state file — precisely what a guard heal did on the desktop. Re-registering the endpoint
    // drops the old receiver, so the buffered frame dies with it and Bob never sees the message.
    let key: StoreKey = [5u8; 32];
    let state = bob.export(&key);
    drop(bob);
    let mut bob = Node::restore(&state, Box::new(net.endpoint("bob")), &key).unwrap();
    alice.send(&bob_on_alice, "the next one").unwrap();
    let next_id = alice.messages(&bob_on_alice).last().unwrap().msg_id.clone();
    bob.pump().unwrap();
    alice.pump().unwrap();

    let by_id = |id: &str| -> String {
        alice
            .messages(&bob_on_alice)
            .into_iter()
            .find(|m| m.msg_id == id)
            .unwrap()
            .delivery
    };
    assert_eq!(by_id(&next_id), "delivered", "this one really did arrive");
    assert_eq!(
        by_id(&lost_id),
        "sent",
        "a message the peer never got must NOT be reported as delivered, however many later \
         messages succeed — this is the whole reason receipts name a message id"
    );
}

/// Taking a blob off the relay is not the same as accepting it, and the delivery ack must say so.
///
/// `Ack` means only "I drained your mailbox". It used to promote every queued message, so acking a
/// frame we then DROPPED reported "Delivered" for a message the peer would never see — the exact
/// lie `Frame::Delivered` was added to end, arriving by the back door. Now `Ack` promotes nothing
/// (ARCHITECTURE.md §11.3) and a receipt is sent only for a frame that produced a message; this
/// pins both from the sender's side.
#[test]
fn a_dropped_relay_message_is_not_acked_as_delivered() {
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let relay = RelayClient::new(relay_addr.to_string());
    let net = MemoryNetwork::new();

    // Alice screens strangers; Bob opens the chat from her published bundle, so his side is
    // authorized at once and he may send while hers is still an unapproved request.
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    alice.set_require_authorization(true);
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());

    let bundle = alice.publish_bundle();
    // As a short-code join: Bob knows Alice's relay set before she approves, so his message goes
    // to her relay rather than being held for her (`optional-relay.md` §3).
    let alice_contact = bob
        .connect_with_bundle_and_set(
            "alice",
            &bundle,
            Some(RelaySet {
                nightdrop: true,
                relays: Vec::new(),
            }),
        )
        .unwrap();
    alice.pump().unwrap();
    assert_eq!(
        alice.pending_authorizations().len(),
        1,
        "Bob should be an unapproved request on Alice's side"
    );

    // Alice offline: Bob's message goes to the relay and sits there.
    net.disconnect("alice");
    bob.send(&alice_contact, "hello stranger").unwrap();
    let queued = bob
        .messages(&alice_contact)
        .into_iter()
        .next_back()
        .unwrap();
    assert_eq!(queued.delivery, "queued");
    let dropped_id = queued.msg_id.clone();

    // Alice drains it — and drops it, because Bob is not approved. (Only Alice's endpoint is
    // deregistered, so she can still reach Bob: exactly the shape where a wrong ack gets out.)
    let bob_contact = alice.pending_authorizations()[0].id.clone();
    alice.poll_relay().unwrap();
    assert!(
        alice.messages(&bob_contact).is_empty(),
        "the unapproved sender's message must not land in a chat"
    );

    // Bob picks up whatever Alice sent back. There must be no ack among it.
    bob.pump().unwrap();
    let after = bob
        .messages(&alice_contact)
        .into_iter()
        .find(|m| m.msg_id == dropped_id)
        .expect("Bob still has his own message");
    assert_ne!(
        after.delivery, "delivered",
        "Alice dropped this message — reporting it delivered is the lie the receipts exist to end"
    );
    assert_eq!(after.delivery, "queued", "it is still sitting on the relay");

    // Control: once Bob is approved, a message that really lands still flips to delivered, so the
    // assertion above is about the drop and not about receipts being broken outright.
    alice.authorize(&bob_contact, true).unwrap();
    bob.pump().unwrap();
    bob.send(&alice_contact, "second try").unwrap();
    let second_id = bob
        .messages(&alice_contact)
        .into_iter()
        .next_back()
        .unwrap()
        .msg_id;
    alice.poll_relay().unwrap();
    bob.pump().unwrap();
    let second = bob
        .messages(&alice_contact)
        .into_iter()
        .find(|m| m.msg_id == second_id)
        .unwrap();
    assert_eq!(
        second.delivery, "delivered",
        "an approved contact's message that actually landed must still be acked"
    );
}

/// A successful dial is not delivery, and nothing but a receipt naming the message may say it is.
///
/// Three things used to claim it without evidence: a direct send succeeding, a message arriving
/// from the peer, and their relay `Ack`. All three mean only "they are alive" — which is exactly
/// what the message lost on 2026-08-02 looked like from the sender's side, right up until it turned
/// out the peer never had it.
#[test]
fn nothing_but_a_receipt_marks_a_message_delivered() {
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

    // Bob is unreachable, so Alice's message goes to the relay and waits there.
    net.disconnect("bob");
    alice.send(&bob_contact, "are you there").unwrap();
    let queued_id = alice
        .messages(&bob_contact)
        .into_iter()
        .next_back()
        .unwrap()
        .msg_id;
    let status = |n: &Node, id: &str| {
        n.messages(&bob_contact)
            .into_iter()
            .find(|m| m.msg_id == id)
            .unwrap()
            .delivery
    };
    assert_eq!(status(&alice, &queued_id), "queued");

    // Bob — still able to reach Alice — sends her something. That proves he is alive and proves
    // nothing about the message sitting on the relay, which he has not collected.
    bob.send(&alice_contact, "different conversation").unwrap();
    alice.pump().unwrap();
    assert_eq!(
        status(&alice, &queued_id),
        "queued",
        "hearing from the peer is not them collecting your mail"
    );

    // Now he collects it. The receipt he sends back names it, and only then is it delivered.
    bob.poll_relay().unwrap();
    alice.pump().unwrap();
    assert_eq!(
        status(&alice, &queued_id),
        "delivered",
        "a receipt naming the message is what confirms it"
    );
}

/// A message the peer's onion accepted but never receipted must not simply be forgotten: it goes
/// on the relay, where it survives the peer being offline, restarted or torn down mid-flight.
///
/// Also covers the other half — the peer eventually getting *both* copies must not show the message
/// twice, and must still receipt the duplicate, or the sender would retry a message it already has.
#[test]
fn an_unacknowledged_burn_message_is_never_re_queued_as_a_permanent_one() {
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
    alice
        .chats
        .get_mut(&bob_contact)
        .unwrap()
        .contact
        .peer_supports_burn = Some(true);

    // Handed over, never receipted — as when the receipt is slower than RECEIPT_TIMEOUT.
    alice.send_burn(&bob_contact, "gone in ten", 10).unwrap();
    alice.backdate_unconfirmed(crate::node::messaging::RECEIPT_TIMEOUT.as_secs() + 1);
    assert!(
        alice.sweep_unconfirmed().is_empty(),
        "a burn is never re-queued"
    );
    bob.poll_relay().unwrap();
    assert!(
        bob.messages(&alice_contact)
            .iter()
            .all(|m| m.system || m.text != "gone in ten"),
        "no plain copy reached the relay"
    );
}

#[test]
fn an_unacknowledged_message_is_re_queued_on_the_relay_and_deduped_on_arrival() {
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

    // The dial succeeds — but Bob never pumps, so the frame sits in his transport unread, exactly
    // as it does when a core is torn down between accepting the stream and processing the frame.
    alice.send(&bob_contact, "did this survive?").unwrap();
    let msg_id = alice
        .messages(&bob_contact)
        .into_iter()
        .next_back()
        .unwrap()
        .msg_id;
    let alice_status = |n: &Node| {
        n.messages(&bob_contact)
            .into_iter()
            .find(|m| m.msg_id == msg_id)
            .unwrap()
            .delivery
    };
    assert_eq!(
        alice_status(&alice),
        "sent",
        "handed over, and honestly not more than that"
    );

    // Nothing receipted it, so the sweep puts a copy on the relay.
    alice.backdate_unconfirmed(crate::node::messaging::RECEIPT_TIMEOUT.as_secs() + 1);
    let affected = alice.sweep_unconfirmed();
    assert_eq!(affected, vec![bob_contact.clone()]);
    assert_eq!(
        alice_status(&alice),
        "queued",
        "an unacknowledged message belongs on the relay, not in limbo"
    );

    // Bob collects the relay copy and receipts it.
    bob.poll_relay().unwrap();
    let seen: Vec<_> = bob
        .messages(&alice_contact)
        .into_iter()
        .filter(|m| !m.from_me && !m.system)
        .collect();
    assert_eq!(seen.len(), 1, "the relay copy arrives once");
    assert_eq!(seen[0].text, "did this survive?");
    alice.pump().unwrap();
    assert_eq!(alice_status(&alice), "delivered");

    // …and now the original direct frame finally gets processed. Same id: it must not appear a
    // second time, and Bob must still receipt it so a sender in this position stops retrying.
    bob.pump().unwrap();
    let seen_after: Vec<_> = bob
        .messages(&alice_contact)
        .into_iter()
        .filter(|m| !m.from_me && !m.system)
        .collect();
    assert_eq!(
        seen_after.len(),
        1,
        "the message arrived twice by two paths and must be shown once"
    );
    alice.pump().unwrap();
    assert_eq!(alice_status(&alice), "delivered", "still settled");
}

/// The direct path is "wedged" only when this device can reach **nothing** — never because one
/// contact happens to be offline.
///
/// This is the client-side health signal the guard heal was missing. A phone (2026-08-03) published
/// its own descriptor to 8/8 HSDirs while 245 circuit builds died in its guard set, so `onion_ready`
/// said healthy, no heal ever fired, and every message went by relay instead — silently, forever.
///
/// The discriminator is the relay: it is dialled over the *same* Tor path, so a relay that answers
/// proves the circuits work. Each case gets its own node because the signals are deliberately
/// once-per-run — a path that has proven itself once is not re-suspected later in the same session.
#[test]
fn the_direct_path_is_wedged_only_when_nothing_ever_gets_through() {
    // A paired node whose peer is unreachable, with `relay` attached as given.
    fn offline_peer_node(
        net: &MemoryNetwork,
        tag: &str,
        relay: Option<RelayClient>,
    ) -> (Node, String) {
        let mut alice = Node::new(Box::new(net.endpoint(&format!("alice{tag}"))));
        let mut bob = Node::new(Box::new(net.endpoint(&format!("bob{tag}"))));
        let bundle = alice.publish_bundle();
        bob.connect_with_bundle(&format!("alice{tag}"), &bundle)
            .unwrap();
        alice.pump().unwrap();
        let bob_contact = alice.contacts()[0].id.clone();
        if let Some(r) = relay {
            alice.set_relay(r);
        }
        net.disconnect(&format!("bob{tag}"));
        (alice, bob_contact)
    }

    let net = MemoryNetwork::new();

    // 1. Peer offline, RELAY REACHABLE — the everyday case. However many messages pile up, this
    //    device's Tor path is demonstrably fine and tearing it down would help nobody.
    let relay_addr = RelayServer::spawn("127.0.0.1:0").unwrap();
    let (mut alice, contact) =
        offline_peer_node(&net, "1", Some(RelayClient::new(relay_addr.to_string())));
    for i in 0..(crate::node::messaging::DIRECT_WEDGED_THRESHOLD + 3) {
        alice.send(&contact, &format!("relay is up {i}")).unwrap();
    }
    assert!(
        !alice.direct_path_wedged(),
        "a reachable relay proves the circuits work — an offline contact is not ours to heal"
    );

    // 2. Peer offline AND no relay answers — nothing at all gets through, so this device is the
    //    suspect. Exactly the phone's state.
    let (mut alice, contact) =
        offline_peer_node(&net, "2", Some(RelayClient::new("127.0.0.1:1".to_string())));
    assert!(!alice.direct_path_wedged(), "no evidence yet");
    for i in 0..crate::node::messaging::DIRECT_WEDGED_THRESHOLD {
        alice.send(&contact, &format!("into the void {i}")).unwrap();
    }
    assert!(
        alice.direct_path_wedged(),
        "with neither a peer nor a relay reachable, repeated failures are the device's own fault"
    );

    // 3. …and a single delivered message clears the suspicion for the rest of the run, so a peer
    //    that goes offline later never looks like a broken transport.
    let net3 = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net3.endpoint("alice3")));
    let mut bob = Node::new(Box::new(net3.endpoint("bob3")));
    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice3", &bundle).unwrap();
    alice.pump().unwrap();
    let contact = alice.contacts()[0].id.clone();
    alice.set_relay(RelayClient::new("127.0.0.1:1".to_string())); // no relay either
    alice.send(&contact, "this one lands").unwrap();
    net3.disconnect("bob3");
    for i in 0..(crate::node::messaging::DIRECT_WEDGED_THRESHOLD + 5) {
        alice.send(&contact, &format!("gone now {i}")).unwrap();
    }
    assert!(
        !alice.direct_path_wedged(),
        "one delivered message proves the path; an offline contact must not trigger a teardown"
    );
}

#[test]
fn a_private_relay_access_key_is_stable_and_survives_a_restart() {
    use crate::storage;
    // The operator authorizes one public key. With arti's keystore in memory, a key that was not
    // kept would be gone after a restart, and the next request would mint a different one the
    // relay refuses — locking this device out of its private relay on every launch.
    let key: storage::StoreKey = [22u8; 32];
    let net = MemoryNetwork::new();
    let minted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut alice = Node::new(Box::new(KeyRestoreSpy {
        inner: net.endpoint("alice"),
        inserted: std::sync::Arc::default(),
        minted: std::sync::Arc::clone(&minted),
    }));
    let first = alice.relay_access_key("relay.onion").unwrap().unwrap();
    let again = alice.relay_access_key("relay.onion").unwrap().unwrap();
    assert_eq!(first, again, "a repeat request returns the authorized key");
    assert_eq!(minted.lock().unwrap().len(), 1, "and does not mint another");

    let state = alice.export(&key);
    let inserted2: InsertLog = std::sync::Arc::default();
    let minted2 = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut alice2 = Node::restore(
        &state,
        Box::new(KeyRestoreSpy {
            inner: net.endpoint("alice2"),
            inserted: std::sync::Arc::clone(&inserted2),
            minted: std::sync::Arc::clone(&minted2),
        }),
        &key,
    )
    .unwrap();
    alice2.restore_client_keys();
    assert_eq!(
        inserted2.lock().unwrap().as_slice(),
        &[("relay.onion".to_string(), [1u8; 32])],
        "the saved secret goes back into the keystore at startup"
    );
    assert_eq!(
        alice2.relay_access_key("relay.onion").unwrap().unwrap(),
        first
    );
    assert!(
        minted2.lock().unwrap().is_empty(),
        "nothing re-minted after the restart"
    );
}

/// Found on two Linux devices, 2026-10-11: a message handed to a peer whose app was frozen (the
/// Android app freezer does the same) sat unread in its connection; with no receipt, the sweep put a
/// relay copy behind it. Editing or unsending it then recalled every relay copy and called that
/// invisible, but the direct copy was still on its way: the peer showed the draft and the unsent
/// message. A message that was ever handed directly can only be edited or unsent out loud.
#[test]
fn edit_and_unsend_of_a_message_also_handed_directly_reach_the_peer_despite_a_full_recall() {
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
    bob.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();

    // Bob's app is frozen: both frames sit unread in his transport.
    alice.send(&bob_contact, "draft").unwrap();
    alice.send(&bob_contact, "to be unsent").unwrap();
    let ids: Vec<String> = alice
        .messages(&bob_contact)
        .into_iter()
        .filter(|m| m.from_me && !m.system)
        .map(|m| m.msg_id)
        .collect();
    let (draft, unsent) = (ids[ids.len() - 2].clone(), ids[ids.len() - 1].clone());
    alice.backdate_unconfirmed(crate::node::messaging::RECEIPT_TIMEOUT.as_secs() + 1);
    alice.sweep_unconfirmed();

    alice.edit_message(&bob_contact, &draft, "final").unwrap();
    alice.unsend_message(&bob_contact, &unsent).unwrap();

    // Bob thaws: the direct frames first, then whatever the relay and the edit/unsend left.
    bob.pump().unwrap();
    bob.poll_relay().unwrap();
    alice.pump().unwrap();
    bob.pump().unwrap();
    let texts: Vec<String> = bob
        .messages(&alice_contact)
        .into_iter()
        .filter(|m| !m.from_me && !m.system)
        .map(|m| m.text)
        .collect();
    assert!(
        texts.contains(&"final".to_string()),
        "the edit reached Bob: {texts:?}"
    );
    assert!(
        !texts.contains(&"draft".to_string()),
        "no draft left: {texts:?}"
    );
    assert!(
        !texts.contains(&"to be unsent".to_string()),
        "the unsend reached Bob: {texts:?}"
    );
}

/// Seen 2026-10-11: a message typed right after changing the timer landed above the timer notice
/// on the peer's side, because the poller sends queued control frames on another thread than
/// messages. The timer frame now goes to the caller, which sends it before returning.
#[test]
fn a_timer_change_is_sent_by_the_caller_not_left_for_the_poller() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(AsyncMem(net.endpoint("alice"))));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_contact = alice.contacts()[0].id.clone();
    let _ = alice.take_outbox(); // whatever pairing queued

    alice.set_disappearing(&bob_contact, 3600).unwrap();
    let sends = alice.take_caller_outbox();
    assert_eq!(sends.len(), 1, "the timer frame is the caller's to send");
    alice.apply_outbound(crate::node::run_outbox(sends));
    bob.pump().unwrap();
    assert!(
        bob.messages(&alice_contact)
            .iter()
            .any(|m| m.system && m.text.contains("set disappearing messages to 1 hour.")),
        "and it arrives, worded properly"
    );
}

#[test]
fn timer_labels_are_singular_or_plural() {
    assert_eq!(super::disappearing_label(3600), "1 hour");
    assert_eq!(super::disappearing_label(7200), "2 hours");
    assert_eq!(super::disappearing_label(86400), "1 day");
    assert_eq!(super::disappearing_label(604800), "1 week");
    assert_eq!(super::disappearing_label(90), "90 seconds");
    assert_eq!(super::disappearing_label(0), "off");
}

#[test]
fn a_joiner_announces_its_capture_settings_to_the_inviter() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    // As the app does at launch, before any pairing.
    alice.announce_captures(true);
    alice.announce_block(true);
    let bundle = bob.publish_bundle();
    let _bob_contact = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let c = &bob.contacts()[0];
    assert_eq!(
        c.peer_captures_silent,
        Some(false),
        "capability reaches the inviter"
    );
    assert_eq!(
        c.peer_blocks_capture,
        Some(true),
        "block reaches the inviter"
    );
}

/// A joiner announces its standing properties as it joins, before the inviter has read the
/// request; over a real network those frames can arrive first and be dropped, yet count as
/// delivered. Approval is the first moment the inviter certainly knows the joiner: it re-announces.
#[test]
fn a_joiner_repeats_what_it_announced_once_it_is_approved() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice"))); // inviter
    alice.set_require_authorization(true);
    let mut bob = Node::new(Box::new(net.endpoint("bob"))); // joiner
    bob.announce_captures(true);
    bob.announce_block(true);
    bob.set_app_version("0.2.0+416");
    let bundle = alice.publish_bundle();
    let alice_contact = bob.connect_with_bundle("alice", &bundle).unwrap();
    bob.note_awaiting_approval(&alice_contact); // as the API's join paths do
    alice.pump().unwrap();
    let bob_id = alice.pending_authorizations()[0].id.clone();

    // Simulate the race: whatever Bob announced at join was lost on Alice's side.
    {
        let chat = alice.chats.get_mut(&bob_id).unwrap();
        chat.contact.peer_captures_silent = None;
        chat.contact.peer_blocks_capture = None;
        chat.contact.peer_app_version = None;
        chat.peer_block_seq = 0;
    }

    alice.authorize(&bob_id, true).unwrap();
    bob.pump().unwrap(); // Bob reads the approval and announces again
    alice.pump().unwrap();
    let c = alice
        .contacts()
        .into_iter()
        .find(|c| c.id == bob_id)
        .unwrap();
    assert_eq!(c.peer_captures_silent, Some(false));
    assert_eq!(c.peer_blocks_capture, Some(true));
    assert_eq!(c.peer_app_version.as_deref(), Some("0.2.0"));
}
