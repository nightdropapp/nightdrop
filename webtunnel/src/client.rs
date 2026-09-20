//! Dialling one WebTunnel bridge: TCP, TLS, the HTTP/1.1 Upgrade, then a raw byte stream.

use crate::config::{ClientConfig, Remote};
use crate::{tls, Error};
use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{LazyLock, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;

/// Upper bound on TCP + TLS + upgrade. Tor has its own, longer, channel timeouts above this;
/// this only stops a half-open bridge from pinning a connection forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// A 101 response is a status line and three short headers; anything near this is not one.
const MAX_RESPONSE_HEAD: usize = 16 * 1024;

/// The tunnelled byte stream: after the 101, this carries Tor's own TLS link, unframed.
pub struct WebTunnelStream {
    inner: Inner,
}

enum Inner {
    Plain(TcpStream),
    Tls(Box<tls::TlsStream>),
}

/// Open a tunnel to the bridge described by `config`.
pub async fn connect(config: &ClientConfig) -> Result<WebTunnelStream, Error> {
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, connect_inner(config)).await {
        Ok(result) => result,
        Err(_) => Err(Error::io(
            "WebTunnel handshake",
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("no tunnel after {HANDSHAKE_TIMEOUT:?}"),
            ),
        )),
    }
}

async fn connect_inner(config: &ClientConfig) -> Result<WebTunnelStream, Error> {
    let tcp = dial_tcp(&config.remote).await?;
    let _ = tcp.set_nodelay(true);

    let (mut stream, sni) = match &config.tls {
        None => (
            WebTunnelStream {
                inner: Inner::Plain(tcp),
            },
            None,
        ),
        Some(tls_cfg) => {
            let sni = SNI_PICKER.current(&tls_cfg.server_names);
            let tls = tls::connect_tls(tcp, &sni, tls_cfg).await?;
            (
                WebTunnelStream {
                    inner: Inner::Tls(Box::new(tls)),
                },
                Some((tls_cfg.server_names.clone(), sni)),
            )
        }
    };

    match upgrade(&mut stream, &config.http_host, &config.path).await {
        Ok(()) => Ok(stream),
        Err(e) => {
            // lyrebird moves to the next SNI candidate only when the upgrade fails, so a
            // servername that reaches the server but not the bridge gets rotated away from.
            if let Some((names, used)) = sni {
                SNI_PICKER.advance_if_current(&names, &used);
            }
            Err(e)
        }
    }
}

async fn dial_tcp(remote: &Remote) -> Result<TcpStream, Error> {
    match remote {
        Remote::Explicit(addr) => TcpStream::connect(addr.as_str())
            .await
            .map_err(|e| Error::io(format!("connect {addr}"), e)),
        Remote::Resolve { host, port } => {
            let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), *port))
                .await
                .map_err(|e| Error::io(format!("resolve {host}"), e))?
                .filter(|a| is_public(a.ip()))
                .collect();
            if resolved.is_empty() {
                return Err(Error::Config(format!("{host} has no public address")));
            }
            let mut last = None;
            for addr in resolved {
                match TcpStream::connect(addr).await {
                    Ok(s) => return Ok(s),
                    Err(e) => last = Some(e),
                }
            }
            Err(Error::io(
                format!("connect {host}:{port}"),
                last.expect("at least one address was tried"),
            ))
        }
    }
}

/// The addresses Go's WebTunnel client refuses to dial for a resolved hostname: loopback,
/// unspecified, multicast, link-local and private ranges.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_link_local()
                || v4.is_private())
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unicast_link_local()
                || v6.is_unique_local())
        }
    }
}

/// The request, byte for byte as Go's `http.Request.Write` produces it for the reference
/// client: `Host`, then `User-Agent`, then the remaining headers sorted by name. Matching it
/// exactly means a bridge operator's logs can't tell this client from lyrebird's.
fn upgrade_request(host: &str, path: &str) -> String {
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    format!(
        "GET /{path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Go-http-client/1.1\r\n\
         Connection: upgrade\r\nUpgrade: websocket\r\n\r\n"
    )
}

async fn upgrade(stream: &mut WebTunnelStream, host: &str, path: &str) -> Result<(), Error> {
    stream
        .write_all(upgrade_request(host, path).as_bytes())
        .await
        .map_err(|e| Error::io("send upgrade request", e))?;
    stream
        .flush()
        .await
        .map_err(|e| Error::io("send upgrade request", e))?;

    // Read the response head one byte at a time. The bridge may start sending Tor bytes right
    // behind the blank line, and those belong to the caller; reading in bulk would swallow
    // them (the Go client's own `bufio` has exactly this bug, per its TODO).
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_RESPONSE_HEAD {
            return Err(Error::Upgrade("response head too large".into()));
        }
        let n = stream
            .read(&mut byte)
            .await
            .map_err(|e| Error::io("read upgrade response", e))?;
        if n == 0 {
            return Err(Error::Upgrade(format!(
                "connection closed after {} bytes of response",
                head.len()
            )));
        }
        head.push(byte[0]);
    }
    check_response(&head)
}

fn check_response(head: &[u8]) -> Result<(), Error> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut resp = httparse::Response::new(&mut headers);
    match resp.parse(head) {
        Ok(httparse::Status::Complete(_)) => {}
        Ok(httparse::Status::Partial) => return Err(Error::Upgrade("incomplete response".into())),
        Err(e) => return Err(Error::Upgrade(format!("unparseable response: {e}"))),
    }
    if resp.code != Some(101) {
        return Err(Error::Upgrade(format!(
            "status {} {}",
            resp.code.unwrap_or(0),
            resp.reason.unwrap_or("")
        )));
    }
    // Go compares the first value of each header, lowercased, for equality.
    let first = |name: &str| {
        resp.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| String::from_utf8_lossy(h.value).trim().to_ascii_lowercase())
    };
    if first("Upgrade").as_deref() != Some("websocket")
        || first("Connection").as_deref() != Some("upgrade")
    {
        return Err(Error::Upgrade(
            "101 without Upgrade: websocket / Connection: upgrade".into(),
        ));
    }
    Ok(())
}

/// lyrebird's SNI rotation: per server-name list, a random starting candidate, kept until an
/// upgrade through it fails.
struct SniPicker {
    positions: Mutex<HashMap<Vec<String>, usize>>,
}

static SNI_PICKER: LazyLock<SniPicker> = LazyLock::new(|| SniPicker {
    positions: Mutex::new(HashMap::new()),
});

impl SniPicker {
    fn current(&self, names: &[String]) -> String {
        let mut positions = self.positions.lock().unwrap_or_else(|p| p.into_inner());
        let i = *positions
            .entry(names.to_vec())
            .or_insert_with(|| (RandomState::new().hash_one(names) as usize) % names.len());
        names[i].clone()
    }

    fn advance_if_current(&self, names: &[String], used: &str) {
        let mut positions = self.positions.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(i) = positions.get_mut(names) {
            if names[*i] == used {
                *i = (*i + 1) % names.len();
            }
        }
    }
}

impl AsyncRead for WebTunnelStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut self.inner {
            Inner::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Inner::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for WebTunnelStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.inner {
            Inner::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Inner::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.inner {
            Inner::Plain(s) => Pin::new(s).poll_flush(cx),
            Inner::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.inner {
            Inner::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Inner::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_matches_go_byte_for_byte() {
        // Captured from Go's httpupgrade.Transport.Client (webtunnel 11334a2) writing to a
        // buffer, for Host "cdn.example.org" and path "x9ab".
        assert_eq!(upgrade_request("cdn.example.org", "x9ab"), GO_REQUEST);
    }

    const GO_REQUEST: &str =
        "GET /x9ab HTTP/1.1\r\nHost: cdn.example.org\r\nUser-Agent: Go-http-client/1.1\r\nConnection: upgrade\r\nUpgrade: websocket\r\n\r\n";

    #[test]
    fn accepts_a_real_101() {
        let ok = b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: websocket\r\n\r\n";
        check_response(ok).unwrap();
        // nginx capitalises and adds its own headers; still fine.
        let nginx = b"HTTP/1.1 101 Switching Protocols\r\nServer: nginx\r\nDate: x\r\nConnection: Upgrade\r\nUpgrade: WebSocket\r\n\r\n";
        check_response(nginx).unwrap();
    }

    #[test]
    fn refuses_anything_else() {
        assert!(check_response(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").is_err());
        assert!(check_response(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: h2c\r\nConnection: upgrade\r\n\r\n"
        )
        .is_err());
        assert!(
            check_response(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n")
                .is_err()
        );
        assert!(check_response(b"garbage\r\n\r\n").is_err());
    }

    #[test]
    fn private_addresses_are_not_public() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.88.1",
            "172.16.0.1",
            "169.254.1.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
            "224.0.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["69.72.55.130", "2001:4860:4860::8888", "::ffff:8.8.8.8"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn sni_picker_rotates_only_on_the_failing_name() {
        let names = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let first = SNI_PICKER.current(&names);
        SNI_PICKER.advance_if_current(&names, "not-current");
        assert_eq!(
            SNI_PICKER.current(&names),
            first,
            "a stale failure must not rotate"
        );
        SNI_PICKER.advance_if_current(&names, &first);
        assert_ne!(SNI_PICKER.current(&names), first);
    }
}
