//! Tests for the outbox (`Outbound`): control frames and relay retries sealed under the core lock
//! and delivered without it (ARCHITECTURE.md §6, "No network I/O while holding the core lock").
use super::*;
use crate::transport::MemoryNetwork;
use std::sync::atomic::{AtomicBool, Ordering};

/// The in-memory transport, reporting itself asynchronous like Tor (so frames are queued, not
/// sent inline), with a switch that makes every dial fail: a peer whose onion cannot be reached.
struct Deferred {
    inner: crate::transport::MemoryTransport,
    down: Arc<AtomicBool>,
}

impl Transport for Deferred {
    fn address(&self) -> Address {
        self.inner.address()
    }
    fn is_synchronous(&self) -> bool {
        false
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        if self.down.load(Ordering::Relaxed) {
            anyhow::bail!("unreachable");
        }
        self.inner.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        self.inner.try_recv()
    }
}

/// Alice on a [`Deferred`] transport, paired with Bob on a plain in-memory one, with her pairing
/// frames already delivered. Returns (alice, bob, bob's id at alice, alice's id at bob, switch).
fn paired() -> (Node, Node, String, String, Arc<AtomicBool>) {
    let net = MemoryNetwork::new();
    let down = Arc::new(AtomicBool::new(false));
    let mut alice = Node::new(Box::new(Deferred {
        inner: net.endpoint("alice"),
        down: Arc::clone(&down),
    }));
    let mut bob = Node::new(Box::new(net.endpoint("bob")));
    let bundle = alice.publish_bundle();
    let alice_at_bob = bob.connect_with_bundle("alice", &bundle).unwrap();
    alice.pump().unwrap();
    let bob_at_alice = alice.contacts()[0].id.clone();
    // Whatever pairing queued on Alice's side goes out now, as the poller would send it.
    let sent = run_outbox(alice.take_outbox());
    alice.apply_outbound(sent);
    bob.pump().unwrap();
    (alice, bob, bob_at_alice, alice_at_bob, down)
}

#[test]
fn an_announcement_on_an_async_transport_is_queued_then_sent_off_the_lock() {
    let (mut alice, mut bob, bob_id, _, _) = paired();
    alice.version_announced.clear();
    alice.set_app_version("0.2.0");

    alice.announce_version();
    assert!(
        bob.contacts()[0].peer_app_version.as_deref() != Some("0.2.0"),
        "nothing is dialled while the announcement is only queued"
    );
    let queued = alice.outbox.len();
    assert!(queued >= 1, "the announcement waits in the outbox");
    assert!(
        alice.version_announced.contains_key(&bob_id),
        "flagged while in flight"
    );

    // The next tick must not queue a second copy of a frame already on its way.
    alice.announce_version();
    assert_eq!(alice.outbox.len(), queued);

    let sent = run_outbox(alice.take_outbox());
    alice.apply_outbound(sent);
    bob.pump().unwrap();
    assert_eq!(
        bob.contacts()[0].peer_app_version.as_deref(),
        Some("0.2.0"),
        "delivered once the outbox ran"
    );
    assert!(
        alice.version_announced.contains_key(&bob_id),
        "and recorded as done"
    );
}

#[test]
fn a_failed_announcement_clears_its_flag_so_the_next_tick_retries() {
    let (mut alice, _bob, bob_id, _, down) = paired();
    alice.version_announced.clear();
    alice.set_app_version("0.2.0");
    down.store(true, Ordering::Relaxed); // no relay either: reaches no one

    alice.announce_version();
    let sent = run_outbox(alice.take_outbox());
    alice.apply_outbound(sent);
    assert!(
        !alice.version_announced.contains_key(&bob_id),
        "a failed delivery must not count as announced"
    );

    alice.let_backoff_elapse(); // no relay: the retry waits out its backoff
    alice.announce_version();
    assert!(
        !alice.outbox.is_empty(),
        "and the next tick queues it again"
    );
}

#[test]
fn a_chat_delete_notice_stays_persisted_until_a_copy_lands() {
    let (mut alice, mut bob, bob_id, alice_id, down) = paired();
    down.store(true, Ordering::Relaxed);

    alice.delete_chat(&bob_id).unwrap();
    assert_eq!(alice.pending_control.len(), 1, "held from the start");
    assert_eq!(
        alice.export_pending_control().len(),
        1,
        "and saved with the state while it is in flight"
    );
    assert!(alice.pending_control[0].in_flight);
    alice.flush_pending_control();
    assert_eq!(
        alice.outbox.len(),
        1,
        "an in-flight notice is not queued twice"
    );

    // The first attempt reaches no one: still held, free for the next poll.
    let sent = run_outbox(alice.take_outbox());
    alice.apply_outbound(sent);
    assert_eq!(alice.pending_control.len(), 1);
    assert!(!alice.pending_control[0].in_flight);

    // The peer comes back: the retry (once its backoff runs out, there being no relay) lands
    // and the entry goes.
    down.store(false, Ordering::Relaxed);
    alice.let_backoff_elapse();
    alice.flush_pending_control();
    let sent = run_outbox(alice.take_outbox());
    alice.apply_outbound(sent);
    assert!(
        alice.pending_control.is_empty(),
        "dropped once a copy landed"
    );
    bob.pump().unwrap();
    assert!(
        bob.messages(&alice_id)
            .iter()
            .any(|m| m.system && m.text.contains("deleted")),
        "and the peer was told: {:?}",
        bob.messages(&alice_id)
    );
}

#[test]
fn an_unsent_outbox_is_recorded_as_failed_so_retries_survive_a_shutdown() {
    let (mut alice, _bob, bob_id, _, down) = paired();
    down.store(true, Ordering::Relaxed);
    alice.version_announced.clear();
    alice.set_app_version("0.2.0");
    alice.announce_version();
    alice.report_screenshot(&bob_id);
    assert!(alice.version_announced.contains_key(&bob_id));
    assert_eq!(alice.pending_control.len(), 1);

    // The poller stops before the round ran.
    let unsent: Vec<Finished> = alice
        .take_outbox()
        .into_iter()
        .map(Outbound::abandon)
        .collect();
    alice.apply_outbound(unsent);
    assert!(
        !alice.version_announced.contains_key(&bob_id),
        "an announcement that never went out is not done"
    );
    assert_eq!(
        alice.pending_control.len(),
        1,
        "the screenshot notice is still held"
    );
    assert!(
        !alice.pending_control[0].in_flight,
        "and free to be retried"
    );
}

#[test]
fn a_caller_entry_is_left_for_its_caller_and_the_rest_for_the_poller() {
    let (mut alice, _bob, bob_id, _, _) = paired();
    alice.version_announced.clear();
    alice.set_app_version("0.2.0");
    alice.announce_version(); // the poller's
    alice.edit_message_queued_for_test(&bob_id);
    let mine = alice.take_caller_outbox();
    assert_eq!(mine.len(), 1, "the edit is the caller's");
    assert!(mine.iter().all(Outbound::is_caller));
    assert!(
        !alice.outbox.is_empty() && !alice.outbox.iter().any(Outbound::is_caller),
        "the announcement stays for the poller"
    );
}

impl Node {
    /// Send a message (delivered at once by a direct dial on this transport) and edit it within
    /// the window: path 2, an `Edit` frame queued for the caller.
    fn edit_message_queued_for_test(&mut self, contact_id: &str) {
        self.send(contact_id, "first").unwrap();
        let sent = execute_sends(&self.plan_pending_sends().unwrap());
        self.apply_send_outcomes(sent);
        let msg_id = self
            .messages(contact_id)
            .into_iter()
            .rev()
            .find(|m| m.from_me && m.text == "first")
            .unwrap()
            .msg_id;
        let plan = self.begin_edit(contact_id, &msg_id, "second").unwrap();
        assert!(plan.is_none(), "a sent message needs no recall");
        self.finish_edit(contact_id, &msg_id, "second", None)
            .unwrap();
    }
}

/// A joiner whose Tor is minutes old can reach neither the inviter's onion nor the relay with its
/// first Hello. Tried once, the join would fail for good: the code spent, the inviter never shown a
/// request. The Hello is held and retried until it lands.
#[test]
fn a_hello_that_cannot_go_out_yet_is_held_and_arrives_later() {
    let net = MemoryNetwork::new();
    let down = Arc::new(AtomicBool::new(true));
    let mut inviter = Node::new(Box::new(net.endpoint("inviter")));
    inviter.set_require_authorization(true);
    let mut joiner = Node::new(Box::new(Deferred {
        inner: net.endpoint("joiner"),
        down: Arc::clone(&down),
    }));
    let bundle = inviter.publish_bundle();
    joiner
        .connect_with_bundle("inviter", &bundle)
        .expect("joining does not fail on a network that is not up yet");
    assert_eq!(joiner.pending_control.len(), 1, "the Hello is held");
    assert_eq!(
        joiner.export_pending_control().len(),
        1,
        "and saved with the state"
    );

    // First attempt: nobody reachable.
    let sent = run_outbox(joiner.take_outbox());
    joiner.apply_outbound(sent);
    inviter.pump().unwrap();
    assert_eq!(inviter.pending_count(), 0, "nothing arrived yet");
    assert_eq!(joiner.pending_control.len(), 1, "still held");

    // The network comes up; the next retry delivers it (with no relay here, once its backoff has
    // run out).
    down.store(false, Ordering::Relaxed);
    joiner.let_backoff_elapse();
    joiner.flush_pending_control();
    let sent = run_outbox(joiner.take_outbox());
    joiner.apply_outbound(sent);
    inviter.pump().unwrap();
    assert_eq!(
        inviter.pending_count(),
        1,
        "the inviter now shows the request"
    );
    assert!(
        joiner.pending_control.is_empty(),
        "and the joiner stops retrying"
    );
}

/// The held queue on an asynchronous transport, as on a phone: a send that reaches no one goes
/// through the poller's plan, execute and apply steps into the queue, and the retry goes out
/// through the outbox, one frame at a time and in order (`optional-relay.md` §4).
#[test]
fn held_messages_go_out_through_the_outbox_in_order() {
    let (mut alice, mut bob, bob_id, alice_id, down) = paired();
    down.store(true, Ordering::Relaxed);
    alice.send(&bob_id, "first").unwrap();
    alice.send(&bob_id, "second").unwrap();
    let plan = alice.plan_pending_sends().unwrap();
    alice.apply_send_outcomes(execute_sends(&plan));
    assert_eq!(alice.held_count(), 2);
    assert!(alice
        .messages(&bob_id)
        .iter()
        .filter(|m| m.from_me)
        .all(|m| m.delivery == "waiting"));

    down.store(false, Ordering::Relaxed);
    alice.let_backoff_elapse();
    alice.flush_held();
    assert_eq!(alice.outbox.len(), 1, "only the oldest goes first");
    let sent = run_outbox(alice.take_outbox());
    alice.apply_outbound(sent);
    assert_eq!(alice.outbox.len(), 1, "and the next follows once it landed");
    let sent = run_outbox(alice.take_outbox());
    alice.apply_outbound(sent);
    assert_eq!(alice.held_count(), 0);

    bob.pump().unwrap();
    let got: Vec<String> = bob
        .messages(&alice_id)
        .into_iter()
        .filter(|m| !m.from_me && !m.system)
        .map(|m| m.text)
        .collect();
    assert_eq!(got, vec!["first", "second"]);
}
