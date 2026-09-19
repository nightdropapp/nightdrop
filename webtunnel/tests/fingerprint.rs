//! Does our TLS ClientHello look like Chrome opening a WebSocket?
//!
//! WebTunnel hides Tor inside what should look like a browser's `wss://` connection, so the
//! reference is a real one: Chromium's ClientHello for `new WebSocket("wss://…")`. Chrome offers
//! only `http/1.1` there, which is exactly what WebTunnel needs — a page load offers `h2`, and a
//! bridge's nginx would take it, breaking the HTTP Upgrade.
//!
//! This is the drift check for step 2 (docs/design/android-bridges.md §5): it builds the same
//! BoringSSL ClientHello the client will use, captures it on a local listener, computes its JA4,
//! and asserts it still equals the Chrome JA4 we validated by capture. When BoringSSL or the
//! profile drifts, this fails and we re-validate against a current Chrome.
//!
//! Gated behind `chrome-proto` so the ordinary build and CI never compile BoringSSL:
//!   cargo test -p webtunnel-client --features chrome-proto --test fingerprint
#![cfg(feature = "chrome-proto")]

use boring::ssl::{
    CertificateCompressionAlgorithm, CertificateCompressor, SslConnector, SslMethod, SslVersion,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

/// JA4 of Chromium 152 opening a WebSocket, captured 2026-09-19 (the 15-extension profile;
/// Chrome intermittently also sends the `trust_anchors` draft extension → a 16-ext variant).
/// GREASE is excluded from JA4 by construction, so this is stable per Chrome build. Refresh it,
/// and the profile below, when this test fails against a current Chrome.
const CHROME_JA4: &str = "t13d1515h1_8daaf6152771_f04195365787";

// -------------------------------------------------------------- the profile under test

/// Chrome's TLS 1.2 cipher list; BoringSSL prepends the three TLS 1.3 suites.
const CIPHERS: &str = "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:\
ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:\
ECDHE-RSA-CHACHA20-POLY1305:ECDHE-RSA-AES128-SHA:ECDHE-RSA-AES256-SHA:AES128-GCM-SHA256:\
AES256-GCM-SHA384:AES128-SHA:AES256-SHA";

struct Brotli;
impl CertificateCompressor for Brotli {
    const ALGORITHM: CertificateCompressionAlgorithm = CertificateCompressionAlgorithm::BROTLI;
    const CAN_COMPRESS: bool = false;
    const CAN_DECOMPRESS: bool = true;
    fn decompress<W: Write>(&self, input: &[u8], output: &mut W) -> std::io::Result<()> {
        std::io::copy(&mut brotli::Decompressor::new(input, 4096), output).map(|_| ())
    }
}

/// Send one ClientHello with the Chrome-matching profile to `addr`. This is the exact
/// configuration the client's TLS layer will use; keep the two in sync.
fn send_chrome_hello(addr: &str) {
    let mut b = SslConnector::builder(SslMethod::tls()).unwrap();
    b.set_min_proto_version(Some(SslVersion::TLS1_2)).unwrap();
    b.set_max_proto_version(Some(SslVersion::TLS1_3)).unwrap();
    b.set_grease_enabled(true);
    b.set_permute_extensions(true);
    b.set_cipher_list(CIPHERS).unwrap();
    b.set_curves_list("X25519MLKEM768:X25519:P-256:P-384")
        .unwrap();
    b.set_alpn_protos(b"\x08http/1.1").unwrap();
    b.enable_ocsp_stapling();
    b.enable_signed_cert_timestamps();
    b.add_certificate_compression_algorithm(Brotli).unwrap();
    let mut cc = b.build().configure().unwrap();
    cc.set_use_server_name_indication(true);
    cc.set_verify_hostname(false);
    cc.set_enable_ech_grease(true);
    let sock = TcpStream::connect(addr).unwrap();
    let _ = cc.connect("ws.test", sock); // fails at the closed listener; the hello is already out
}

// -------------------------------------------------------------- capture + parse

/// Bind a listener, run `send_chrome_hello` against it, and return the first TLS record body
/// (the ClientHello handshake message).
fn capture_hello() -> Vec<u8> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
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
    send_chrome_hello(&addr);
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

// tiny local hex, to avoid a dependency
mod hex {
    pub fn encode(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}

// -------------------------------------------------------------- the checks

#[test]
fn matches_chrome_websocket() {
    let h = parse(&capture_hello());
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
    let got = ja4(&h);
    assert_eq!(
        got, CHROME_JA4,
        "JA4 drifted from the validated Chrome profile"
    );
}

#[test]
fn ja4_is_stable_across_connections() {
    // GREASE randomises per connection; JA4 must not.
    assert_eq!(ja4(&parse(&capture_hello())), ja4(&parse(&capture_hello())));
}
