//! A plain TCP transport (`Transport` impl) for LAN / local use and the desktop
//! two-client demo. Same contract as the Tor transport — one length-prefixed frame per
//! connection — but over `std::net` TCP (synchronous, no async runtime needed).
//!
//! This is NOT anonymized; it's for development and same-machine/LAN testing. Production
//! uses `transport::tor`. The peer address is `host:port`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use crate::transport::{Address, Transport};
use crate::Result;

/// A TCP-backed transport endpoint. Listens on an address and dials peers by `host:port`.
pub struct TcpTransport {
    address: Address,
    inbound: Mutex<Receiver<(Address, Vec<u8>)>>,
}

impl TcpTransport {
    /// Bind a listener (e.g. `127.0.0.1:7001`) and start accepting inbound frames.
    pub fn bind(addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let address = listener.local_addr()?.to_string();
        let (tx, rx) = channel();
        thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let tx = tx.clone();
                thread::spawn(move || {
                    let mut stream = conn;
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    if let Ok(frame) = read_frame(&mut stream) {
                        // The TCP peer address isn't the app-level reply address (which the
                        // peer advertises in its Hello), so we report it as empty.
                        let _ = tx.send((Address::new(), frame));
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

impl Transport for TcpTransport {
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
    fn tcp_endpoints_exchange_a_frame() {
        let a = TcpTransport::bind("127.0.0.1:0").unwrap();
        let b = TcpTransport::bind("127.0.0.1:0").unwrap();
        b.send(&a.address(), b"hello tcp").unwrap();

        // The accept happens on another thread; poll briefly.
        let mut got = None;
        for _ in 0..50 {
            if let Some((_, frame)) = a.try_recv() {
                got = Some(frame);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(got.unwrap(), b"hello tcp");
    }
}
