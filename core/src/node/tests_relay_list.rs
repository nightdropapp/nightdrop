//! The relay list (`docs/design/optional-relay.md` §2): the Night Drop relay is an ordinary entry,
//! the list is capped, an empty list means direct only, an upgraded identity keeps exactly what it
//! was using, and a relay that leaves the list is still drained for a day.
use super::*;
use crate::relay_client::{RelayClient, RelayServer};
use crate::transport::MemoryNetwork;

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 24 * 60 * 60;

fn onion(addr: &str) -> RelayEntry {
    RelayEntry::Onion(addr.to_string())
}

fn spawn_relay() -> RelayClient {
    RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string())
}

/// The relays a drain round at `now` asks, once each and sorted (the plan shuffles its jobs):
/// `None` is the Night Drop client.
fn polled(n: &Node, now: u64) -> Vec<Option<String>> {
    let mut seen = Vec::new();
    for job in n
        .relay_drain_plan_at(now)
        .map(|p| p.jobs)
        .unwrap_or_default()
    {
        if !seen.contains(&job.addr) {
            seen.push(job.addr);
        }
    }
    seen.sort();
    seen
}

/// Alice and Bob paired over one Night Drop relay; returns Bob's id as Alice knows him.
fn pair(net: &MemoryNetwork, relay: &RelayClient) -> (Node, Node, String) {
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());
    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle).unwrap();
    for _ in 0..3 {
        alice.pump().unwrap();
        bob.pump().unwrap();
    }
    let bob_id = alice.contacts()[0].id.clone();
    (alice, bob, bob_id)
}

#[test]
fn a_new_identity_starts_on_the_night_drop_relay() {
    let n = Node::new(Box::new(MemoryNetwork::new().endpoint("me")));
    assert_eq!(n.relays, vec![RelayEntry::NightDrop]);
    assert_eq!(
        n.my_relay_set(),
        RelaySet {
            nightdrop: true,
            relays: vec![]
        }
    );
}

#[test]
fn an_upgraded_identity_keeps_exactly_what_it_was_using() {
    let net = MemoryNetwork::new();
    let n = Node::new(Box::new(net.endpoint("me")));
    let key = [5u8; 32];
    let mut state = n.export(&key);
    // What a 0.1.x state looks like: no list, only the extras.
    state.relays = None;
    state.relays_draining = Vec::new();
    state.my_relays = vec!["a.onion".into(), "b.onion".into()];

    let restored = Node::restore(&state, Box::new(net.endpoint("me2")), &key).unwrap();
    assert_eq!(
        restored.relays,
        vec![RelayEntry::NightDrop, onion("a.onion"), onion("b.onion")]
    );
    assert!(restored.relays_draining.is_empty());

    // 0.1.x had no cap. What it cuts is not dropped on the spot: contacts still post there until
    // they hear the new set, so it drains out like any removed relay.
    state.my_relays = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|s| format!("{s}.onion"))
        .collect();
    let restored = Node::restore(&state, Box::new(net.endpoint("me3")), &key).unwrap();
    assert_eq!(restored.relays.len(), MAX_RELAYS);
    assert_eq!(restored.relays[0], RelayEntry::NightDrop);
    let draining: Vec<_> = restored
        .relays_draining
        .iter()
        .map(|(e, _)| e.clone())
        .collect();
    assert_eq!(draining, vec![onion("d.onion"), onion("e.onion")]);
}

#[test]
fn the_list_and_what_is_draining_survive_a_restart() {
    let net = MemoryNetwork::new();
    let mut n = Node::new(Box::new(net.endpoint("me")));
    n.set_relay_list_at(vec![onion("a.onion"), onion("b.onion")], NOW);
    let key = [5u8; 32];
    let restored = Node::restore(&n.export(&key), Box::new(net.endpoint("me2")), &key).unwrap();
    assert_eq!(
        restored.relays,
        vec![onion("a.onion"), onion("b.onion")],
        "the order is the user's, and Night Drop stays removed"
    );
    assert_eq!(
        restored.relays_draining,
        vec![(RelayEntry::NightDrop, NOW + DAY)]
    );

    // Direct only is a list too, and must not come back as "no list" (= upgrade from 0.1.x).
    n.set_relay_list_at(Vec::new(), NOW);
    let restored = Node::restore(&n.export(&key), Box::new(net.endpoint("me3")), &key).unwrap();
    assert!(restored.relays.is_empty());
}

#[test]
fn duplicates_and_blanks_are_dropped_and_the_cap_holds() {
    let mut n = Node::new(Box::new(MemoryNetwork::new().endpoint("me")));
    n.set_relay_list_at(
        vec![
            RelayEntry::NightDrop,
            onion("a.onion"),
            onion(" a.onion "),
            onion(""),
            RelayEntry::NightDrop,
            onion("b.onion"),
            onion("c.onion"),
            onion("d.onion"),
        ],
        NOW,
    );
    assert_eq!(
        n.relays,
        vec![
            RelayEntry::NightDrop,
            onion("a.onion"),
            onion("b.onion"),
            onion("c.onion")
        ]
    );
    assert_eq!(n.relays_draining, vec![(onion("d.onion"), NOW + DAY)]);
}

#[test]
fn a_removed_relay_is_drained_for_a_day_and_a_re_added_one_stops_draining() {
    let mut n = Node::new(Box::new(MemoryNetwork::new().endpoint("me")));
    n.set_relay(spawn_relay());
    n.set_relay_list_at(vec![RelayEntry::NightDrop, onion("a.onion")], NOW);
    assert_eq!(polled(&n, NOW), vec![None, Some("a.onion".into())]);

    n.set_relay_list_at(vec![RelayEntry::NightDrop], NOW);
    assert_eq!(n.my_relays(), Vec::<String>::new());
    assert_eq!(
        polled(&n, NOW + DAY - 1),
        vec![None, Some("a.onion".into())],
        "mail already queued there is still collected"
    );
    assert_eq!(polled(&n, NOW + DAY), vec![None], "and then it is dropped");

    n.set_relay_list_at(vec![RelayEntry::NightDrop, onion("a.onion")], NOW + 60);
    assert!(
        n.relays_draining.is_empty(),
        "back in the list, not draining"
    );

    // A later edit forgets drain-outs that have run their course.
    n.set_relay_list_at(vec![RelayEntry::NightDrop], NOW);
    n.set_relay_list_at(vec![], NOW + DAY + 1);
    assert_eq!(
        n.relays_draining,
        vec![(RelayEntry::NightDrop, NOW + 2 * DAY + 1)]
    );
}

#[test]
fn removing_night_drop_takes_it_out_of_our_own_traffic() {
    let net = MemoryNetwork::new();
    let nightdrop = spawn_relay();
    let own = spawn_relay();
    let own_addr = own.addr().unwrap().to_string();
    let mut n = Node::new(Box::new(net.endpoint("me")));
    n.set_relay(nightdrop.clone());
    n.set_relay_list_at(vec![onion(&own_addr)], NOW);

    // Cover traffic goes where contacts post to us, and nowhere else.
    n.send_cover_traffic();
    let handle = n.cover_handle(super::mailbox::current_epoch());
    assert_eq!(own.peek(&handle).unwrap(), 1);
    assert_eq!(nightdrop.peek(&handle).unwrap(), 0);

    // Short codes meet on our own relays only.
    let rendezvous: Vec<_> = n
        .rendezvous_relays()
        .iter()
        .map(|r| r.addr().map(str::to_string))
        .collect();
    assert_eq!(rendezvous, vec![Some(own_addr.clone())]);

    // Night Drop is still drained for its day, then not at all.
    assert_eq!(polled(&n, NOW), vec![None, Some(own_addr.clone())]);
    assert_eq!(polled(&n, NOW + DAY), vec![Some(own_addr)]);
}

#[test]
fn a_direct_only_device_posts_to_no_relay_even_for_a_contact_who_has_one() {
    let net = MemoryNetwork::new();
    let relay = spawn_relay();
    let (mut alice, _bob, bob_id) = pair(&net, &relay);
    alice.set_relay_list_at(Vec::new(), NOW);
    assert!(alice.my_relay_set().is_empty());

    net.disconnect("bob");
    let _ = alice.send(&bob_id, "held, not posted");
    assert_eq!(
        relay.peek(&alice.post_handle(&bob_id).unwrap()).unwrap(),
        0,
        "direct only is a property of this device's traffic, not only of its mailbox"
    );
    assert!(
        alice
            .messages(&bob_id)
            .iter()
            .any(|m| m.from_me && m.text == "held, not posted"),
        "the message stays on the sender"
    );

    alice.send_cover_traffic();
    assert_eq!(
        relay
            .peek(&alice.cover_handle(super::mailbox::current_epoch()))
            .unwrap(),
        0
    );
    assert!(alice.rendezvous_relays().is_empty(), "no short codes");
    assert!(
        alice.relay_drain_plan_at(NOW + DAY).is_none(),
        "nothing polled once the drain-out ends"
    );
}

#[test]
fn a_contact_set_past_the_cap_is_cut_to_it() {
    let n = Node::new(Box::new(MemoryNetwork::new().endpoint("me")));
    let set = RelaySet {
        nightdrop: false,
        relays: (0..100).map(|i| format!("{i}.onion")).collect(),
    };
    let (primary, targets) = n.post_route(&set);
    assert!(primary.is_none(), "no Night Drop copy for a set without it");
    assert_eq!(targets.len(), MAX_RELAYS);
}

/// What direct only turns off (`optional-relay.md` §6), and what needs relays on both sides.
#[test]
fn direct_only_turns_off_what_needs_a_relay() {
    let net = MemoryNetwork::new();
    let relay = spawn_relay();
    let (mut alice, mut bob, bob_id) = pair(&net, &relay);
    let alice_id = alice.identity_key();
    assert!(alice.server_storage_available(&bob_id));
    assert!(alice.uses_nightdrop_relay());
    assert!(alice.directory_fetch_due(u64::MAX));

    // Bob goes direct only: Alice can no longer store server copies for him.
    bob.set_relay_list(Vec::new());
    bob.announce_relay_set();
    alice.pump().unwrap();
    assert!(!alice.server_storage_available(&bob_id));
    assert_eq!(alice.contacts()[0].peer_relay_state, "direct");
    assert!(!alice.contacts()[0].server_storage_available);
    let err = alice.set_remote_storage(&bob_id, true).unwrap_err();
    assert!(err.to_string().contains("this contact has none"), "{err}");
    alice.set_remote_storage(&bob_id, false).unwrap();

    // Bob himself: no short codes, no server backup, no relay list to fetch.
    assert!(bob.direct_only());
    assert!(!bob.uses_nightdrop_relay());
    assert!(!bob.directory_fetch_due(u64::MAX));
    let err = bob
        .stage_short_code_invite("slot", "secret words", Duration::from_secs(600))
        .unwrap_err();
    assert!(err.to_string().contains("QR"), "{err}");
    let err = bob.set_remote_storage(&alice_id, true).unwrap_err();
    assert!(err.to_string().contains("direct only"), "{err}");
}

#[test]
fn a_contacts_relay_state_reads_unknown_until_told() {
    let net = MemoryNetwork::new();
    let relay = spawn_relay();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay);
    let alice_id = bob
        .connect_with_bundle("alice", &alice.publish_bundle())
        .unwrap();
    let state = |n: &Node| {
        n.contacts()
            .into_iter()
            .find(|c| c.id == alice_id)
            .unwrap()
            .peer_relay_state
    };
    assert_eq!(state(&bob), "unknown");
    alice.pump().unwrap();
    bob.pump().unwrap();
    assert_eq!(state(&bob), "relays");
}
