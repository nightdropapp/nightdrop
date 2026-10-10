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
    let (mut alice, mut bob, alice_id, bob_id) = paired(&net, "0.2.0", "0.2.0");
    // What Alice holds for a contact on 0.1: the version it announced, and nothing readable from
    // it since (an in-memory Bob is really 0.2, and anything of his she could read would prove it).
    alice
        .chats
        .get_mut(&bob_id)
        .unwrap()
        .contact
        .peer_app_version = Some("0.1.29".into());
    bob.set_app_version("0.1.29");
    bob.chats.get_mut(&alice_id).unwrap().told_version = Some("0.1.29".into());
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

/// Make `n` hold `contact` as a 0.1 contact: the version they last announced, nothing since.
fn as_if_on_0_1(n: &mut Node, contact: &str) {
    n.chats.get_mut(contact).unwrap().contact.peer_app_version = Some("0.1.29".into());
}

#[test]
fn a_contact_on_0_1_is_told_once_in_a_format_its_app_can_read() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, alice_id, bob_id) = paired(&net, "0.2.0", "0.2.0");
    as_if_on_0_1(&mut alice, &bob_id);
    alice.send_upgrade_notices();

    // What reaches Bob's onion is a v2 Message: what a 0.1 app reads and shows.
    let (_, bytes) = bob.transport.try_recv().expect("the notice went out");
    assert_eq!(
        wire::legacy_sender(&bytes).as_deref(),
        Some(alice_id.as_str())
    );
    let mut as_v3 = bytes.clone();
    let at = as_v3.windows(5).position(|w| w == b"\"v\":2").unwrap();
    as_v3[at + 4] = b'3';
    let Ok(Frame::Message { message, .. }) = wire::decode(&as_v3) else {
        panic!("a plain Message, the one frame 0.1 and 0.2 share");
    };
    let text = crypto::decrypt(
        &mut bob.chats.get_mut(&alice_id).unwrap().session,
        &message.to_olm().unwrap(),
    )
    .unwrap();
    assert_eq!(text, UPGRADE_NOTICE_TEXT.as_bytes());

    // Our own chat says what was sent on our behalf, and it is sent once, across restarts too.
    assert!(alice
        .messages(&bob_id)
        .iter()
        .any(|m| m.system && m.text.contains("asking them to update")));
    alice.send_upgrade_notices();
    assert!(bob.transport.try_recv().is_none(), "once");
    let key = [5u8; 32];
    let mut alice =
        Node::restore(&alice.export(&key), Box::new(net.endpoint("alice2")), &key).unwrap();
    alice.send_upgrade_notices();
    assert!(
        !alice.upgrade_notice_in_flight.contains(&bob_id),
        "not again after a restart"
    );
}

/// Both upgraded while each still held the other's 0.1 version. Each would hold everything for
/// the other, and with it the version announcement that would tell them otherwise, forever.
#[test]
fn two_contacts_who_both_updated_find_out_from_each_other() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, alice_id, bob_id) = paired(&net, "0.2.0", "0.2.0");
    as_if_on_0_1(&mut alice, &bob_id);
    as_if_on_0_1(&mut bob, &alice_id);
    alice.chats.get_mut(&bob_id).unwrap().told_version = Some("0.1.29".into());
    bob.chats.get_mut(&alice_id).unwrap().told_version = Some("0.1.29".into());
    alice.send(&bob_id, "after both updated").unwrap();
    assert_eq!(status(&alice, &bob_id, "after both updated"), "update");

    // The relay tick: versions go out even to a contact thought to be on 0.1.
    alice.version_announced.clear();
    bob.version_announced.clear();
    alice.announce_version();
    bob.announce_version();
    alice.pump().unwrap();
    bob.pump().unwrap();
    assert!(!alice.peer_outdated(&bob_id) && !bob.peer_outdated(&alice_id));
    alice.flush_held();
    bob.pump().unwrap();
    assert_eq!(received(&bob, &alice_id), vec!["after both updated"]);
}

/// Seen on the VMs: Bob updated while Alice (already on 0.2) still held his 0.1 version, and he
/// still held hers. His version announcement told her he updated; the upgrade notice he then sent
/// her — old-format on purpose, since he thought *she* was on 0.1 — arrived after it and marked
/// him as 0.1 again. Neither announces twice in a run, so both held everything until a restart.
#[test]
fn an_upgrade_notice_from_a_contact_who_announced_0_2_does_not_mark_them_old_again() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, alice_id, bob_id) = paired(&net, "0.2.0", "0.2.0");
    as_if_on_0_1(&mut alice, &bob_id);
    as_if_on_0_1(&mut bob, &alice_id);
    alice.chats.get_mut(&bob_id).unwrap().told_version = Some("0.1.29".into());
    bob.chats.get_mut(&alice_id).unwrap().told_version = Some("0.1.29".into());
    alice.send(&bob_id, "held while you were old").unwrap();

    // Bob's first 0.2 run: his version, then the notice, in that order.
    bob.version_announced.clear();
    bob.announce_version();
    bob.send_upgrade_notices();
    let note = |n: &Node, c: &str| {
        n.messages(c)
            .iter()
            .any(|m| m.system && m.text == UPGRADE_NOTE_TEXT)
    };
    assert!(note(&bob, &alice_id), "he still thinks she is on 0.1");
    alice.pump().unwrap();
    assert!(
        !alice.peer_outdated(&bob_id),
        "his own announcement said 0.2; the notice after it is not news"
    );
    alice.flush_held();
    bob.pump().unwrap();
    assert_eq!(received(&bob, &alice_id), vec!["held while you were old"]);
    assert!(
        !bob.peer_outdated(&alice_id),
        "and what she sent tells him she updated"
    );
    assert!(
        !note(&bob, &alice_id),
        "so his chat no longer says her app is too old"
    );
    assert!(!bob.chats[&alice_id].told_upgrade);
}
