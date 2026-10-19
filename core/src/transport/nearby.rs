//! Nearby (`docs/design/nearby.md`): the main transport (Tor) plus a same-Wi-Fi path for the chats
//! where both people turned "Also connect on the same Wi-Fi" on.
//!
//! [`NearbyTransport`] wraps the main transport and keeps its address (the onion). For a peer with
//! a usable route — both sides on, a private address in our subnet — a send tries the local network
//! first and falls back to the main transport on any failure. Each local connection opens with a
//! token exchange (§5): the dialer presents the token the listener gave that chat, the listener
//! answers with the dialer's, and anything else is closed. The tokens only keep strangers and wrong
//! hosts out; what is sent is end-to-end encrypted and authenticated like any frame.
//!
//! This is the second direct (non-Tor) path `ARCHITECTURE.md` §1a allows, and only for a contact
//! whose own app told us, end to end, where to reach it.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::inbound::{read_frame, InboundBudget};
use super::{AbortHandle, Address, FileFetch, Transport};
use crate::Result;

/// Opening line of a local connection; followed by a 43-character token and `\n`.
const PREAMBLE: &[u8] = b"ND-NEARBY/1 ";
/// A token: 32 random bytes, base64url without padding.
pub const TOKEN_CHARS: usize = 43;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
/// An accepted connection that sends nothing for this long is closed.
const READ_IDLE: Duration = Duration::from_secs(30);
/// At most this many local connections are served at once; more are refused.
const MAX_CONNECTIONS: usize = 16;
/// A route counts as reachable this long after a frame last went through it, either way.
const REACHABLE_FOR: Duration = Duration::from_secs(120);
const ACCEPT_POLL: Duration = Duration::from_millis(200);
/// After a local attempt fails, sends skip the local path this long (each would otherwise wait out
/// the connect timeout first), and the route is probed again after it.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// The lowest port a nearby announcement may name: the start of the ephemeral range our listener
/// binds in (Linux and Android 32768+, Windows and macOS 49152+). Below it sit services — a
/// printer's raw port, a router's admin page — that a contact must not make us send a line to.
pub const MIN_PORT: u16 = 32768;
/// Bytes of local frames held for the core at once. Smaller than Tor's: a nearby contact is one
/// person, not the network.
const INBOUND_BUDGET_BYTES: usize = 16 * 1024 * 1024;

/// What the node tells the transport about one chat with Nearby on (ours).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyRoute {
    /// The token we gave this chat: a connection presenting it is theirs.
    pub our_token: String,
    /// Where they listen and the token they gave us, once they turned it on too.
    pub peer: Option<(SocketAddrV4, String)>,
}

/// Where this device's local address comes from (replaced in tests, which run on loopback).
type IpSource = fn() -> Option<Ipv4Addr>;

struct Listening {
    port: u16,
    stop: Arc<AtomicBool>,
}

struct Shared {
    /// Peer onion → route.
    routes: Mutex<HashMap<String, NearbyRoute>>,
    reachable: Mutex<HashMap<String, Instant>>,
    /// When the local path to a peer last failed (see [`RETRY_AFTER`]).
    failed: Mutex<HashMap<String, Instant>>,
    /// Peers with a probe running.
    probing: Mutex<std::collections::HashSet<String>>,
    budget: InboundBudget,
    connections: AtomicUsize,
    ip_source: IpSource,
    /// The device's interfaces and their IPv4 addresses, as the app last listed them
    /// ([`Transport::set_local_addresses`]). Preferred over `ip_source`, which only sees the
    /// default route: on a phone sharing its connection that is mobile data, not the hotspot.
    interfaces: Mutex<Option<Vec<(String, Ipv4Addr)>>>,
    /// Loopback counts as a local network (tests only).
    allow_loopback: bool,
}

// Generic over the inner transport rather than holding a `Box<dyn Transport>`: the bridge generator
// walks every `Transport` impl, and a struct naming `dyn Transport` sends it round in a circle.
pub struct NearbyTransport<T> {
    inner: T,
    shared: Arc<Shared>,
    listening: Mutex<Option<Listening>>,
    tx: Sender<(Address, Vec<u8>)>,
    rx: Mutex<Receiver<(Address, Vec<u8>)>>,
}

impl<T: Transport> NearbyTransport<T> {
    pub fn new(inner: T) -> Self {
        Self::with_ip_source(inner, super::lan::local_lan_ip, false)
    }

    /// On loopback, which stands in for a local network in tests.
    #[cfg(test)]
    pub(crate) fn on_loopback(inner: T) -> Self {
        Self::with_ip_source(inner, || Some(Ipv4Addr::LOCALHOST), true)
    }

    fn with_ip_source(inner: T, ip_source: IpSource, allow_loopback: bool) -> Self {
        let (tx, rx) = channel();
        Self {
            inner,
            shared: Arc::new(Shared {
                routes: Mutex::new(HashMap::new()),
                reachable: Mutex::new(HashMap::new()),
                failed: Mutex::new(HashMap::new()),
                probing: Mutex::new(std::collections::HashSet::new()),
                budget: InboundBudget::with_limit(INBOUND_BUDGET_BYTES),
                connections: AtomicUsize::new(0),
                ip_source,
                interfaces: Mutex::new(None),
                allow_loopback,
            }),
            listening: Mutex::new(None),
            tx,
            rx: Mutex::new(rx),
        }
    }

    fn start_listening(&self) -> Option<Listening> {
        let listener = match TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)) {
            Ok(l) => l,
            Err(e) => {
                crate::diag!("nearby: could not listen: {}", e.kind());
                return None;
            }
        };
        listener.set_nonblocking(true).ok()?;
        let port = listener.local_addr().ok()?.port();
        crate::diag!("nearby: listening on port {port}");
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, tx, halt) = (Arc::clone(&self.shared), self.tx.clone(), Arc::clone(&stop));
        thread::spawn(move || {
            while !halt.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, SocketAddr::V4(from))) if shared.admits_source(*from.ip()) => {
                        if shared.connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
                            shared.connections.fetch_sub(1, Ordering::AcqRel);
                            continue;
                        }
                        let (shared, tx, halt) =
                            (Arc::clone(&shared), tx.clone(), Arc::clone(&halt));
                        thread::spawn(move || {
                            if let Err(e) = serve(stream, &shared, &tx, &halt) {
                                crate::diag!("nearby: incoming connection ended: {e}");
                            }
                            shared.connections.fetch_sub(1, Ordering::AcqRel);
                        });
                    }
                    // IPv6, or not from our local network: dropped unanswered.
                    Ok((_, from)) => crate::diag!(
                        "nearby: refused a connection from {} (not one of our local networks)",
                        net_of(&from)
                    ),
                    Err(_) => thread::sleep(ACCEPT_POLL),
                }
            }
        });
        Some(Listening { port, stop })
    }

    /// Try the local path to `peer`. `Err` means: use the main transport.
    fn send_nearby(&self, peer: &str, frame: &[u8]) -> Result<()> {
        anyhow::ensure!(
            !self.shared.recently_failed(peer),
            "local path failed recently"
        );
        let mut stream = self.shared.dial(peer)?;
        let result = (|| {
            stream.write_all(&(frame.len() as u32).to_be_bytes())?;
            stream.write_all(frame)?;
            stream.flush()?;
            Ok(())
        })();
        self.shared.record(peer, result.is_ok());
        match &result {
            Ok(()) => crate::diag!("nearby: sent {} bytes over the local network", frame.len()),
            Err(e) => crate::diag!("nearby: local send failed after connecting ({e}); using Tor"),
        }
        result
    }
}

/// The Tor transport with the nearby path beside it: nothing listens or dials locally until a chat
/// turns it on.
#[cfg(feature = "tor")]
pub fn over_tor(tor: super::tor::TorTransport) -> Box<dyn Transport> {
    Box::new(NearbyTransport::new(tor))
}

impl Shared {
    /// Open a local connection to `peer` and run the token exchange (§5).
    fn dial(&self, peer: &str) -> Result<TcpStream> {
        let route = self.routes.lock().unwrap().get(peer).cloned();
        let Some(NearbyRoute {
            our_token,
            peer: Some((addr, their_token)),
        }) = route
        else {
            anyhow::bail!("no nearby route");
        };
        if !self.on_our_network(*addr.ip()) {
            crate::diag!(
                "nearby: contact listens on {}, ours are [{}]: not the same network, using Tor",
                net_of_ip(*addr.ip()),
                self.local_ips()
                    .into_iter()
                    .map(net_of_ip)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            anyhow::bail!("not on the same network");
        }
        // Only from here is a connection attempted, and only its failure starts the back-off
        // ([`RETRY_AFTER`]): a send with no route yet must not hold off the first real attempt.
        let attempt = Self::exchange(addr, &their_token, &our_token);
        match &attempt {
            Ok(_) => crate::diag!(
                "nearby: connected to the contact at {}",
                net_of_ip(*addr.ip())
            ),
            Err(e) => {
                crate::diag!(
                    "nearby: could not reach the contact at {} ({e}); using Tor, retrying in {}s",
                    net_of_ip(*addr.ip()),
                    RETRY_AFTER.as_secs()
                );
                self.record(peer, false);
            }
        }
        attempt
    }

    fn exchange(addr: SocketAddrV4, their_token: &str, our_token: &str) -> Result<TcpStream> {
        let mut stream = TcpStream::connect_timeout(&SocketAddr::V4(addr), CONNECT_TIMEOUT)?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.write_all(&preamble(their_token))?;
        let mut answer = [0u8; TOKEN_CHARS + 1];
        stream.read_exact(&mut answer)?;
        anyhow::ensure!(
            tokens_equal(&answer[..TOKEN_CHARS], our_token.as_bytes())
                && answer[TOKEN_CHARS] == b'\n',
            "the other end is not this contact"
        );
        Ok(stream)
    }

    fn record(&self, peer: &str, ok: bool) {
        if ok {
            self.reachable
                .lock()
                .unwrap()
                .insert(peer.to_string(), Instant::now());
            self.failed.lock().unwrap().remove(peer);
        } else {
            self.failed
                .lock()
                .unwrap()
                .insert(peer.to_string(), Instant::now());
        }
    }

    fn recently_failed(&self, peer: &str) -> bool {
        self.failed
            .lock()
            .unwrap()
            .get(peer)
            .is_some_and(|t| t.elapsed() < RETRY_AFTER)
    }

    fn reachable_recently(&self, peer: &str, within: Duration) -> bool {
        self.reachable
            .lock()
            .unwrap()
            .get(peer)
            .is_some_and(|t| t.elapsed() < within)
    }

    /// The address we announce: the best-ranked usable interface the app listed (see
    /// [`interface_rank`]), else the default route's address.
    fn local_ip(&self) -> Option<Ipv4Addr> {
        let listed = self.interfaces.lock().unwrap().clone();
        match listed {
            Some(list) => list
                .into_iter()
                .filter(|(_, ip)| self.is_local(*ip))
                .filter_map(|(name, ip)| interface_rank(&name).map(|rank| (rank, ip)))
                .min_by_key(|(rank, _)| *rank)
                .map(|(_, ip)| ip),
            None => (self.ip_source)().filter(|ip| self.is_local(*ip)),
        }
    }

    /// Every local address we could be reached on, for the same-network checks: a contact on our
    /// hotspot shares that interface's subnet, not the announced one's, if they differ.
    fn local_ips(&self) -> Vec<Ipv4Addr> {
        let listed = self.interfaces.lock().unwrap().clone();
        match listed {
            Some(list) => list
                .into_iter()
                .filter(|(name, ip)| self.is_local(*ip) && interface_rank(name).is_some())
                .map(|(_, ip)| ip)
                .collect(),
            None => self.local_ip().into_iter().collect(),
        }
    }

    fn on_our_network(&self, peer: Ipv4Addr) -> bool {
        self.local_ips()
            .into_iter()
            .any(|ours| same_subnet(ours, peer))
    }

    fn is_local(&self, ip: Ipv4Addr) -> bool {
        is_private(ip) || (self.allow_loopback && ip.is_loopback())
    }

    /// A connection is answered only from a local address in our own subnet.
    fn admits_source(&self, from: Ipv4Addr) -> bool {
        self.is_local(from) && self.on_our_network(from)
    }
}

/// Serve one accepted connection: check its token, answer with the dialer's, then read frames.
fn serve(
    mut stream: TcpStream,
    shared: &Shared,
    tx: &Sender<(Address, Vec<u8>)>,
    halt: &AtomicBool,
) -> Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let mut hello = [0u8; PREAMBLE.len() + TOKEN_CHARS + 1];
    stream.read_exact(&mut hello)?;
    anyhow::ensure!(
        hello.starts_with(PREAMBLE) && hello[hello.len() - 1] == b'\n',
        "not a nearby peer"
    );
    let presented = &hello[PREAMBLE.len()..PREAMBLE.len() + TOKEN_CHARS];
    // Every route is compared, so the time taken does not say which token came close.
    let mut found = None;
    for (onion, route) in shared.routes.lock().unwrap().iter() {
        if tokens_equal(presented, route.our_token.as_bytes()) {
            if let Some((_, theirs)) = &route.peer {
                found = Some((onion.clone(), theirs.clone()));
            }
        }
    }
    let (onion, theirs) = found.ok_or_else(|| {
        anyhow::anyhow!(
            "unknown token (not a contact with Nearby on, or they have not told us yet)"
        )
    })?;
    crate::diag!("nearby: a contact connected locally");
    stream.write_all(theirs.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    shared.record(&onion, true);
    stream.set_read_timeout(Some(READ_IDLE))?;
    while !halt.load(Ordering::Relaxed) {
        let frame = read_frame(&mut stream, &shared.budget)?;
        shared
            .reachable
            .lock()
            .unwrap()
            .insert(onion.clone(), Instant::now());
        if tx.send((Address::new(), frame)).is_err() {
            break;
        }
    }
    Ok(())
}

fn preamble(token: &str) -> Vec<u8> {
    [PREAMBLE, token.as_bytes(), b"\n"].concat()
}

/// A local address as the diagnostics may show it: its network, never the host (`192.168.4.*`,
/// `172.16.*.*`, `10.*.*.*`), which is what matters for "the same network" and says nothing more.
/// [`net_of_ip`] for the node's diagnostics.
pub fn network_of(ip: Ipv4Addr) -> String {
    net_of_ip(ip)
}

fn net_of_ip(ip: Ipv4Addr) -> String {
    let o = ip.octets();
    match o {
        [10, ..] => "10.*.*.*".into(),
        [192, 168, c, _] => format!("192.168.{c}.*"),
        [127, ..] => "127.*.*.*".into(),
        [a, b, ..] if is_private(ip) => format!("{a}.{b}.*.*"),
        _ => "a public address".into(),
    }
}

fn net_of(addr: &SocketAddr) -> String {
    match addr {
        SocketAddr::V4(a) => net_of_ip(*a.ip()),
        SocketAddr::V6(_) => "an IPv6 address".into(),
    }
}

/// Constant-time equality for two tokens of the expected length.
fn tokens_equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == TOKEN_CHARS
        && b.len() == TOKEN_CHARS
        && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// RFC 1918 and link-local: the only addresses a nearby announcement may name (§4). A public one
/// would have us connect to it outside Tor, which shows the announcer our public IP.
pub fn is_private(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local()
}

/// Whether two private addresses are on one local network: /24 for 192.168, /16 for 172.16/12 and
/// link-local, /8 for 10.
pub fn same_subnet(a: Ipv4Addr, b: Ipv4Addr) -> bool {
    let (x, y) = (a.octets(), b.octets());
    match x {
        [127, ..] => y[0] == 127,
        [10, ..] => y[0] == 10,
        [192, 168, ..] => y[..3] == x[..3],
        _ => y[..2] == x[..2],
    }
}

/// A port a nearby announcement may name ([`MIN_PORT`]).
pub fn valid_port(port: u16) -> bool {
    port >= MIN_PORT
}

/// How suitable an interface is for nearby contacts, by its name: lower is better, `None` never.
/// Wi-Fi and the phone's own hotspot first, then wired; never mobile data (a carrier's private
/// range is not a network anyone nearby shares), VPNs, or virtual bridges.
pub fn interface_rank(name: &str) -> Option<u8> {
    let n = name.to_ascii_lowercase();
    const NEVER: [&str; 14] = [
        "lo", "rmnet", "ccmni", "v4-", "tun", "tap", "ppp", "wg", "utun", "dummy", "docker",
        "veth", "virbr", "vboxnet",
    ];
    if NEVER.iter().any(|p| n.starts_with(p)) || n.starts_with("br-") || n.starts_with("podman") {
        return None;
    }
    const WIFI: [&str; 7] = ["wlan", "swlan", "ap", "softap", "wl", "wi-fi", "wifi"];
    const WIRED: [&str; 4] = ["eth", "en", "ethernet", "usb"];
    if WIFI.iter().any(|p| n.starts_with(p)) {
        Some(0)
    } else if WIRED.iter().any(|p| n.starts_with(p)) {
        Some(1)
    } else {
        Some(2)
    }
}

/// A token as the announcement carries it: exactly [`TOKEN_CHARS`] base64url characters.
pub fn valid_token(token: &str) -> bool {
    token.len() == TOKEN_CHARS
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl<T: Transport> Transport for NearbyTransport<T> {
    fn address(&self) -> Address {
        self.inner.address()
    }

    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        self.send_nearby(peer, frame)
            .or_else(|_| self.inner.send(peer, frame))
    }

    fn send_fresh(&self, peer: &str, frame: &[u8]) -> Result<()> {
        // A nearby send always opens its own connection, so it is fresh already.
        self.send_nearby(peer, frame)
            .or_else(|_| self.inner.send_fresh(peer, frame))
    }

    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        if let Ok(item) = self.rx.lock().unwrap().try_recv() {
            self.shared.budget.release(item.1.len());
            return Some(item);
        }
        self.inner.try_recv()
    }

    fn published(&self) -> bool {
        self.inner.published()
    }

    fn is_synchronous(&self) -> bool {
        self.inner.is_synchronous()
    }

    fn relay_dialer_isolated(
        &self,
        addr: &str,
        group: u64,
    ) -> Option<crate::relay_client::RelayDialer> {
        self.inner.relay_dialer_isolated(addr, group)
    }

    fn abort_handle(&self) -> Option<AbortHandle> {
        self.inner.abort_handle()
    }

    fn relay_dialer(&self, addr: &str) -> Option<crate::relay_client::RelayDialer> {
        self.inner.relay_dialer(addr)
    }

    fn onion_get(&self, onion: &str, port: u16, path: &str) -> Option<Result<Vec<u8>>> {
        self.inner.onion_get(onion, port, path)
    }

    fn onion_get_capped(
        &self,
        onion: &str,
        port: u16,
        path: &str,
        max: usize,
    ) -> Option<Result<Vec<u8>>> {
        self.inner.onion_get_capped(onion, port, path, max)
    }

    fn onion_get_to_file(
        &self,
        req: FileFetch<'_>,
        progress: &dyn Fn(u64, Option<u64>),
    ) -> Option<Result<u64>> {
        self.inner.onion_get_to_file(req, progress)
    }

    fn make_client_key(&self, peer_onion: &str) -> Option<Result<(String, [u8; 32])>> {
        self.inner.make_client_key(peer_onion)
    }

    fn mint_pass(&self) -> Option<Result<(String, [u8; 32])>> {
        self.inner.mint_pass()
    }

    fn insert_client_key(&self, peer_onion: &str, secret: &[u8; 32]) -> Result<()> {
        self.inner.insert_client_key(peer_onion, secret)
    }

    fn authorize_client(&self, contact_id: &str, key: &str) -> Result<()> {
        self.inner.authorize_client(contact_id, key)
    }

    fn revoke_client(&self, contact_id: &str) -> Result<()> {
        self.inner.revoke_client(contact_id)
    }

    fn forget_peer_key(&self, peer_onion: &str) -> Result<()> {
        self.inner.forget_peer_key(peer_onion)
    }

    fn onion_identity(&self) -> Option<[u8; 64]> {
        self.inner.onion_identity()
    }

    fn set_nearby(&self, routes: HashMap<String, NearbyRoute>) {
        let listen = !routes.is_empty();
        *self.shared.routes.lock().unwrap() = routes;
        let mut listening = self.listening.lock().unwrap();
        match (listen, listening.is_some()) {
            (true, false) => *listening = self.start_listening(),
            (false, true) => {
                if let Some(l) = listening.take() {
                    l.stop.store(true, Ordering::Relaxed);
                    crate::diag!("nearby: stopped listening (no chat has it on)");
                }
            }
            _ => {}
        }
    }

    fn nearby_address(&self) -> Option<SocketAddrV4> {
        let port = self.listening.lock().unwrap().as_ref()?.port;
        Some(SocketAddrV4::new(self.shared.local_ip()?, port))
    }

    fn nearby_reachable(&self, peer: &str) -> bool {
        self.shared.reachable_recently(peer, REACHABLE_FOR)
    }

    fn set_local_addresses(&self, interfaces: Vec<(String, Ipv4Addr)>) {
        let changed = self.shared.interfaces.lock().unwrap().as_ref() != Some(&interfaces);
        *self.shared.interfaces.lock().unwrap() = Some(interfaces.clone());
        if changed {
            let listed: Vec<String> = interfaces
                .iter()
                .map(|(name, ip)| {
                    let use_ = if !self.shared.is_local(*ip) {
                        "not private"
                    } else if interface_rank(name).is_none() {
                        "never used"
                    } else {
                        "usable"
                    };
                    format!("{name} {} ({use_})", net_of_ip(*ip))
                })
                .collect();
            crate::diag!(
                "nearby: interfaces [{}]; announcing {}",
                listed.join(", "),
                self.shared
                    .local_ip()
                    .map_or("nothing".to_string(), net_of_ip)
            );
        }
    }

    fn nearby_probe(&self) {
        let due: Vec<String> = self
            .shared
            .routes
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, r)| r.peer.is_some())
            .map(|(onion, _)| onion.clone())
            .filter(|onion| {
                !self.shared.reachable_recently(onion, RETRY_AFTER)
                    && !self.shared.recently_failed(onion)
            })
            .collect();
        for onion in due {
            if !self.shared.probing.lock().unwrap().insert(onion.clone()) {
                continue;
            }
            let shared = Arc::clone(&self.shared);
            thread::spawn(move || {
                // `dial` records a failed attempt itself.
                if shared.dial(&onion).is_ok() {
                    shared.record(&onion, true);
                }
                shared.probing.lock().unwrap().remove(&onion);
            });
        }
    }
}

impl<T> Drop for NearbyTransport<T> {
    fn drop(&mut self) {
        if let Some(l) = self.listening.lock().unwrap().take() {
            l.stop.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{MemoryNetwork, MemoryTransport};

    type Nt = NearbyTransport<MemoryTransport>;

    fn loopback() -> Option<Ipv4Addr> {
        Some(Ipv4Addr::LOCALHOST)
    }

    fn token(c: char) -> String {
        std::iter::repeat_n(c, TOKEN_CHARS).collect()
    }

    fn pair() -> (MemoryNetwork, Nt, Nt) {
        let net = MemoryNetwork::new();
        let a = NearbyTransport::with_ip_source(net.endpoint("a.onion"), loopback, true);
        let b = NearbyTransport::with_ip_source(net.endpoint("b.onion"), loopback, true);
        (net, a, b)
    }

    fn recv(t: &Nt) -> Option<Vec<u8>> {
        for _ in 0..100 {
            if let Some((_, f)) = t.try_recv() {
                return Some(f);
            }
            thread::sleep(Duration::from_millis(20));
        }
        None
    }

    /// Both sides on: the frame goes over the local network, even with the main transport down.
    fn connect(a: &Nt, b: &Nt) {
        let (ta, tb) = (token('a'), token('b'));
        a.set_nearby(HashMap::from([(
            "b.onion".into(),
            NearbyRoute {
                our_token: ta.clone(),
                peer: None,
            },
        )]));
        b.set_nearby(HashMap::from([(
            "a.onion".into(),
            NearbyRoute {
                our_token: tb.clone(),
                peer: None,
            },
        )]));
        let (pa, pb) = (a.nearby_address().unwrap(), b.nearby_address().unwrap());
        a.set_nearby(HashMap::from([(
            "b.onion".into(),
            NearbyRoute {
                our_token: ta.clone(),
                peer: Some((pb, tb.clone())),
            },
        )]));
        b.set_nearby(HashMap::from([(
            "a.onion".into(),
            NearbyRoute {
                our_token: tb,
                peer: Some((pa, ta)),
            },
        )]));
    }

    #[test]
    fn a_frame_goes_over_the_local_network_when_both_are_on() {
        let (net, a, b) = pair();
        connect(&a, &b);
        net.disconnect("b.onion"); // the main transport cannot reach b
        a.send("b.onion", b"hello nearby").unwrap();
        assert_eq!(recv(&b).as_deref(), Some(&b"hello nearby"[..]));
        assert!(a.nearby_reachable("b.onion") && b.nearby_reachable("a.onion"));
    }

    #[test]
    fn without_a_route_the_main_transport_carries_it() {
        let (_net, a, b) = pair();
        a.send("b.onion", b"over tor").unwrap();
        assert_eq!(recv(&b).as_deref(), Some(&b"over tor"[..]));
        assert!(!a.nearby_reachable("b.onion"));
        assert!(
            a.nearby_address().is_none(),
            "nothing listens while no chat has it on"
        );
    }

    #[test]
    fn a_wrong_token_is_refused_and_the_main_transport_is_used() {
        let (net, a, b) = pair();
        connect(&a, &b);
        // b announced a token a does not hold: b's listener refuses a's preamble.
        let pb = b.nearby_address().unwrap();
        a.set_nearby(HashMap::from([(
            "b.onion".into(),
            NearbyRoute {
                our_token: token('a'),
                peer: Some((pb, token('z'))),
            },
        )]));
        net.disconnect("b.onion");
        assert!(
            a.send("b.onion", b"x").is_err(),
            "refused locally, and the main path is down"
        );
        net.reconnect("b.onion");
        a.send("b.onion", b"via main").unwrap();
        assert_eq!(recv(&b).as_deref(), Some(&b"via main"[..]));
    }

    #[test]
    fn a_listener_that_answers_with_the_wrong_token_is_not_trusted() {
        let (net, a, b) = pair();
        connect(&a, &b);
        // a expects b to answer with `a`'s token; make a believe its own token is different.
        let pb = b.nearby_address().unwrap();
        a.set_nearby(HashMap::from([(
            "b.onion".into(),
            NearbyRoute {
                our_token: token('q'),
                peer: Some((pb, token('b'))),
            },
        )]));
        net.disconnect("b.onion");
        assert!(a.send("b.onion", b"x").is_err());
        assert!(recv(&b).is_none(), "nothing was sent after the bad answer");
    }

    #[test]
    fn turning_every_chat_off_stops_the_listener() {
        let (_net, a, b) = pair();
        connect(&a, &b);
        let pb = b.nearby_address().unwrap();
        b.set_nearby(HashMap::new());
        assert!(b.nearby_address().is_none());
        thread::sleep(ACCEPT_POLL * 3);
        assert!(TcpStream::connect_timeout(
            &SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, pb.port())),
            CONNECT_TIMEOUT
        )
        .is_err());
    }

    #[test]
    fn a_probe_finds_the_route_without_sending_anything() {
        let (_net, a, b) = pair();
        connect(&a, &b);
        assert!(!a.nearby_reachable("b.onion"));
        a.nearby_probe();
        for _ in 0..100 {
            if a.nearby_reachable("b.onion") && b.nearby_reachable("a.onion") {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(a.nearby_reachable("b.onion") && b.nearby_reachable("a.onion"));
        assert!(recv(&b).is_none(), "a probe carries no frame");
    }

    #[test]
    fn after_a_failure_sends_skip_the_local_path_for_a_while() {
        let (_net, a, b) = pair();
        connect(&a, &b);
        // b stops listening, so a's next local attempt fails...
        b.set_nearby(HashMap::new());
        thread::sleep(ACCEPT_POLL * 3);
        a.send("b.onion", b"one").unwrap(); // falls back to the main transport
        assert_eq!(recv(&b).as_deref(), Some(&b"one"[..]));
        // ...and the next send does not try it again (no connect wait at all).
        let t = Instant::now();
        a.send("b.onion", b"two").unwrap();
        assert!(t.elapsed() < Duration::from_millis(500));
        assert_eq!(recv(&b).as_deref(), Some(&b"two"[..]));
    }

    #[test]
    fn only_private_addresses_in_our_subnet_count() {
        let p = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        assert!(
            is_private(p("192.168.1.5"))
                && is_private(p("10.2.3.4"))
                && is_private(p("172.20.1.1"))
        );
        assert!(is_private(p("169.254.10.1")));
        assert!(
            !is_private(p("8.8.8.8"))
                && !is_private(p("100.64.1.1"))
                && !is_private(p("172.32.0.1"))
        );
        assert!(same_subnet(p("192.168.1.5"), p("192.168.1.200")));
        assert!(!same_subnet(p("192.168.1.5"), p("192.168.2.5")));
        assert!(same_subnet(p("10.1.2.3"), p("10.200.0.1")));
        assert!(
            same_subnet(p("172.16.4.1"), p("172.16.9.9"))
                && !same_subnet(p("172.16.4.1"), p("172.17.4.1"))
        );
    }

    #[test]
    fn the_hotspot_or_wifi_interface_is_announced_not_mobile_data() {
        let net = MemoryNetwork::new();
        let t = NearbyTransport::with_ip_source(net.endpoint("a.onion"), || None, false);
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        // A phone sharing its connection: the carrier's private range on rmnet, the hotspot on
        // swlan0, a VPN on tun0.
        t.set_local_addresses(vec![
            ("rmnet_data0".into(), ip("10.120.4.9")),
            ("tun0".into(), ip("10.8.0.2")),
            ("swlan0".into(), ip("192.168.212.1")),
        ]);
        assert_eq!(t.shared.local_ip(), Some(ip("192.168.212.1")));
        assert!(
            t.shared.on_our_network(ip("192.168.212.57")),
            "a phone on our hotspot"
        );
        assert!(
            !t.shared.on_our_network(ip("10.120.4.200")),
            "not over mobile data"
        );
        // A laptop on Ethernet and Wi-Fi: both count as our networks, Wi-Fi is announced.
        t.set_local_addresses(vec![
            ("enp3s0".into(), ip("192.168.1.20")),
            ("wlp2s0".into(), ip("192.168.50.4")),
        ]);
        assert_eq!(t.shared.local_ip(), Some(ip("192.168.50.4")));
        assert!(t.shared.on_our_network(ip("192.168.1.77")));
        // Nothing usable: no address at all.
        t.set_local_addresses(vec![("rmnet0".into(), ip("10.1.1.1"))]);
        assert_eq!(t.shared.local_ip(), None);
    }

    #[test]
    fn interfaces_are_ranked_by_name() {
        assert_eq!(interface_rank("wlan0"), Some(0));
        assert_eq!(interface_rank("swlan0"), Some(0));
        assert_eq!(interface_rank("ap0"), Some(0));
        assert_eq!(interface_rank("Wi-Fi"), Some(0));
        assert_eq!(interface_rank("eno1"), Some(1));
        assert_eq!(interface_rank("Ethernet 2"), Some(1));
        for never in [
            "lo",
            "rmnet_data1",
            "ccmni0",
            "v4-rmnet0",
            "tun0",
            "ppp0",
            "wg0",
            "docker0",
            "br-1a2b",
            "podman1",
            "veth9",
        ] {
            assert_eq!(interface_rank(never), None, "{never}");
        }
    }

    #[test]
    fn diagnostics_show_the_network_never_the_host() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        assert_eq!(net_of_ip(ip("192.168.88.94")), "192.168.88.*");
        assert_eq!(net_of_ip(ip("10.20.30.40")), "10.*.*.*");
        assert_eq!(net_of_ip(ip("172.20.1.9")), "172.20.*.*");
        assert_eq!(net_of_ip(ip("8.8.8.8")), "a public address");
    }

    #[test]
    fn tokens_are_compared_whole() {
        assert!(tokens_equal(token('a').as_bytes(), token('a').as_bytes()));
        assert!(!tokens_equal(token('a').as_bytes(), token('b').as_bytes()));
        assert!(!tokens_equal(b"short", b"short"));
        assert!(valid_token(&token('a')) && !valid_token("a b") && !valid_token(&token('+')));
    }
}
