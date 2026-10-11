//! Holding messages for direct delivery (`docs/design/optional-relay.md` §4): with no relay
//! between two people, a message to an offline contact stays on the sender's device, survives a
//! restart, is retried on a backoff, goes out in send order, and is sent at once when the contact
//! is heard from.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;
use crate::relay_client::{RelayClient, RelayServer};
use crate::transport::{MemoryNetwork, MemoryTransport};

/// A memory transport that counts the dials it is asked to make.
struct Counting {
    inner: MemoryTransport,
    dials: Arc<AtomicUsize>,
}

impl Transport for Counting {
    fn address(&self) -> Address {
        self.inner.address()
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        self.dials.fetch_add(1, Ordering::Relaxed);
        self.inner.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        self.inner.try_recv()
    }
    fn is_synchronous(&self) -> bool {
        true // like the memory transport it wraps: sends run inline
    }
}

/// Alice (counting her dials) and Bob, both direct only, paired and settled.
fn direct_pair(net: &MemoryNetwork) -> (Node, Node, String, Arc<AtomicUsize>) {
    let dials = Arc::new(AtomicUsize::new(0));
    let mut alice = Node::new(Box::new(Counting {
        inner: net.endpoint("alice"),
        dials: Arc::clone(&dials),
    }));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay_list(Vec::new());
    bob.set_relay_list(Vec::new());
    bob.connect_with_bundle("alice", &alice.publish_bundle())
        .unwrap();
    for _ in 0..3 {
        alice.pump().unwrap();
        bob.pump().unwrap();
    }
    let bob_id = bob.identity_key();
    (alice, bob, bob_id, dials)
}

fn status(n: &Node, contact: &str, text: &str) -> String {
    n.messages(contact)
        .into_iter()
        .find(|m| m.from_me && m.text == text)
        .map(|m| m.delivery)
        .unwrap_or_default()
}

fn received(n: &Node, contact: &str) -> Vec<String> {
    n.messages(contact)
        .into_iter()
        .filter(|m| !m.from_me && !m.system)
        .map(|m| m.text)
        .collect()
}

#[test]
fn a_message_to_an_offline_contact_waits_and_is_delivered_when_they_return() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, bob_id, _) = direct_pair(&net);
    let alice_id = alice.identity_key();
    net.disconnect("bob");
    alice.send(&bob_id, "while you were out").unwrap();
    assert_eq!(status(&alice, &bob_id, "while you were out"), "waiting");
    assert_eq!(alice.held_count(), 1);

    net.reconnect("bob");
    alice.let_backoff_elapse();
    alice.flush_held();
    bob.pump().unwrap();
    assert_eq!(received(&bob, &alice_id), vec!["while you were out"]);
    assert_eq!(alice.held_count(), 0);
    assert_eq!(status(&alice, &bob_id, "while you were out"), "sent");
    alice.pump().unwrap(); // Bob's receipt
    assert_eq!(status(&alice, &bob_id, "while you were out"), "delivered");
}

#[test]
fn held_messages_survive_a_restart_of_the_sender() {
    let net = MemoryNetwork::new();
    let (alice, mut bob, bob_id, _) = direct_pair(&net);
    let mut alice = alice;
    let alice_id = alice.identity_key();
    net.disconnect("bob");
    alice.send(&bob_id, "kept across a restart").unwrap();
    let key = [5u8; 32];
    let state = alice.export(&key);
    drop(alice);

    let mut alice = Node::restore(&state, Box::new(net.endpoint("alice")), &key).unwrap();
    assert_eq!(alice.held_count(), 1, "the only copy is not lost");
    assert_eq!(status(&alice, &bob_id, "kept across a restart"), "waiting");
    net.reconnect("bob");
    alice.flush_held(); // after a restart everything is due once
    bob.pump().unwrap();
    assert_eq!(received(&bob, &alice_id), vec!["kept across a restart"]);
}

#[test]
fn held_frames_go_out_in_order_and_an_offline_contact_costs_one_dial_per_retry() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, bob_id, dials) = direct_pair(&net);
    let alice_id = alice.identity_key();
    net.disconnect("bob");
    alice.send(&bob_id, "one").unwrap();
    alice.send(&bob_id, "two").unwrap();
    alice.set_disappearing(&bob_id, 3600).unwrap(); // a control frame, held in order
    alice.send(&bob_id, "three").unwrap();
    assert_eq!(alice.held_count(), 4);

    // Not before the backoff runs out.
    let before = dials.load(Ordering::Relaxed);
    alice.flush_held();
    assert_eq!(
        dials.load(Ordering::Relaxed),
        before,
        "too soon to try again"
    );

    // A retry while they are still away: one dial, not one per held frame.
    alice.let_backoff_elapse();
    alice.flush_held();
    assert_eq!(dials.load(Ordering::Relaxed), before + 1);
    assert_eq!(alice.held_count(), 4);

    net.reconnect("bob");
    alice.let_backoff_elapse();
    alice.flush_held();
    assert_eq!(alice.held_count(), 0, "the first landing sends the rest");
    bob.pump().unwrap();
    assert_eq!(received(&bob, &alice_id), vec!["one", "two", "three"]);
    assert_eq!(
        bob.chats[&alice_id].contact.disappearing_secs, 3600,
        "the control frame arrived too"
    );
}

#[test]
fn hearing_from_the_contact_flushes_at_once() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, bob_id, _) = direct_pair(&net);
    let alice_id = alice.identity_key();
    net.disconnect("bob");
    alice.send(&bob_id, "flushed on contact").unwrap();
    net.reconnect("bob");

    // Bob writes first. Alice's backoff has not run out, but hearing him proves he is online.
    bob.send(&alice_id, "hi").unwrap();
    alice.pump().unwrap();
    bob.pump().unwrap();
    assert_eq!(received(&bob, &alice_id), vec!["flushed on contact"]);
}

#[test]
fn the_retry_backs_off_to_five_minutes_open_and_fifteen_in_the_background() {
    let net = MemoryNetwork::new();
    let (mut alice, _bob, bob_id, _) = direct_pair(&net);
    let mut delays = Vec::new();
    for _ in 0..6 {
        alice.held_retry_failed(&bob_id, 1_000);
        delays.push(alice.held_backoff[&bob_id].0);
    }
    assert_eq!(delays, vec![60, 120, 240, 300, 300, 300]);
    assert!(!alice.may_retry(&bob_id, 1_299));
    assert!(alice.may_retry(&bob_id, 1_300));

    alice.set_background(true);
    for _ in 0..3 {
        alice.held_retry_failed(&bob_id, 1_000);
    }
    assert_eq!(alice.held_backoff[&bob_id].0, 900);
}

#[test]
fn editing_or_unsending_a_waiting_message_changes_only_this_device() {
    let net = MemoryNetwork::new();
    let (mut alice, mut bob, bob_id, _) = direct_pair(&net);
    let alice_id = alice.identity_key();
    net.disconnect("bob");
    alice.send(&bob_id, "typo").unwrap();
    alice.send(&bob_id, "never mind").unwrap();
    let id_of = |n: &Node, text: &str| {
        n.messages(&bob_id)
            .into_iter()
            .find(|m| m.text == text)
            .unwrap()
            .msg_id
    };
    let typo = id_of(&alice, "typo");
    let never = id_of(&alice, "never mind");
    alice.edit_message(&bob_id, &typo, "fixed").unwrap();
    alice.unsend_message(&bob_id, &never).unwrap();
    assert_eq!(
        alice.held_count(),
        1,
        "re-sealed in place, and the unsent one dropped"
    );
    assert!(
        alice
            .messages(&bob_id)
            .iter()
            .all(|m| m.text != "never mind"),
        "an unsent message they never had leaves no tombstone"
    );

    net.reconnect("bob");
    alice.let_backoff_elapse();
    alice.flush_held();
    bob.pump().unwrap();
    let got = bob.messages(&alice_id);
    let texts: Vec<_> = got
        .iter()
        .filter(|m| !m.system)
        .map(|m| m.text.clone())
        .collect();
    assert_eq!(
        texts,
        vec!["fixed"],
        "only the final text, and no edit or unsend frame"
    );
    assert!(
        !got.iter().any(|m| m.edited),
        "it never existed in its old form for them"
    );
}

#[test]
fn a_waiting_message_does_not_expire() {
    let net = MemoryNetwork::new();
    let (mut alice, _bob, bob_id, _) = direct_pair(&net);
    net.disconnect("bob");
    alice.send(&bob_id, "patient").unwrap();
    for m in alice.chats.get_mut(&bob_id).unwrap().history.iter_mut() {
        m.at = m.at.saturating_sub(RELAY_TTL.as_secs() * 3);
    }
    alice.sweep_time();
    assert_eq!(
        status(&alice, &bob_id, "patient"),
        "waiting",
        "held until delivered or the chat is deleted, not 24 h"
    );
}

#[test]
fn deleting_the_chat_drops_what_was_held_for_it() {
    let net = MemoryNetwork::new();
    let (mut alice, _bob, bob_id, _) = direct_pair(&net);
    net.disconnect("bob");
    alice.send(&bob_id, "gone with the chat").unwrap();
    alice.delete_chat(&bob_id).unwrap();
    alice.flush_held();
    assert_eq!(alice.held_count(), 0);
}

/// A relay sender and a direct-only recipient, and the reverse: either way nothing is posted, and
/// the message waits on the sender (`optional-relay.md` §9 tests).
#[test]
fn one_side_direct_only_means_nothing_is_posted_either_way() {
    for direct_only_sender in [false, true] {
        let net = MemoryNetwork::new();
        let relay = RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string());
        let mut alice = Node::new(Box::new(net.endpoint("alice")));
        let mut bob = Node::new(Box::new(net.endpoint("bob")));
        alice.set_relay(relay.clone());
        bob.set_relay(relay.clone());
        if direct_only_sender {
            alice.set_relay_list(Vec::new());
        } else {
            bob.set_relay_list(Vec::new());
        }
        bob.connect_with_bundle("alice", &alice.publish_bundle())
            .unwrap();
        for _ in 0..3 {
            alice.pump().unwrap();
            bob.pump().unwrap();
        }
        let bob_id = bob.identity_key();
        net.disconnect("bob");
        alice.send(&bob_id, "held").unwrap();
        assert_eq!(relay.peek(&alice.post_handle(&bob_id).unwrap()).unwrap(), 0);
        assert_eq!(status(&alice, &bob_id, "held"), "waiting");
    }
}

/// [`ANNOUNCE_ONLINE_ON_START`], both settings: with it on, a contact's launch reaches us and our
/// held messages go to them at once; with it off (shipped), they wait for our next retry.
#[test]
fn the_online_signal_decides_whether_a_launch_flushes_what_contacts_hold() {
    for online in [false, true] {
        let net = MemoryNetwork::new();
        let (mut alice, mut bob, bob_id, _) = direct_pair(&net);
        let alice_id = alice.identity_key();
        net.disconnect("bob");
        alice.send(&bob_id, "waiting for your launch").unwrap();

        // Bob launches: nothing changed, so only the online signal would say so.
        net.reconnect("bob");
        bob.announce_online = online;
        bob.burns_announced.clear();
        bob.version_announced.clear();
        bob.address_announced.clear();
        bob.announce_burns();
        bob.announce_version();
        bob.reannounce_address();
        alice.pump().unwrap();
        bob.pump().unwrap();
        assert_eq!(
            !received(&bob, &alice_id).is_empty(),
            online,
            "online signal {online}"
        );
    }
}

/// Seen with a direct-only phone (2026-10-10): the joiner started the private-mailbox agreement at
/// pairing, before any relay route existed. (From 0.2 a pair's mailbox comes from the `Hello`
/// instead, so no agreement runs at pairing at all.) The direct-only side never answers one, so once it was
/// seen alive the joiner's chat said it was "on an older version". No agreement starts without a
/// relay route, and an unanswered one left over from before says nothing about the contact's app.
#[test]
fn a_direct_only_contact_gets_no_mailbox_agreement_and_is_never_called_old() {
    for direct_only_inviter in [false, true] {
        let net = MemoryNetwork::new();
        let relay = RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string());
        let mut alice = Node::new(Box::new(net.endpoint("alice")));
        let mut bob = Node::new(Box::new(net.endpoint("bob")));
        alice.set_relay(relay.clone());
        bob.set_relay(relay.clone());
        if direct_only_inviter {
            alice.set_relay_list(Vec::new());
        } else {
            bob.set_relay_list(Vec::new());
        }
        bob.connect_with_bundle("alice", &alice.publish_bundle())
            .unwrap();
        for _ in 0..3 {
            alice.pump().unwrap();
            bob.pump().unwrap();
            alice.announce_mailbox();
            bob.announce_mailbox();
        }
        let (alice_id, bob_id) = (alice.identity_key(), bob.identity_key());
        assert!(
            alice.mailbox_announced.is_empty() && bob.mailbox_announced.is_empty(),
            "neither side sent an agreement"
        );
        assert_eq!(
            alice.chats[&bob_id].mailbox, bob.chats[&alice_id].mailbox,
            "the Hello's seed is the pair's mailbox, ready if a relay is added later"
        );

        // One started before this fix, never answered, and the contact active long after.
        let chat = alice.chats.get_mut(&bob_id).unwrap();
        let mut pair = super::mailbox::MailboxPair::fresh();
        pair.announced_at = Some(1);
        chat.mailbox = Some(pair);
        chat.last_seen = Some(crate::api::now_secs());
        assert!(
            Node::peer_on_old_version(&alice.chats[&bob_id]),
            "the evidence is there"
        );
        assert!(
            !alice.contacts()[0].peer_on_old_version,
            "but a direct-only contact never answers one"
        );
    }
}
