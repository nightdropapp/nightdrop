//! Local-network (Wi-Fi / LAN) transport (`Transport` impl) — a Briar-style **offline path** for
//! when the internet or Tor is blocked but two paired devices share a network (same room, same
//! Wi-Fi, a field hotspot, an air-gapped router). Frames move directly over `std::net` TCP; no
//! internet, no relay, no Tor.
//!
//! ## How it fits the existing design
//! It is an ordinary [`Transport`], so a core built with [`crate::api::NightdropCore::new_with_transport`]
//! can use it unchanged. A peer's address here is a `host:port` on the local network, learned the
//! same way an `.onion` is — from the `Hello` frame at pairing — and kept fresh by the existing
//! in-band address-rotation signal (`Frame::Address`, §5c) as DHCP hands out new IPs. Pairing by QR
//! in the same room (the canonical Night Drop moment) already puts both devices on the LAN, so this
//! needs no new discovery step to be useful.
//!
//! ## Why there is no auto-discovery beacon (deliberate)
//! A naive mDNS/UDP-broadcast "who's here" beacon would have to advertise *something linkable* on
//! the local network — broadcasting a raw long-term identity key would leak presence + a stable
//! identity to anyone sniffing the LAN, violating the anonymity invariant (`CLAUDE.md`). Doing
//! discovery *right* means broadcasting a **rotating token only a paired contact can recognize**
//! (derived from the pairing secret, Briar-style). That is worthwhile follow-up work; until then we
//! rely on pairing + address rotation rather than ship a metadata-leaking shortcut. See
//! `ARCHITECTURE.md` §6.
//!
//! ## Anonymity note
//! LAN traffic is **not anonymized** — a local observer sees two IPs talking. What they do *not*
//! see is content: every frame is still end-to-end encrypted and (v2) fixed-size padded before it
//! reaches this layer. Use this when Tor is unavailable and the local network is already trusted
//! enough for the participants to be in the same place; it is not a replacement for Tor's anonymity.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use crate::transport::{Address, Transport};
use crate::Result;

/// How long to wait for a peer to send its (one or more) length-prefixed frames on an accepted
/// connection before giving up, so a half-open connection can't pin a reader thread forever.
const READ_IDLE: Duration = Duration::from_secs(30);

/// A LAN-backed [`Transport`]: listens on all interfaces and advertises the machine's detected
/// local-network IP so paired peers on the same network can dial it directly.
pub struct LanTransport {
    /// The advertised `host:port` peers dial — the detected LAN IP, not `0.0.0.0`/loopback.
    address: Address,
    inbound: Mutex<Receiver<(Address, Vec<u8>)>>,
}

impl LanTransport {
    /// Bind a listener on every interface at `port` (`0` = an ephemeral port) and start accepting
    /// inbound frames. The advertised [`address`](Transport::address) uses the detected LAN IP so a
    /// peer on the same network can reach us; if no LAN IP can be detected (e.g. no configured
    /// interface) we fall back to the raw bound address, which the caller can still override.
    pub fn bind(port: u16) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))?;
        let bound = listener.local_addr()?;
        let address = match local_lan_ip() {
            Some(ip) => format!("{ip}:{}", bound.port()),
            None => bound.to_string(),
        };
        let (tx, rx) = channel();
        thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let tx = tx.clone();
                thread::spawn(move || {
                    let mut stream = conn;
                    let _ = stream.set_read_timeout(Some(READ_IDLE));
                    // A connection may carry many frames (a peer that keeps it warm); drain until
                    // it closes or idles out. The TCP source address is not the app-level reply
                    // address (peers advertise that in their Hello), so report it empty.
                    while let Ok(frame) = read_frame(&mut stream) {
                        if tx.send((Address::new(), frame)).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        Ok(Self {
            address,
            inbound: Mutex::new(rx),
        })
    }
}

impl Transport for LanTransport {
    fn address(&self) -> Address {
        self.address.clone()
    }

    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        let mut stream = TcpStream::connect(peer)?;
        write_frame(&mut stream, frame)?;
        stream.flush()?;
        Ok(())
    }

    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        self.inbound.lock().unwrap().try_recv().ok()
    }
}

/// Best-effort detection of this machine's primary LAN IPv4. Opens a UDP socket and "connects" it
/// to an off-link address: no packet is sent, but the OS binds the socket to the IP of the
/// interface it *would* route through — i.e. our LAN address. Returns `None` if there is no usable
/// non-loopback interface (fully isolated host), so the caller can fall back.
pub fn local_lan_ip() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // Any routable target works; 192.0.2.1 is TEST-NET-1 (RFC 5737) so we never actually talk to a
    // real host even if a packet somehow escaped. Fall back to a public IP if the host has a
    // default route but no LAN-scoped one.
    sock.connect((Ipv4Addr::new(192, 0, 2, 1), 9))
        .or_else(|_| sock.connect((Ipv4Addr::new(8, 8, 8, 8), 53)))
        .ok()?;
    match sock.local_addr().ok()? {
        SocketAddr::V4(a) if !a.ip().is_loopback() && !a.ip().is_unspecified() => Some(*a.ip()),
        _ => None,
    }
}

fn write_frame(w: &mut impl Write, frame: &[u8]) -> Result<()> {
    w.write_all(&(frame.len() as u32).to_be_bytes())?;
    w.write_all(frame)?;
    Ok(())
}

fn read_frame(r: &mut impl Read) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lan_endpoints_exchange_frames_over_a_real_socket() {
        let a = LanTransport::bind(0).unwrap();
        let b = LanTransport::bind(0).unwrap();

        // Dial `a` at its *bound* port on loopback (the advertised LAN IP may not be routable in a
        // sandbox); the point is the framed round-trip works end to end.
        let a_port: u16 = a.address.rsplit(':').next().unwrap().parse().unwrap();
        b.send(&format!("127.0.0.1:{a_port}"), b"hello lan")
            .unwrap();

        let mut got = None;
        for _ in 0..100 {
            if let Some((_, frame)) = a.try_recv() {
                got = Some(frame);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(got.unwrap(), b"hello lan");
    }

    #[test]
    fn two_frames_share_one_warm_connection() {
        let a = LanTransport::bind(0).unwrap();
        let a_port: u16 = a.address.rsplit(':').next().unwrap().parse().unwrap();

        // Write two frames on a single connection; the read loop must surface both.
        let mut stream = TcpStream::connect(format!("127.0.0.1:{a_port}")).unwrap();
        write_frame(&mut stream, b"one").unwrap();
        write_frame(&mut stream, b"two").unwrap();
        stream.flush().unwrap();

        let mut seen = Vec::new();
        for _ in 0..100 {
            if let Some((_, frame)) = a.try_recv() {
                seen.push(frame);
                if seen.len() == 2 {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(seen, vec![b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn advertised_address_has_a_port_and_never_panics() {
        let t = LanTransport::bind(0).unwrap();
        // Whether or not a LAN IP was detected, the address is a parseable host:port.
        assert!(t
            .address()
            .rsplit(':')
            .next()
            .unwrap()
            .parse::<u16>()
            .is_ok());
        // Detection is best-effort and must never panic; the result is just an Option.
        let _ = local_lan_ip();
    }
}
