//! Does `connect()`'s TLS ClientHello look like Chrome opening a WebSocket?
//!
//! WebTunnel hides Tor inside what should look like a browser's `wss://` connection, so the
//! reference is a real one: Chromium's ClientHello for `new WebSocket("wss://…")`. Chrome offers
//! only `http/1.1` there, which is exactly what WebTunnel needs — a page load offers `h2`, and a
//! bridge's nginx would take it, breaking the HTTP Upgrade.
//!
//! This is the drift check for step 2 (docs/design/android-bridges.md §5): it drives the real
//! `connect()` at a local listener that captures the first TLS record, computes its JA4, and
//! asserts it still equals the Chrome JA4 we validated by capture. When BoringSSL or the profile
//! drifts, this fails and we re-validate against a current Chrome.
//!
//! Gated behind `chrome-proto`, the feature that makes `connect()` use BoringSSL; the default
//! (rustls) build emits a different hello and never compiles BoringSSL:
//!   cargo test -p webtunnel-client --features chrome-proto --test fingerprint
#![cfg(feature = "chrome-proto")]

use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::thread;
use webtunnel_client::{connect, ClientConfig, PtArgs};

use webtunnel_client::ja4::{is_grease, ja4, parse, CHROME_WEBSOCKET_JA4 as CHROME_JA4};

/// Drive `connect()` at a listener that captures the first TLS record, and return that
/// ClientHello. `connect()` reaches TLS (the listener closes right after, so the handshake then
/// fails — but the hello is already on the wire) via `addr=` so it dials loopback directly.
async fn capture_hello() -> Vec<u8> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut hdr = [0u8; 5];
        conn.read_exact(&mut hdr).unwrap();
        assert_eq!(hdr[0], 22, "not a TLS handshake record");
        let len = u16::from_be_bytes([hdr[3], hdr[4]]) as usize;
        let mut body = vec![0u8; len];
        conn.read_exact(&mut body).unwrap();
        body
    });
    let line = format!("url=https://ws.test/secret;addr={addr}");
    let cfg = ClientConfig::from_args(&PtArgs::parse(&line).unwrap()).unwrap();
    let _ = connect(&cfg).await; // fails after the hello; that is fine
    handle.join().unwrap()
}

#[tokio::test]
async fn matches_chrome_websocket() {
    let h = parse(&capture_hello().await);
    // Structural asserts first: they give a readable failure before the opaque JA4 diff.
    assert!(h.ciphers.iter().any(|c| is_grease(*c)), "no GREASE cipher");
    assert!(
        h.groups.contains(&0x11ec),
        "no X25519MLKEM768 key-share group"
    );
    assert!(
        h.ext_types.contains(&0x001b),
        "no compress_certificate extension"
    );
    assert_eq!(h.alpn, ["http/1.1"], "ALPN must be exactly http/1.1");
    assert_eq!(
        ja4(&h),
        CHROME_JA4,
        "JA4 drifted from the validated Chrome profile"
    );
}

#[tokio::test]
async fn ja4_is_stable_across_connections() {
    // GREASE randomises per connection; JA4 must not.
    assert_eq!(
        ja4(&parse(&capture_hello().await)),
        ja4(&parse(&capture_hello().await))
    );
}
