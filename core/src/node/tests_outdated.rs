//! Contacts still on 0.1.x: 0.2 cannot talk to them (wire v3 against v2), so the chat says so and
//! what we send waits on this device until they update, instead of being handed to an app that
//! takes it and discards it. And the first 0.2 launch re-sends what 0.1.x never got confirmed.
use super::*;
use crate::transport::MemoryNetwork;

/// Alice and Bob paired, both announcing `alice_version` / `bob_version`, everything settled.
fn paired(
    net: &MemoryNetwork,
    alice_version: &str,
    bob_version: &str,
) -> (Node, Node, String, String) {
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_app_version(alice_version);
    bob.set_app_version(bob_version);
    bob.connect_with_bundle("alice", &alice.publish_bundle())
        .unwrap();
    for _ in 0..3 {
        alice.pump().unwrap();
        bob.pump().unwrap();
        alice.announce_version();
        bob.announce_version();
    }
    alice.pump().unwrap();
    bob.pump().unwrap();
    let (alice_id, bob_id) = (alice.identity_key(), bob.identity_key());
    (alice, bob, alice_id, bob_id)
}

fn received(n: &Node, contact: &str) -> Vec<String> {
    n.messages(contact)
        .into_iter()
        .filter(|m| !m.from_me && !m.system)
        .map(|m| m.text)
        .collect()
}

fn status(n: &Node, contact: &str, text: &str) -> String {
    n.messages(contact)
        .into_iter()
        .find(|m| m.from_me && m.text == text)
        .map(|m| m.delivery)
        .unwrap_or_default()
}

#[test]
fn which_versions_are_too_old() {
    assert!(version_outdated("0.1.29"));
    assert!(version_outdated("0.1.0"));
    assert!(!version_outdated("0.2.0"));
    assert!(!version_outdated("0.10.1"));
    assert!(!version_outdated("1.0.0"));
}

#[test]
fn messages_to_a_contact_on_0_1_wait_until_they_update() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, alice_id, bob_id) = paired(&net, "0.2.0", "0.1.29");
    assert!(alice.peer_outdated(&bob_id));
    assert!(alice.contacts()[0].peer_outdated, "the chat says so");

    alice.send(&bob_id, "for when you update").unwrap();
    alice.set_my_name(&bob_id, "Alice").unwrap(); // a control frame waits too
    assert_eq!(status(&alice, &bob_id, "for when you update"), "update");
    assert_eq!(alice.held_count(), 2);
    alice.let_backoff_elapse();
    alice.flush_held();
    bob.pump().unwrap();
    assert!(
        received(&bob, &alice_id).is_empty(),
        "nothing is handed to the old app"
    );

    // Bob updates: his new version announces itself, and what waited goes out, in order.
    bob.set_app_version("0.2.0");
    bob.announce_version();
    alice.pump().unwrap();
    assert!(!alice.peer_outdated(&bob_id));
    alice.flush_held();
    bob.pump().unwrap();
    assert_eq!(received(&bob, &alice_id), vec!["for when you update"]);
    assert_eq!(bob.chats[&alice_id].contact.their_name, "Alice");
    assert_eq!(alice.held_count(), 0);
}

#[test]
fn a_frame_in_the_old_format_marks_the_contact_and_a_new_one_clears_it() {
    let net = MemoryNetwork::new();
    // No versions announced: the format of what arrives is the only evidence.
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    bob.connect_with_bundle("alice", &alice.publish_bundle())
        .unwrap();
    alice.pump().unwrap();
    bob.pump().unwrap();
    let bob_id = bob.identity_key();
    assert!(!alice.peer_outdated(&bob_id));

    // What a 0.1.x app puts on the wire: the same envelope at version 2.
    let frame = Frame::Message {
        from: bob_id.clone(),
        id: "m".into(),
        message: WireOlm::from_olm(
            &crypto::encrypt(
                &mut bob.chats.get_mut(&alice.identity_key()).unwrap().session,
                b"x",
            )
            .unwrap(),
        ),
    };
    let mut old = wire::encode(&frame);
    let at = old.windows(5).position(|w| w == b"\"v\":3").unwrap();
    old[at + 4] = b'2';
    assert_eq!(wire::legacy_sender(&old).as_deref(), Some(bob_id.as_str()));
    bob.transport.send("alice", &old).unwrap();
    alice.pump().unwrap();
    assert!(
        alice.peer_outdated(&bob_id),
        "an old-format frame says they are on 0.1"
    );

    bob.send(&alice.identity_key(), "now on 0.2").unwrap();
    alice.pump().unwrap();
    assert!(
        !alice.peer_outdated(&bob_id),
        "anything we can read says they updated"
    );
}

#[test]
fn the_first_0_2_launch_sends_again_what_was_never_confirmed() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, alice_id, bob_id) = paired(&net, "0.2.0", "0.2.0");
    net.disconnect("bob");
    alice.send(&bob_id, "lost in the mixed window").unwrap();
    // As a 0.1.x app left it: handed to the peer's onion, never receipted, nothing held.
    alice.held.clear();
    for m in alice.chats.get_mut(&bob_id).unwrap().history.iter_mut() {
        if m.from_me {
            m.delivery = "sent".into();
        }
    }
    let key = [5u8; 32];
    let mut state = alice.export(&key);
    state.relays = None; // a state written before 0.2
    drop(alice);
    net.reconnect("bob");

    let mut alice = Node::restore(&state, Box::new(net.endpoint("alice")), &key).unwrap();
    alice.set_app_version("0.2.0");
    assert_eq!(alice.held_count(), 1, "held again on the first 0.2 launch");
    alice.flush_held();
    bob.pump().unwrap();
    assert_eq!(received(&bob, &alice_id), vec!["lost in the mixed window"]);

    // A copy that did arrive after all is not shown twice.
    let mut again = alice.export(&key);
    again.relays = None;
    let mut alice = Node::restore(&again, Box::new(net.endpoint("alice2")), &key).unwrap();
    alice.flush_held();
    bob.pump().unwrap();
    assert_eq!(received(&bob, &alice_id).len(), 1);
}
