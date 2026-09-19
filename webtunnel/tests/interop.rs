//! End-to-end tests against the Tor Project's own WebTunnel server.
//!
//! Topology, mirroring a real bridge:
//!
//!   client ──TLS──▶ front (stands in for the bridge's nginx: terminates TLS,
//!                   404s any path but the secret one)
//!                   ──▶ official Go webtunnel server ──▶ "ORPort" (test echo)
//!
//! Tests marked `#[ignore]` need the Go server binary in `WEBTUNNEL_SERVER_BIN`; run them with
//! `webtunnel/interop.sh`, which builds it from a pinned commit. The rest need nothing.

use nightdrop_webtunnel::socks::{Access, SocksServer, SECRET_ARG};
use nightdrop_webtunnel::{chain_hash, connect, ClientConfig, Error, PtArgs};
use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};

const HOST: &str = "bridge.test";
const PATH: &str = "s3cr3t-path";
/// What the fake ORPort sends the instant a tunnel reaches it, before the client says anything.
/// Tor's own link handshake is client-first, but nothing in WebTunnel promises that, and a
/// client that over-reads the 101 would lose these bytes.
const GREETING: &[u8] = b"ORPORT-HELLO\n";

// ---------------------------------------------------------------- fixtures

/// Accepts connections, sends GREETING, then echoes everything back.
async fn orport() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                s.write_all(GREETING).await.unwrap();
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}

struct GoServer {
    addr: SocketAddr,
    _child: Child,
}

async fn go_server(orport: SocketAddr) -> GoServer {
    let bin = std::env::var("WEBTUNNEL_SERVER_BIN")
        .expect("WEBTUNNEL_SERVER_BIN not set — run webtunnel/interop.sh");
    let port = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let state = std::env::temp_dir().join(format!("wt-state-{port}"));
    let mut child = Command::new(bin)
        .env("TOR_PT_MANAGED_TRANSPORT_VER", "1")
        .env("TOR_PT_SERVER_TRANSPORTS", "webtunnel")
        .env(
            "TOR_PT_SERVER_BINDADDR",
            format!("webtunnel-127.0.0.1:{port}"),
        )
        .env("TOR_PT_ORPORT", orport.to_string())
        .env(
            "TOR_PT_SERVER_TRANSPORT_OPTIONS",
            format!("webtunnel:url=https://{HOST}/{PATH}"),
        )
        .env("TOR_PT_STATE_LOCATION", &state)
        .env("TOR_PT_EXIT_ON_STDIN_CLOSE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn webtunnel server");
    // The PT protocol: the server prints SMETHODS DONE once it is listening.
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            assert!(
                !line.contains("ERROR"),
                "Go server refused its config: {line}"
            );
            if line == "SMETHODS DONE" {
                return;
            }
        }
        panic!("Go server exited before SMETHODS DONE");
    });
    ready.await.expect("Go server did not start");
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    GoServer {
        addr: SocketAddr::from(([127, 0, 0, 1], port)),
        _child: child,
    }
}

/// Self-signed certificate for HOST; returns the rustls server config and the lyrebird pin.
fn cert() -> (Arc<rustls::ServerConfig>, String) {
    let ck = rcgen::generate_simple_self_signed(vec![HOST.to_string()]).unwrap();
    let der = ck.cert.der().clone();
    let pin = chain_hash([der.as_ref()]);
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(ck.key_pair.serialize_der().into());
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![der], key)
    .unwrap();
    use base64::Engine as _;
    (
        Arc::new(cfg),
        base64::engine::general_purpose::STANDARD.encode(pin),
    )
}

/// The bridge's "nginx": TLS in, 404 unless the request line names the secret path, otherwise
/// the request is replayed to `upstream` and the connection spliced. `upstream = None` answers
/// every correct request with a 200, for tests that must fail before the Go server matters.
async fn front(tls: Arc<rustls::ServerConfig>, upstream: Option<SocketAddr>) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut s) = acceptor.accept(s).await else {
                    return;
                };
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if s.read(&mut b).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(b[0]);
                }
                let expected = format!("GET /{PATH} HTTP/1.1\r\n");
                match upstream {
                    Some(up) if head.starts_with(expected.as_bytes()) => {
                        let mut u = TcpStream::connect(up).await.unwrap();
                        u.write_all(&head).await.unwrap();
                        let _ = tokio::io::copy_bidirectional(&mut s, &mut u).await;
                    }
                    None if head.starts_with(expected.as_bytes()) => {
                        let _ = s
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                            .await;
                    }
                    _ => {
                        let _ = s
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                            .await;
                    }
                }
            });
        }
    });
    addr
}

fn args(s: &str) -> ClientConfig {
    ClientConfig::from_args(&PtArgs::parse(s).unwrap()).unwrap()
}

/// Read the greeting, then push `len` bytes of a pattern through the echo and check every byte
/// comes back, concurrently, so neither direction can hide a stall behind the other.
async fn exercise<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    len: usize,
) {
    let (mut r, mut w) = tokio::io::split(stream);
    let mut greeting = vec![0u8; GREETING.len()];
    tokio::time::timeout(Duration::from_secs(10), r.read_exact(&mut greeting))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        greeting, GREETING,
        "bytes sent right behind the 101 must reach the caller"
    );

    let data: Vec<u8> = (0..len).map(|i| (i * 7 + i / 251) as u8).collect();
    let expect = data.clone();
    let writer = tokio::spawn(async move {
        w.write_all(&data).await.unwrap();
        w.flush().await.unwrap();
        w
    });
    let mut got = vec![0u8; len];
    tokio::time::timeout(Duration::from_secs(30), r.read_exact(&mut got))
        .await
        .unwrap()
        .unwrap();
    assert!(got == expect, "echoed data differs");
    writer.await.unwrap();
}

// ---------------------------------------------- against the official Go server

#[tokio::test]
#[ignore = "needs WEBTUNNEL_SERVER_BIN; run webtunnel/interop.sh"]
async fn plain_http_round_trip() {
    let go = go_server(orport().await).await;
    let c = args(&format!("url=http://{HOST}/{PATH};addr={}", go.addr));
    exercise(connect(&c).await.unwrap(), 1 << 20).await;
}

#[tokio::test]
#[ignore = "needs WEBTUNNEL_SERVER_BIN; run webtunnel/interop.sh"]
async fn tls_round_trip_with_pinned_cert() {
    let go = go_server(orport().await).await;
    let (tls, pin) = cert();
    let front = front(tls, Some(go.addr)).await;
    let c = args(&format!(
        "url=https://{HOST}/{PATH};addr={front};cert={pin};ver=0.0.3"
    ));
    exercise(connect(&c).await.unwrap(), 4 << 20).await;
}

#[tokio::test]
#[ignore = "needs WEBTUNNEL_SERVER_BIN; run webtunnel/interop.sh"]
async fn many_concurrent_tunnels() {
    let go = go_server(orport().await).await;
    let (tls, pin) = cert();
    let front = front(tls, Some(go.addr)).await;
    let c = Arc::new(args(&format!(
        "url=https://{HOST}/{PATH};addr={front};cert={pin}"
    )));
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let c = c.clone();
            tokio::spawn(async move { exercise(connect(&c).await.unwrap(), 256 << 10).await })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
}

#[tokio::test]
#[ignore = "needs WEBTUNNEL_SERVER_BIN; run webtunnel/interop.sh"]
async fn through_socks_like_arti() {
    let go = go_server(orport().await).await;
    let (tls, pin) = cert();
    let front = front(tls, Some(go.addr)).await;
    let server = SocksServer::bind("127.0.0.1:0".parse().unwrap(), Access::Secret("k3y".into()))
        .await
        .unwrap();
    let socks = server.local_addr().unwrap();
    tokio::spawn(server.serve());

    let list = PtArgs::from_pairs([
        ("url", format!("https://{HOST}/{PATH}")),
        ("addr", front.to_string()),
        ("cert", pin),
        (SECRET_ARG, "k3y".to_string()),
    ]);
    let (stream, code) = socks_connect(socks, &list.encode()).await;
    assert_eq!(code, 0x00);
    exercise(stream, 1 << 20).await;
}

// ------------------------------------------------- no Go server needed

#[tokio::test]
async fn wrong_path_is_refused() {
    let (tls, pin) = cert();
    let front = front(tls, None).await;
    let c = args(&format!(
        "url=https://{HOST}/not-the-path;addr={front};cert={pin}"
    ));
    match connect(&c).await {
        Err(Error::Upgrade(msg)) => assert!(msg.contains("404"), "{msg}"),
        Err(e) => panic!("expected an upgrade refusal, got {e}"),
        Ok(_) => panic!("expected an upgrade refusal, got a tunnel"),
    }
}

#[tokio::test]
async fn non_upgrade_answer_is_refused() {
    // Right path, but the server answers like an ordinary website.
    let (tls, pin) = cert();
    let front = front(tls, None).await;
    let c = args(&format!(
        "url=https://{HOST}/{PATH};addr={front};cert={pin}"
    ));
    assert!(matches!(connect(&c).await, Err(Error::Upgrade(_))));
}

#[tokio::test]
async fn wrong_pin_is_refused() {
    let (tls, _) = cert();
    let (_, other_pin) = cert();
    let front = front(tls, None).await;
    let c = args(&format!(
        "url=https://{HOST}/{PATH};addr={front};cert={other_pin}"
    ));
    let err = connect(&c)
        .await
        .err()
        .expect("a mismatched pin must not connect");
    assert!(matches!(err, Error::Tls(_)), "{err}");
}

#[tokio::test]
async fn self_signed_without_pin_is_refused() {
    // Proves CA validation is on by default: the test cert is not in Mozilla's roots.
    let (tls, _) = cert();
    let front = front(tls, None).await;
    let c = args(&format!("url=https://{HOST}/{PATH};addr={front}"));
    let err = connect(&c)
        .await
        .err()
        .expect("an untrusted certificate must not connect");
    assert!(matches!(err, Error::Tls(_)), "{err}");
}

#[tokio::test]
async fn socks_refuses_requests_without_the_secret() {
    let server = SocksServer::bind("127.0.0.1:0".parse().unwrap(), Access::Secret("k3y".into()))
        .await
        .unwrap();
    let socks = server.local_addr().unwrap();
    tokio::spawn(server.serve());
    // A perfectly good bridge line, minus the secret: another app on the device, say.
    let list = PtArgs::from_pairs([("url", "https://example.com/p")]);
    assert_eq!(socks_connect(socks, &list.encode()).await.1, 0x02);
    let list = PtArgs::from_pairs([("url", "https://example.com/p"), (SECRET_ARG, "k3Y")]);
    assert_eq!(socks_connect(socks, &list.encode()).await.1, 0x02);
}

#[tokio::test]
async fn socks_reports_a_bad_bridge_line() {
    let server = SocksServer::bind("127.0.0.1:0".parse().unwrap(), Access::Open)
        .await
        .unwrap();
    let socks = server.local_addr().unwrap();
    tokio::spawn(server.serve());
    assert_eq!(socks_connect(socks, "ver=0.0.3").await.1, 0x01, "no url=");
}

#[tokio::test]
async fn private_hostnames_are_not_dialled() {
    // "localhost" resolves to loopback only; without addr= that must be refused, not dialled.
    let c = args("url=https://localhost/p");
    let err = connect(&c).await.err().unwrap();
    assert!(
        matches!(err, Error::Config(ref m) if m.contains("no public address")),
        "{err}"
    );
}

/// A SOCKS5 CONNECT the way arti's `settings_to_protocol` + `do_socks_handshake` send it:
/// username/password auth with the argument list in the username (password = one NUL byte
/// when it fits), and the bridge's placeholder address as the target.
async fn socks_connect(socks: SocketAddr, arg_list: &str) -> (TcpStream, u8) {
    let mut s = TcpStream::connect(socks).await.unwrap();
    s.write_all(&[5, 1, 0x02]).await.unwrap();
    let mut m = [0u8; 2];
    s.read_exact(&mut m).await.unwrap();
    assert_eq!(m, [5, 0x02]);
    let bytes = arg_list.as_bytes();
    let (user, pass): (&[u8], &[u8]) = if bytes.len() <= 255 {
        (bytes, &[0])
    } else {
        bytes.split_at(255)
    };
    let mut auth = vec![1, user.len() as u8];
    auth.extend_from_slice(user);
    auth.push(pass.len() as u8);
    auth.extend_from_slice(pass);
    s.write_all(&auth).await.unwrap();
    let mut a = [0u8; 2];
    s.read_exact(&mut a).await.unwrap();
    assert_eq!(a, [1, 0], "auth sub-negotiation");
    // CONNECT [2001:db8::…]:443, the synthetic address WebTunnel bridge lines carry.
    let mut req = vec![5, 1, 0, 4];
    req.extend_from_slice(
        &"2001:db8::1"
            .parse::<std::net::Ipv6Addr>()
            .unwrap()
            .octets(),
    );
    req.extend_from_slice(&443u16.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[0], 5);
    (s, rep[1])
}
