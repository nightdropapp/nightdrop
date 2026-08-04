//! Smoke test for the embedded Tor transport (`transport::tor`). Requires the `tor`
//! feature AND outbound network access — it bootstraps a real Tor circuit and publishes
//! an onion-service descriptor, which takes (tens of seconds to) minutes. It is therefore
//! `#[ignore]`d so normal `cargo test` stays fast and offline.
//!
//! Run it explicitly:
//!   cargo test -p nightdrop --features tor --test tor_smoke -- --ignored --nocapture
#![cfg(feature = "tor")]

use nightdrop::transport::tor::TorTransport;
use nightdrop::transport::{client_auth, Transport};

#[test]
#[ignore = "needs network; bootstraps a real Tor circuit (slow)"]
fn bootstrap_and_publish_onion_address() {
    let transport = TorTransport::bootstrap("nightdroptest", None, None, None)
        .expect("bootstrap Tor and launch onion service");
    let address = transport.address();
    eprintln!("Tor onion address: {address}");
    assert!(
        address.ends_with(".onion"),
        "expected an onion address, got `{address}`"
    );
    // v3 onion addresses are 56 base32 chars + ".onion".
    assert_eq!(
        address.len(),
        56 + ".onion".len(),
        "unexpected onion address length"
    );
}

/// Full onion-to-onion round-trip of a frame. Slower/flakier than the bootstrap smoke
/// test (it waits for A's descriptor to publish and B to build a rendezvous circuit), so
/// it allows generous retries.
#[test]
#[ignore = "needs network; two onion services + a rendezvous circuit (very slow)"]
fn two_onions_round_trip_a_frame() {
    use std::time::Duration;

    let a = TorTransport::bootstrap("nightdropa", None, None, None).expect("bootstrap A");
    let b = TorTransport::bootstrap("nightdropb", None, None, None).expect("bootstrap B");
    let a_addr = a.address();

    // Retry the dial until A's descriptor is reachable.
    let mut sent = false;
    for _ in 0..30 {
        if b.send(&a_addr, b"hello over tor").is_ok() {
            sent = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    assert!(sent, "could not dial A's onion service");

    // Poll A for the frame.
    let mut received = None;
    for _ in 0..600 {
        if let Some((_, frame)) = a.try_recv() {
            received = Some(frame);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(received.expect("frame over Tor"), b"hello over tor");
}

/// A fresh, unique temp dir for a sub-path of this test run (no `tempfile` dev-dep).
fn scratch(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "nightdrop-torauth-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.to_string_lossy().into_owned()
}

/// End-to-end validation of **onion client authorization** (restricted discovery, #22): an
/// authorized client can reach a restricted onion; an unauthorized one cannot.
///
/// Flow (mirrors `node::announce_client_key` pairing, minus the app layer):
/// 1. Bootstrap A as a normal **public** onion (empty auth dir) to learn its stable address.
/// 2. B generates its client descriptor-encryption key **for A's onion** and A authorizes it
///    (writes it into A's watched key dir) — exactly what `Frame::ClientKey` carries at pairing.
/// 3. Re-launch A on the **same** keystore: now `authorized_count > 0`, so A publishes a
///    **restricted** descriptor encrypted to B's key (same `.onion`).
/// 4. B (holding the matching secret in its keymgr) connects and delivers a frame — **admitted**.
/// 5. C, which never exchanged a key with A, tries to connect — **refused** (can't decrypt the
///    descriptor). We only assert this after proving B succeeded, so a failure here is the
///    restriction working, not the network being down.
///
/// Very slow: four onion bootstraps + two descriptor (re)publications. Keep `#[ignore]`d.
#[test]
#[ignore = "needs network; four onion bootstraps + restricted-discovery republish (very slow)"]
fn restricted_onion_admits_authorized_client_and_refuses_unauthorized() {
    use std::path::Path;
    use std::time::Duration;

    let a_state = scratch("a-state");
    let a_auth = scratch("a-auth");
    let b_state = scratch("b-state");
    let c_state = scratch("c-state");

    // 1. A as a public onion first, to learn its address — and to take custody of the identity
    //    behind it, since nothing else will.
    let (a_addr, a_onion_key) = {
        let a = TorTransport::bootstrap("nightdropauth-a", Some(&a_state), Some(&a_auth), None)
            .expect("bootstrap A (public)");
        let addr = a.address();
        // Read the generated identity back out of the in-memory keystore, exactly as the app does
        // after a first run (`docs/design/onion-key-at-rest.md`). Since that change, arti's
        // keystore is **Ephemeral** unless we hand it a saved key, so it persists nothing: without
        // this, the relaunch at step 3 mints a brand-new identity and A is simply a different
        // service. This test asserted the address survived while guaranteeing it could not —
        // it compiled, and failed on every run. Caught 2026-08-02.
        let key = a
            .onion_key_material()
            .expect("read back A's generated onion identity");
        assert!(client_auth::authorized_count(Path::new(&a_auth)) == 0);
        eprintln!("A public onion: {addr}");
        (addr, key)
        // A dropped here: its onion service stops so the re-launch below owns the identity.
    };
    std::thread::sleep(Duration::from_secs(3));

    // 2. B mints its client key FOR A's onion; A authorizes it (the pairing exchange).
    let b = TorTransport::bootstrap("nightdropauth-b", Some(&b_state), None, None)
        .expect("bootstrap B");
    // `make_service_discovery_key` now mints the x25519 secret itself and hands back both halves:
    // the public key A authorizes, and the secret B keeps (sealed in the store, no longer written
    // to arti's on-disk keystore — `docs/design/onion-key-at-rest.md`).
    let (key_b, _secret_b) = b
        .make_service_discovery_key(&a_addr)
        .expect("B generates its descriptor-encryption key for A");
    eprintln!("B client key for A: {key_b}");
    client_auth::authorize(Path::new(&a_auth), "contact-b", &key_b)
        .expect("A authorizes B's client key");
    assert_eq!(client_auth::authorized_count(Path::new(&a_auth)), 1);

    // 3. Re-launch A on its OWN saved identity + the now-non-empty auth dir → restricted onion at
    //    the same address. Handing the key back is what makes the address survive; it is also
    //    precisely what the app does on every start after the first.
    let a = TorTransport::bootstrap(
        "nightdropauth-a",
        Some(&a_state),
        Some(&a_auth),
        Some(a_onion_key),
    )
    .expect("re-bootstrap A (restricted)");
    assert_eq!(
        a.address(),
        a_addr,
        "onion address must survive the relaunch"
    );

    // Wait for the restricted descriptor to (re)publish.
    let mut published = false;
    for _ in 0..180 {
        if a.published() {
            published = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    assert!(
        published,
        "A's restricted descriptor never became reachable"
    );

    // 4. B (authorized) connects and delivers a frame → admitted.
    let mut b_sent = false;
    for _ in 0..40 {
        if b.send(&a_addr, b"authorized hello").is_ok() {
            b_sent = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    assert!(
        b_sent,
        "authorized client B could not reach the restricted onion"
    );

    let mut got = None;
    for _ in 0..600 {
        if let Some((_, frame)) = a.try_recv() {
            got = Some(frame);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        got.expect("A receives the authorized client's frame"),
        b"authorized hello"
    );

    // 5. C never exchanged a key with A → refused. B just proved the descriptor is live and
    //    reachable, so C failing over an equivalent window is the restriction, not a flaky net.
    let c = TorTransport::bootstrap("nightdropauth-c", Some(&c_state), None, None)
        .expect("bootstrap C");
    let mut c_reached = false;
    for _ in 0..40 {
        if c.send(&a_addr, b"unauthorized hello").is_ok() {
            c_reached = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    assert!(
        !c_reached,
        "unauthorized client C reached the restricted onion — client authorization is NOT enforced"
    );

    // Best-effort cleanup.
    for d in [&a_state, &a_auth, &b_state, &c_state] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Is a relay actually reachable from a client, over Tor, right now?
///
/// Debug aid for the "could not reach any relay to start pairing" failure (`TODO.md` #6/#7),
/// where the joiner can't post its opener. That error can't tell you *why*: the relay could be
/// down, its descriptor unpublished, or the address stale. This bootstraps a real client exactly
/// as the app does and does one `peek` round-trip against the relay, which separates "the relay
/// is unreachable" from anything in the pairing logic itself.
///
/// Run it against the address the apps have baked in:
///   NIGHTDROP_RELAY=$(cat relay-state/onion) \
///     cargo test -p nightdrop --features tor --test tor_smoke -- --ignored --nocapture relay_is_reachable
#[test]
#[ignore = "needs network + a running relay; bootstraps a real Tor circuit (slow)"]
fn relay_is_reachable_over_tor() {
    let Ok(relay_onion) = std::env::var("NIGHTDROP_RELAY") else {
        panic!("set NIGHTDROP_RELAY=<relay .onion> (e.g. from relay-state/onion)");
    };
    eprintln!("probing relay: {relay_onion}");

    // Optionally bootstrap against a *copy* of the app's real state dir, to reproduce a failure
    // that only happens with the app's Tor state (NIGHTDROP_STATE=/path/to/app/support/dir).
    let state = std::env::var("NIGHTDROP_STATE").ok();
    if let Some(s) = &state {
        eprintln!("using app state dir: {s}");
    }
    let transport = TorTransport::bootstrap("nightdropprobe", state.as_deref(), None, None)
        .expect("bootstrap Tor and launch onion service");
    eprintln!("client Tor is up (our onion: {})", transport.address());

    let relay = nightdrop::relay_client::RelayClient::with_dialer(
        transport.make_relay_dialer(relay_onion.clone()),
    );
    // Control: can this client reach ANY onion right now? If a known-good public onion also
    // fails, the problem is this host's Tor path, not the relay. (DuckDuckGo's v3 onion.)
    {
        const KNOWN: &str = "duckduckgogg42xjoc72x3sjasowoarfbgcmvfimaftt6twagswzczad.onion";
        let dial = transport.make_relay_dialer(KNOWN.to_string());
        // We don't speak the relay protocol to it — just see if a circuit/stream opens at all.
        match nightdrop::relay_client::RelayClient::with_dialer(dial).peek("x") {
            Ok(_) => eprintln!("CONTROL: reached a known public onion (client Tor path OK)"),
            Err(e) => {
                let s = format!("{e:#}");
                // Classify on what the message MEANS, not on whether the word "circuit" appears in
                // it. We dial this onion on the relay's port, which it does not serve, so the
                // healthy outcome here is the service refusing the stream — which arti reports as
                // "Received an END cell with reason DONE", wrapped in text that mentions building a
                // circuit. The old check keyed on "circuit" and so reported the exact success case
                // as a failure. On 2026-08-02 that sent an hour of debugging at the network and the
                // Tor stack while the real fault (the relay's descriptor not published for one time
                // period) sat untouched. A control that lies is worse than no control at all.
                //
                // Reaching the service at all is the whole question, so anything that proves the
                // rendezvous completed counts as reachable, however the stream then ended.
                let reached = s.contains("END cell")
                    || s.contains("remote stream closed")
                    || s.contains("Protocol error");
                if reached {
                    eprintln!("CONTROL: reached a known public onion — stream refused at the service, which is expected on this port (client Tor path OK)");
                } else {
                    eprintln!("CONTROL: client CANNOT reach a known public onion either: {s}");
                }
            }
        }
    }

    // Content-free: asks only how many blobs wait under a handle nobody uses. A reply of any
    // kind proves the round-trip works, which is all we're asking.
    match relay.peek("rdv:probe:j") {
        Ok(n) => eprintln!("RELAY REACHABLE — peek returned {n}"),
        Err(e) => panic!("RELAY UNREACHABLE from this client: {e:#}"),
    }

    // Also exercise POST — the exact op a joiner's opener uses, in case the relay accepts peeks
    // but rejects posts (a size/version/rate issue would show here, not in peek).
    match relay.post(
        "rdv:probe:j",
        b"opener-probe",
        std::time::Duration::from_secs(60),
    ) {
        Ok(r) => eprintln!("POST OK — msg_id {}", r.msg_id),
        Err(e) => panic!("POST FAILED (peek worked): {e:#}"),
    }
}

/// Poll `cond` until it holds or `bound` elapses. `false` on timeout.
fn wait_until(bound: std::time::Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + bound;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    cond()
}

/// Where arti actually left `guards.json`, used only to explain a failure. `reset_tor_guards`
/// hard-codes `<base>/arti-state/state/`, so if arti ever moves the file the in-app guard heal
/// silently stops resetting anything — and this is the only test that would notice.
fn find_guards(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "guards.json") {
                return Some(path);
            }
        }
    }
    None
}

/// The client rebuilt by a **guard heal** must come up *writable*.
///
/// arti holds an exclusive on-disk lock on its state dir. If anything from the old core still
/// holds it when the replacement bootstraps, arti logs "Another process has the lock on our state
/// files" and proceeds **read-only** — so it cannot persist the fresh guards the heal just picked,
/// the next launch inherits the same wedged set, and it heals again. That looped for an afternoon
/// on a desktop, 2026-08-02.
///
/// The lock itself isn't observable from here, but its consequence is: a read-only client never
/// rewrites `guards.json`. So this deletes that file exactly as the in-app heal does and asserts
/// the replacement puts it back.
///
/// The teardown is staged at the worst possible moment on purpose — the background poller blocked
/// inside a relay round-trip, holding a dialer that clones the arti client *and* the tokio runtime.
/// That is the handle that used to outlive the swap, and `shutdown()` waiting for it is the fix.
#[test]
#[ignore = "needs network; bootstraps Tor twice over one state dir (very slow)"]
fn a_core_rebuilt_over_the_same_state_dir_can_still_persist_its_guards() {
    use nightdrop::api::{reset_tor_guards, NightdropCore};
    use nightdrop::relay_client::{RelayClient, RelayDialer};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const NICKNAME: &str = "nightdropheal";
    // Nowhere in particular: the dial fails fast, which is fine. What this test needs is that the
    // poller is *holding* a Tor-backed dialer when the teardown lands, not that it hangs in one.
    const RELAY: &str = "nightdrop-test-relay.onion";

    let base = scratch("heal-state");
    let guards = Path::new(&base)
        .join("arti-state")
        .join("state")
        .join("guards.json");

    let transport =
        TorTransport::bootstrap(NICKNAME, Some(&base), None, None).expect("bootstrap the client");
    let first_addr = transport.address();
    // The app holds the onion identity in its own sealed store (arti's keystore is ephemeral here)
    // and hands it back on every later start. Do the same, so this is the rebuild the app performs
    // — and so the address assertion below means something.
    let onion_key = transport
        .onion_key_material()
        .expect("read back the generated onion identity");
    eprintln!("first client is up (onion: {first_addr})");

    assert!(
        wait_until(Duration::from_secs(120), || guards.exists()),
        "arti never wrote {} — {}",
        guards.display(),
        match find_guards(Path::new(&base)) {
            Some(found) => format!(
                "it is at {} instead, so reset_tor_guards deletes the wrong path",
                found.display()
            ),
            None => "and there is no guards.json anywhere under the state dir".to_string(),
        }
    );

    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let relay = {
        // Built before the transport moves into the core: this closure clones the arti client and
        // the tokio runtime, which is precisely what kept the state lock alive past a teardown.
        let tor_dialer = transport.make_relay_dialer(RELAY.to_string());
        let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
        let start = Instant::now();
        let dialer: RelayDialer = Arc::new(move |request: &str| {
            eprintln!(
                "[{:?}] poller entered the relay round-trip",
                start.elapsed()
            );
            entered.store(true, Ordering::Relaxed);
            while !release.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
            eprintln!("[{:?}] released; dialing over Tor", start.elapsed());
            let outcome = tor_dialer(request);
            eprintln!(
                "[{:?}] dial returned: {}",
                start.elapsed(),
                match &outcome {
                    Ok(_) => "ok".to_string(),
                    Err(e) => format!("{e:#}"),
                }
            );
            outcome
        });
        RelayClient::with_dialer(dialer)
    };
    let core = NightdropCore::new_with_transport(Box::new(transport), Some(relay), true);

    assert!(
        wait_until(Duration::from_secs(60), || entered.load(Ordering::Relaxed)),
        "the background poller never started a relay drain, so this test would prove nothing"
    );

    // Let go shortly after the teardown begins, so `shutdown` is genuinely waiting on a poller
    // that is holding arti handles rather than on an idle one.
    let releaser = {
        let release = Arc::clone(&release);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            release.store(true, Ordering::Relaxed);
        })
    };
    eprintln!("calling shutdown with the poller inside the round-trip");
    let started = Instant::now();
    core.shutdown();
    let waited = started.elapsed();
    releaser.join().unwrap();
    drop(core);
    eprintln!("shutdown returned after {waited:?}");
    assert!(
        waited >= Duration::from_millis(250),
        "shutdown returned in {waited:?} — it did not wait for the in-flight relay drain"
    );
    // Under, not at, `POLLER_EXIT_TIMEOUT` (8s): landing *on* the bound means the wait expired
    // rather than observing the poller exit, which is a silent half-fix — the lock is released
    // whenever it is released, and a rebuild right after can still race it. An early version of
    // this bound was 3s and expired 200ms short of a ~3.2s teardown; this assertion is what
    // caught it.
    assert!(
        waited < Duration::from_secs(7),
        "shutdown took {waited:?} — it hit its own timeout instead of seeing the poller exit"
    );

    // The heal itself: reset the guards with the core down, then rebuild over the same state dir.
    reset_tor_guards(base.clone());
    assert!(
        !guards.exists(),
        "reset_tor_guards left {} in place",
        guards.display()
    );

    let rebuilt = TorTransport::bootstrap(NICKNAME, Some(&base), None, Some(onion_key))
        .expect("bootstrap the replacement client");
    assert_eq!(
        rebuilt.address(),
        first_addr,
        "the heal must keep the .onion — a new address strands every contact with no notice"
    );
    assert!(
        wait_until(Duration::from_secs(120), || guards.exists()),
        "the replacement client never persisted guards.json, so it came up read-only: something \
         from the first core still held arti's state lock. This is the guard heal that healed \
         forever (2026-08-02)."
    );

    drop(rebuilt);
    let _ = std::fs::remove_dir_all(&base);
}
