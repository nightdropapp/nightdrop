//! JA4 (FoxIO) TLS ClientHello fingerprints, and the Chrome reference they are checked against.
//!
//! JA4 is what a censor's tooling logs: GREASE is filtered and ciphers and extensions are sorted
//! before hashing, so it is stable across Chrome's per-connection randomisation. Used by this
//! crate's `tests/fingerprint.rs` and by the `moat` crate's, so both check against one reference.

/// JA4 of Chromium 152 opening a WebSocket, captured 2026-09-19 (the 15-extension profile;
/// Chrome intermittently also sends the `trust_anchors` draft extension → a 16-ext variant).
/// GREASE is excluded from JA4 by construction, so this is stable per Chrome build. Refresh it,
/// and the profile in `tls.rs`, when the fingerprint tests fail against a current Chrome.
pub const CHROME_WEBSOCKET_JA4: &str = "t13d1515h1_8daaf6152771_f04195365787";

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([b[i], b[i + 1]])
}

pub fn is_grease(v: u16) -> bool {
    v & 0x0f0f == 0x0a0a && v >> 8 == v & 0xff
}

/// The parts of a ClientHello that JA4 and the structural checks read.
pub struct Hello {
    pub ciphers: Vec<u16>,
    pub ext_types: Vec<u16>,
    pub alpn: Vec<String>,
    pub sigalgs: Vec<u16>,
    pub groups: Vec<u16>,
    pub sni: bool,
}

/// Parse a ClientHello handshake message (the body of the first TLS record).
pub fn parse(msg: &[u8]) -> Hello {
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
pub fn ja4(h: &Hello) -> String {
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
