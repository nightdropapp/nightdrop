//! Desktop end-to-end proof: bootstrap real Tor through the in-process WebTunnel client.
//!
//! Chain under test:
//!
//!   arti ──SOCKS5──▶ webtunnel-client SocksServer ──TLS+HTTP upgrade──▶ real WebTunnel bridge
//!        (unmanaged transport, proxy_addr)                             ──▶ the Tor network
//!
//! arti is configured with the bridge line and an *unmanaged* `webtunnel` transport pointing at
//! our local SOCKS listener; arti hands it the bridge's target and PT args over SOCKS, and our
//! client dials the bridge. If bootstrap reaches 100% and a stream opens, WebTunnel really carries
//! Tor — not just bytes (the interop tests already prove bytes).
//!
//! Bridge lines come from https://bridges.torproject.org/bridges/en?transport=webtunnel . Their
//! `addr` is a non-routable `2001:db8:` placeholder by design — the real endpoint is the `url=`
//! host, which our client resolves; arti's bogus target is discarded by the SOCKS layer.
//!
//!   cargo run -p nightdrop --features tor --example webtunnel_bootstrap -- '<bridge line>'
//!
//! With no argument it uses a built-in default line (edit below to a fresh one if it has rotated).

use std::net::SocketAddr;
use std::time::Duration;

use arti_client::config::pt::TransportConfigBuilder;
use arti_client::config::{BridgeConfigBuilder, TorClientConfigBuilder};
use arti_client::TorClient;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use webtunnel_client::socks::{Access, SocksServer};
use webtunnel_client::TRANSPORT_NAME;

const DEFAULT_BRIDGE: &str = "webtunnel [2001:db8:ae03:9e61:9e65:cce8:fdb6:66a8]:443 \
CD0DFB72DE3124704AEA1BEF3A2CCD62347F6376 \
url=https://www2.shallotfarm.org/gKOwKgm0McUdTydo3boBiFFM ver=0.0.5";

const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(180);

fn main() -> anyhow::Result<()> {
    let bridge_line = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_BRIDGE.to_string());
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(run(bridge_line))
}

async fn run(bridge_line: String) -> anyhow::Result<()> {
    // 1. Our WebTunnel SOCKS front end, on an OS-assigned loopback port, with a per-run secret —
    //    exactly as core/src/transport/tor.rs wires it (the loopback port is reachable by any app
    //    on Android, so the transport args must carry the secret the listener authorises on).
    let secret = "harness-secret-0123456789abcdef".to_string();
    let server = SocksServer::bind("127.0.0.1:0".parse()?, Access::Secret(secret.clone())).await?;
    let socks_addr: SocketAddr = server.local_addr()?;
    tokio::spawn(server.serve());
    eprintln!("webtunnel SOCKS listener: {socks_addr}");

    // The secret rides on the bridge line as one more transport arg; arti forwards it over SOCKS.
    let bridge_line = format!("{bridge_line} listener-secret={secret}");

    // 2. A throwaway Tor state/cache dir so this never touches the app's identity.
    let dir = std::env::temp_dir().join(format!("wt-bootstrap-{}", std::process::id()));
    let state = dir.join("state");
    let cache = dir.join("cache");
    std::fs::create_dir_all(&state)?;
    std::fs::create_dir_all(&cache)?;

    // 3. arti config: use the bridge, and route its `webtunnel` transport to our SOCKS listener
    //    as an *unmanaged* proxy (no external binary, no path).
    let mut builder = TorClientConfigBuilder::from_directories(&state, &cache);
    // Relax arti's fs-mistrust for this throwaway temp dir (as the core does for its writable
    // base); otherwise a group/other-readable /tmp dir is rejected before bootstrap.
    builder.storage().permissions().dangerously_trust_everyone();
    let bridge: BridgeConfigBuilder = bridge_line.parse()?;
    builder.bridges().bridges().push(bridge);
    let mut transport = TransportConfigBuilder::default();
    transport
        .protocols(vec![TRANSPORT_NAME.parse()?])
        .proxy_addr(socks_addr);
    builder.bridges().transports().push(transport);
    let config = builder.build()?;

    // 4. Bootstrap, bounded — a censored/dead path would otherwise hang forever.
    eprintln!("bootstrapping Tor through WebTunnel (up to {BOOTSTRAP_TIMEOUT:?})…");
    let started = std::time::Instant::now();
    let client = tokio::time::timeout(BOOTSTRAP_TIMEOUT, TorClient::create_bootstrapped(config))
        .await
        .map_err(|_| anyhow::anyhow!("bootstrap timed out after {BOOTSTRAP_TIMEOUT:?}"))??;
    eprintln!("✅ bootstrapped in {:.1}s", started.elapsed().as_secs_f64());

    // 5. Prove a working circuit: open a stream through Tor and read one HTTP status line.
    eprintln!("opening a Tor stream to example.com:80 …");
    let mut stream = client.connect(("example.com", 80)).await?;
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: example.com\r\nConnection: close\r\n\r\n")
        .await?;
    stream.flush().await?;
    let mut buf = [0u8; 128];
    let n = stream.read(&mut buf).await?;
    let status = String::from_utf8_lossy(&buf[..n]);
    let first = status.lines().next().unwrap_or("").trim();
    eprintln!("✅ Tor stream replied: {first:?}");

    println!("\nRESULT: WebTunnel carried a full Tor bootstrap and circuit.");
    Ok(())
}
