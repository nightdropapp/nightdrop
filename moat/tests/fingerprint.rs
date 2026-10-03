//! Does the meek connection to a CDN front open with Chrome's TLS handshake?
//!
//! What a censor sees of a bridge fetch is a TLS connection to the front. With the `chrome`
//! feature it must be the same ClientHello Chrome sends when it opens a WebSocket — the profile
//! WebTunnel already uses and pins (`webtunnel_client::ja4::CHROME_WEBSOCKET_JA4`). Drives the real
//! `FrontedMeek` at a local listener that captures the first TLS record.
//!   cargo test -p moat --features chrome --test fingerprint
#![cfg(feature = "chrome")]

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::thread;

use moat::meek::{chrome_tls, FrontedMeek, RoundTrip};
use webtunnel_client::ja4::{is_grease, ja4, parse, CHROME_WEBSOCKET_JA4};

const FRONT: &str = "cdn.zk.mk";

/// The ClientHello `FrontedMeek` sends to `FRONT` (the handshake then fails against the capturing
/// listener, which is fine: the hello is already on the wire).
fn capture_hello() -> Vec<u8> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
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
    let targets = moat::targets::parse(&format!("https://reflector.example|{FRONT}")).unwrap();
    // One attempt only reaches the listener; later retries find it gone and fail, which is fine.
    let dial = Box::new(move |_front: &str| TcpStream::connect(("127.0.0.1", port)));
    let mut meek = FrontedMeek::new(targets, chrome_tls(), dial).unwrap();
    let _ = meek.round_trip(b"");
    handle.join().unwrap()
}

#[test]
fn the_front_connection_looks_like_chrome_opening_a_websocket() {
    let hello = capture_hello();
    let h = parse(&hello);
    assert!(h.sni, "no SNI");
    assert!(
        hello.windows(FRONT.len()).any(|w| w == FRONT.as_bytes()),
        "the SNI must name the front, the only name the network sees"
    );
    assert!(
        !hello.windows(9).any(|w| w == b"reflector"),
        "the reflector must never appear in the handshake"
    );
    assert!(h.ciphers.iter().any(|c| is_grease(*c)), "no GREASE cipher");
    assert_eq!(
        h.alpn,
        ["http/1.1"],
        "ALPN must be exactly http/1.1, as Chrome's WebSocket hello"
    );
    assert_eq!(
        ja4(&h),
        CHROME_WEBSOCKET_JA4,
        "JA4 drifted from the validated Chrome profile"
    );
}

#[test]
fn the_fingerprint_is_stable_across_connections() {
    assert_eq!(ja4(&parse(&capture_hello())), ja4(&parse(&capture_hello())));
}
