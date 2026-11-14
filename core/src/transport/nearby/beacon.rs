//! Finding a contact on the local network without the internet (`docs/design/nearby.md` §5a).
//!
//! While a chat has Nearby on locally, each device broadcasts a small UDP beacon on each of its
//! local networks: a random nonce, the port it listens on, and a tag over both, over the sending
//! address and over the current ten-minute window. New beacons use a per-chat discovery key derived
//! from the pair secret shared at pairing, so the contact can recognize one before an online
//! Nearby-on announcement. Legacy beacons use the per-toggle token once it has been exchanged.
//! To anyone else either format is 30 random-looking bytes that never repeat. A beacon replayed
//! from another address does not verify.
//!
//! The selected per-chat key also keys the challenge-response that opens a local connection
//! ([`proof`]); a different HKDF salt keeps the beacon tag and connection proof apart, and neither
//! key ever crosses the network.

use std::net::Ipv4Addr;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use hkdf::Hkdf;
use sha2::Sha256;

/// The UDP port every device listens on for beacons. Fixed, since that is the point: a device
/// with no other way to learn where its contact is sends to this port.
pub const PORT: u16 = 41_873;
/// Bytes in a beacon: magic, nonce, port, tag.
pub const LEN: usize = 4 + NONCE + 2 + TAG;
const MAGIC: &[u8; 4] = b"NDB1";
const DISCOVERY_MAGIC: &[u8; 4] = b"NDB2";
const NONCE: usize = 8;
const TAG: usize = 16;
/// The tag covers the window it was made in; one window either side is accepted for clock skew.
const WINDOW_SECS: u64 = 600;

const PROOF_SALT: &[u8] = b"nightdrop/nearby/proof/v1";
const BEACON_SALT: &[u8] = b"nightdrop/nearby/beacon/v1";
const DISCOVERY_BEACON_SALT: &[u8] = b"nightdrop/nearby/beacon/v2";

/// A handshake nonce as sent: 16 random bytes, base64url without padding.
pub const NONCE_CHARS: usize = 22;
/// A handshake proof as sent: 32 bytes, base64url without padding.
pub const PROOF_CHARS: usize = 43;

/// Which side of a local connection a [`proof`] is from, so one side's answer can never be
/// replayed as the other's.
#[derive(Clone, Copy)]
pub enum Role {
    Dialer,
    Listener,
}

/// A fresh handshake nonce ([`NONCE_CHARS`] characters).
pub fn fresh_nonce() -> String {
    let mut bytes = [0u8; 16];
    rand::Rng::fill(&mut rand::thread_rng(), &mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Proof of holding `token` for this one connection (§5): keyed from the token, over the role and
/// both sides' nonces. A fresh listener nonce each time, so a recorded proof is worth nothing.
pub fn proof(token: &str, role: Role, dialer_nonce: &[u8], listener_nonce: &[u8]) -> String {
    let mut info = Vec::with_capacity(1 + dialer_nonce.len() + listener_nonce.len());
    info.push(match role {
        Role::Dialer => b'D',
        Role::Listener => b'L',
    });
    info.extend_from_slice(dialer_nonce);
    info.extend_from_slice(listener_nonce);
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(Some(PROOF_SALT), token.as_bytes())
        .expand(&info, &mut out)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out)
}

fn tag(token: &str, salt: &[u8], window: u64, ip: Ipv4Addr, port: u16, nonce: &[u8]) -> [u8; TAG] {
    let mut info = Vec::with_capacity(8 + 4 + 2 + NONCE);
    info.extend_from_slice(&window.to_be_bytes());
    info.extend_from_slice(&ip.octets());
    info.extend_from_slice(&port.to_be_bytes());
    info.extend_from_slice(nonce);
    let mut out = [0u8; TAG];
    Hkdf::<Sha256>::new(Some(salt), token.as_bytes())
        .expand(&info, &mut out)
        .expect("16 bytes is a valid HKDF-SHA256 output length");
    out
}

/// The current window.
pub fn window_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / WINDOW_SECS)
}

/// A beacon saying: the holder of `token` listens at `ip:port`.
pub fn make(token: &str, ip: Ipv4Addr, port: u16, window: u64) -> [u8; LEN] {
    make_with(MAGIC, BEACON_SALT, token, ip, port, window)
}

/// A beacon keyed by the pair's stable Nearby discovery key, usable before the peer's Nearby-on
/// announcement arrives over Tor.
pub fn make_discovery(key: &str, ip: Ipv4Addr, port: u16, window: u64) -> [u8; LEN] {
    make_with(
        DISCOVERY_MAGIC,
        DISCOVERY_BEACON_SALT,
        key,
        ip,
        port,
        window,
    )
}

fn make_with(
    magic: &[u8; 4],
    salt: &[u8],
    token: &str,
    ip: Ipv4Addr,
    port: u16,
    window: u64,
) -> [u8; LEN] {
    let mut nonce = [0u8; NONCE];
    rand::Rng::fill(&mut rand::thread_rng(), &mut nonce);
    let mut out = [0u8; LEN];
    out[..4].copy_from_slice(magic);
    out[4..4 + NONCE].copy_from_slice(&nonce);
    out[4 + NONCE..6 + NONCE].copy_from_slice(&port.to_be_bytes());
    out[6 + NONCE..].copy_from_slice(&tag(token, salt, window, ip, port, &nonce));
    out
}

/// The port a beacon names, if it is from the holder of `token` and was sent from `from`.
pub fn verify(packet: &[u8], from: Ipv4Addr, token: &str, window: u64) -> Option<u16> {
    verify_with(packet, MAGIC, BEACON_SALT, from, token, window)
}

/// Verify a beacon keyed by the stable pair discovery key.
pub fn verify_discovery(packet: &[u8], from: Ipv4Addr, key: &str, window: u64) -> Option<u16> {
    verify_with(
        packet,
        DISCOVERY_MAGIC,
        DISCOVERY_BEACON_SALT,
        from,
        key,
        window,
    )
}

fn verify_with(
    packet: &[u8],
    magic: &[u8; 4],
    salt: &[u8],
    from: Ipv4Addr,
    token: &str,
    window: u64,
) -> Option<u16> {
    if packet.len() != LEN || &packet[..4] != magic {
        return None;
    }
    let nonce = &packet[4..4 + NONCE];
    let port = u16::from_be_bytes([packet[4 + NONCE], packet[5 + NONCE]]);
    let got = &packet[6 + NONCE..];
    // Every window is checked, so the time taken does not say which one came close.
    let mut ok = false;
    for w in [window.saturating_sub(1), window, window + 1] {
        let want = tag(token, salt, w, from, port, nonce);
        ok |= want.iter().zip(got).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0;
    }
    ok.then_some(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(c: char) -> String {
        std::iter::repeat_n(c, super::super::TOKEN_CHARS).collect()
    }

    #[test]
    fn a_beacon_verifies_only_for_its_token_address_and_time() {
        let ip = Ipv4Addr::new(10, 226, 120, 90);
        let b = make(&token('a'), ip, 40_001, 1000);
        assert_eq!(verify(&b, ip, &token('a'), 1000), Some(40_001));
        assert_eq!(
            verify(&b, ip, &token('a'), 1001),
            Some(40_001),
            "clock skew"
        );
        assert_eq!(verify(&b, ip, &token('a'), 999), Some(40_001), "clock skew");
        assert_eq!(
            verify(&b, ip, &token('b'), 1000),
            None,
            "another chat's token"
        );
        assert_eq!(
            verify(&b, Ipv4Addr::new(10, 226, 120, 91), &token('a'), 1000),
            None,
            "replayed from another address"
        );
        assert_eq!(verify(&b, ip, &token('a'), 1002), None, "too old");
        let mut moved = b;
        moved[4 + NONCE..6 + NONCE].copy_from_slice(&40_002u16.to_be_bytes());
        assert_eq!(verify(&moved, ip, &token('a'), 1000), None, "port changed");
        assert_eq!(verify(&b[..LEN - 1], ip, &token('a'), 1000), None);
    }

    #[test]
    fn beacons_never_repeat_and_do_not_show_the_token() {
        let ip = Ipv4Addr::new(192, 168, 1, 5);
        let (x, y) = (
            make(&token('a'), ip, 40_001, 7),
            make(&token('a'), ip, 40_001, 7),
        );
        assert_ne!(x, y, "a fresh nonce each time");
        assert!(!x
            .windows(8)
            .any(|w| token('a').as_bytes().windows(8).any(|t| t == w)));
    }

    #[test]
    fn a_proof_is_bound_to_its_token_role_and_both_nonces() {
        let (d, l) = (fresh_nonce(), fresh_nonce());
        assert_eq!(d.len(), NONCE_CHARS);
        assert_ne!(d, l);
        let p = |t: char, r, d: &str, l: &str| proof(&token(t), r, d.as_bytes(), l.as_bytes());
        let mine = p('a', Role::Dialer, &d, &l);
        assert_eq!(mine.len(), PROOF_CHARS);
        assert_eq!(mine, p('a', Role::Dialer, &d, &l));
        assert_ne!(mine, p('b', Role::Dialer, &d, &l), "another token");
        assert_ne!(mine, p('a', Role::Listener, &d, &l), "the other side's");
        assert_ne!(
            mine,
            p('a', Role::Dialer, &d, &fresh_nonce()),
            "another connection"
        );
        assert_ne!(mine, p('a', Role::Dialer, &fresh_nonce(), &l));
    }
}
