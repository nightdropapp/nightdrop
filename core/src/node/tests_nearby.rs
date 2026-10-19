//! Nearby between two nodes (`docs/design/nearby.md`), over a real local socket on loopback.

use super::*;
use crate::transport::nearby::NearbyTransport;
use crate::transport::MemoryNetwork;

fn node(net: &MemoryNetwork, name: &str) -> Node {
    Node::new(Box::new(NearbyTransport::on_loopback(net.endpoint(name))))
}

fn paired() -> (MemoryNetwork, Node, Node, String, String) {
    let net = MemoryNetwork::new();
    let mut alice = node(&net, "alice");
    let mut bob = node(&net, "bob");
    let bundle = bob.publish_bundle();
    let bob_id = alice.connect_with_bundle("bob", &bundle).unwrap();
    bob.pump().unwrap();
    let alice_id = bob.contacts()[0].id.clone();
    (net, alice, bob, alice_id, bob_id)
}

fn texts(n: &Node, contact: &str) -> Vec<String> {
    n.messages(contact)
        .into_iter()
        .filter(|m| !m.from_me && !m.system)
        .map(|m| m.text)
        .collect()
}

/// Pump until `cond` holds: local frames arrive on a reader thread.
fn pump_until(n: &mut Node, cond: impl Fn(&Node) -> bool) -> bool {
    for _ in 0..100 {
        n.pump().unwrap();
        if cond(n) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

#[test]
fn both_turn_it_on_and_messages_go_over_the_local_network() {
    let (net, mut alice, mut bob, alice_id, bob_id) = paired();

    // Alice asks; Bob sees the request and that it is not on for him yet.
    assert!(alice.set_nearby(&bob_id, true));
    bob.pump().unwrap();
    let b = &bob.contacts()[0];
    assert!(b.peer_nearby && !b.nearby && !b.nearby_connected);
    assert!(bob.messages(&alice_id).iter().any(|m| m.system
        && m.text
            .contains("They'd like to also connect on the same Wi-Fi")));

    // Bob agrees. Both now hold the other's address and token.
    assert!(bob.set_nearby(&alice_id, true));
    alice.pump().unwrap();
    assert!(alice.contacts()[0].peer_nearby);

    // With the main transport down between them, a message still arrives — over the local path.
    net.disconnect("bob");
    alice.send(&bob_id, "hello from across the room").unwrap();
    assert!(pump_until(&mut bob, |b| texts(b, &alice_id)
        .contains(&"hello from across the room".to_string())));
    assert!(alice.contacts()[0].nearby_connected);
}

#[test]
fn on_one_side_only_nothing_goes_locally() {
    let (net, mut alice, mut bob, alice_id, bob_id) = paired();
    alice.set_nearby(&bob_id, true);
    bob.pump().unwrap();
    // Bob never agreed: Alice has no route, so with the main transport down nothing arrives.
    net.disconnect("bob");
    let _ = alice.send(&bob_id, "only over tor");
    assert!(!pump_until(&mut bob, |b| !texts(b, &alice_id).is_empty()));
    assert!(!alice.contacts()[0].nearby_connected);
}

#[test]
fn turning_it_off_tells_them_and_stops_the_local_path() {
    let (net, mut alice, mut bob, alice_id, bob_id) = paired();
    alice.set_nearby(&bob_id, true);
    bob.pump().unwrap();
    bob.set_nearby(&alice_id, true);
    alice.pump().unwrap();

    bob.set_nearby(&alice_id, false);
    alice.pump().unwrap();
    assert!(!alice.contacts()[0].peer_nearby, "Alice was told");
    net.disconnect("bob");
    let _ = alice.send(&bob_id, "after off");
    assert!(
        !pump_until(&mut bob, |b| !texts(b, &alice_id).is_empty()),
        "no local path any more"
    );
}

#[test]
fn a_restored_backup_starts_with_it_off_and_says_so() {
    let (net, mut alice, mut bob, alice_id, bob_id) = paired();
    alice.set_nearby(&bob_id, true);
    bob.pump().unwrap();
    assert!(bob.contacts()[0].peer_nearby);

    let password = "correct horse battery staple";
    let backup = alice.backup(password).unwrap();
    drop(alice);
    let mut restored = Node::restore_from_backup(
        &backup,
        password,
        Box::new(NearbyTransport::on_loopback(net.endpoint("alice"))),
    )
    .unwrap();
    assert!(!restored.contacts()[0].nearby, "off on the restored device");
    restored.nearby_tick();
    bob.pump().unwrap();
    assert!(!bob.contacts()[0].peer_nearby, "and the contact was told");
    let _ = alice_id;
}

#[test]
fn an_older_announcement_arriving_late_is_ignored() {
    let (_net, mut alice, mut bob, _alice_id, bob_id) = paired();
    alice.set_nearby(&bob_id, true);
    bob.pump().unwrap();
    let seq_on = bob.chats.values().next().unwrap().peer_nearby_seq;
    // Bob receives an "off" with an older sequence (as if delayed): ignored.
    let alice_key = alice.identity_key();
    let stale = format!("{}:0", seq_on.saturating_sub(1));
    assert!(!bob.receive_nearby(&alice_key, stale.as_bytes()));
    assert!(bob.contacts()[0].peer_nearby);
}

#[test]
fn a_public_address_in_an_announcement_is_refused() {
    let (_net, alice, mut bob, _alice_id, _bob_id) = paired();
    let alice_key = alice.identity_key();
    let token = "A".repeat(crate::transport::nearby::TOKEN_CHARS);
    let body = format!("{}:1:203.0.113.7:40000:{token}", u64::MAX / 2);
    assert!(!bob.receive_nearby(&alice_key, body.as_bytes()));
    assert!(!bob.contacts()[0].peer_nearby);
}
