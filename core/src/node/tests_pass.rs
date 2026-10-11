//! Pairing passes (`docs/design/optional-relay.md` §5): pairing without a relay when the side being
//! dialled admits only its contacts ("restricted discovery"). The fake transport here enforces
//! restriction the way Tor does in effect: a dial reaches a restricted onion only if its
//! authorized-keys directory holds the public half of the key we installed for it.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::*;
use crate::transport::{client_auth, MemoryNetwork, MemoryTransport};

/// What every fake onion can see of the others: where each keeps its authorized keys, and which
/// ones admit only those.
#[derive(Default)]
struct World {
    auth_dirs: HashMap<String, PathBuf>,
    restricted: HashSet<String>,
}

struct Restricted {
    inner: MemoryTransport,
    world: Arc<Mutex<World>>,
    /// Our installed client secret per peer onion (arti's keystore: one per onion).
    installed: Mutex<HashMap<String, [u8; 32]>>,
}

/// A well-formed `descriptor:x25519:` string standing for the public half of `secret`.
fn public_of(secret: &[u8; 32]) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let mut s = String::from("descriptor:x25519:");
    for b in &Sha256::digest(secret)[..20] {
        let _ = write!(s, "{b:02X}");
    }
    s
}

impl Restricted {
    fn admits(&self, peer: &str) -> bool {
        let world = self.world.lock().unwrap();
        if !world.restricted.contains(peer) {
            return true;
        }
        let Some(secret) = self.installed.lock().unwrap().get(peer).copied() else {
            return false;
        };
        let want = public_of(&secret);
        let Some(dir) = world.auth_dirs.get(peer) else {
            return false;
        };
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| std::fs::read_to_string(e.path()).is_ok_and(|k| k == want))
    }
}

impl Transport for Restricted {
    fn address(&self) -> Address {
        self.inner.address()
    }
    fn is_synchronous(&self) -> bool {
        true
    }
    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        if !self.admits(peer) {
            anyhow::bail!("descriptor not readable: not authorized");
        }
        self.inner.send(peer, frame)
    }
    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        self.inner.try_recv()
    }
    fn mint_pass(&self) -> Option<Result<(String, [u8; 32])>> {
        let mut secret = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut secret);
        Some(Ok((public_of(&secret), secret)))
    }
    fn make_client_key(&self, peer_onion: &str) -> Option<Result<(String, [u8; 32])>> {
        let (public, secret) = self.mint_pass()?.ok()?;
        self.installed
            .lock()
            .unwrap()
            .insert(peer_onion.to_string(), secret);
        Some(Ok((public, secret)))
    }
    fn insert_client_key(&self, peer_onion: &str, secret: &[u8; 32]) -> Result<()> {
        self.installed
            .lock()
            .unwrap()
            .insert(peer_onion.to_string(), *secret);
        Ok(())
    }
    fn authorize_client(&self, contact_id: &str, key: &str) -> Result<()> {
        let dir = self.world.lock().unwrap().auth_dirs[&self.inner.address()].clone();
        client_auth::authorize(&dir, contact_id, key)
    }
    fn revoke_client(&self, contact_id: &str) -> Result<()> {
        let dir = self.world.lock().unwrap().auth_dirs[&self.inner.address()].clone();
        client_auth::revoke(&dir, contact_id)
    }
}

struct Setup {
    net: MemoryNetwork,
    world: Arc<Mutex<World>>,
    base: PathBuf,
}

impl Setup {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("nightdrop-pass-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        Setup {
            net: MemoryNetwork::new(),
            world: Arc::default(),
            base,
        }
    }

    /// A direct-only node at `name`, its onion restricted (as once it has a contact) if asked.
    fn node(&self, name: &str, restricted: bool) -> Node {
        {
            let mut world = self.world.lock().unwrap();
            world
                .auth_dirs
                .insert(name.to_string(), self.base.join(name));
            if restricted {
                world.restricted.insert(name.to_string());
            }
        }
        std::fs::create_dir_all(self.base.join(name)).unwrap();
        let mut n = Node::new(Box::new(Restricted {
            inner: self.net.endpoint(name),
            world: Arc::clone(&self.world),
            installed: Mutex::default(),
        }));
        n.set_relay_list(Vec::new());
        n.set_require_authorization(true);
        n
    }

    fn auth_files(&self, name: &str) -> usize {
        client_auth::authorized_count(&self.base.join(name))
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// The QR payload `inviter` would show, as `create_invite` builds it.
fn qr(inviter: &mut Node) -> String {
    let bundle = inviter.publish_bundle();
    let mut payload = format!(
        "nightdrop://pair?addr={}&ik={}&otk={}",
        inviter.address(),
        bundle.identity_key,
        bundle.one_time_key
    );
    if let Some(ck) = inviter.issue_pass(&bundle.one_time_key) {
        payload.push_str(&format!("&ck={ck}"));
    }
    payload
}

fn join(joiner: &mut Node, payload: &str) -> String {
    let (addr, bundle) = crate::api::parse_invite(payload).unwrap();
    let id = joiner
        .connect_with_invite(&addr, &bundle, crate::api::parse_invite_extras(payload))
        .unwrap();
    joiner.note_awaiting_approval(&id); // as `connect_via_qr` does
    id
}

/// Without this section a direct-only device could pair once and never again: its onion becomes
/// restricted, and a new contact's Hello is refused with nothing to fall back on.
#[test]
fn a_restricted_onion_pairs_a_new_contact_by_qr_with_no_relay() {
    let s = Setup::new("qr");
    let mut alice = s.node("alice", true);
    let mut bob = s.node("bob", true);
    let payload = qr(&mut alice);
    assert!(payload.contains("&ck="), "the QR carries a pass");
    join(&mut bob, &payload);
    alice.pump().unwrap();
    assert_eq!(
        alice.pending_count(),
        1,
        "the Hello got through, as a request"
    );

    // Accept: onion access both ways, and a conversation, with no relay anywhere.
    let bob_id = bob.identity_key();
    let alice_id = alice.identity_key();
    alice.authorize(&bob_id, true).unwrap();
    bob.pump().unwrap();
    bob.send(&alice_id, "hello alice").unwrap();
    alice.pump().unwrap();
    alice.send(&bob_id, "hello bob").unwrap();
    bob.pump().unwrap();
    let texts = |n: &Node, c: &str| -> Vec<String> {
        n.messages(c)
            .into_iter()
            .filter(|m| !m.from_me && !m.system)
            .map(|m| m.text)
            .collect()
    };
    assert_eq!(texts(&alice, &bob_id), vec!["hello alice"]);
    assert_eq!(texts(&bob, &alice_id), vec!["hello bob"]);
    assert!(
        bob.chats[&alice_id].own_key_pending.is_none(),
        "the joiner now presents its own key, not the invite's pass"
    );
}

#[test]
fn without_the_pass_the_hello_is_refused() {
    let s = Setup::new("nopass");
    let mut alice = s.node("alice", true);
    let mut bob = s.node("bob", false);
    let payload = qr(&mut alice);
    let stripped = payload.split("&ck=").next().unwrap().to_string();
    join(&mut bob, &stripped);
    alice.pump().unwrap();
    assert_eq!(
        alice.pending_count(),
        0,
        "a restricted onion turns a stranger away"
    );
    assert_eq!(bob.pending_control.len(), 1, "held for a retry, not lost");
}

#[test]
fn a_pass_is_single_use_and_lapses_after_a_day() {
    let s = Setup::new("single");
    let mut alice = s.node("alice", true);
    let mut bob = s.node("bob", false);
    let payload = qr(&mut alice);
    assert_eq!(
        s.auth_files("alice"),
        1,
        "the pass is authorized while the QR is up"
    );
    join(&mut bob, &payload);
    alice.pump().unwrap();
    assert_eq!(s.auth_files("alice"), 0, "spent by the Hello that used it");

    // A second scan of the same QR (a screenshot posted online) knocks on a locked door.
    let mut carol = s.node("carol", false);
    join(&mut carol, &payload);
    alice.pump().unwrap();
    assert_eq!(alice.pending_count(), 1, "only Bob's request");

    // An unused one lapses.
    qr(&mut alice);
    assert_eq!(s.auth_files("alice"), 1);
    alice.expire_passes(crate::api::now_secs() + PASS_TTL_SECS + 1);
    assert_eq!(s.auth_files("alice"), 0);
}

#[test]
fn a_declined_request_gets_no_access_either_way() {
    let s = Setup::new("decline");
    let mut alice = s.node("alice", true);
    let mut bob = s.node("bob", true);
    let payload = qr(&mut alice);
    join(&mut bob, &payload);
    alice.pump().unwrap();
    let bob_id = bob.identity_key();
    assert_eq!(
        s.auth_files("alice"),
        0,
        "nothing of Bob's authorized while pending"
    );

    alice.authorize(&bob_id, false).unwrap();
    assert_eq!(s.auth_files("alice"), 0, "and nothing after a decline");
    let alice_id = alice.identity_key();
    bob.send(&alice_id, "anyone there?").unwrap_or_default();
    alice.pump().unwrap();
    assert!(!alice.chats.contains_key(&bob_id));
}

#[test]
fn passes_and_waiting_keys_survive_a_restart() {
    let s = Setup::new("restart");
    let mut alice = s.node("alice", true);
    let mut bob = s.node("bob", true);
    let payload = qr(&mut alice);
    join(&mut bob, &payload);
    alice.pump().unwrap();
    qr(&mut alice); // a second invite, still unused
    let key = [5u8; 32];
    let state = alice.export(&key);
    drop(alice);
    let mut alice = Node::restore(
        &state,
        Box::new(Restricted {
            inner: s.net.endpoint("alice"),
            world: Arc::clone(&s.world),
            installed: Mutex::default(),
        }),
        &key,
    )
    .unwrap();
    assert_eq!(
        alice.passes.len(),
        1,
        "the unused pass is remembered, so it can still lapse"
    );
    let bob_id = bob.identity_key();
    assert!(alice.chats[&bob_id].peer_key_pending.is_some());
    assert!(alice.chats[&bob_id].pass_pending.is_some());
    alice.authorize(&bob_id, true).unwrap();
    bob.pump().unwrap();
    let alice_id = alice.identity_key();
    assert!(
        bob.chats[&alice_id]
            .history
            .iter()
            .any(|m| m.kind == "approved"),
        "the approval reaches Bob through his reply pass"
    );
}

#[test]
fn a_short_code_response_carries_a_pass_too() {
    let s = Setup::new("short");
    let mut alice = s.node("alice", true);
    // Short codes meet on a relay (direct only has none, §5), so Alice keeps Night Drop here.
    alice.set_relay_list(vec![RelayEntry::NightDrop]);
    alice.set_relay(crate::relay_client::RelayClient::new(
        "127.0.0.1:9".to_string(),
    ));
    alice
        .stage_short_code_invite("slot", "secret words", Duration::from_secs(600))
        .unwrap();
    let payload = alice.pending_invites[0].payload.clone();
    assert!(crate::api::parse_invite_extras(&payload).pass.is_some());
}
