//! Standing announcements (burn support, app version, address, screenshot capability, relay
//! set) are sent when they
//! change, not on every launch: a launch must not tell every contact we just came online
//! (`docs/design/optional-relay.md` §4). [`ANNOUNCE_ONLINE_ON_START`] turns that signal on; both
//! settings are tested so flipping it later is a one-line change.
use super::*;
use crate::transport::MemoryNetwork;

/// Alice and Bob paired, every announcement delivered both ways.
fn paired(net: &MemoryNetwork) -> (Node, Node, String) {
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_app_version("0.2.0");
    bob.set_app_version("0.2.0");
    alice.announce_captures(true);
    bob.connect_with_bundle("alice", &alice.publish_bundle())
        .unwrap();
    for _ in 0..3 {
        alice.pump().unwrap();
        bob.pump().unwrap();
        alice.announce_burns();
        alice.announce_version();
        alice.reannounce_address();
        alice.announce_relay_set();
        alice.reannounce_captures();
    }
    bob.pump().unwrap();
    let bob_id = bob.identity_key();
    (alice, bob, bob_id)
}

/// What a fresh launch has in memory: nothing sent this run. What contacts confirmed is kept.
fn relaunch(n: &mut Node) {
    n.burns_announced.clear();
    n.version_announced.clear();
    n.address_announced.clear();
    n.relay_set_in_flight.clear();
    n.captures_announced.clear();
    n.captures_visible = None;
}

/// Run every launch-time announcement, then report whether Bob received any frame at all.
fn launch_reaches_bob(alice: &mut Node, bob: &mut Node) -> bool {
    relaunch(alice);
    alice.announce_burns();
    alice.announce_version();
    alice.reannounce_address();
    alice.announce_relay_set();
    alice.announce_captures(true); // the app reports it on every launch
    bob.dirty = false;
    bob.pump().unwrap();
    bob.dirty // set by any inbound frame, silent control frames included
}

#[test]
fn a_launch_tells_no_contact_we_are_online() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, bob_id) = paired(&net);
    let chat = &alice.chats[&bob_id];
    assert!(chat.told_burns);
    assert_eq!(chat.told_version.as_deref(), Some("0.2.0"));
    assert_eq!(chat.told_address, Some(alice.address()));
    assert_eq!(chat.told_captures, Some(true));

    assert!(!alice.announce_online, "the shipped setting");
    assert!(
        !launch_reaches_bob(&mut alice, &mut bob),
        "nothing changed, so nothing is sent"
    );
}

#[test]
fn with_the_online_signal_on_a_launch_reaches_every_contact() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, _) = paired(&net);
    alice.announce_online = true;
    assert!(launch_reaches_bob(&mut alice, &mut bob));
}

#[test]
fn a_change_is_still_told() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, bob_id) = paired(&net);
    alice.set_app_version("0.2.1");
    assert!(launch_reaches_bob(&mut alice, &mut bob));
    let alice_id = alice.identity_key();
    assert_eq!(
        bob.chats[&alice_id].contact.peer_app_version.as_deref(),
        Some("0.2.1")
    );
    assert_eq!(alice.chats[&bob_id].told_version.as_deref(), Some("0.2.1"));
    assert!(!launch_reaches_bob(&mut alice, &mut bob), "and only once");
}

#[test]
fn what_contacts_were_told_survives_a_restart() {
    let net = MemoryNetwork::new();
    let (alice, _bob, bob_id) = paired(&net);
    let key = [5u8; 32];
    let restored =
        Node::restore(&alice.export(&key), Box::new(net.endpoint("alice2")), &key).unwrap();
    let (before, after) = (&alice.chats[&bob_id], &restored.chats[&bob_id]);
    assert!(after.told_burns);
    assert_eq!(after.told_captures, before.told_captures);
    assert_eq!(after.told_version, before.told_version);
    assert_eq!(after.told_address, before.told_address);
    // Restored onto another onion, as a lost keystore would: the address differs from what Bob
    // holds, so it is re-sent — the repair the old every-launch announcement existed for.
    assert_ne!(restored.address(), alice.address());
    assert_ne!(after.told_address, Some(restored.address()));
}

#[test]
fn a_chat_from_before_0_2_is_told_once() {
    let net = MemoryNetwork::new();
    let (alice, mut bob, bob_id) = paired(&net);
    let key = [5u8; 32];
    let mut state = alice.export(&key);
    let chat = &mut state.chats[0];
    chat.told_burns = false;
    chat.told_version = None;
    chat.told_address = None;
    chat.told_captures = None;
    drop(alice);
    let mut alice = Node::restore(&state, Box::new(net.endpoint("alice")), &key).unwrap();
    alice.set_app_version("0.2.0");
    assert!(launch_reaches_bob(&mut alice, &mut bob));
    let chat = &alice.chats[&bob_id];
    assert!(chat.told_burns && chat.told_version.is_some() && chat.told_address.is_some());
    assert_eq!(chat.told_captures, Some(true));
    assert!(!launch_reaches_bob(&mut alice, &mut bob));
}

#[test]
fn a_capture_change_reaches_a_contact_who_was_offline_for_it() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, bob_id) = paired(&net);
    net.disconnect("bob");
    alice.announce_captures(false);
    assert_eq!(
        alice.chats[&bob_id].told_captures,
        Some(true),
        "not delivered"
    );

    net.reconnect("bob");
    alice.let_backoff_elapse(); // no relay: the retry waits out its backoff
    alice.reannounce_captures(); // the next relay tick
    bob.pump().unwrap();
    let alice_id = alice.identity_key();
    assert_eq!(
        bob.chats[&alice_id].contact.peer_captures_silent,
        Some(true)
    );
    assert_eq!(alice.chats[&bob_id].told_captures, Some(false));
}
