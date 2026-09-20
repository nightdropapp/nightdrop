//! Phase 0 acceptance: the **deployed** relay, exercised over **real Tor**.
//!
//! Offline delivery and short-code pairing both ride the relay's store-and-forward mailbox
//! (`ARCHITECTURE.md` §6/§11.2). The in-memory node tests prove the protocol; this proves the
//! *live* relay is reachable over Tor and speaks it correctly end to end — the gap the
//! `MemoryNetwork` tests can't reach.
//!
//! It dials the relay `.onion` exactly as the app does (`TorTransport::make_relay_dialer` +
//! `RelayClient::with_dialer`) and runs a sender→recipient roundtrip plus a recall (unsend).
//!
//! `#[ignore]`d (needs network + a running relay). Run it against the deployed onion:
//!   RELAY_ONION=<relay>.onion \
//!     cargo test -p nightdrop --features tor --test relay_e2e -- --ignored --nocapture
#![cfg(feature = "tor")]

use std::time::{Duration, SystemTime};

use nightdrop::relay_client::RelayClient;
use nightdrop::transport::tor::TorTransport;

/// Unique mailbox handle per run so reruns never collide on the shared relay.
fn unique_handle(tag: &str) -> String {
    let ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("phase0-{tag}-{}-{ms}", std::process::id())
}

/// Retry `post` until the relay's onion descriptor is reachable. Tor is slow (cold HS
/// descriptor lookups can take minutes), so be patient — window is env-tunable via
/// `RELAY_E2E_TRIES` (default 150) × 4s ≈ 10 min. Fails loudly if never reachable.
fn post_with_retry(
    client: &RelayClient,
    handle: &str,
    blob: &[u8],
) -> nightdrop::relay_client::PostReceipt {
    let tries: u32 = std::env::var("RELAY_E2E_TRIES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(150);
    let start = SystemTime::now();
    for attempt in 1..=tries {
        match client.post(handle, blob, Duration::from_secs(3600)) {
            Ok(r) => {
                let secs = start.elapsed().map(|d| d.as_secs()).unwrap_or(0);
                eprintln!("  posted on attempt {attempt} (reachable after ~{secs}s)");
                return r;
            }
            Err(e) => {
                eprintln!("  post attempt {attempt}/{tries} not yet reachable: {e}");
                std::thread::sleep(Duration::from_secs(4));
            }
        }
    }
    panic!(
        "could not reach the deployed relay to POST after {tries} tries — is it up + published?"
    );
}

#[test]
#[ignore = "needs network + a deployed relay; set RELAY_ONION=<relay>.onion"]
fn deployed_relay_store_and_forward_and_recall_over_tor() {
    // Client-side arti tracing when RUST_LOG is set (e.g. tor_hsclient=debug,tor_circmgr=debug)
    // — pinpoints where an onion dial fails: descriptor fetch, intro point, or rendezvous.
    if std::env::var("RUST_LOG").is_ok() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .try_init();
    }

    let relay = std::env::var("RELAY_ONION")
        .expect("set RELAY_ONION=<relay>.onion (from relay-state/onion)");
    assert!(relay.ends_with(".onion"), "RELAY_ONION must be a .onion");
    eprintln!("== Phase 0 relay acceptance ==\n  relay: {relay}");

    // Dial the relay exactly as the app does: a Tor client + the relay dialer. Use a
    // PERSISTENT state dir (like the real apps) so a rerun reuses cached consensus and
    // bootstraps/looks-up descriptors much faster than a cold client.
    let state =
        std::env::var("RELAY_E2E_STATE").unwrap_or_else(|_| "/tmp/nd-relay-e2e-client".into());
    eprintln!("  bootstrapping Tor (state: {state}; cold bootstrap can take minutes)…");
    let boot = SystemTime::now();
    let tor =
        TorTransport::bootstrap("ndrelaye2e", Some(&state), None, None).expect("bootstrap Tor");
    eprintln!(
        "  Tor bootstrapped in ~{}s",
        boot.elapsed().map(|d| d.as_secs()).unwrap_or(0)
    );
    let client = RelayClient::with_dialer(tor.make_relay_dialer(relay.clone()));

    // --- 1. Store-and-forward: sender drops an opaque blob; recipient drains it. ---
    let handle = unique_handle("saf");
    let sealed = b"sealed-offline-envelope-\x00\x01\x02-over-tor";
    eprintln!("[1] sender posts an offline message…");
    post_with_retry(&client, &handle, sealed);

    eprintln!("[2] recipient peeks (content-free count)…");
    assert_eq!(
        client.peek(&handle).expect("peek"),
        1,
        "relay should report exactly 1 queued blob"
    );

    eprintln!("[3] recipient drains (take) — must be byte-exact…");
    let drained = client.take(&handle).expect("take");
    assert_eq!(drained.len(), 1, "expected exactly one drained blob");
    assert_eq!(
        drained[0].as_slice(),
        sealed,
        "drained blob must byte-match what was posted"
    );

    eprintln!("[4] draining consumed it — mailbox is now empty…");
    assert_eq!(
        client.peek(&handle).expect("peek-after"),
        0,
        "mailbox must be empty after take"
    );

    // --- 2. Recall (unsend): a still-queued blob can be pulled back before delivery. ---
    let handle2 = unique_handle("recall");
    eprintln!("[5] sender posts, then recalls (unsend) before the recipient drains…");
    let receipt = post_with_retry(&client, &handle2, b"to-be-unsent");
    assert_eq!(client.peek(&handle2).expect("peek-before-recall"), 1);
    assert!(
        client.recall(&handle2, &receipt).expect("recall"),
        "recall should report success"
    );
    assert_eq!(
        client.peek(&handle2).expect("peek-after-recall"),
        0,
        "recalled blob must be gone — the recipient never receives it"
    );

    eprintln!("== PASS: live relay store-and-forward + recall verified over Tor ==");
}
