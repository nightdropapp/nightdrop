//! meek client: a byte stream carried in HTTP POSTs to a domain-fronted reflector.
//!
//! Each round trip POSTs whatever bytes are waiting to go up (possibly none) and returns what the
//! reflector sent back down (possibly none). `X-Session-Id` ties the requests into one stream.
//! The reflector forwards the stream to its fixed destination — for moat, `bridges.torproject.org`
//! — and answers **570** once that connection has closed, which is the end of the stream, not an
//! error. Protocol as in lyrebird's `transports/meeklite`.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

use crate::http;
use crate::targets::Target;

/// What one round trip produced.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    /// Bytes from the far end (empty on an idle poll).
    Data(Vec<u8>),
    /// The reflector's connection to the destination has closed: no more data will come.
    Closed,
}

/// One meek round trip. A trait so the tunnel above it can be tested without a network.
pub trait RoundTrip {
    fn round_trip(&mut self, up: &[u8]) -> Result<Reply>;
}

/// The status meek's server uses for "the destination connection is gone".
const STATUS_CLOSED: u16 = 570;
/// meek caps a request body at 64 KiB; so do we.
pub const MAX_PAYLOAD: usize = 0x10000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// Attempts per round trip, rotating through the fronts, before giving up.
const MAX_ATTEMPTS: usize = 4;

type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// How to open the TCP connection to a front. Real use resolves `front:443`; tests point it at a
/// local server.
pub type Dialer = Box<dyn Fn(&str) -> std::io::Result<TcpStream> + Send>;

/// meek over real, domain-fronted HTTPS.
pub struct FrontedMeek {
    targets: Vec<Target>,
    current: usize,
    session_id: String,
    tls: Arc<rustls::ClientConfig>,
    dial: Dialer,
    conn: Option<TlsStream>,
    /// Which front carried the last successful round trip (for the caller's diagnostics).
    pub last_front: Option<String>,
}

impl FrontedMeek {
    /// `tls` verifies the **front's** certificate (Mozilla roots in production).
    pub fn new(targets: Vec<Target>, tls: Arc<rustls::ClientConfig>, dial: Dialer) -> Result<Self> {
        if targets.is_empty() {
            bail!("no meek targets");
        }
        Ok(Self {
            targets,
            current: 0,
            session_id: session_id()?,
            tls,
            dial,
            conn: None,
            last_front: None,
        })
    }

    /// Dial `front:443` with a timeout, the way production does.
    pub fn default_dialer() -> Dialer {
        Box::new(|front: &str| {
            let addrs: Vec<_> = (front, 443).to_socket_addrs()?.collect();
            let mut last = std::io::Error::other("front resolved to no address");
            for a in addrs {
                match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
                    Ok(s) => return Ok(s),
                    Err(e) => last = e,
                }
            }
            Err(last)
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn connect(&mut self) -> Result<&mut TlsStream> {
        if self.conn.is_none() {
            let t = &self.targets[self.current];
            let tcp = (self.dial)(&t.front)
                .with_context(|| format!("connecting to front {}", t.front))?;
            tcp.set_read_timeout(Some(IO_TIMEOUT))?;
            tcp.set_write_timeout(Some(IO_TIMEOUT))?;
            let name = rustls::pki_types::ServerName::try_from(t.front.clone())
                .map_err(|_| anyhow!("front {} is not a valid TLS name", t.front))?;
            let tls = rustls::ClientConnection::new(self.tls.clone(), name)?;
            self.conn = Some(rustls::StreamOwned::new(tls, tcp));
        }
        Ok(self.conn.as_mut().expect("just set"))
    }

    fn try_once(&mut self, up: &[u8]) -> Result<Reply> {
        let t = self.targets[self.current].clone();
        let session = self.session_id.clone();
        let conn = self.connect()?;
        // The reflector is named only here, inside the encrypted connection; the network sees the
        // front. Empty User-Agent, as lyrebird sends.
        let head = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nX-Session-Id: {}\r\nUser-Agent: \r\nContent-Length: {}\r\n\r\n",
            t.path,
            t.host,
            session,
            up.len()
        );
        conn.write_all(head.as_bytes())?;
        conn.write_all(up)?;
        conn.flush()?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        let resp = loop {
            if let Some((resp, _)) = http::parse(&buf)? {
                break resp;
            }
            let n = conn.read(&mut chunk)?;
            if n == 0 {
                bail!("front {} closed the connection mid-response", t.front);
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        match resp.status {
            200 if resp.body.len() <= MAX_PAYLOAD => Ok(Reply::Data(resp.body)),
            200 => bail!(
                "meek reply of {} bytes is over the 64 KiB cap",
                resp.body.len()
            ),
            STATUS_CLOSED => Ok(Reply::Closed),
            s => bail!("front {} answered HTTP {s}", t.front),
        }
    }
}

impl RoundTrip for FrontedMeek {
    fn round_trip(&mut self, up: &[u8]) -> Result<Reply> {
        if up.len() > MAX_PAYLOAD {
            bail!("meek payload of {} bytes is over the 64 KiB cap", up.len());
        }
        let mut last = anyhow!("no attempt made");
        for _ in 0..MAX_ATTEMPTS {
            match self.try_once(up) {
                Ok(r) => {
                    self.last_front = Some(self.targets[self.current].front.clone());
                    return Ok(r);
                }
                Err(e) => {
                    // A failed front may be blocked or retired: drop the connection and move on.
                    // The same session id continues on the next front — the reflector is shared.
                    self.conn = None;
                    self.current = (self.current + 1) % self.targets.len();
                    last = e;
                }
            }
        }
        Err(last.context("every meek front failed"))
    }
}

/// 32 hex characters of fresh randomness, as lyrebird's meek client sends.
fn session_id() -> Result<String> {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| anyhow!("no system randomness"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// TLS settings for the outer (front) connection: Mozilla roots, HTTP/1.1 only.
pub fn front_tls_config() -> Arc<rustls::ClientConfig> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring supports the default TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(cfg)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A TLS server certificate for `names`, and a client config that trusts only it.
    pub(crate) fn test_pki(
        names: &[&str],
    ) -> (Arc<rustls::ServerConfig>, Arc<rustls::ClientConfig>) {
        let ck = rcgen::generate_simple_self_signed(
            names.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .unwrap();
        let cert = ck.cert.der().clone();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(ck.key_pair.serialize_der().into());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let server = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        let client = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        (Arc::new(server), Arc::new(client))
    }

    /// What the fake front saw on one request.
    #[derive(Debug)]
    pub(crate) struct Seen {
        pub sni: Option<String>,
        pub head: String,
        pub body: Vec<u8>,
        pub conn_no: usize,
    }

    /// A fake domain front: TLS server answering each request with the next scripted
    /// (status, body). Reports every request on the channel.
    fn fake_front(
        script: Vec<(u16, Vec<u8>)>,
    ) -> (u16, mpsc::Receiver<Seen>, Arc<rustls::ClientConfig>) {
        let (server_cfg, client_cfg) = test_pki(&["front.example", "other.example"]);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut script = script.into_iter();
            for (conn_no, tcp) in listener.incoming().enumerate() {
                let Ok(tcp) = tcp else { return };
                let conn = rustls::ServerConnection::new(server_cfg.clone()).unwrap();
                let mut s = rustls::StreamOwned::new(conn, tcp);
                loop {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let req = loop {
                        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..end]).to_string();
                            let len: usize = head
                                .lines()
                                .find_map(|l| l.strip_prefix("Content-Length: "))
                                .map(|v| v.trim().parse().unwrap())
                                .unwrap_or(0);
                            if buf.len() >= end + 4 + len {
                                break Some((head, buf[end + 4..end + 4 + len].to_vec()));
                            }
                        }
                        match s.read(&mut chunk) {
                            Ok(0) | Err(_) => break None,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    // A failed or closed connection ends that connection, never the server: the
                    // client is expected to retry on a new one.
                    let Some((head, body)) = req else { break };
                    let sni = s.conn.server_name().map(str::to_string);
                    tx.send(Seen {
                        sni,
                        head,
                        body,
                        conn_no,
                    })
                    .unwrap();
                    let Some((status, reply)) = script.next() else {
                        return;
                    };
                    let _ = write!(
                        s,
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n\r\n",
                        reply.len()
                    );
                    let _ = s.write_all(&reply);
                    let _ = s.flush();
                }
            }
        });
        (port, rx, client_cfg)
    }

    fn meek_to(
        port: u16,
        client_cfg: Arc<rustls::ClientConfig>,
        targets: &str,
        refuse: &'static str,
    ) -> FrontedMeek {
        // Every front dials the local fake, except `refuse`, which fails like a blocked front.
        let dial: Dialer = Box::new(move |front: &str| {
            if front == refuse {
                return Err(std::io::Error::other("blocked"));
            }
            TcpStream::connect(("127.0.0.1", port))
        });
        // The fake's certificate is ours, so `client_cfg` trusts it instead of the Mozilla roots.
        let t = crate::targets::parse(targets).unwrap();
        FrontedMeek::new(t, client_cfg, dial).unwrap()
    }

    #[test]
    fn a_round_trip_names_the_front_in_tls_and_the_reflector_only_inside() {
        let (port, seen, pki) =
            fake_front(vec![(200, b"down".to_vec()), (200, vec![]), (570, vec![])]);
        let mut m = meek_to(
            port,
            pki,
            "https://reflector.example/meek|front.example",
            "",
        );
        assert_eq!(m.round_trip(b"up").unwrap(), Reply::Data(b"down".to_vec()));
        let s = seen.recv().unwrap();
        // What an observer of the connection could see is the front, and only the front.
        assert_eq!(s.sni.as_deref(), Some("front.example"));
        assert!(s.head.starts_with("POST /meek HTTP/1.1\r\n"), "{}", s.head);
        assert!(s.head.contains("\r\nHost: reflector.example\r\n"));
        let sid = s
            .head
            .lines()
            .find_map(|l| l.strip_prefix("X-Session-Id: "))
            .unwrap();
        assert_eq!(sid.len(), 32);
        assert!(sid.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(s.body, b"up");

        // An idle poll keeps the connection (keep-alive) and the session id.
        assert_eq!(m.round_trip(b"").unwrap(), Reply::Data(vec![]));
        let s2 = seen.recv().unwrap();
        assert_eq!(s2.conn_no, s.conn_no, "same TLS connection");
        assert!(s2.head.contains(sid));
        assert!(s2.body.is_empty());

        // 570 is the end of the tunnelled stream, not an error.
        assert_eq!(m.round_trip(b"").unwrap(), Reply::Closed);
    }

    #[test]
    fn a_blocked_front_rotates_to_the_next() {
        let (port, seen, pki) = fake_front(vec![(200, b"ok".to_vec())]);
        let mut m = meek_to(
            port,
            pki,
            "https://reflector.example|blocked.example+other.example",
            "blocked.example",
        );
        assert_eq!(m.round_trip(b"x").unwrap(), Reply::Data(b"ok".to_vec()));
        assert_eq!(seen.recv().unwrap().sni.as_deref(), Some("other.example"));
        assert_eq!(m.last_front.as_deref(), Some("other.example"));
    }

    #[test]
    fn every_front_failing_is_an_error_not_a_hang() {
        let (_, pki) = test_pki(&["front.example"]);
        let mut m = meek_to(
            1,
            pki,
            "https://reflector.example|blocked.example",
            "blocked.example",
        );
        let e = m.round_trip(b"x").unwrap_err();
        assert!(
            format!("{e:#}").contains("every meek front failed"),
            "{e:#}"
        );
    }
}
