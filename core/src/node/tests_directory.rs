//! The relay list from our onion site (`directory::SITE_PATH`, `optional-relay.md` §2): what is
//! adopted, and when the next fetch happens — a day after a good one, a few hours after a bad one,
//! and never more often because the app restarted.
use super::*;
use crate::directory::{self, RelayDirectory, CHECK_INTERVAL_SECS, RETRY_INTERVAL_SECS};
use crate::transport::MemoryNetwork;
use ed25519_dalek::SigningKey;

const NOW: u64 = 1_800_000_000;

fn key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}
fn pubkey() -> [u8; 32] {
    key().verifying_key().to_bytes()
}
fn recovery() -> SigningKey {
    SigningKey::from_bytes(&[11u8; 32])
}
/// The test deployment's keys: [`key`] signs lists, [`recovery`] may rotate it.
fn roots() -> crate::keyroll::Roots {
    crate::keyroll::Roots {
        purpose: crate::keyroll::Purpose::Directory,
        signing: pubkey(),
        recovery: recovery().verifying_key().to_bytes(),
    }
}
fn list(version: u64, relays: &[&str]) -> String {
    let dir = RelayDirectory {
        version,
        issued_at: NOW,
        relays: relays.iter().map(|s| s.to_string()).collect(),
    };
    directory::sign(&dir, &key()).to_wire()
}
fn site(fetched: Option<Result<String>>) -> DirectoryFetchResult {
    DirectoryFetchResult {
        site: fetched,
        relay_lists: Vec::new(),
    }
}
fn node() -> Node {
    Node::new(Box::new(MemoryNetwork::new().endpoint("me")))
}

#[test]
fn a_good_list_is_adopted_and_the_next_fetch_is_a_day_away() {
    let mut n = node();
    assert!(
        n.directory_fetch_due(NOW),
        "a fresh install fetches once Tor is up"
    );
    n.begin_directory_fetch(NOW);
    assert!(
        !n.directory_fetch_due(NOW + 60),
        "no second fetch while one is out"
    );

    n.finish_directory_fetch(
        site(Some(Ok(list(3, &["a.onion", " b.onion ", ""])))),
        NOW,
        &roots(),
    );
    assert_eq!(n.directory_version, 3);
    assert_eq!(
        n.discovered_relays,
        vec!["a.onion", "b.onion"],
        "trimmed, blanks dropped"
    );
    assert!(!n.directory_fetch_due(NOW + CHECK_INTERVAL_SECS - 1));
    assert!(n.directory_fetch_due(NOW + CHECK_INTERVAL_SECS));
}

#[test]
fn an_unchanged_list_still_counts_as_a_good_fetch() {
    let mut n = node();
    n.finish_directory_fetch(site(Some(Ok(list(3, &["a.onion"])))), NOW, &roots());
    // The site keeps serving version 3: nothing to adopt, but the site answered with a genuine
    // list, so this is not a failure to retry in a few hours.
    let later = NOW + CHECK_INTERVAL_SECS;
    n.finish_directory_fetch(site(Some(Ok(list(3, &["a.onion"])))), later, &roots());
    assert!(!n.directory_fetch_due(later + RETRY_INTERVAL_SECS));
    assert!(n.directory_fetch_due(later + CHECK_INTERVAL_SECS));
}

#[test]
fn a_forged_or_older_list_cannot_move_the_relay_set() {
    let mut n = node();
    n.finish_directory_fetch(site(Some(Ok(list(5, &["good.onion"])))), NOW, &roots());

    // Signed by someone else (a compromised website, say): rejected, and retried sooner.
    let forged = directory::sign(
        &RelayDirectory {
            version: 9,
            issued_at: NOW,
            relays: vec!["evil.onion".into()],
        },
        &SigningKey::from_bytes(&[1u8; 32]),
    )
    .to_wire();
    let t = NOW + CHECK_INTERVAL_SECS;
    n.finish_directory_fetch(site(Some(Ok(forged))), t, &roots());
    assert_eq!(n.discovered_relays, vec!["good.onion"]);
    assert_eq!(n.directory_version, 5);
    assert!(
        n.directory_fetch_due(t + RETRY_INTERVAL_SECS),
        "a bad list is a failed fetch"
    );

    // Genuine but older (a rollback): ignored.
    assert_eq!(
        n.adopt_directory(&list(4, &["old.onion"]), &roots()),
        DirectoryOutcome::Current
    );
    assert_eq!(n.discovered_relays, vec!["good.onion"]);
    // Not JSON at all.
    assert_eq!(
        n.adopt_directory("<html>404</html>", &roots()),
        DirectoryOutcome::Invalid
    );
}

#[test]
fn a_failed_or_impossible_fetch_retries_in_a_few_hours() {
    for fetched in [Some(Err(anyhow::anyhow!("onion unreachable"))), None] {
        let mut n = node();
        let _plan = n.begin_directory_fetch(NOW);
        n.finish_directory_fetch(site(fetched), NOW, &roots());
        assert!(!n.directory_fetch_due(NOW + RETRY_INTERVAL_SECS - 1));
        assert!(n.directory_fetch_due(NOW + RETRY_INTERVAL_SECS));
        assert!(n.discovered_relays.is_empty());
    }
}

#[test]
fn the_schedule_survives_a_restart() {
    let mut n = node();
    n.finish_directory_fetch(site(Some(Ok(list(2, &["a.onion"])))), NOW, &roots());
    let key: crate::storage::StoreKey = [7u8; 32];
    let state = n.export(&key);
    let back = Node::restore(&state, Box::new(MemoryNetwork::new().endpoint("me")), &key).unwrap();
    assert_eq!(back.directory_version, 2);
    assert_eq!(back.discovered_relays, vec!["a.onion"]);
    assert!(
        !back.directory_fetch_due(NOW + 3600),
        "a restart does not trigger another fetch"
    );
    assert!(back.directory_fetch_due(NOW + CHECK_INTERVAL_SECS));
}

/// The fetch asks our onion site for `/relays.json`, and nothing else.
#[test]
fn the_fetch_goes_to_our_onion_site_and_is_size_capped() {
    use std::sync::Mutex;
    struct Site {
        body: Vec<u8>,
        asked: Mutex<Vec<(String, u16, String, usize)>>,
    }
    impl crate::transport::Transport for Site {
        fn address(&self) -> crate::transport::Address {
            "me".into()
        }
        fn send(&self, _p: &str, _f: &[u8]) -> Result<()> {
            Ok(())
        }
        fn try_recv(&self) -> Option<(crate::transport::Address, Vec<u8>)> {
            None
        }
        fn onion_get_capped(
            &self,
            onion: &str,
            port: u16,
            path: &str,
            max_bytes: usize,
        ) -> Option<Result<Vec<u8>>> {
            self.asked
                .lock()
                .unwrap()
                .push((onion.into(), port, path.into(), max_bytes));
            Some(Ok(self.body.clone()))
        }
    }
    let wire = list(1, &["a.onion"]);
    let site = Site {
        body: wire.clone().into_bytes(),
        asked: Mutex::new(Vec::new()),
    };
    assert_eq!(directory::fetch_from_site(&site).unwrap().unwrap(), wire);
    assert_eq!(
        site.asked.lock().unwrap()[0],
        (
            crate::update::UPDATE_ONION.to_string(),
            crate::update::UPDATE_PORT,
            "/relays.json".to_string(),
            directory::MAX_SITE_BYTES
        )
    );
    let big = Site {
        body: vec![b' '; directory::MAX_SITE_BYTES + 1],
        asked: Mutex::new(Vec::new()),
    };
    assert!(directory::fetch_from_site(&big).unwrap().is_err());
}

#[test]
fn when_the_site_fails_a_relay_served_list_is_still_adopted() {
    let mut n = node();
    let result = DirectoryFetchResult {
        site: Some(Err(anyhow::anyhow!("site down"))),
        relay_lists: vec![
            "garbage".into(),
            list(4, &["from-relay.onion"]),
            list(6, &["later-relay.onion"]),
        ],
    };
    n.finish_directory_fetch(result, NOW, &roots());
    assert_eq!(
        n.discovered_relays,
        vec!["from-relay.onion"],
        "first genuine, newer list wins"
    );
    assert_eq!(n.directory_version, 4);
    assert!(
        n.directory_fetch_due(NOW + RETRY_INTERVAL_SECS),
        "the site is still retried soon"
    );
}

/// The relays are only asked when the site gave nothing usable.
#[test]
fn the_relays_are_not_asked_when_the_site_answers() {
    struct NoRelays;
    impl crate::transport::Transport for NoRelays {
        fn address(&self) -> crate::transport::Address {
            "me".into()
        }
        fn send(&self, _p: &str, _f: &[u8]) -> Result<()> {
            Ok(())
        }
        fn try_recv(&self) -> Option<(crate::transport::Address, Vec<u8>)> {
            None
        }
        fn onion_get_capped(
            &self,
            _o: &str,
            _p: u16,
            _path: &str,
            _m: usize,
        ) -> Option<Result<Vec<u8>>> {
            Some(Ok(b"{}".to_vec()))
        }
    }
    // A listening relay address: if the fetch asked it, a connection would be waiting.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let plan = DirectoryFetchPlan {
        transport: std::sync::Arc::new(NoRelays),
        relays: vec![RelayClient::new(listener.local_addr().unwrap().to_string())],
    };
    let r = fetch_directory(&plan);
    assert!(matches!(r.site, Some(Ok(_))));
    assert!(r.relay_lists.is_empty());
    assert_eq!(
        listener.accept().map(|_| ()).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "the relay was never contacted"
    );

    // And when the site has nothing, the same relay IS asked.
    struct NoSite;
    impl crate::transport::Transport for NoSite {
        fn address(&self) -> crate::transport::Address {
            "me".into()
        }
        fn send(&self, _p: &str, _f: &[u8]) -> Result<()> {
            Ok(())
        }
        fn try_recv(&self) -> Option<(crate::transport::Address, Vec<u8>)> {
            None
        }
    }
    let plan = DirectoryFetchPlan {
        transport: std::sync::Arc::new(NoSite),
        relays: plan.relays,
    };
    let asked = std::thread::spawn(move || fetch_directory(&plan));
    let mut contacted = false;
    for _ in 0..200 {
        if listener.accept().is_ok() {
            contacted = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(contacted, "with no site answer the relays are asked");
    drop(listener);
    let _ = asked.join();
}

/// The signed list names the primary too. Counted twice, every rendezvous post and take went to
/// the same relay twice (a joiner's opener "posted to 2/2 relays", 2026-10-07).
#[test]
fn the_primary_listed_again_by_the_directory_is_one_relay() {
    let mut n = node();
    n.set_relay(RelayClient::new("primary.onion"));
    n.set_my_relays(vec!["mine.onion".into(), "primary.onion".into()]);
    assert_eq!(
        n.adopt_directory(
            &list(1, &["primary.onion", "other.onion", "mine.onion"]),
            &roots()
        ),
        DirectoryOutcome::Applied { changed: true }
    );
    let mut addrs: Vec<String> = n
        .rendezvous_relays()
        .iter()
        .map(|r| r.addr().unwrap().to_string())
        .collect();
    addrs.sort();
    assert_eq!(addrs, ["mine.onion", "other.onion", "primary.onion"]);
    assert_eq!(n.directory_relays().len(), 3);
}

#[test]
fn a_rotated_list_key_is_adopted_and_the_old_one_refused_from_then_on() {
    // Self-audit 2026-10-13, key-compromise plan: the list-signing key leaked, so the operator
    // names a new one with the recovery key.
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let net = MemoryNetwork::new();
    let mut n = Node::new(Box::new(net.endpoint("me")));
    let new_key = SigningKey::from_bytes(&[12u8; 32]);
    let rotation = crate::keyroll::sign_rotation(
        &b64.encode(recovery().to_bytes()),
        crate::keyroll::Purpose::Directory,
        1,
        &b64.encode(new_key.verifying_key().to_bytes()),
    )
    .unwrap();
    let dir = |version: u64, relays: &[&str]| RelayDirectory {
        version,
        issued_at: NOW,
        relays: relays.iter().map(|s| s.to_string()).collect(),
    };
    let mut signed = directory::sign(&dir(5, &["new.onion"]), &new_key);
    signed.rotation = Some(rotation);
    assert!(matches!(
        n.adopt_directory(&signed.to_wire(), &roots()),
        DirectoryOutcome::Applied { .. }
    ));
    assert_eq!(n.directory_key.as_ref().map(|t| t.seq), Some(1));
    // The leaked old key can no longer move the list, even with a higher version.
    let stale = directory::sign(&dir(9, &["evil.onion"]), &key()).to_wire();
    assert!(matches!(
        n.adopt_directory(&stale, &roots()),
        DirectoryOutcome::Invalid
    ));
    assert!(!n.discovered_relays.iter().any(|r| r == "evil.onion"));
}
