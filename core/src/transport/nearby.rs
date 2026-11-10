//! Nearby (`docs/design/nearby.md`): the main transport (Tor) plus a same-Wi-Fi path for the chats
//! where both people turned "Also connect on the same Wi-Fi" on.
//!
//! [`NearbyTransport`] wraps the main transport and keeps its address (the onion). For a peer with
//! a usable route — both sides on, a private address in our subnet — a send tries the local network
//! first and falls back to the main transport on any failure. Each local connection opens with a
//! challenge-response on the chat's two tokens (§5, [`beacon::proof`]): each side proves it holds
//! the token the other gave that chat, over both sides' fresh nonces, so nothing seen on the network
//! can be replayed and the tokens never cross it. Anything else is closed. The tokens only keep
//! strangers and wrong hosts out; what is sent is end-to-end encrypted and authenticated like any
//! frame.
//!
//! The contact's address comes from their announcement over Tor or, when that cannot arrive (no
//! internet, a new network), from their [`beacon`] on the local network, which only the holder of
//! their token can make.
//!
//! This is the second direct (non-Tor) path `ARCHITECTURE.md` §1a allows, and only for a contact
//! whose own app told us, end to end or with a beacon only they can make, where to reach it.

pub mod beacon;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::inbound::{read_frame, InboundBudget};
use super::{AbortHandle, Address, FileFetch, Transport};
use crate::Result;

/// Opening line of a local connection; followed by the dialer's nonce and `\n` (§5).
const PREAMBLE: &[u8] = b"ND-NEARBY/3 ";
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
/// Beacons go out this often, for [`BEACON_BURST`] after listening starts or our networks change,
/// and then stop: a device left on a network does not keep telling it that a Night Drop with Nearby
/// on is there. A contact who arrives later is still found at once: their own burst starts on
/// arrival, and finding a contact answers with ours straight away.
const BEACON_EVERY: Duration = Duration::from_secs(5);
const BEACON_BURST: Duration = Duration::from_secs(120);
/// At most this many beacons are checked a second; the rest are dropped unread, so a flood of
/// junk on the beacon port costs a bounded amount of work.
const MAX_BEACONS_PER_SEC: u32 = 50;
/// How often the beacon threads wake outside a burst (to notice a new network, an answer due, or
/// the stop): a phone in the background need not wake five times a second for them.
const BEACON_IDLE_POLL: Duration = Duration::from_secs(1);
const BEACON_REPORT_EVERY: Duration = Duration::from_secs(60);

/// What the node tells the transport about one chat with Nearby on (ours).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyRoute {
    /// The token we gave this chat: a connection that proves it holds it is theirs, and our
    /// beacons are made with it.
    pub our_token: String,
    /// The token they gave us, once they turned it on too: we prove we hold it, and recognise
    /// their beacons by it.
    pub peer_token: Option<String>,
    /// Where they last announced, over Tor, that they listen. A beacon can find them elsewhere.
    pub peer_at: Option<SocketAddrV4>,
}

/// Where this device's local address comes from (replaced in tests, which run on loopback).
type IpSource = fn() -> Option<Ipv4Addr>;

struct Listening {
    port: u16,
    /// Where we receive beacons, if that port could be bound.
    #[cfg_attr(not(test), allow(dead_code))]
    beacon_port: Option<u16>,
    stop: Arc<AtomicBool>,
}

/// Where beacons go: the local broadcast address on each of our networks, or (tests) one socket.
#[derive(Clone, Copy)]
enum BeaconTarget {
    Broadcast,
    #[cfg(test)]
    To(SocketAddrV4),
}

struct Shared {
    /// Peer onion → route.
    routes: Mutex<HashMap<String, NearbyRoute>>,
    reachable: Mutex<HashMap<String, Instant>>,
    /// When the local path to a peer last failed (see [`RETRY_AFTER`]).
    failed: Mutex<HashMap<String, Instant>>,
    /// Peers with a probe running.
    probing: Mutex<std::collections::HashSet<String>>,
    /// When each peer was last probed: at most once per [`RETRY_AFTER`], whatever came of it. A
    /// probe that ends before connecting (the contact is on another network) records no failure,
    /// and the node probes every poll tick.
    probed: Mutex<HashMap<String, Instant>>,
    budget: InboundBudget,
    connections: AtomicUsize,
    ip_source: IpSource,
    /// The device's interfaces and their IPv4 addresses, as the app last listed them
    /// ([`Transport::set_local_addresses`]). Preferred over `ip_source`, which only sees the
    /// default route: on a phone sharing its connection that is mobile data, not the hotspot.
    interfaces: Mutex<Option<Vec<(String, Ipv4Addr)>>>,
    /// Loopback counts as a local network (tests only).
    allow_loopback: bool,
    /// Peer onion → where a beacon found them, when that differs from their announcement. Held in
    /// memory only; their next announcement over Tor replaces it.
    learned: Mutex<HashMap<String, SocketAddrV4>>,
    /// Send our beacons now (we just found a contact, who may not have found us).
    beacon_now: AtomicBool,
    /// The UDP port we receive beacons on ([`beacon::PORT`]; any free one in tests).
    beacon_port: u16,
    beacon_target: Mutex<BeaconTarget>,
    beacon_stats: Mutex<BeaconStats>,
}

/// Beacon counts since the last diagnostics report ([`Shared::report_beacons`]).
struct BeaconStats {
    since: Instant,
    sent: u32,
    /// Failures by reason.
    failed: std::collections::BTreeMap<String, u32>,
    /// Beacons from our subnet, and how many of those were a contact's.
    heard: u32,
    from_contact: u32,
    /// Packets on the beacon port that were the wrong size or from outside our subnet.
    ignored: u32,
}

impl Default for BeaconStats {
    fn default() -> Self {
        Self {
            since: Instant::now(),
            sent: 0,
            failed: Default::default(),
            heard: 0,
            from_contact: 0,
            ignored: 0,
        }
    }
}

/// Where a beacon from `ip` goes: the subnet's own broadcast address, taken as the /24 (what
/// homes and phone hotspots use), and the limited broadcast. With 255.255.255.255 alone, the
/// beacons of a phone hosting a hotspot never reached the phone on it (S23 → S25, 2026-11-09);
/// Android routes the hotspot by its subnet, which the subnet broadcast follows.
fn broadcast_targets(ip: Ipv4Addr) -> Vec<SocketAddrV4> {
    let [a, b, c, _] = ip.octets();
    vec![
        SocketAddrV4::new(Ipv4Addr::new(a, b, c, 255), beacon::PORT),
        SocketAddrV4::new(Ipv4Addr::BROADCAST, beacon::PORT),
    ]
}

// Generic over the inner transport rather than holding a `Box<dyn Transport>`: the bridge generator
// walks every `Transport` impl, and a struct naming `dyn Transport` sends it round in a circle.
pub struct NearbyTransport<T> {
    inner: T,
    shared: Arc<Shared>,
    listening: Mutex<Option<Listening>>,
    /// When starting to listen last failed: tried again only after [`RETRY_AFTER`], since the node
    /// hands over its routes every poll tick.
    listen_failed: Mutex<Option<Instant>>,
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
                probed: Mutex::new(HashMap::new()),
                budget: InboundBudget::with_limit(INBOUND_BUDGET_BYTES),
                connections: AtomicUsize::new(0),
                ip_source,
                interfaces: Mutex::new(None),
                allow_loopback,
                learned: Mutex::new(HashMap::new()),
                beacon_now: AtomicBool::new(false),
                beacon_port: if allow_loopback { 0 } else { beacon::PORT },
                beacon_target: Mutex::new(BeaconTarget::Broadcast),
                beacon_stats: Mutex::new(BeaconStats::default()),
            }),
            listening: Mutex::new(None),
            listen_failed: Mutex::new(None),
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
        let beacon_port = beacon_receiver(&self.shared, &stop);
        beacon_sender(&self.shared, port, &stop);
        Some(Listening {
            port,
            beacon_port,
            stop,
        })
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
            peer_token: Some(their_token),
            peer_at,
        }) = route
        else {
            anyhow::bail!("no nearby route");
        };
        let learned = self.learned.lock().unwrap().get(peer).copied();
        let Some(addr) = learned.or(peer_at) else {
            anyhow::bail!("no local address for this contact yet");
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

    /// The dialer's side of §5: send our nonce, read theirs, prove we hold `their_token`, and
    /// check they prove they hold `our_token`.
    fn exchange(addr: SocketAddrV4, their_token: &str, our_token: &str) -> Result<TcpStream> {
        use beacon::{proof, Role, NONCE_CHARS, PROOF_CHARS};
        let mut stream = TcpStream::connect_timeout(&SocketAddr::V4(addr), CONNECT_TIMEOUT)?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let ours = beacon::fresh_nonce();
        stream.write_all(&[PREAMBLE, ours.as_bytes(), b"\n"].concat())?;
        let line = read_line::<{ NONCE_CHARS + 1 }>(&mut stream)?;
        let theirs = &line[..NONCE_CHARS];
        let mine = proof(their_token, Role::Dialer, ours.as_bytes(), theirs);
        stream.write_all(&[mine.as_bytes(), b"\n"].concat())?;
        let answer = read_line::<{ PROOF_CHARS + 1 }>(&mut stream)?;
        let want = proof(our_token, Role::Listener, ours.as_bytes(), theirs);
        anyhow::ensure!(
            bytes_equal(&answer[..PROOF_CHARS], want.as_bytes()),
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

    /// One beacon per chat where both have Nearby on, from each of our local networks. A chat whose
    /// contact has not turned it on gets none: nobody could use it, and it would only add to what
    /// the network sees.
    fn send_beacons(&self, networks: &[Ipv4Addr], listen_port: u16) {
        let tokens: Vec<String> = self
            .routes
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.peer_token.is_some())
            .map(|r| r.our_token.clone())
            .collect();
        if tokens.is_empty() {
            return;
        }
        let target = *self.beacon_target.lock().unwrap();
        let window = beacon::window_now();
        let mut stats = self.beacon_stats.lock().unwrap();
        for &ip in networks {
            let socket = match UdpSocket::bind((ip, 0)) {
                Ok(s) => s,
                Err(e) => {
                    *stats
                        .failed
                        .entry(format!("bind: {}", e.kind()))
                        .or_default() += 1;
                    continue;
                }
            };
            let to = match target {
                BeaconTarget::Broadcast => {
                    if let Err(e) = socket.set_broadcast(true) {
                        *stats
                            .failed
                            .entry(format!("broadcast: {}", e.kind()))
                            .or_default() += 1;
                        continue;
                    }
                    broadcast_targets(ip)
                }
                #[cfg(test)]
                BeaconTarget::To(to) => vec![to],
            };
            for token in &tokens {
                let packet = beacon::make(token, ip, listen_port, window);
                for to in &to {
                    match socket.send_to(&packet, to) {
                        Ok(_) => stats.sent += 1,
                        Err(e) => {
                            *stats.failed.entry(format!("send: {e}")).or_default() += 1;
                        }
                    }
                }
            }
        }
    }

    /// Once a minute in a diagnostics build: what the beacons did, so a network that drops them
    /// shows up.
    fn report_beacons(&self, networks: &[Ipv4Addr]) {
        let mut stats = self.beacon_stats.lock().unwrap();
        if stats.since.elapsed() < BEACON_REPORT_EVERY {
            return;
        }
        let s = std::mem::take(&mut *stats);
        if s.sent == 0 && s.heard == 0 && s.ignored == 0 && s.failed.is_empty() {
            return; // quiet between bursts: nothing to say
        }
        let failed: Vec<String> = s.failed.iter().map(|(e, n)| format!("{n}× {e}")).collect();
        crate::diag!(
            "nearby: beacons in {}s: sent {} on [{}]{}; heard {} ({} from a contact, {} ignored)",
            s.since.elapsed().as_secs(),
            s.sent,
            networks
                .iter()
                .map(|ip| net_of_ip(*ip))
                .collect::<Vec<_>>()
                .join(", "),
            if failed.is_empty() {
                String::new()
            } else {
                format!(", failed {}", failed.join(", "))
            },
            s.heard,
            s.from_contact,
            s.ignored
        );
    }

    /// A beacon from `from`: if it is a contact's, dial them there from now on.
    fn heard_beacon(&self, packet: &[u8], from: Ipv4Addr) {
        let window = beacon::window_now();
        let found = self.routes.lock().unwrap().iter().find_map(|(onion, r)| {
            let port = beacon::verify(packet, from, r.peer_token.as_deref()?, window)?;
            Some((onion.clone(), SocketAddrV4::new(from, port), r.peer_at))
        });
        {
            let mut stats = self.beacon_stats.lock().unwrap();
            stats.heard += 1;
            stats.from_contact += u32::from(found.is_some());
        }
        let Some((onion, at, announced)) = found else {
            return;
        };
        if !valid_port(at.port()) {
            return;
        }
        let mut learned = self.learned.lock().unwrap();
        let current = learned.get(&onion).copied().or(announced);
        if current == Some(at) {
            return;
        }
        // An address that works stays: a contact on two of our networks sends a beacon from each,
        // and switching on every one would have us both answer each other's beacons for ever.
        if current.is_some_and(|c| self.on_our_network(*c.ip()))
            && self.reachable_recently(&onion, RETRY_AFTER)
            && !self.recently_failed(&onion)
        {
            return;
        }
        if announced == Some(at) {
            learned.remove(&onion);
        } else {
            learned.insert(onion.clone(), at);
        }
        drop(learned);
        crate::diag!(
            "nearby: found a contact on the local network at {} (their beacon)",
            net_of_ip(from)
        );
        // Where they were is no longer where they are: try the new address now.
        self.reachable.lock().unwrap().remove(&onion);
        self.failed.lock().unwrap().remove(&onion);
        self.probed.lock().unwrap().remove(&onion);
        self.beacon_now.store(true, Ordering::Release);
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
    use beacon::{proof, Role, NONCE_CHARS, PROOF_CHARS};
    let opening = read_line::<{ PREAMBLE.len() + NONCE_CHARS + 1 }>(&mut stream)?;
    anyhow::ensure!(opening.starts_with(PREAMBLE), "not a nearby peer");
    let dialer_nonce = &opening[PREAMBLE.len()..PREAMBLE.len() + NONCE_CHARS];
    let ours = beacon::fresh_nonce();
    stream.write_all(&[ours.as_bytes(), b"\n"].concat())?;
    let line = read_line::<{ PROOF_CHARS + 1 }>(&mut stream)?;
    let presented = &line[..PROOF_CHARS];
    // Every route is checked, so the time taken does not say which one came close.
    let mut found = None;
    for (onion, route) in shared.routes.lock().unwrap().iter() {
        let want = proof(
            &route.our_token,
            Role::Dialer,
            dialer_nonce,
            ours.as_bytes(),
        );
        if bytes_equal(presented, want.as_bytes()) {
            if let Some(theirs) = &route.peer_token {
                found = Some((onion.clone(), theirs.clone()));
            }
        }
    }
    let (onion, theirs) = found.ok_or_else(|| {
        anyhow::anyhow!(
            "unknown proof (not a contact with Nearby on, or they have not told us yet)"
        )
    })?;
    crate::diag!("nearby: a contact connected locally");
    let answer = proof(&theirs, Role::Listener, dialer_nonce, ours.as_bytes());
    stream.write_all(&[answer.as_bytes(), b"\n"].concat())?;
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

/// Read exactly `N` bytes ending in `\n` (one handshake line). The caller drops the `\n`.
fn read_line<const N: usize>(stream: &mut TcpStream) -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    stream.read_exact(&mut buf)?;
    anyhow::ensure!(buf[N - 1] == b'\n', "malformed handshake line");
    Ok(buf)
}

/// Receive beacons (§5a) while listening: a contact's beacon, from our own subnet, sets where we
/// dial them. Returns the UDP port, or `None` if it could not be bound (then we can still be
/// found, by our own beacons, but cannot find).
fn beacon_receiver(shared: &Arc<Shared>, stop: &Arc<AtomicBool>) -> Option<u16> {
    let socket = match bind_beacon_socket(shared.beacon_port) {
        Ok(s) => s,
        Err(e) => {
            crate::diag!("nearby: cannot receive beacons ({e}); a contact who moved network is found only over Tor");
            return None;
        }
    };
    let port = socket.local_addr().ok()?.port();
    let (shared, halt) = (Arc::clone(shared), Arc::clone(stop));
    thread::spawn(move || {
        let mut buf = [0u8; beacon::LEN + 1];
        let (mut second, mut checked) = (Instant::now(), 0u32);
        while !halt.load(Ordering::Relaxed) {
            let Ok((n, SocketAddr::V4(from))) = socket.recv_from(&mut buf) else {
                continue; // the read timeout, or IPv6
            };
            if second.elapsed() >= Duration::from_secs(1) {
                (second, checked) = (Instant::now(), 0);
            }
            checked += 1;
            if n == beacon::LEN
                && checked <= MAX_BEACONS_PER_SEC
                && shared.admits_source(*from.ip())
            {
                shared.heard_beacon(&buf[..n], *from.ip());
            } else {
                shared.beacon_stats.lock().unwrap().ignored += 1;
            }
        }
    });
    Some(port)
}

/// The beacon socket. Shared with any other Night Drop on this device (a second install, a
/// second account on a desktop): each gets every broadcast.
fn bind_beacon_socket(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).into())?;
    let socket: UdpSocket = socket.into();
    socket.set_read_timeout(Some(BEACON_IDLE_POLL))?;
    Ok(socket)
}

/// Send our beacons while listening: every [`BEACON_EVERY`] for [`BEACON_BURST`] after we start or
/// our networks change, then nothing; and once, no more often than every [`BEACON_EVERY`], when we
/// have just found a contact (who may not have found us).
fn beacon_sender(shared: &Arc<Shared>, listen_port: u16, stop: &Arc<AtomicBool>) {
    let (shared, halt) = (Arc::clone(shared), Arc::clone(stop));
    thread::spawn(move || {
        let mut networks = Vec::new();
        let mut burst_until = Instant::now() + BEACON_BURST;
        let mut last: Option<Instant> = None;
        while !halt.load(Ordering::Relaxed) {
            let now = Instant::now();
            let ours = shared.local_ips();
            if ours != networks {
                networks = ours;
                burst_until = now + BEACON_BURST;
                last = None;
            }
            shared.report_beacons(&networks);
            let rested = last.is_none_or(|t| now.duration_since(t) >= BEACON_EVERY);
            let answer = rested && shared.beacon_now.swap(false, Ordering::AcqRel);
            if rested && (now < burst_until || answer) {
                shared.send_beacons(&networks, listen_port);
                last = Some(now);
            }
            thread::sleep(if now < burst_until {
                ACCEPT_POLL
            } else {
                BEACON_IDLE_POLL
            });
        }
    });
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

/// Constant-time equality for two byte strings of the same length.
fn bytes_equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
    // Mobile data first (Qualcomm rmnet, MediaTek ccmni/ccinet, Unisoc seth, Exynos pdp/rmnet,
    // laptop modems wwan/wwp*, Windows "Cellular", Android's 464xlat v4-/clat), then VPNs and
    // virtual bridges.
    const NEVER: [&str; 21] = [
        "lo", "rmnet", "ccmni", "ccinet", "seth", "pdp", "wwan", "wwp", "cellular", "clat", "v4-",
        "tun", "tap", "ppp", "wg", "utun", "ipsec", "dummy", "docker", "veth", "virbr",
    ];
    if NEVER.iter().any(|p| n.starts_with(p))
        || ["br-", "podman", "vboxnet"]
            .iter()
            .any(|p| n.starts_with(p))
    {
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
        {
            let mut current = self.shared.routes.lock().unwrap();
            // A newer announcement from the contact replaces what a beacon found.
            self.shared.learned.lock().unwrap().retain(|onion, _| {
                match (current.get(onion), routes.get(onion)) {
                    (Some(old), Some(new)) => {
                        old.peer_at == new.peer_at && old.peer_token == new.peer_token
                    }
                    _ => false,
                }
            });
            self.shared
                .probed
                .lock()
                .unwrap()
                .retain(|onion, _| routes.contains_key(onion));
            *current = routes;
        }
        let mut listening = self.listening.lock().unwrap();
        match (listen, listening.is_some()) {
            (true, false) => {
                let mut failed = self.listen_failed.lock().unwrap();
                if failed.is_none_or(|t| t.elapsed() >= RETRY_AFTER) {
                    *listening = self.start_listening();
                    *failed = listening.is_none().then(Instant::now);
                }
            }
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
            .filter(|(onion, r)| {
                r.peer_token.is_some()
                    && (r.peer_at.is_some()
                        || self.shared.learned.lock().unwrap().contains_key(*onion))
            })
            .map(|(onion, _)| onion.clone())
            .filter(|onion| {
                !self.shared.reachable_recently(onion, RETRY_AFTER)
                    && !self.shared.recently_failed(onion)
                    && self
                        .shared
                        .probed
                        .lock()
                        .unwrap()
                        .get(onion)
                        .is_none_or(|t| t.elapsed() >= RETRY_AFTER)
            })
            .collect();
        for onion in due {
            if !self.shared.probing.lock().unwrap().insert(onion.clone()) {
                continue;
            }
            self.shared
                .probed
                .lock()
                .unwrap()
                .insert(onion.clone(), Instant::now());
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
                peer_token: None,
                peer_at: None,
            },
        )]));
        b.set_nearby(HashMap::from([(
            "a.onion".into(),
            NearbyRoute {
                our_token: tb.clone(),
                peer_token: None,
                peer_at: None,
            },
        )]));
        let (pa, pb) = (a.nearby_address().unwrap(), b.nearby_address().unwrap());
        a.set_nearby(HashMap::from([(
            "b.onion".into(),
            NearbyRoute {
                our_token: ta.clone(),
                peer_token: Some(tb.clone()),
                peer_at: Some(pb),
            },
        )]));
        b.set_nearby(HashMap::from([(
            "a.onion".into(),
            NearbyRoute {
                our_token: tb,
                peer_token: Some(ta),
                peer_at: Some(pa),
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

    /// Only tokens known, no address (they moved network with no internet): a's beacon tells b
    /// where a is, and b's frame then goes over the local network with the main transport down.
    #[test]
    fn a_beacon_finds_a_contact_whose_address_we_never_heard() {
        let (net, a, b) = pair();
        let (ta, tb) = (token('a'), token('b'));
        let route = |ours: &str, theirs: &str| NearbyRoute {
            our_token: ours.into(),
            peer_token: Some(theirs.into()),
            peer_at: None,
        };
        b.set_nearby(HashMap::from([("a.onion".into(), route(&tb, &ta))]));
        let b_beacons = b
            .listening
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .beacon_port
            .unwrap();
        *a.shared.beacon_target.lock().unwrap() =
            BeaconTarget::To(SocketAddrV4::new(Ipv4Addr::LOCALHOST, b_beacons));
        net.disconnect("a.onion");
        assert!(
            b.send("a.onion", b"x").is_err(),
            "no address yet, main path down"
        );
        b.shared.failed.lock().unwrap().clear();
        a.set_nearby(HashMap::from([("b.onion".into(), route(&ta, &tb))]));
        for _ in 0..100 {
            if b.shared.learned.lock().unwrap().contains_key("a.onion") {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            b.shared.learned.lock().unwrap().get("a.onion").copied(),
            a.nearby_address(),
            "b learned where a listens"
        );
        b.send("a.onion", b"found you").unwrap();
        assert_eq!(recv(&a).as_deref(), Some(&b"found you"[..]));
        // a's next announcement over Tor replaces what the beacon said.
        let elsewhere = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 40_000);
        b.set_nearby(HashMap::from([(
            "a.onion".into(),
            NearbyRoute {
                peer_at: Some(elsewhere),
                ..route(&tb, &ta)
            },
        )]));
        assert!(b.shared.learned.lock().unwrap().is_empty());
    }

    #[test]
    fn a_beacon_made_with_another_token_or_from_another_address_is_ignored() {
        let (_net, _a, b) = pair();
        b.set_nearby(HashMap::from([(
            "a.onion".into(),
            NearbyRoute {
                our_token: token('b'),
                peer_token: Some(token('a')),
                peer_at: None,
            },
        )]));
        let lo = Ipv4Addr::LOCALHOST;
        let w = beacon::window_now();
        b.shared
            .heard_beacon(&beacon::make(&token('z'), lo, 40_001, w), lo);
        b.shared.heard_beacon(
            &beacon::make(&token('a'), Ipv4Addr::new(127, 0, 0, 2), 40_001, w),
            lo,
        );
        b.shared
            .heard_beacon(&beacon::make(&token('a'), lo, 9100, w), lo);
        assert!(b.shared.learned.lock().unwrap().is_empty());
        b.shared
            .heard_beacon(&beacon::make(&token('a'), lo, 40_001, w), lo);
        assert_eq!(
            b.shared.learned.lock().unwrap().get("a.onion").copied(),
            Some(SocketAddrV4::new(lo, 40_001))
        );
    }

    /// Someone on the network who recorded a whole handshake cannot replay it: the listener's
    /// nonce is new each time, so the recorded proof no longer matches and nothing gets in.
    #[test]
    fn a_recorded_handshake_cannot_be_replayed() {
        use beacon::{proof, Role, NONCE_CHARS};
        let (_net, a, b) = pair();
        connect(&a, &b);
        let pb = b.nearby_address().unwrap();
        let open = |nonce: &str| {
            let mut s = TcpStream::connect(SocketAddr::V4(pb)).unwrap();
            s.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).unwrap();
            s.write_all(&[PREAMBLE, nonce.as_bytes(), b"\n"].concat())
                .unwrap();
            let line = read_line::<{ NONCE_CHARS + 1 }>(&mut s).unwrap();
            (s, line[..NONCE_CHARS].to_vec())
        };
        // A genuine handshake (a holds b's token), recorded.
        let nonce = beacon::fresh_nonce();
        let (mut first, theirs) = open(&nonce);
        let recorded = proof(&token('b'), Role::Dialer, nonce.as_bytes(), &theirs);
        first
            .write_all(&[recorded.as_bytes(), b"\n"].concat())
            .unwrap();
        assert!(read_line::<{ beacon::PROOF_CHARS + 1 }>(&mut first).is_ok());
        // Replayed: same opening, same proof, but b chose a new nonce.
        let (mut again, fresh) = open(&nonce);
        assert_ne!(theirs, fresh);
        again
            .write_all(&[recorded.as_bytes(), b"\n"].concat())
            .unwrap();
        assert!(
            read_line::<{ beacon::PROOF_CHARS + 1 }>(&mut again).is_err(),
            "closed without an answer"
        );
    }

    /// A contact on another network is probed once a minute, not on every poll tick: that probe
    /// ends before connecting and records no failure.
    #[test]
    fn a_contact_on_another_network_is_not_probed_every_tick() {
        let (_net, a, _b) = pair();
        a.set_nearby(HashMap::from([(
            "b.onion".into(),
            NearbyRoute {
                our_token: token('a'),
                peer_token: Some(token('b')),
                peer_at: Some("192.168.77.5:40000".parse().unwrap()),
            },
        )]));
        a.nearby_probe();
        let first = a.shared.probed.lock().unwrap()["b.onion"];
        thread::sleep(Duration::from_millis(50));
        for _ in 0..5 {
            a.nearby_probe();
        }
        assert_eq!(
            a.shared.probed.lock().unwrap()["b.onion"],
            first,
            "not probed again within the minute"
        );
        // Their beacon from a new place lets the next probe go at once.
        let lo = Ipv4Addr::LOCALHOST;
        a.shared.heard_beacon(
            &beacon::make(&token('b'), lo, 40_001, beacon::window_now()),
            lo,
        );
        assert!(!a.shared.probed.lock().unwrap().contains_key("b.onion"));
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
                peer_token: Some(token('z')),
                peer_at: Some(pb),
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
                peer_token: Some(token('b')),
                peer_at: Some(pb),
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
            "wwan0",
            "wwp0s20f0u2",
            "seth_lte0",
            "ccinet0",
            "pdp_ip0",
            "vboxnet0",
            "Cellular",
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
        assert!(bytes_equal(token('a').as_bytes(), token('a').as_bytes()));
        assert!(!bytes_equal(token('a').as_bytes(), token('b').as_bytes()));
        assert!(!bytes_equal(b"short", b"shorter"));
        assert!(valid_token(&token('a')) && !valid_token("a b") && !valid_token(&token('+')));
    }
}
