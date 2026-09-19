//! A SOCKS5 listener that serves WebTunnel to a Tor client as an *unmanaged* pluggable
//! transport (arti: a `[[bridges.transports]]` entry with `proxy_addr`).
//!
//! The Tor client connects, puts the bridge's argument list in the SOCKS5 username/password
//! (pt-spec §3.5), and asks to CONNECT to the bridge's address. For WebTunnel that address is
//! a placeholder — the real destination is in `url=` — so it is ignored, the tunnel is opened,
//! and the two streams are spliced.
//!
//! **Access control.** On Android, a loopback port is reachable by every app on the device, and
//! the username/password fields are already spoken for. So the listener can require a secret
//! *bridge argument* ([`SECRET_ARG`]), which the core appends to each bridge line it hands arti;
//! a connection without it is refused before anything is dialled.

use crate::{connect, ClientConfig, Error, PtArgs};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Bridge argument carrying the listener's shared secret.
pub const SECRET_ARG: &str = "nightdrop-secret";

/// How long a client gets to finish the SOCKS exchange (not the tunnel; see `client`).
const SOCKS_TIMEOUT: Duration = Duration::from_secs(10);

/// Who may use the listener.
#[derive(Clone, Debug)]
pub enum Access {
    /// Anyone who can reach the port. Tests and desktop experiments only.
    Open,
    /// Only requests whose argument list carries `nightdrop-secret=<this>`.
    Secret(String),
}

pub struct SocksServer {
    listener: TcpListener,
    access: Access,
}

impl SocksServer {
    /// Bind the listener. Pass `127.0.0.1:0` to let the OS pick a free port, then read it back
    /// with [`SocksServer::local_addr`] for arti's `proxy_addr`.
    pub async fn bind(addr: SocketAddr, access: Access) -> std::io::Result<Self> {
        Ok(SocksServer {
            listener: TcpListener::bind(addr).await?,
            access,
        })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accept connections until the task is dropped. Each connection runs on its own task.
    pub async fn serve(self) {
        loop {
            let (conn, _) = match self.listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    // Typically EMFILE; back off rather than spin.
                    tracing::warn!("webtunnel SOCKS accept: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
            };
            let access = self.access.clone();
            tokio::spawn(async move {
                if let Err(e) = handle(conn, &access).await {
                    // May name the bridge host; debug only, never shipped to anyone.
                    tracing::debug!("webtunnel: {e}");
                }
            });
        }
    }
}

/// SOCKS5 reply codes (RFC 1928 §6) that Tor distinguishes.
mod reply {
    pub const SUCCEEDED: u8 = 0x00;
    pub const GENERAL_FAILURE: u8 = 0x01;
    pub const NOT_ALLOWED: u8 = 0x02;
    pub const HOST_UNREACHABLE: u8 = 0x04;
    pub const COMMAND_NOT_SUPPORTED: u8 = 0x07;
    pub const ADDRESS_NOT_SUPPORTED: u8 = 0x08;
}

async fn handle(mut conn: TcpStream, access: &Access) -> Result<(), Error> {
    let _ = conn.set_nodelay(true);
    let args = match tokio::time::timeout(SOCKS_TIMEOUT, negotiate(&mut conn)).await {
        Ok(r) => r?,
        Err(_) => return Err(Error::Socks("client stalled in the SOCKS handshake".into())),
    };

    if let Access::Secret(expected) = access {
        let ok = args
            .get(SECRET_ARG)
            .is_some_and(|got| constant_time_eq(got.as_bytes(), expected.as_bytes()));
        if !ok {
            send_reply(&mut conn, reply::NOT_ALLOWED).await;
            return Err(Error::Socks("request without the listener's secret".into()));
        }
    }

    let config = match ClientConfig::from_args(&args) {
        Ok(c) => c,
        Err(e) => {
            send_reply(&mut conn, reply::GENERAL_FAILURE).await;
            return Err(e);
        }
    };
    let mut tunnel = match connect(&config).await {
        Ok(t) => t,
        Err(e) => {
            send_reply(&mut conn, reply::HOST_UNREACHABLE).await;
            return Err(e);
        }
    };
    send_reply(&mut conn, reply::SUCCEEDED).await;

    // Either side closing ends the splice; errors here are ordinary disconnects.
    let _ = tokio::io::copy_bidirectional(&mut conn, &mut tunnel).await;
    Ok(())
}

/// Method selection, username/password sub-negotiation and the CONNECT request. Returns the
/// bridge's arguments; the CONNECT target is read and discarded.
async fn negotiate(conn: &mut TcpStream) -> Result<PtArgs, Error> {
    let io = |e| Error::io("SOCKS read", e);

    let mut head = [0u8; 2];
    conn.read_exact(&mut head).await.map_err(io)?;
    if head[0] != 5 {
        return Err(Error::Socks(format!("version {} is not SOCKS5", head[0])));
    }
    let mut methods = vec![0u8; head[1] as usize];
    conn.read_exact(&mut methods).await.map_err(io)?;

    let args = if methods.contains(&0x02) {
        conn.write_all(&[5, 0x02])
            .await
            .map_err(|e| Error::io("SOCKS write", e))?;
        // RFC 1929: VER=1, ULEN, UNAME, PLEN, PASSWD.
        let mut ver_len = [0u8; 2];
        conn.read_exact(&mut ver_len).await.map_err(io)?;
        if ver_len[0] != 1 {
            return Err(Error::Socks(format!(
                "auth sub-negotiation version {}",
                ver_len[0]
            )));
        }
        let mut user = vec![0u8; ver_len[1] as usize];
        conn.read_exact(&mut user).await.map_err(io)?;
        let mut plen = [0u8; 1];
        conn.read_exact(&mut plen).await.map_err(io)?;
        let mut pass = vec![0u8; plen[0] as usize];
        conn.read_exact(&mut pass).await.map_err(io)?;
        match PtArgs::from_socks_auth(&user, &pass) {
            Ok(a) => {
                conn.write_all(&[1, 0])
                    .await
                    .map_err(|e| Error::io("SOCKS write", e))?;
                a
            }
            Err(e) => {
                let _ = conn.write_all(&[1, 1]).await;
                return Err(e);
            }
        }
    } else if methods.contains(&0x00) {
        // No arguments at all; ClientConfig will reject the missing url= with a proper reply.
        conn.write_all(&[5, 0x00])
            .await
            .map_err(|e| Error::io("SOCKS write", e))?;
        PtArgs::default()
    } else {
        let _ = conn.write_all(&[5, 0xFF]).await;
        return Err(Error::Socks("no acceptable authentication method".into()));
    };

    // Request: VER CMD RSV ATYP DST.ADDR DST.PORT
    let mut req = [0u8; 4];
    conn.read_exact(&mut req).await.map_err(io)?;
    if req[0] != 5 {
        return Err(Error::Socks(format!("request version {}", req[0])));
    }
    let addr_len = match req[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut l = [0u8; 1];
            conn.read_exact(&mut l).await.map_err(io)?;
            l[0] as usize
        }
        other => {
            send_reply(conn, reply::ADDRESS_NOT_SUPPORTED).await;
            return Err(Error::Socks(format!("address type {other}")));
        }
    };
    let mut target = vec![0u8; addr_len + 2];
    conn.read_exact(&mut target).await.map_err(io)?;
    if req[1] != 0x01 {
        send_reply(conn, reply::COMMAND_NOT_SUPPORTED).await;
        return Err(Error::Socks(format!("command {} is not CONNECT", req[1])));
    }
    Ok(args)
}

/// A reply with an all-zero IPv4 bound address; Tor ignores BND.ADDR for transports.
async fn send_reply(conn: &mut TcpStream, code: u8) {
    let _ = conn.write_all(&[5, code, 0, 0x01, 0, 0, 0, 0, 0, 0]).await;
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
