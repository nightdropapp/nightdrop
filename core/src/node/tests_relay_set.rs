//! Telling contacts our relay set (`docs/design/optional-relay.md` §3): `Frame::RelaySet` carries
//! the complete set, it travels inside pairing, a sender posts to exactly the set it was told, an
//! unknown set holds rather than guessing, and a chat from before 0.2 is seeded with what it was
//! already using.
use super::*;
use crate::relay_client::{RelayClient, RelayServer};
use crate::transport::MemoryNetwork;

fn spawn_relay() -> RelayClient {
    RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string())
}

fn nightdrop_only() -> RelaySet {
    RelaySet {
        nightdrop: true,
        relays: Vec::new(),
    }
}

/// Bob joins Alice by QR over one Night Drop relay. Nothing is pumped yet.
fn joined(net: &MemoryNetwork, relay: &RelayClient) -> (Node, Node, String, String) {
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());
    let alice_id = bob
        .connect_with_bundle("alice", &alice.publish_bundle())
        .unwrap();
    let bob_id = bob.identity_key();
    (alice, bob, alice_id, bob_id)
}

/// [`joined`], then both sides read what the other sent.
fn paired(net: &MemoryNetwork, relay: &RelayClient) -> (Node, Node, String, String) {
    let (mut alice, mut bob, alice_id, bob_id) = joined(net, relay);
    for _ in 0..3 {
        alice.pump().unwrap();
        bob.pump().unwrap();
    }
    (alice, bob, alice_id, bob_id)
}

#[test]
fn the_joiners_set_rides_in_the_hello() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    bob.set_relay_list(vec![RelayEntry::Onion("bob.onion".into())]);
    bob.connect_with_bundle("alice", &alice.publish_bundle())
        .unwrap();
    alice.pump().unwrap();
    let bob_id = bob.identity_key();
    assert_eq!(
        alice.peer_relay_set(&bob_id),
        Some(RelaySet {
            nightdrop: false,
            relays: vec!["bob.onion".into()]
        }),
        "known from the first frame, before anything else arrives"
    );
    assert!(
        alice.messages(&bob_id).iter().all(|m| m.system),
        "the Hello's body is not shown as a message"
    );
}

#[test]
fn the_joiner_holds_until_the_inviters_set_arrives() {
    let net = MemoryNetwork::new();
    let relay = spawn_relay();
    let (mut alice, mut bob, alice_id, _) = joined(&net, &relay);
    assert_eq!(
        bob.peer_relay_set(&alice_id),
        None,
        "a QR does not carry it"
    );
    let (primary, targets) = bob.route_to(&alice_id);
    assert!(
        primary.is_none() && targets.is_empty(),
        "unknown is held, never treated as \"use Night Drop\""
    );

    alice.pump().unwrap(); // takes the Hello, answers with her set
    bob.pump().unwrap();
    assert_eq!(bob.peer_relay_set(&alice_id), Some(nightdrop_only()));
    let (primary, _) = bob.route_to(&alice_id);
    assert!(
        primary.is_some(),
        "now her mail goes to the relay she named"
    );
}

#[test]
fn a_short_code_response_carries_the_inviters_set() {
    let net = MemoryNetwork::new();
    let relay = spawn_relay();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    alice.set_relay(relay.clone());
    alice.set_relay_list(vec![
        RelayEntry::NightDrop,
        RelayEntry::Onion("alice.onion".into()),
    ]);
    alice
        .stage_short_code_invite("slot", "secret words", Duration::from_secs(600))
        .unwrap();
    let payload = alice.pending_invites[0].payload.clone();
    let expected = RelaySet {
        nightdrop: true,
        relays: vec!["alice.onion".into()],
    };
    assert_eq!(
        crate::api::parse_invite_relay_set(&payload),
        Some(expected.clone())
    );

    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    bob.set_relay(relay);
    let alice_id = bob.connect_from_invite_payload(&payload).unwrap().id;
    assert_eq!(
        bob.peer_relay_set(&alice_id),
        Some(expected),
        "known before her first reply"
    );
}

#[test]
fn an_edited_list_reaches_contacts_and_moves_their_posts() {
    let net = MemoryNetwork::new();
    let nightdrop = spawn_relay();
    let own = spawn_relay();
    let own_addr = own.addr().unwrap().to_string();
    let (mut alice, mut bob, _, bob_id) = paired(&net, &nightdrop);

    bob.set_relay_list(vec![RelayEntry::Onion(own_addr.clone())]);
    bob.announce_relay_set();
    alice.pump().unwrap();
    assert_eq!(
        alice.peer_relay_set(&bob_id),
        Some(RelaySet {
            nightdrop: false,
            relays: vec![own_addr.clone()]
        })
    );

    net.disconnect("bob");
    alice.send(&bob_id, "to his own relay").unwrap();
    let handle = alice.post_handle(&bob_id);
    assert_eq!(own.peek(&handle).unwrap(), 1);
    assert_eq!(
        nightdrop.peek(&handle).unwrap(),
        0,
        "a contact who removed Night Drop really has nothing on it"
    );
}

#[test]
fn a_direct_only_contact_gets_nothing_posted() {
    let net = MemoryNetwork::new();
    let relay = spawn_relay();
    let (mut alice, mut bob, _, bob_id) = paired(&net, &relay);
    bob.set_relay_list(Vec::new());
    bob.announce_relay_set();
    alice.pump().unwrap();
    assert_eq!(alice.peer_relay_set(&bob_id), Some(RelaySet::default()));

    net.disconnect("bob");
    let _ = alice.send(&bob_id, "held for direct delivery");
    assert_eq!(relay.peek(&alice.post_handle(&bob_id)).unwrap(), 0);
}

#[test]
fn an_announcement_that_did_not_land_is_retried_until_it_does() {
    // No relay anywhere, so a set can only go direct.
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    bob.connect_with_bundle("alice", &alice.publish_bundle())
        .unwrap();
    alice.pump().unwrap();
    bob.pump().unwrap();
    let bob_id = bob.identity_key();
    let changed = vec![RelayEntry::Onion("new.onion".into())];

    net.disconnect("alice");
    bob.set_relay_list(changed.clone());
    bob.announce_relay_set();
    let alice_id = alice.identity_key();
    assert_ne!(
        bob.chats[&alice_id].relays_told,
        Some(bob.my_relay_set()),
        "not marked told when it did not land"
    );

    net.reconnect("alice");
    bob.announce_relay_set(); // the next tick
    alice.pump().unwrap();
    assert_eq!(bob.chats[&alice_id].relays_told, Some(bob.my_relay_set()));
    assert_eq!(alice.peer_relay_set(&bob_id), Some(RelaySet::of(&changed)));

    // Told once: further ticks send nothing.
    bob.announce_relay_set();
    assert!(bob.relay_set_in_flight.is_empty());
}

#[test]
fn a_chat_from_before_0_2_is_seeded_and_then_told_once() {
    let net = MemoryNetwork::new();
    let relay = spawn_relay();
    let (alice, _bob, _, bob_id) = paired(&net, &relay);
    let key = [5u8; 32];
    let mut state = alice.export(&key);
    // What a 0.1.x state holds for a contact: only the extras they announced.
    let chat = &mut state.chats[0];
    chat.relay_set = None;
    chat.relay_set_unknown = false;
    chat.relays_told = None;
    chat.peer_relays = vec!["extra.onion".into()];

    let mut restored = Node::restore(&state, Box::new(net.endpoint("alice2")), &key).unwrap();
    assert_eq!(
        restored.peer_relay_set(&bob_id),
        Some(RelaySet {
            nightdrop: true,
            relays: vec!["extra.onion".into()]
        }),
        "exactly what 0.1.x posted to, so no existing chat starts holding"
    );
    // 0.1.x never told anyone a complete set: the first 0.2 tick does.
    assert_eq!(restored.chats[&bob_id].relays_told, None);
    restored.announce_relay_set();
    assert!(restored.relay_set_in_flight.is_empty(), "sent and settled");
    assert_eq!(
        restored.chats[&bob_id].relays_told,
        Some(restored.my_relay_set())
    );
}

#[test]
fn an_unknown_set_stays_unknown_across_a_restart() {
    let net = MemoryNetwork::new();
    let relay = spawn_relay();
    let (_alice, bob, alice_id, _) = joined(&net, &relay);
    let key = [5u8; 32];
    let restored = Node::restore(&bob.export(&key), Box::new(net.endpoint("bob2")), &key).unwrap();
    assert_eq!(
        restored.peer_relay_set(&alice_id),
        None,
        "not mistaken for a 0.1.x chat and seeded with Night Drop"
    );
}

#[test]
fn a_contacts_set_is_cleaned_and_capped() {
    let set = sanitize_peer_set(RelaySet {
        nightdrop: true,
        relays: [" a.onion ", "", "a.onion", &"x".repeat(500)]
            .iter()
            .map(|s| s.to_string())
            .chain((0..100).map(|i| format!("{i}.onion")))
            .collect(),
    });
    assert!(set.nightdrop);
    assert_eq!(set.relays.len(), MAX_RELAYS);
    assert_eq!(
        set.relays[..2],
        ["a.onion".to_string(), "0.onion".to_string()]
    );

    assert_eq!(
        hello_relay_set(b""),
        None,
        "unreadable is unknown, not empty"
    );
    assert_eq!(hello_relay_set(b"not json"), None);
}
