//! v2 mailbox handles (`docs/design/mailbox-handles.md`, `mailbox.rs`). The property that matters
//! most is the one the design puts first: **no message is ever addressed to a handle its recipient
//! cannot compute.** Each test here is a way that could go wrong.
use super::mailbox::{
    current_epoch, pair_secret, v2_handle, MailboxPair, OLD_VERSION_GRACE_SECS, POLL_PAD,
};
use super::*;
use crate::relay_client::{RelayClient, RelayServer};
use crate::transport::MemoryNetwork;

struct Pair {
    net: MemoryNetwork,
    relay: RelayClient,
    alice: Node,
    bob: Node,
    /// Bob as Alice knows him, and Alice as Bob knows her.
    bob_id: String,
    alice_id: String,
}

/// Pair Alice and Bob over one relay, then let their pumps run. `legacy_bob` makes Bob a build
/// from before v2 (no seed in his `Hello`, and he never sends or reads a `MailboxKey`), so the
/// pair has no mailbox, as a 0.1.x pair that never agreed one.
fn pair(legacy_bob: bool) -> Pair {
    let relay = RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());
    bob.legacy_no_mailbox = legacy_bob;
    let bundle = alice.publish_bundle();
    let alice_id = bob.connect_with_bundle("alice", &bundle).unwrap();
    for _ in 0..3 {
        alice.pump().unwrap();
        bob.pump().unwrap();
    }
    let bob_id = alice.contacts()[0].id.clone();
    Pair {
        net,
        relay,
        alice,
        bob,
        bob_id,
        alice_id,
    }
}

fn confirmed(node: &Node, contact: &str) -> bool {
    node.chats[contact]
        .mailbox
        .as_ref()
        .is_some_and(|p| p.peer_confirmed)
}

fn received_texts(node: &Node, contact: &str) -> Vec<String> {
    node.messages(contact)
        .into_iter()
        .filter(|m| !m.from_me && !m.system)
        .map(|m| m.text)
        .collect()
}

#[test]
fn a_new_pair_has_its_mailbox_from_the_hello_and_offline_mail_travels_under_it() {
    let mut p = pair(false);
    assert!(
        confirmed(&p.alice, &p.bob_id) && confirmed(&p.bob, &p.alice_id),
        "the seed in Bob's Hello is the agreement: nothing else had to cross"
    );
    assert_eq!(
        p.alice.chats[&p.bob_id].mailbox, p.bob.chats[&p.alice_id].mailbox,
        "both sides hold the same secret"
    );

    p.net.disconnect("bob");
    p.alice.send(&p.bob_id, "under v2").unwrap();
    let v2 = p
        .alice
        .post_handle(&p.bob_id)
        .expect("a seeded pair has a handle");
    assert_eq!(
        p.relay.peek(&v2).unwrap(),
        1,
        "the copy sits under today's v2 handle"
    );

    p.net.reconnect("bob");
    p.bob.poll_relay().unwrap();
    assert!(received_texts(&p.bob, &p.alice_id).contains(&"under v2".to_string()));
}

#[test]
fn the_seed_reaches_the_inviter_before_any_approval() {
    // The relay copy of an approval to an offline joiner needs the handle already: the Hello is
    // the only frame that crosses before it, and it carries the seed.
    let relay = RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    alice.set_relay(relay.clone());
    bob.set_relay(relay);
    alice.require_authorization = true;
    let bundle = alice.publish_bundle();
    bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_id = bob.identity_key();
    assert!(
        !alice.chats[&bob_id].authorized,
        "still a request awaiting approval"
    );
    assert!(
        confirmed(&alice, &bob_id),
        "and already holds the pair's secret"
    );
}

#[test]
fn a_pair_without_an_agreed_mailbox_has_no_relay_route() {
    let mut p = pair(true);
    assert!(
        !confirmed(&p.alice, &p.bob_id),
        "an old build never confirms"
    );
    assert_eq!(p.alice.post_handle(&p.bob_id), None);
    assert!(
        !p.alice.has_relay_route(&p.bob_id),
        "no handle he polls, so nothing to post to: direct only, like a contact with no relay"
    );

    // Offline: held for him, and nothing at all on the relay.
    p.net.disconnect("bob");
    p.alice.send(&p.bob_id, "held for the old build").unwrap();
    assert_eq!(p.alice.held_count(), 1);
    assert_eq!(
        p.alice.messages(&p.bob_id).last().unwrap().delivery,
        "waiting"
    );
    p.net.reconnect("bob");
    p.alice.let_backoff_elapse();
    p.alice.flush_held();
    p.bob.pump().unwrap();
    assert!(received_texts(&p.bob, &p.alice_id).contains(&"held for the old build".to_string()));
}

#[test]
fn half_an_agreement_never_posts() {
    let mut p = pair(false);
    // Alice has Bob's contribution but not his confirmation — as if that frame were lost.
    let pair_state = p
        .alice
        .chats
        .get_mut(&p.bob_id)
        .unwrap()
        .mailbox
        .as_mut()
        .unwrap();
    pair_state.peer_confirmed = false;
    assert_eq!(
        p.alice.post_handle(&p.bob_id),
        None,
        "without proof the peer holds the secret, nothing goes to a relay"
    );
}

#[test]
fn a_peer_that_restores_an_old_backup_is_not_left_with_unreadable_mail() {
    let mut p = pair(false);
    assert!(confirmed(&p.alice, &p.bob_id));

    // Bob loses his side (restored from before the agreement) and starts again.
    p.bob.chats.get_mut(&p.alice_id).unwrap().mailbox = Some(MailboxPair::fresh());
    p.bob.mailbox_announced.clear();
    p.bob.announce_mailbox();
    p.alice.pump().unwrap();

    // Before anything else crosses, Alice must already have stopped posting to the old handle.
    // (She has his new contribution and replied; his confirmation of the NEW secret is not in yet.)
    assert_eq!(
        p.alice.post_handle(&p.bob_id),
        None,
        "a changed contribution takes the pair off the relay at once"
    );

    // And they settle on the new secret.
    for _ in 0..3 {
        p.bob.pump().unwrap();
        p.alice.pump().unwrap();
    }
    assert!(confirmed(&p.alice, &p.bob_id) && confirmed(&p.bob, &p.alice_id));
    p.net.disconnect("bob");
    p.alice.send(&p.bob_id, "after the re-agreement").unwrap();
    p.net.reconnect("bob");
    p.bob.poll_relay().unwrap();
    assert!(received_texts(&p.bob, &p.alice_id).contains(&"after the re-agreement".to_string()));
}

/// Times within today's UTC day, for tests that depend on where in the day they run.
fn at(hours: u64) -> u64 {
    current_epoch() * super::mailbox::EPOCH_SECS + hours * 60 * 60
}

/// Bob's v2 handle for mail to him on `epoch`.
fn bob_handle(p: &Pair, epoch: u64) -> String {
    let pair = p.bob.chats[&p.alice_id].mailbox.clone().unwrap();
    let secret = pair_secret(
        &p.bob.identity_key(),
        &p.alice_id,
        &pair.own,
        &pair.peer.unwrap(),
    );
    v2_handle(&secret, &p.bob.identity_key(), epoch)
}

#[test]
fn near_midnight_readers_poll_both_neighbouring_days() {
    let p = pair(false);
    let today = current_epoch();
    let polled = p.bob.drain_handles_at(at(23));
    for epoch in [today - 1, today, today + 1] {
        assert!(
            polled.contains(&bob_handle(&p, epoch)),
            "Bob polls day {epoch} late in the day (today is {today})"
        );
    }
    let pair = p.bob.chats[&p.alice_id].mailbox.clone().unwrap();
    let secret = pair_secret(
        &p.bob.identity_key(),
        &p.alice_id,
        &pair.own,
        &pair.peer.unwrap(),
    );
    assert!(
        !polled.contains(&v2_handle(&secret, &p.alice_id, today)),
        "never the other direction's handle — that is Alice's mail"
    );
}

#[test]
fn tomorrow_is_polled_only_in_the_last_stretch_of_the_day() {
    let p = pair(false);
    let today = current_epoch();
    assert!(!p
        .bob
        .drain_handles_at(at(12))
        .contains(&bob_handle(&p, today + 1)));
    assert!(p
        .bob
        .drain_handles_at(at(21))
        .contains(&bob_handle(&p, today + 1)));
}

#[test]
fn yesterday_is_polled_until_a_drain_past_the_margin_empties_it() {
    let p = pair(false);
    let today = current_epoch();
    // Offline across midnight and back at noon: yesterday is still polled, late or not.
    assert!(p
        .bob
        .drain_handles_at(at(12))
        .contains(&bob_handle(&p, today - 1)));
    let mut bob = p.bob;
    bob.prev_epoch_drained = Some(today - 1);
    let polled = bob.drain_handles_at(at(12));
    assert!(!polled.contains(&bob_handle_of(&bob, &p.alice_id, today - 1)));
    assert!(polled.contains(&bob_handle_of(&bob, &p.alice_id, today)));
}

fn bob_handle_of(bob: &Node, alice_id: &str, epoch: u64) -> String {
    let pair = bob.chats[alice_id].mailbox.clone().unwrap();
    let secret = pair_secret(
        &bob.identity_key(),
        alice_id,
        &pair.own,
        &pair.peer.unwrap(),
    );
    v2_handle(&secret, &bob.identity_key(), epoch)
}

#[test]
fn the_agreement_survives_a_restart() {
    let p = pair(false);
    let key = [5u8; 32];
    let saved = p.alice.export(&key);
    let restored = Node::restore(&saved, Box::new(p.net.endpoint("alice2")), &key).unwrap();
    assert_eq!(
        restored.chats[&p.bob_id].mailbox, p.alice.chats[&p.bob_id].mailbox,
        "own and peer contributions and both flags come back as they were"
    );
    assert_eq!(
        restored.post_handle(&p.bob_id),
        p.alice.post_handle(&p.bob_id)
    );
}

#[test]
fn an_edit_recalls_a_copy_posted_under_v2() {
    let mut p = pair(false);
    p.net.disconnect("bob");
    p.alice.send(&p.bob_id, "first draft").unwrap();
    let v2 = p.alice.post_handle(&p.bob_id).unwrap();
    assert_eq!(p.relay.peek(&v2).unwrap(), 1);
    let msg_id = p.alice.messages(&p.bob_id).last().unwrap().msg_id.clone();
    p.alice.edit_message(&p.bob_id, &msg_id, "final").unwrap();
    // The recall found the v2 copy (else the old text would still be waiting beside the new).
    p.net.reconnect("bob");
    p.bob.poll_relay().unwrap();
    let got = received_texts(&p.bob, &p.alice_id);
    assert!(got.contains(&"final".to_string()));
    assert!(
        !got.contains(&"first draft".to_string()),
        "the recalled draft never arrives"
    );
}

// ---- Stage 2: fragmented, isolated polling and per-recipient posting (§5a/§5c, §8) ----

use crate::relay_client::RelayDialer;
use crate::transport::{Address, MemoryTransport, Transport};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// (isolation group — `None` for the default circuits, op, handle), one per relay request.
type Log = Arc<Mutex<Vec<(Option<u64>, String, String)>>>;

fn recording_dialer(log: &Log, group: Option<u64>) -> RelayDialer {
    let log = Arc::clone(log);
    Arc::new(move |line: &str| {
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        let op = v
            .as_object()
            .and_then(|o| {
                o.get("op")
                    .or_else(|| o.keys().next().and_then(|k| o.get(k)))
            })
            .map(|x| x.to_string())
            .unwrap_or_default();
        // `take_many` names a list; everything else one `handle`. One log row per handle.
        let req = v.get("req").unwrap_or(&v);
        let handles: Vec<String> = match req.get("handles") {
            Some(list) => list
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h.as_str().unwrap().to_string())
                .collect(),
            None => vec![req
                .get("handle")
                .and_then(|h| h.as_str())
                .unwrap_or_default()
                .to_string()],
        };
        for handle in handles {
            log.lock().unwrap().push((group, op.clone(), handle));
        }
        // A well-formed reply: a malformed one reads as a dead relay, whose remaining fragments
        // the drain then skips for the round.
        Ok(format!(
            r#"{{"v":{},"resp":{{"ok":true,"msg_id":"m","delete_token":"t"}}}}"#,
            crate::relay_client::RELAY_VERSION
        ))
    })
}

/// A memory transport whose isolated relay dialers record which group carried what.
struct Recording {
    inner: MemoryTransport,
    log: Log,
}

impl Transport for Recording {
    fn address(&self) -> Address {
        self.inner.address()
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        self.inner.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        self.inner.try_recv()
    }
    fn is_synchronous(&self) -> bool {
        self.inner.is_synchronous()
    }
    fn published(&self) -> bool {
        self.inner.published()
    }
    fn relay_dialer_isolated(&self, _addr: &str, group: u64) -> Option<RelayDialer> {
        Some(recording_dialer(&self.log, Some(group)))
    }
}

/// Alice on a recording transport, paired (and agreed) with each of `names`.
fn recorded(names: &[&str]) -> (Node, Log, Vec<(Node, String)>, MemoryNetwork) {
    let net = MemoryNetwork::new();
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut alice = Node::new(Box::new(Recording {
        inner: net.endpoint("alice"),
        log: Arc::clone(&log),
    }));
    alice.set_relay(RelayClient::with_dialer_for(
        "relay.onion",
        recording_dialer(&log, None),
    ));
    let mut peers = Vec::new();
    for name in names {
        let mut peer = Node::new(Box::new(net.endpoint(name)));
        // A relay of their own, as every launch with a relay list sets: without a relay route
        // there is no mailbox to agree on. Never dialled here (everything lands directly).
        peer.set_relay(RelayClient::new("127.0.0.1:9".to_string()));
        let bundle = alice.publish_bundle();
        peer.connect_with_bundle("alice", &bundle).unwrap();
        for _ in 0..3 {
            alice.pump().unwrap();
            peer.pump().unwrap();
        }
        let id = peer.identity_key();
        assert!(confirmed(&alice, &id), "{name} agreed with Alice");
        peers.push((peer, id));
    }
    log.lock().unwrap().clear();
    (alice, log, peers, net)
}

#[test]
fn every_poll_rides_its_fragment() {
    let (alice, log, peers, _net) = recorded(&["bob", "carol"]);
    crate::node::drain_relay_mailboxes(&alice.relay_drain_plan_at(at(23)).unwrap());
    let takes: Vec<_> = log.lock().unwrap().clone();
    assert!(!takes.is_empty());
    assert!(
        takes.iter().all(|(g, _, _)| g.is_some()),
        "no poll uses the shared default circuits"
    );

    // One pair's handles for neighbouring days never share a fragment.
    let (_, bob_id) = &peers[0];
    let pair = alice.chats[bob_id].mailbox.clone().unwrap();
    let secret = pair_secret(
        &alice.identity_key(),
        bob_id,
        &pair.own,
        &pair.peer.unwrap(),
    );
    let today = current_epoch();
    let groups: Vec<_> = [today - 1, today, today + 1]
        .iter()
        .map(|e| {
            let h = v2_handle(&secret, &alice.identity_key(), *e);
            takes.iter().find(|(_, _, x)| *x == h).expect("polled").0
        })
        .collect();
    assert!(
        groups[0] != groups[1] && groups[1] != groups[2] && groups[0] != groups[2],
        "yesterday, today and tomorrow are in different fragments"
    );
}

#[test]
fn the_partition_is_fixed_for_the_epoch_and_survives_a_restart() {
    let (alice, _log, _peers, net) = recorded(&["bob", "carol", "dave"]);
    let shape = |n: &Node| {
        let mut v: Vec<(u64, Vec<String>)> = n
            .poll_fragments_at(at(23))
            .into_iter()
            .map(|f| {
                let mut h = f.handles;
                h.sort();
                (f.group, h)
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(shape(&alice), shape(&alice), "the same every round");
    let key = [3u8; 32];
    let restored =
        Node::restore(&alice.export(&key), Box::new(net.endpoint("alice2")), &key).unwrap();
    assert_eq!(
        shape(&restored),
        shape(&alice),
        "a restart must not re-draw it — a fresh partition mid-epoch is the §5c trap"
    );
}

#[test]
fn each_epoch_is_padded_so_one_round_does_not_count_contacts() {
    for names in [&[][..], &["bob"][..], &["bob", "carol", "dave"][..]] {
        let (alice, _log, _peers, _net) = recorded(names);
        let polled: Vec<String> = alice
            .poll_fragments_at(at(23))
            .into_iter()
            .flat_map(|f| f.handles)
            .collect();
        assert_eq!(
            polled.len(),
            3 * POLL_PAD,
            "{} contact(s) poll the same {} v2 handles as nobody",
            names.len(),
            3 * POLL_PAD
        );
        let mut unique = polled.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), polled.len(), "no dummy repeats a real handle");
    }
}

#[test]
fn posts_for_different_recipients_never_share_circuits() {
    let (mut alice, log, peers, net) = recorded(&["bob", "carol"]);
    net.disconnect("bob");
    net.disconnect("carol");
    let (bob, carol) = (&peers[0].1, &peers[1].1);
    alice.send(bob, "one for bob").unwrap();
    alice.send(bob, "another for bob").unwrap();
    alice.send(carol, "one for carol").unwrap();
    let posts: Vec<_> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, op, _)| op.contains("post") || op.contains("Post"))
        .cloned()
        .collect();
    let group_for = |h: &str| -> Vec<Option<u64>> {
        posts
            .iter()
            .filter(|(_, _, x)| x == h)
            .map(|(g, _, _)| *g)
            .collect()
    };
    let to_bob = group_for(&alice.post_handle(bob).unwrap());
    let to_carol = group_for(&alice.post_handle(carol).unwrap());
    assert_eq!(to_bob.len(), 2);
    assert_eq!(to_carol.len(), 1);
    assert!(
        to_bob.iter().chain(&to_carol).all(Option::is_some),
        "posts are isolated"
    );
    assert_eq!(
        to_bob[0], to_bob[1],
        "one recipient keeps one set of circuits"
    );
    assert_ne!(to_bob[0], to_carol[0], "two recipients never share");
}

#[test]
fn the_contact_cap_refuses_a_new_contact_and_says_why() {
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut peers = Vec::new();
    for i in 0..super::mailbox::MAX_CONTACTS {
        let mut peer = Node::new(Box::new(net.endpoint(&format!("p{i}"))));
        let bundle = peer.publish_bundle();
        alice
            .connect_with_bundle(&format!("p{i}"), &bundle)
            .unwrap();
        peers.push(peer);
    }
    let mut one_more = Node::new(Box::new(net.endpoint("late")));
    let bundle = one_more.publish_bundle();
    let err = alice
        .connect_with_bundle("late", &bundle)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("50 contacts") && err.contains("relay"),
        "{err}"
    );
    assert!(
        one_more.pump().unwrap().is_empty(),
        "nothing reached the network for the refused contact"
    );
    // Re-pairing someone already in the list is not a new contact.
    let bundle = peers[0].publish_bundle();
    alice
        .connect_with_bundle("p0", &bundle)
        .expect("re-pairing an existing contact at the cap is allowed");
}

#[test]
fn a_dead_relay_costs_two_attempts_per_round_not_one_per_fragment() {
    // A relay that never answers: every dial fails, and each would wait out the full timeout.
    let attempts = Arc::new(Mutex::new(0usize));
    let counter = Arc::clone(&attempts);
    let dead_dialer: RelayDialer = Arc::new(move |_line: &str| {
        *counter.lock().unwrap() += 1;
        anyhow::bail!("relay dial timed out")
    });
    let dead = RelayClient::with_dialer_for("dead.onion", dead_dialer);
    let live = RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string());
    live.post("mbx:live", b"mail", Duration::from_secs(60))
        .unwrap();

    let job = |addr: &str, client: &RelayClient, handle: &str| DrainJob {
        addr: Some(addr.to_string()),
        client: client.clone(),
        epoch: None,
        handles: vec![handle.to_string()],
    };
    let mut jobs: Vec<DrainJob> = (0..5)
        .map(|i| job("dead.onion", &dead, &format!("mbx:d{i}")))
        .collect();
    jobs.insert(2, job("live.onion", &live, "mbx:live"));
    let harvest = drain_relay_mailboxes(&RelayDrainPlan {
        jobs,
        settles: None,
        stagger: false,
    });

    assert_eq!(
        *attempts.lock().unwrap(),
        2,
        "the dead relay was tried twice, not once per fragment"
    );
    assert_eq!(
        harvest.blobs,
        vec![b"mail".to_vec()],
        "the live relay still delivered"
    );
    assert!(harvest
        .reachability
        .contains(&("dead.onion".to_string(), false)));
    assert!(harvest
        .reachability
        .contains(&("live.onion".to_string(), true)));
}

// ---- Stage 3: telling the user their contact is on an older version (§5.4) ----

/// Move the pairing — our contribution's send time and the peer's activity so far — `secs` into
/// the past, standing in for the wait.
fn backdate_announce(node: &mut Node, contact: &str, secs: u64) {
    let chat = node.chats.get_mut(contact).unwrap();
    chat.last_seen = chat.last_seen.map(|t| t - secs);
    // A pair seeded at pairing announced nothing, so there is nothing to move.
    let pair = chat.mailbox.as_mut().unwrap();
    pair.announced_at = pair.announced_at.map(|t| t - secs);
}

fn flagged_old(node: &Node, contact: &str) -> bool {
    node.contacts()
        .iter()
        .find(|c| c.id == contact)
        .unwrap()
        .peer_on_old_version
}

#[test]
fn an_old_peer_is_flagged_once_it_is_active_and_still_silent() {
    let mut p = pair(true);
    assert!(
        !flagged_old(&p.alice, &p.bob_id),
        "not while their reply could still be on its way"
    );
    backdate_announce(&mut p.alice, &p.bob_id, OLD_VERSION_GRACE_SECS + 60);
    assert!(
        !flagged_old(&p.alice, &p.bob_id),
        "not on activity from before the grace ran out"
    );
    // Bob is active now, long after Alice's contribution reached him, and never answered it.
    p.bob.send(&p.alice_id, "still here").unwrap();
    p.alice.pump().unwrap();
    assert!(flagged_old(&p.alice, &p.bob_id));

    // It survives a restart: the send time is persisted with the agreement.
    let key = [5u8; 32];
    let saved = p.alice.export(&key);
    let mut restored = Node::restore(&saved, Box::new(p.net.endpoint("alice2")), &key).unwrap();
    restored.set_relay(p.relay.clone()); // as every launch does
    assert!(flagged_old(&restored, &p.bob_id), "after a restart");
}

#[test]
fn a_current_peer_is_never_flagged_even_long_after() {
    let mut p = pair(false);
    backdate_announce(&mut p.alice, &p.bob_id, 24 * 60 * 60);
    p.bob.send(&p.alice_id, "hi").unwrap();
    p.alice.pump().unwrap();
    assert!(!flagged_old(&p.alice, &p.bob_id));
}

#[test]
fn an_offline_peer_is_not_mistaken_for_an_old_one() {
    let mut p = pair(true);
    // A day has passed and Bob has sent nothing since: silence proves nothing about his version.
    backdate_announce(&mut p.alice, &p.bob_id, 24 * 60 * 60);
    assert!(!flagged_old(&p.alice, &p.bob_id));
}

#[test]
fn the_notice_clears_when_the_old_peer_updates() {
    let mut p = pair(true);
    backdate_announce(&mut p.alice, &p.bob_id, OLD_VERSION_GRACE_SECS + 60);
    p.bob.send(&p.alice_id, "old").unwrap();
    p.alice.pump().unwrap();
    assert!(flagged_old(&p.alice, &p.bob_id));

    // Bob updates: his next launch announces his contribution, and the pair agrees.
    p.bob.legacy_no_mailbox = false;
    p.bob.announce_mailbox();
    for _ in 0..3 {
        p.alice.pump().unwrap();
        p.bob.pump().unwrap();
    }
    assert!(confirmed(&p.alice, &p.bob_id));
    assert!(!flagged_old(&p.alice, &p.bob_id));
}

#[test]
fn a_relay_that_answered_keeps_its_fragments_after_a_cold_miss() {
    // Answers, then fails once (a cold isolated connection that timed out), then answers again.
    let calls = Arc::new(Mutex::new(0usize));
    let counter = Arc::clone(&calls);
    let core = Arc::new(crate::relay_client::RelayCore::new(None));
    let c = Arc::clone(&core);
    let flaky: RelayDialer = Arc::new(move |line: &str| {
        let mut n = counter.lock().unwrap();
        *n += 1;
        if *n == 2 {
            anyhow::bail!("Unable to download hidden service descriptor")
        }
        Ok(c.handle_line(line))
    });
    let relay = RelayClient::with_dialer_for("flaky.onion", flaky);
    let jobs: Vec<DrainJob> = (0..4)
        .map(|i| DrainJob {
            addr: Some("flaky.onion".to_string()),
            client: relay.clone(),
            epoch: None,
            handles: vec![format!("mbx:f{i}")],
        })
        .collect();
    let harvest = drain_relay_mailboxes(&RelayDrainPlan {
        jobs,
        settles: None,
        stagger: false,
    });
    assert_eq!(
        *calls.lock().unwrap(),
        4,
        "one miss on a relay that answered does not abandon its other fragments"
    );
    assert!(harvest
        .reachability
        .contains(&("flaky.onion".to_string(), true)));
}

#[test]
fn the_primary_listed_again_by_the_directory_is_used_once() {
    let mut p = pair(false);
    let primary = p.relay.addr().unwrap().to_string();
    // The signed directory lists the primary itself, as the real one does.
    p.alice.discovered_relays = vec![primary.clone()];
    p.bob.discovered_relays = vec![primary];

    p.net.disconnect("bob");
    p.alice.send(&p.bob_id, "once").unwrap();
    assert_eq!(
        p.relay
            .peek(&p.alice.post_handle(&p.bob_id).unwrap())
            .unwrap(),
        1,
        "one copy on the relay, not one per name"
    );

    let plan = p.bob.relay_drain_plan_at(at(12)).unwrap();
    let fragments = p.bob.poll_fragments_at(at(12)).len();
    assert_eq!(plan.jobs.len(), fragments, "each fragment polled once");
}

#[test]
fn a_drain_retires_yesterday_only_past_the_margin_and_only_if_every_fragment_answered() {
    let (mut alice, _log, _peers, _net) = recorded(&["bob", "carol"]);
    let yesterday = current_epoch() - 1;
    let drain = |alice: &mut Node, hours: u64, break_one: bool| {
        let mut plan = alice.relay_drain_plan_at(at(hours)).unwrap();
        if break_one {
            let dead: RelayDialer = Arc::new(|_: &str| anyhow::bail!("relay dial timed out"));
            let job = plan
                .jobs
                .iter_mut()
                .find(|j| j.epoch == Some(yesterday))
                .unwrap();
            job.client = RelayClient::with_dialer_for("relay.onion", dead);
        }
        let harvest = crate::node::drain_relay_mailboxes(&plan);
        alice.apply_relay_harvest(harvest).unwrap();
    };
    drain(&mut alice, 1, false);
    assert_eq!(
        alice.prev_epoch_drained, None,
        "not inside the margin: a slow-clocked sender may still post to yesterday"
    );
    drain(&mut alice, 4, true);
    assert_eq!(
        alice.prev_epoch_drained, None,
        "not while one of yesterday's fragments went unanswered"
    );
    drain(&mut alice, 4, false);
    assert_eq!(alice.prev_epoch_drained, Some(yesterday));
    assert_eq!(
        alice.polled_epochs(at(12)),
        vec![current_epoch()],
        "and the rest of the day polls today alone: 1 + 7 groups per relay"
    );
}

/// A memory transport whose `published()` the test controls, as Tor's is false until its onion
/// is up.
struct Gated {
    inner: MemoryTransport,
    up: Arc<std::sync::atomic::AtomicBool>,
}

impl Transport for Gated {
    fn address(&self) -> Address {
        self.inner.address()
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        self.inner.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        self.inner.try_recv()
    }
    fn is_synchronous(&self) -> bool {
        self.inner.is_synchronous()
    }
    fn published(&self) -> bool {
        self.up.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[test]
fn launch_announcements_wait_for_the_onion_or_the_fallback() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for via_fallback in [false, true] {
        let net = MemoryNetwork::new();
        let up = Arc::new(AtomicBool::new(false));
        let mut alice = Node::new(Box::new(Gated {
            inner: net.endpoint("alice"),
            up: Arc::clone(&up),
        }));
        let mut bob = Node::new(Box::new(net.endpoint("bob")));
        // Mailboxes live on relays: with no relay route there is nothing to agree.
        let relay = RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string());
        alice.set_relay(relay.clone());
        bob.set_relay(relay);
        bob.legacy_no_mailbox = true; // never answers, so the pair stays open to announce
        let bundle = alice.publish_bundle();
        let bob_id = bob.connect_with_bundle("alice", &bundle).unwrap();
        let _ = bob_id;
        for _ in 0..3 {
            alice.pump().unwrap();
            bob.pump().unwrap();
        }
        let bob_key = alice.contacts()[0].id.clone();

        alice.announce_mailbox();
        assert!(
            !alice.mailbox_announced.contains(&bob_key),
            "nothing sent while the onion is still publishing"
        );
        if via_fallback {
            alice.started -= super::ANNOUNCE_FALLBACK;
        } else {
            up.store(true, Ordering::Relaxed);
        }
        alice.announce_mailbox();
        assert!(
            alice.mailbox_announced.contains(&bob_key),
            "sent once {}",
            if via_fallback {
                "the fallback ran out"
            } else {
                "published"
            }
        );
    }
}

/// Bob's transport with a kept-open connection that has gone dead underneath him: while `stale`,
/// an ordinary `send` "succeeds" and the frame goes nowhere — what a write into a Tor stream whose
/// other end has vanished looks like — but a `send_fresh` dials and tells the truth.
struct StaleStream {
    inner: MemoryTransport,
    stale: Arc<AtomicBool>,
    /// Cleared, it behaves like Tor: receipts are sealed under the lock and sent afterwards as
    /// [`DetachedSend`]s rather than inline. Pairing runs synchronous either way.
    synchronous: Arc<AtomicBool>,
}

impl Transport for StaleStream {
    fn address(&self) -> Address {
        self.inner.address()
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        if self.stale.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.inner.send(peer, frame)
    }
    fn send_fresh(&self, peer: &str, frame: &[u8]) -> Result<()> {
        self.inner.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        self.inner.try_recv()
    }
    fn is_synchronous(&self) -> bool {
        self.synchronous.load(Ordering::SeqCst)
    }
}

/// The 2026-09-30 Windows ↔ phone case: Alice's message went to the relay, Bob drained it, and his
/// receipt went into a connection to Alice that was already dead. `send` reported success, so the
/// relay fallback never ran and Alice's message stayed "Held for delivery". The receipt must dial
/// fresh, fail, and reach her through the relay instead.
#[test]
fn a_receipt_is_not_lost_in_a_dead_kept_open_connection() {
    receipt_survives_a_dead_connection(true);
}

/// The same on the path the app takes over Tor, where receipts leave after the lock is released.
#[test]
fn a_detached_receipt_is_not_lost_in_a_dead_kept_open_connection() {
    receipt_survives_a_dead_connection(false);
}

fn receipt_survives_a_dead_connection(synchronous: bool) {
    let relay = RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string());
    let net = MemoryNetwork::new();
    let stale = Arc::new(AtomicBool::new(false));
    let sync_flag = Arc::new(AtomicBool::new(true));
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    let mut bob = Node::new(Box::new(StaleStream {
        inner: net.endpoint("bob"),
        stale: Arc::clone(&stale),
        synchronous: Arc::clone(&sync_flag),
    }));
    alice.set_relay(relay.clone());
    bob.set_relay(relay.clone());
    let bundle = alice.publish_bundle();
    let alice_id = bob.connect_with_bundle("alice", &bundle).unwrap();
    for _ in 0..3 {
        alice.pump().unwrap();
        bob.pump().unwrap();
    }
    let bob_id = alice.contacts()[0].id.clone();

    // Bob is away, so Alice's message goes to the relay.
    net.disconnect("bob");
    alice.send(&bob_id, "via the relay").unwrap();
    let delivery = |alice: &Node| {
        alice.chats[&bob_id]
            .history
            .iter()
            .rev()
            .find(|m| m.from_me && m.text == "via the relay")
            .map(|m| m.delivery.clone())
            .unwrap()
    };
    assert_ne!(delivery(&alice), "delivered");

    // Bob comes back and drains it, but Alice is gone now and his connection to her is dead.
    net.reconnect("bob");
    net.disconnect("alice");
    stale.store(true, Ordering::SeqCst);
    sync_flag.store(synchronous, Ordering::SeqCst);
    bob.poll_relay().unwrap();
    assert!(received_texts(&bob, &alice_id).contains(&"via the relay".to_string()));
    for send in bob.take_receipt_sends() {
        send.execute();
    }

    // Alice returns and drains her mailbox: the receipt is there.
    net.reconnect("alice");
    alice.poll_relay().unwrap();
    assert_eq!(delivery(&alice), "delivered");
}

// ---- 0.2: no v1 handle, and where cover traffic goes (§10) ----

/// The 0.1.x v1 handle, rebuilt here only to prove nothing uses it any more.
fn old_v1_handle(identity_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"nightdrop/mailbox/v1");
    h.update(identity_key.as_bytes());
    format!("mbx:{}", base64_handle(&h.finalize()[..15]))
}

#[test]
fn nothing_is_posted_to_or_polled_under_a_handle_from_the_identity_key_alone() {
    let mut p = pair(false);
    p.net.disconnect("bob");
    p.alice.send(&p.bob_id, "offline mail").unwrap();
    p.alice.send_cover_traffic();
    for (node, me) in [(&p.alice, &p.alice_id), (&p.bob, &p.bob_id)] {
        let v1 = old_v1_handle(me);
        assert_eq!(p.relay.peek(&v1).unwrap(), 0, "nothing posted under v1");
        assert!(
            !node.drain_handles_at(at(23)).contains(&v1),
            "and nobody polls it"
        );
    }
    // A pair with no agreed mailbox gets no fallback to it either.
    let mut old = pair(true);
    old.net.disconnect("bob");
    old.alice.send(&old.bob_id, "held").unwrap();
    assert_eq!(old.relay.peek(&old_v1_handle(&old.bob_id)).unwrap(), 0);
}

#[test]
fn cover_lands_in_a_contacts_mailbox_to_us_and_is_drained_unseen() {
    let mut p = pair(false);
    let epoch = current_epoch();
    let cover = p.alice.cover_handle(epoch);
    // With one contact the pick is Bob: the handle his real mail to Alice uses.
    let pair = p.alice.chats[&p.bob_id].mailbox.clone().unwrap();
    let secret = pair_secret(
        &p.alice.identity_key(),
        &p.bob_id,
        &pair.own,
        &pair.peer.unwrap(),
    );
    assert_eq!(cover, v2_handle(&secret, &p.alice.identity_key(), epoch));
    assert!(
        p.alice.drain_handles_at(at(12)).contains(&cover),
        "a handle Alice polls"
    );
    assert!(
        !p.bob.drain_handles_at(at(12)).contains(&cover),
        "and not one Bob polls: cover never reaches a contact"
    );

    p.alice.send_cover_traffic();
    assert_eq!(
        p.relay.peek(&cover).unwrap(),
        1,
        "posted where real mail goes"
    );
    assert!(
        p.alice.poll_relay().unwrap().is_empty(),
        "and surfaces nothing"
    );
    assert_eq!(
        p.relay.peek(&cover).unwrap(),
        0,
        "drained, not left to pile up"
    );
    assert!(received_texts(&p.alice, &p.bob_id).is_empty());
}

#[test]
fn with_no_contact_cover_goes_to_our_own_cover_handle_which_we_poll() {
    let relay = RelayClient::new(RelayServer::spawn("127.0.0.1:0").unwrap().to_string());
    let net = MemoryNetwork::new();
    let mut alice = Node::new(Box::new(net.endpoint("alice")));
    alice.set_relay(relay.clone());
    let cover = alice.cover_handle(current_epoch());
    assert_eq!(
        cover,
        alice.cover_handle(current_epoch()),
        "fixed for the day"
    );
    assert_ne!(
        cover,
        alice.cover_handle(current_epoch() + 1),
        "and rotates like any handle"
    );
    assert!(alice.drain_handles_at(at(12)).contains(&cover));
    alice.send_cover_traffic();
    assert_eq!(relay.peek(&cover).unwrap(), 1);
    assert!(alice.poll_relay().unwrap().is_empty());
    assert_eq!(relay.peek(&cover).unwrap(), 0);
}

#[test]
fn a_pending_frame_saved_by_0_1_for_v1_goes_directly_or_waits() {
    let p = pair(false);
    let entry = PendingControl {
        recipient_ik: p.bob_id.clone(),
        peer_address: "bob".to_string(),
        relays: Vec::new(),
        nightdrop: true,
        bytes: b"x".to_vec(),
        handle: String::new(), // what a 0.1.x state restores for a v1 copy
        in_flight: false,
    };
    assert!(
        !p.alice.relay_route_for_pending(&entry),
        "no handle, no relay: never the static one"
    );
}
