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

/// JA4 of Chromium 152 opening a WebSocket, captured 2026-09-19 (the 15-extension profile;
/// Chrome intermittently also sends the `trust_anchors` draft extension → a 16-ext variant).
/// GREASE is excluded from JA4 by construction, so this is stable per Chrome build. Refresh it,
/// and the profile in `tls.rs`, when this test fails against a current Chrome.
const CHROME_JA4: &str = "t13d1515h1_8daaf6152771_f04195365787";

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

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([b[i], b[i + 1]])
}

fn is_grease(v: u16) -> bool {
    v & 0x0f0f == 0x0a0a && v >> 8 == v & 0xff
}

struct Hello {
    ciphers: Vec<u16>,
    ext_types: Vec<u16>,
    alpn: Vec<String>,
    sigalgs: Vec<u16>,
    groups: Vec<u16>,
    sni: bool,
}

fn parse(msg: &[u8]) -> Hello {
    assert_eq!(msg[0], 1, "not a ClientHello");
    let mut p = 4 + 2 + 32; // type/len, legacy_version, random
    p += 1 + msg[p] as usize; // session id
    let cs_len = u16_at(msg, p) as usize;
    p += 2;
    let ciphers = (0..cs_len).step_by(2).map(|i| u16_at(msg, p + i)).collect();
    p += cs_len;
    p += 1 + msg[p] as usize; // compression methods
    let end = p + 2 + u16_at(msg, p) as usize;
    p += 2;
    let (mut ext_types, mut alpn, mut sigalgs, mut groups, mut sni) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), false);
    while p < end {
        let t = u16_at(msg, p);
        let l = u16_at(msg, p + 2) as usize;
        let d = &msg[p + 4..p + 4 + l];
        ext_types.push(t);
        match t {
            0x0000 => sni = true,
            0x0010 => {
                let mut q = 2;
                while q < d.len() {
                    let n = d[q] as usize;
                    alpn.push(String::from_utf8_lossy(&d[q + 1..q + 1 + n]).into_owned());
                    q += 1 + n;
                }
            }
            0x000d => sigalgs = (2..d.len()).step_by(2).map(|i| u16_at(d, i)).collect(),
            0x000a => groups = (2..d.len()).step_by(2).map(|i| u16_at(d, i)).collect(),
            _ => {}
        }
        p += 4 + l;
    }
    Hello {
        ciphers,
        ext_types,
        alpn,
        sigalgs,
        groups,
        sni,
    }
}

/// JA4 (FoxIO) — the fingerprint a censor's tooling logs. GREASE is filtered; ciphers and
/// extensions are sorted before hashing, so it is order-independent by design.
fn ja4(h: &Hello) -> String {
    use sha2::{Digest, Sha256};
    let sha12 = |s: &str| hex::encode(&Sha256::digest(s.as_bytes())[..6]);
    let ciphers: Vec<u16> = h
        .ciphers
        .iter()
        .copied()
        .filter(|c| !is_grease(*c))
        .collect();
    let exts: Vec<u16> = h
        .ext_types
        .iter()
        .copied()
        .filter(|e| !is_grease(*e))
        .collect();
    let alpn = h.alpn.first().cloned().unwrap_or_default();
    let a_alpn = if alpn.is_empty() {
        "00".into()
    } else {
        let b = alpn.as_bytes();
        format!("{}{}", b[0] as char, b[b.len() - 1] as char)
    };
    let a = format!(
        "t13{}{:02}{:02}{a_alpn}",
        if h.sni { "d" } else { "i" },
        ciphers.len().min(99),
        exts.len().min(99),
    );
    let mut cs: Vec<String> = ciphers.iter().map(|c| format!("{c:04x}")).collect();
    cs.sort();
    let b = sha12(&cs.join(","));
    let mut es: Vec<String> = exts
        .iter()
        .filter(|e| **e != 0x0000 && **e != 0x0010)
        .map(|e| format!("{e:04x}"))
        .collect();
    es.sort();
    let sig: Vec<String> = h
        .sigalgs
        .iter()
        .filter(|s| !is_grease(**s))
        .map(|s| format!("{s:04x}"))
        .collect();
    let c = sha12(&format!("{}_{}", es.join(","), sig.join(",")));
    format!("{a}_{b}_{c}")
}

mod hex {
    pub fn encode(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
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
