//! Embedded Tor transport via [arti](https://gitlab.torproject.org/tpo/core/arti)
//! (`ARCHITECTURE.md` §6), behind the `tor` feature. It implements the synchronous
//! [`Transport`] trait by running an async arti client + onion service on a background
//! tokio runtime and bridging inbound frames over a channel — async stays isolated here.
//!
//! * **Address**: our `.onion` (the onion service hides the *caller*, so peers learn our
//!   reply address from the `Hello` frame, not from the transport).
//! * **Inbound**: launch an onion service; for each rendezvous stream, read one
//!   length-prefixed frame and hand it to the core.
//! * **Outbound**: dial the peer's `.onion` and write one length-prefixed frame.
//!
//! NOTE: bootstrapping a real Tor circuit needs the network and takes time, so this path
//! is not exercised by the unit tests; it is compiled under `--features tor`.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use arti_client::config::pt::TransportConfigBuilder;
use arti_client::config::{BridgeConfigBuilder, PtTransportName, TorClientConfigBuilder};
use arti_client::{DataStream, TorClient, TorClientConfig};
use futures::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, StreamExt};
use safelog::DisplayRedacted as _;
use tokio::runtime::Runtime;
use tor_cell::relaycell::msg::Connected;
use tor_config_path::CfgPath;
use tor_hscrypto::pk::HsId;
use tor_hsservice::config::restricted_discovery::DirectoryKeyProviderBuilder;
use tor_hsservice::config::{OnionServiceConfig, OnionServiceConfigBuilder};
use tor_hsservice::{handle_rend_requests, HsNickname, RunningOnionService};
use tor_keymgr::KeystoreSelector;
use tor_llcrypto::pk::ed25519;
use tor_rtcompat::PreferredRuntime;

use crate::lifecycle::{ExitGuard, StopSignal};
// The virtual port a relay's onion is dialed on lives with the protocol, so the core's transport
// and the relay binary's own self-dial watchdog cannot drift apart on it.
use crate::relay_client::RELAY_PORT;
use crate::transport::{client_auth, Address, Transport};
use crate::Result;

/// Install a tracing subscriber that forwards arti's own logs to the diagnostics channel
/// (`nd-tor` on Android / stderr on desktop). Guarded by a `Once` and only ever called when
/// diagnostics are enabled, so a normal release never installs it and stays silent. Filters to the
/// bootstrap-relevant targets (guards, circuits, directory download, channels) at debug.
fn install_arti_tracing() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        use tracing_subscriber::fmt;
        use tracing_subscriber::EnvFilter;
        let filter = EnvFilter::try_new(
            "info,arti_client=debug,tor_guardmgr=debug,tor_circmgr=debug,tor_dirmgr=debug,\
             tor_chanmgr=debug,tor_netdir=info,tor_proto=info,tor_hsservice=debug",
        )
        .unwrap_or_else(|_| EnvFilter::new("info"));
        let _ = fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_target(true)
            .with_writer(ArtiDiagWriter::default)
            .try_init();
    });
}

/// Per-`connect()` chatter with no diagnostic value, dropped before it reaches the log.
///
/// Every relay request opens a fresh stream, and arti logs both of these each time — so while a
/// short-code invite is outstanding (rendezvous polling every 2s, several requests per poll) they
/// arrive in a steady stream and bury everything else. Filtered by message rather than by lowering
/// `arti_client`/`tor_dirmgr` to `info`, because their debug output is precisely what explains a
/// stuck bootstrap, which is what this log exists for. Seen flooding a device log 2026-08-02.
const ARTI_NOISE: [&str; 2] = [
    "Attempted to bootstrap twice; ignoring",
    "It appears we have the lock on our state files",
];

/// A `std::io::Write` that turns each completed line from the tracing formatter into one
/// [`crate::diag::emit_tor`] call. A fresh instance is made per event (one line ending in `\n`).
#[derive(Default)]
struct ArtiDiagWriter {
    buf: Vec<u8>,
}
impl std::io::Write for ArtiDiagWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end();
            if !ARTI_NOISE.iter().any(|n| line.contains(n)) {
                crate::diag::emit_tor(line);
            }
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            crate::diag::emit_tor(String::from_utf8_lossy(&self.buf).trim_end());
            self.buf.clear();
        }
        Ok(())
    }
}

/// The virtual port our onion service exposes (peers dial `<onion>:NIGHTDROP_PORT`).
const NIGHTDROP_PORT: u16 = 9001;

/// Upper bound on the initial Tor bootstrap. Tor normally connects in well under a minute, but on a
/// blocked/censored or dead network `create_bootstrapped` would otherwise wait indefinitely, leaving
/// the app on a spinner with no way out. On timeout we return a clear error so the UI can offer a
/// retry (and the user can configure bridges — see `docs/bridges.md`). Generous, to avoid failing a
/// merely-slow first run.
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(120);

/// Close a kept-warm outbound stream once it's been idle this long (privacy: don't hold a
/// continuously-observable connection longer than needed; per-peer, so contacts stay
/// circuit-isolated by arti's per-`.onion` routing).
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// How often the reaper looks for streams that have gone idle. Interruptible (it sleeps on the
/// transport's [`StopSignal`]), so this only sets how *late* a stream is closed, never how long a
/// teardown waits.
const IDLE_SWEEP: Duration = Duration::from_secs(30);

/// How long [`TorTransport`]'s drop waits for the idle reaper to let go of the tokio runtime.
/// The reaper wakes from the stop signal immediately, so reaching this bound means it is stuck
/// closing a stream; we log and move on rather than block a logout or a wipe.
const REAPER_EXIT_TIMEOUT: Duration = Duration::from_secs(1);

/// How often an in-flight relay request re-checks whether the transport is closing. Only ticks
/// while a request is actually outstanding, and 100ms is far below every timeout it races.
const CLOSE_CHECK_INTERVAL: Duration = Duration::from_millis(100);

/// Cap a single **direct peer** dial. `send` runs while the core lock is held, so an unbounded
/// dial to an offline peer would freeze every other UI call (and the poller) for as long as arti
/// keeps retrying — up to minutes with a high `HS_CONNECT_ATTEMPTS`. An online peer connects well
/// within this; a slow/offline one fails fast so `send` falls back to the relay (or defers the
/// retry to the poller) instead of blocking. The message is never lost — only delivery is deferred.
const PEER_DIAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Cap a single **relay** connect. Bounds a hung dial without defeating persistence: the callers
/// that need to punch through a flaky path (pairing's `run_join_handshake`, the relay-retry
/// poller) loop over their own schedule, each iteration a fresh, individually-bounded attempt.
const RELAY_DIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Outbound streams we hold open per peer to avoid re-dialing the onion for every frame.
type OutStreams = Arc<Mutex<HashMap<Address, (DataStream, Instant)>>>;

/// A [`Transport`] backed by an embedded Tor client + onion service.
pub struct TorTransport {
    onion: Address,
    runtime: Arc<Runtime>,
    client: Arc<TorClient<PreferredRuntime>>,
    inbound: Mutex<Receiver<(Address, Vec<u8>)>>,
    /// Warm outbound streams, one per peer `.onion` (reused across frames, idle-closed).
    out_streams: OutStreams,
    /// Raised when this transport is dropped. Stops the idle reaper *and* cancels any relay
    /// request in flight on a dialer we handed out — see [`Self::make_relay_dialer`].
    closing: Arc<StopSignal>,
    /// Directory of authorized-client keys backing onion client authorization (restricted
    /// discovery, #22). The node writes/removes `<nickname>.auth` files here as contacts are
    /// added/deleted (see [`client_auth`]); arti watches it. `None` = client auth not in use.
    auth_dir: Option<String>,
    // Keep the running service alive for the lifetime of the transport.
    _service: Arc<RunningOnionService>,
    /// The service nickname, kept so [`onion_key_material`](Self::onion_key_material) can name the
    /// identity key in the keystore after launch.
    nickname: HsNickname,
    /// Last onion-service state seen by [`published`](Transport::published), so only transitions
    /// are logged rather than one line per poll.
    last_state: Mutex<String>,
    /// Whether the service has ever been fully reachable this run — see
    /// [`published`](Transport::published) for why that answer is monotonic.
    ever_published: std::sync::atomic::AtomicBool,
}

impl Drop for TorTransport {
    fn drop(&mut self) {
        // Stop the idle reaper and wait for it, bounded. The reaper holds an `Arc<Runtime>`, and
        // arti's own background tasks live on that runtime holding the state manager that owns the
        // exclusive on-disk lock — so a reaper still asleep keeps that lock held after the
        // transport is otherwise gone. It used to sleep a flat 30s between sweeps, which is a
        // 30-second window in which a rebuilt client (guard heal, restore) comes up read-only.
        self.closing.stop();
        if !self.closing.wait_for_exit(REAPER_EXIT_TIMEOUT) {
            crate::diag!(
                "tor: the idle-stream reaper is still running {}s after the transport was \
                 dropped — arti's state lock is released late, so a client rebuilt right now may \
                 come up read-only",
                REAPER_EXIT_TIMEOUT.as_secs()
            );
        }
    }
}

impl TorTransport {
    /// Bootstrap Tor, launch our onion service, and start accepting inbound frames.
    /// Blocking; can take a while on first run (descriptor publication).
    ///
    /// `state_dir`, when set, is a writable base directory for arti's state + cache. It is
    /// **required on Android** (the default `${ARTI_LOCAL_DATA}` does not resolve in the app
    /// sandbox); on desktop, pass `None` to use arti's standard per-user locations.
    /// `onion_key` is the 64-byte expanded ed25519 secret for our onion identity, held in our own
    /// sealed store rather than arti's keystore (`docs/design/onion-key-at-rest.md`). `Some` on
    /// every run after the first: it is inserted into the in-memory keystore **before** the service
    /// launches, so the address survives restarts. `None` means "first run" and lets arti generate
    /// one, which the caller then reads back with [`onion_key_material`](Self::onion_key_material)
    /// and persists.
    ///
    /// Passing `None` when a key *does* exist would mint a **new identity** — a new address, every
    /// contact stranded with no notice. The caller must never do that on a failed read; it must
    /// fail instead. See §4 of the design note.
    pub fn bootstrap(
        nickname: &str,
        state_dir: Option<&str>,
        client_auth_dir: Option<&str>,
        onion_key: Option<[u8; 64]>,
    ) -> Result<Self> {
        // rustls 0.23 needs a process-default crypto provider before any TLS config is
        // built; install ring once (ignore the error if another call already did).
        let _ = rustls::crypto::ring::default_provider().install_default();
        // When diagnostics are on, route arti's own tracing (guard/circuit/dir-download/bootstrap)
        // to the diag channel so a stuck bootstrap in the field shows *why*. No-op in a release.
        if crate::diag::enabled() {
            install_arti_tracing();
        }
        let on_disk_keystore = keystore_is_on_disk(state_dir, nickname, onion_key.is_some());
        if on_disk_keystore {
            crate::diag!(
                "tor: reading the onion identity from the on-disk keystore once, to move it into \
                 the sealed store; later runs keep the keystore in memory"
            );
        }
        if !on_disk_keystore {
            forget_stale_ipts(state_dir, nickname);
        }
        let config = tor_config(state_dir, on_disk_keystore)?;
        let runtime = Arc::new(Runtime::new().context("tokio runtime")?);
        let nickname_owned: HsNickname = nickname.parse().context("onion service nickname")?;
        let auth_dir = client_auth_dir.map(str::to_string);
        let auth_dir_for_svc = auth_dir.clone();
        let (onion, client, service, inbound_rx) = runtime.block_on(async {
            // Bound the bootstrap so a blocked/dead network fails cleanly instead of hanging the
            // app forever (the UI then offers a retry; bridges can help — docs/bridges.md).
            let client =
                tokio::time::timeout(BOOTSTRAP_TIMEOUT, TorClient::create_bootstrapped(config))
                    .await
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "Tor didn't connect within {}s — check your connection and try again",
                            BOOTSTRAP_TIMEOUT.as_secs()
                        )
                    })?
                    .context("bootstrap Tor")?;

            let nickname: HsNickname = nickname.parse().context("onion service nickname")?;
            // Restore our identity into the in-memory keystore before the service starts. arti
            // would otherwise generate a fresh one and we would come up on a different address.
            if let Some(bytes) = onion_key {
                let expanded = ed25519::ExpandedKeypair::from_secret_key_bytes(bytes)
                    .ok_or_else(|| anyhow::anyhow!("stored onion key is malformed"))?;
                let spec = tor_hsservice::HsIdKeypairSpecifier::new(nickname.clone());
                client
                    .keymgr()
                    .context("keystore unavailable")?
                    .insert(
                        tor_hscrypto::pk::HsIdKeypair::from(expanded),
                        &spec,
                        KeystoreSelector::Primary,
                        true,
                    )
                    .context("restore onion identity into the keystore")?;
                crate::diag!("tor: restored the saved onion identity into the in-memory keystore");
            } else {
                crate::diag!("tor: no saved onion identity — arti will generate one (first run)");
            }
            // Whether our onion is restricted decides who can reach us at all: a peer we haven't
            // authorized can't even fetch the descriptor, so a brand-new contact's Hello has to
            // come via the relay (#22). Worth knowing when a chat request never arrives (#6).
            let restricted = auth_dir_for_svc
                .as_deref()
                .map(|d| client_auth::authorized_count(std::path::Path::new(d)))
                .unwrap_or(0);
            crate::diag!(
                "tor: launching onion service — restricted discovery {} ({restricted} authorized \
                 client(s)); while restricted, unauthorized peers must reach us via the relay",
                if restricted > 0 { "ON" } else { "off" }
            );
            let svc_config = onion_service_config(nickname, auth_dir_for_svc.as_deref())?;
            let (service, rend_requests) = client
                .launch_onion_service(svc_config)
                .context("launch onion service")?
                .context("onion service did not start")?;
            let onion = service
                .onion_address()
                .context("onion service has no address yet")?
                .display_unredacted()
                .to_string();

            // Accept rendezvous streams; a kept-warm stream carries MANY length-prefixed
            // frames, so read in a loop until the peer closes it.
            let (tx, rx) = channel::<(Address, Vec<u8>)>();
            let mut streams = handle_rend_requests(rend_requests);
            tokio::spawn(async move {
                while let Some(request) = streams.next().await {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        if let Ok(mut stream) = request.accept(Connected::new_empty()).await {
                            // The caller is anonymous at the transport layer; routing uses
                            // the frame's own ids / advertised reply address.
                            while let Ok(frame) = read_frame(&mut stream).await {
                                let _ = tx.send((Address::new(), frame));
                            }
                        }
                    });
                }
            });

            Ok::<_, anyhow::Error>((onion, client, service, rx))
        })?;

        let out_streams: OutStreams = Arc::new(Mutex::new(HashMap::new()));
        let closing = StopSignal::new();
        spawn_idle_reaper(
            Arc::clone(&runtime),
            Arc::clone(&out_streams),
            Arc::clone(&closing),
        );

        Ok(Self {
            onion,
            runtime,
            client,
            inbound: Mutex::new(inbound_rx),
            out_streams,
            closing,
            auth_dir,
            _service: service,
            nickname: nickname_owned,
            last_state: Mutex::new(String::from("<start>")),
            ever_published: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Generate (and store, in arti's keymgr) *our* client descriptor-encryption keypair for
    /// connecting to peer `peer_onion`'s restricted onion, returning the **public** key string
    /// (`descriptor:x25519:…`) to hand the peer during pairing so they can authorize us (#22).
    /// arti uses the stored keypair automatically on future connects to that onion.
    pub fn make_service_discovery_key(&self, peer_onion: &str) -> Result<(String, [u8; 32])> {
        let hsid = HsId::from_str(peer_onion).context("parse peer onion address")?;
        // Generated here rather than by `generate_service_discovery_key`, because arti will only
        // hand back the *public* half afterwards — and we need the secret to persist. Minting it
        // ourselves is the only way to keep a copy.
        let mut bytes = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
        let secret = tor_hscrypto::pk::HsClientDescEncSecretKey::from(
            tor_llcrypto::pk::curve25519::StaticSecret::from(bytes),
        );
        let public = self
            .client
            .insert_service_discovery_key(KeystoreSelector::Primary, hsid, secret)
            .context("install client service-discovery key")?;
        Ok((public.to_string(), bytes))
    }

    /// The 64-byte expanded ed25519 secret for our onion identity, read back out of the in-memory
    /// keystore so the caller can seal it into our own store
    /// (`docs/design/onion-key-at-rest.md`).
    ///
    /// Called once, after a first-run bootstrap that let arti generate the identity. Returns `None`
    /// only if the keystore has no such key, which would mean the service launched without one —
    /// the caller must treat that as a failure rather than carrying on, or the next start mints a
    /// different address.
    pub fn onion_key_material(&self) -> Option<[u8; 64]> {
        let spec = tor_hsservice::HsIdKeypairSpecifier::new(self.nickname.clone());
        let keypair: tor_hscrypto::pk::HsIdKeypair =
            self.client.keymgr().ok()?.get(&spec).ok()??;
        let expanded: &ed25519::ExpandedKeypair = keypair.as_ref();
        Some(expanded.to_secret_key_bytes())
    }

    /// A [`RelayDialer`](crate::relay_client::RelayDialer) that round-trips one newline-JSON relay
    /// request to `relay_onion` over this Tor client. Build it **before** the transport is moved
    /// into the node (it captures clones of the arti client + runtime), then hand it to
    /// `RelayClient::with_dialer`. This is how the relay is reached over Tor (§11.2).
    ///
    /// The request is raced against this transport's `closing` signal, so a dial in flight when the
    /// transport is dropped gives up within [`CLOSE_CHECK_INTERVAL`] instead of holding the arti
    /// client for the rest of its [`RELAY_DIAL_TIMEOUT`]. That matters because the background
    /// poller runs these off the core lock: without the cancellation, a teardown during a relay
    /// poll leaves the poller — and therefore arti's on-disk state lock — alive for up to 30s, and
    /// a core rebuilt in that window comes up read-only (see [`NightdropCore::shutdown`]).
    ///
    /// [`NightdropCore::shutdown`]: crate::api::NightdropCore::shutdown
    pub fn make_relay_dialer(&self, relay_onion: String) -> crate::relay_client::RelayDialer {
        let client = Arc::clone(&self.client);
        let runtime = Arc::clone(&self.runtime);
        let closing = Arc::clone(&self.closing);
        Arc::new(move |request_line: &str| -> Result<String> {
            let req = if request_line.ends_with('\n') {
                request_line.to_string()
            } else {
                format!("{request_line}\n")
            };
            runtime.block_on(async {
                let exchange = async {
                    let mut stream = tokio::time::timeout(
                        RELAY_DIAL_TIMEOUT,
                        client.connect((relay_onion.as_str(), RELAY_PORT)),
                    )
                    .await
                    .map_err(|_| anyhow::anyhow!("relay dial timed out"))?
                    .with_context(|| format!("relay connect {relay_onion}"))?;
                    stream.write_all(req.as_bytes()).await?;
                    stream.flush().await?;
                    let mut reader = futures::io::BufReader::new(stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).await?;
                    Ok(line)
                };
                // `futures::select` rather than `tokio::select!` so this needs no new dependency
                // (tokio's `macros` feature); the two futures are pinned locally and neither is
                // resumed after the race, so dropping the loser is the cancellation.
                let exchange = std::pin::pin!(exchange);
                let closed = std::pin::pin!(transport_closed(&closing));
                match futures::future::select(exchange, closed).await {
                    futures::future::Either::Left((result, _)) => result,
                    futures::future::Either::Right(((), _)) => Err(anyhow::anyhow!(
                        "relay request abandoned: the transport is closing"
                    )),
                }
            })
        })
    }
}

/// Completes once the transport that owns `closing` has been dropped. Polled rather than awaited
/// on a channel so it can be raced against any request future without threading a cancellation
/// token through arti; it only runs while a request is outstanding.
async fn transport_closed(closing: &StopSignal) {
    while !closing.stopped() {
        tokio::time::sleep(CLOSE_CHECK_INTERVAL).await;
    }
}

/// Periodically close outbound streams that have been idle past [`IDLE_TIMEOUT`].
///
/// Sleeps on `closing` rather than the clock so that dropping the transport ends this thread at
/// once: it holds the tokio runtime the arti client runs on, and [`Drop for TorTransport`] waits
/// for it before returning.
///
/// [`Drop for TorTransport`]: TorTransport
fn spawn_idle_reaper(runtime: Arc<Runtime>, streams: OutStreams, closing: Arc<StopSignal>) {
    std::thread::spawn(move || {
        let _exit = ExitGuard::new(Arc::clone(&closing));
        while closing.sleep(IDLE_SWEEP) {
            let stale: Vec<DataStream> = {
                let mut map = streams.lock().unwrap();
                let now = Instant::now();
                let keys: Vec<Address> = map
                    .iter()
                    .filter(|(_, (_, last))| now.duration_since(*last) > IDLE_TIMEOUT)
                    .map(|(k, _)| k.clone())
                    .collect();
                keys.into_iter()
                    .filter_map(|k| map.remove(&k).map(|(s, _)| s))
                    .collect()
            };
            for mut s in stale {
                let _ = runtime.block_on(async { s.close().await });
            }
        }
        // Release the runtime — and with it arti's tasks and its state lock — *before* the exit
        // guard fires, since the guard's promise to `wait_for_exit` is "this thread holds nothing
        // any more". A closure's captures outlive its body, so this has to be explicit.
        drop(streams);
        drop(runtime);
    });
}

impl Transport for TorTransport {
    fn address(&self) -> Address {
        self.onion.clone()
    }

    /// True once the onion descriptor is published and the service is reachable (arti reports
    /// `Running`/`DegradedReachable`). False during the ~1–3 min republish window after launch.
    ///
    /// **Monotonic within a run.** A published descriptor stays valid on the HSDirs for hours, so
    /// once we have seen the service reachable, a later dip in arti's aggregate state does not mean
    /// peers can no longer find us — it usually means an introduction point is being replaced. This
    /// used to un-publish us on every such dip, which told the user "others can't pair with you yet"
    /// about a service that was serving fine.
    ///
    /// The state is logged on every **change**, because getting this boolean wrong was expensive:
    /// a device measured on 2026-08-03 published 8/8 HSDirs on both time periods with 4/4 good IPTs
    /// and zero upload failures, and still reported not-ready for eight minutes. Only transitions
    /// are logged, so this is a handful of lines per session, not a poll-rate firehose.
    fn published(&self) -> bool {
        let state = self._service.status().state();
        let ready = state.is_fully_reachable()
            || self
                .ever_published
                .load(std::sync::atomic::Ordering::Relaxed);
        if state.is_fully_reachable() {
            self.ever_published
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if crate::diag::enabled() {
            let seen = format!("{state:?}");
            let mut last = self.last_state.lock().unwrap_or_else(|e| e.into_inner());
            if *last != seen {
                crate::diag!(
                    "tor: onion service state {} -> {seen} (ready={ready})",
                    *last
                );
                *last = seen;
            }
        }
        ready
    }

    /// Reach any relay `.onion` over this Tor client — lets the node fan out to a recipient's
    /// chosen relay set (#17).
    fn relay_dialer(&self, addr: &str) -> Option<crate::relay_client::RelayDialer> {
        Some(self.make_relay_dialer(addr.to_string()))
    }

    /// Mint our client descriptor-encryption key for `peer_onion` (onion client auth, #22).
    fn make_client_key(&self, peer_onion: &str) -> Option<Result<(String, [u8; 32])>> {
        Some(self.make_service_discovery_key(peer_onion))
    }

    fn insert_client_key(&self, peer_onion: &str, secret: &[u8; 32]) -> Result<()> {
        let hsid = HsId::from_str(peer_onion).context("parse peer onion address")?;
        let secret = tor_hscrypto::pk::HsClientDescEncSecretKey::from(
            tor_llcrypto::pk::curve25519::StaticSecret::from(*secret),
        );
        self.client
            .insert_service_discovery_key(KeystoreSelector::Primary, hsid, secret)
            .context("restore client key for a peer")?;
        Ok(())
    }

    /// Write a paired contact's client key into our watched authorized-keys directory, so arti
    /// encrypts our onion descriptor to them (#22). No-op when no auth dir was configured.
    fn authorize_client(&self, contact_id: &str, key: &str) -> Result<()> {
        match self.auth_dir.as_deref() {
            Some(dir) => client_auth::authorize(std::path::Path::new(dir), contact_id, key),
            None => Ok(()),
        }
    }

    /// Remove a contact's authorized-key file, revoking their reachability to our onion (#22).
    fn revoke_client(&self, contact_id: &str) -> Result<()> {
        match self.auth_dir.as_deref() {
            Some(dir) => client_auth::revoke(std::path::Path::new(dir), contact_id),
            None => Ok(()),
        }
    }

    fn forget_peer_key(&self, peer_onion: &str) -> Result<()> {
        // Best-effort: an unparseable address or a key that was never generated is not an error
        // worth failing a chat deletion over. What must not happen is the key silently staying.
        let Ok(hsid) = HsId::from_str(peer_onion) else {
            return Ok(());
        };
        let _ = self
            .client
            .remove_service_discovery_key(KeystoreSelector::Primary, hsid);
        Ok(())
    }

    fn send(&self, peer: &str, frame: &[u8]) -> Result<()> {
        let client = Arc::clone(&self.client);
        let peer = peer.to_string();
        let frame = frame.to_vec();
        // Reuse a warm stream to this peer if we have one; otherwise dial. On any write
        // error the stream is stale — reconnect once. Keeping a per-peer stream (rather than
        // dial-per-frame) avoids repeated rendezvous setup; arti still isolates different
        // peers on different circuits by `.onion`.
        let mut existing = self
            .out_streams
            .lock()
            .unwrap()
            .remove(&peer)
            .map(|(s, _)| s);
        let peer2 = peer.clone();
        let stream = self.runtime.block_on(async move {
            if let Some(mut s) = existing.take() {
                if write_frame(&mut s, &frame).await.is_ok() && s.flush().await.is_ok() {
                    return Ok::<DataStream, anyhow::Error>(s);
                }
                // stale (peer closed it / circuit gone): drop and reconnect.
            }
            let mut s = tokio::time::timeout(
                PEER_DIAL_TIMEOUT,
                client.connect((peer2.as_str(), NIGHTDROP_PORT)),
            )
            .await
            .map_err(|_| anyhow::anyhow!("peer dial timed out"))?
            .context("dial peer onion")?;
            write_frame(&mut s, &frame).await?;
            s.flush().await?;
            Ok(s)
        })?;
        self.out_streams
            .lock()
            .unwrap()
            .insert(peer, (stream, Instant::now()));
        Ok(())
    }

    fn try_recv(&self) -> Option<(Address, Vec<u8>)> {
        self.inbound.lock().unwrap().try_recv().ok()
    }
}

/// Build the arti client config. With `state_dir` we point arti's state + cache at an
/// explicit writable base (required on Android) and relax fs-mistrust's permission checks,
/// since app-sandbox directories don't match arti's default ownership expectations.
/// How many times the client will try to fetch an onion descriptor / build an intro+rendezvous
/// circuit before giving up (arti defaults both to 6). On a slow or lossy path 6 is too few — the
/// relay itself may take dozens of circuit tries just to *publish* — so a client that quits after
/// 6 reports "could not reach any relay" against a perfectly healthy service. Raised well above the
/// default; each attempt is still individually bounded, so the cost of the extra tries is only paid
/// on a path that actually needs them.
const HS_CONNECT_ATTEMPTS: u32 = 32;

/// Apply settings shared by every Tor config we build (see [`HS_CONNECT_ATTEMPTS`]).
fn apply_common_tuning(builder: &mut TorClientConfigBuilder) {
    builder
        .circuit_timing()
        .hs_desc_fetch_attempts(HS_CONNECT_ATTEMPTS)
        .hs_intro_rend_attempts(HS_CONNECT_ATTEMPTS);
}

/// Whether arti's keystore should live **in memory** (the default now) or on disk for one
/// migration run.
///
/// On disk is used exactly once: an install that predates this change has its onion identity in
/// arti's own keystore and nothing else can read it — `get_service_discovery_key` returns only
/// public halves, and the identity file is arti's format. So that run reads it through arti, the
/// caller seals it, and every run afterwards is in-memory.
#[cfg(feature = "tor")]
fn keystore_is_on_disk(state_dir: Option<&str>, nickname: &str, have_saved_key: bool) -> bool {
    if have_saved_key {
        return false; // we hold the identity ourselves — never touch the disk keystore again
    }
    let Some(base) = state_dir else {
        return false;
    };
    std::path::Path::new(base)
        .join("arti-state/keystore/hss")
        .join(nickname)
        .join("ks_hs_id.ed25519_expanded_private")
        .exists()
}

/// Drop arti's record of our previous introduction points when their keys no longer exist.
///
/// `hss/<nickname>/ipts.json` persists on disk, but with the ephemeral keystore the IPT keys it
/// names (`k_hss_ntor`, `k_sid`) live only in memory and are gone by the next launch. arti then
/// hits its own bug assertion on every start —
///
/// ```text
/// ERROR tor_hsservice::ipt_mgr: bug: HS service nightdrop missing previous key
///       ArtiPath("hss/nightdrop/ipts/k_hss_ntor+…"). Regenerating.
/// ```
///
/// — and rebuilds the set from nothing, so every launch spends time at "no good IPTs" before it is
/// reachable again. Introduced by the move to the in-memory keystore (`onion-key-at-rest.md`) and
/// found in a device log on 2026-08-02. Removing the stale record makes the fresh start honest and
/// silences a real bug report we were causing. The address is unaffected — that is the identity
/// key, which we hold sealed; these are per-introduction-point keys, regenerated by design.
fn forget_stale_ipts(state_dir: Option<&str>, nickname: &str) {
    let Some(base) = state_dir else {
        return;
    };
    let dir = std::path::Path::new(base)
        .join("arti-state/hss")
        .join(nickname);
    for name in ["ipts.json", "iptpub.json"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

fn tor_config(state_dir: Option<&str>, on_disk_keystore: bool) -> Result<TorClientConfig> {
    match state_dir {
        None => {
            let mut builder = TorClientConfigBuilder::default();
            apply_common_tuning(&mut builder);
            apply_keystore_kind(&mut builder, on_disk_keystore);
            builder.build().context("build Tor config")
        }
        Some(base) => {
            let state = format!("{base}/arti-state");
            let cache = format!("{base}/arti-cache");
            std::fs::create_dir_all(&state).ok();
            std::fs::create_dir_all(&cache).ok();
            let mut builder = TorClientConfigBuilder::from_directories(&state, &cache);
            builder.storage().permissions().dangerously_trust_everyone();
            apply_common_tuning(&mut builder);
            apply_bridges(&mut builder, base);
            // Register any pluggable transports (obfs4/snowflake) the user configured, so a bridge
            // line that names such a transport has a binary to run it. Must come with the bridges:
            // arti's config validation rejects a PT bridge that has no matching transport entry.
            apply_transports(&mut builder, base);
            apply_keystore_kind(&mut builder, on_disk_keystore);
            builder.build().context("build Tor config")
        }
    }
}

/// Load optional Tor **bridges** from `<base>/bridges.txt` and add them to the config. Bridges are
/// unlisted entry relays, so a client can still reach the Tor network where the public relays are
/// IP-blocked — the censorship-resistance path (ARCHITECTURE.md §6). One bridge line per line;
/// blank lines and `#` comments are ignored, and an optional leading `Bridge ` keyword (torrc
/// style) is tolerated. Absent file → no bridges (today's behavior). A malformed line is skipped
/// (logged to stderr, never fatal). Returns how many bridges were added.
///
/// Vanilla (direct) bridge lines — `ADDR:PORT FINGERPRINT [ED25519-ID]` — work as-is. A bridge line
/// that names a pluggable transport — `obfs4 ADDR FINGERPRINT cert=… iat-mode=…`, `snowflake …` —
/// also parses here; it additionally needs a matching entry in `transports.txt` (see
/// [`apply_transports`]) pointing arti at the PT client binary.
/// Keystore in memory unless this is the one-time migration read. In memory, neither our onion
/// identity nor the per-contact client keys ever reach disk — they live in our sealed store and are
/// re-inserted at startup (`docs/design/onion-key-at-rest.md`).
fn apply_keystore_kind(builder: &mut TorClientConfigBuilder, on_disk: bool) {
    use tor_config::ExplicitOrAuto;
    use tor_keymgr::config::ArtiKeystoreKind;
    builder
        .storage()
        .keystore()
        .primary()
        .kind(ExplicitOrAuto::Explicit(if on_disk {
            ArtiKeystoreKind::Native
        } else {
            ArtiKeystoreKind::Ephemeral
        }));
}

fn apply_bridges(builder: &mut TorClientConfigBuilder, base: &str) -> usize {
    let path = format!("{base}/bridges.txt");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let mut added = 0usize;
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let spec = line.strip_prefix("Bridge ").unwrap_or(line).trim();
        match spec.parse::<BridgeConfigBuilder>() {
            Ok(bridge) => {
                builder.bridges().bridges().push(bridge);
                added += 1;
            }
            // A bridge line is operator-supplied config, not message data, and carries no secret;
            // surface the parse error so a typo is diagnosable instead of silently ignored.
            Err(e) => eprintln!("nightdrop: skipping invalid bridge line: {e}"),
        }
    }
    added
}

/// Register **pluggable transports** (obfs4/snowflake, ARCHITECTURE.md §6) from
/// `<base>/transports.txt`, so a bridge whose line names such a transport has a client binary that
/// can run it. This is the layer above plain bridges: where an ISP/firewall blocks even bridge IPs
/// by deep-packet-inspecting for the Tor protocol, a PT disguises the traffic. arti manages the
/// binary itself, launching it on demand (never at startup) when a channel actually needs it — so a
/// stale/missing entry can't stall bootstrap unless a bridge truly depends on it.
///
/// Format — one transport per line; blank lines and `#` comments are ignored; an optional leading
/// `Transport ` keyword is tolerated:
///
/// ```text
/// # <protocols>            <path-to-client-binary>
/// obfs4                    /usr/bin/lyrebird
/// snowflake                /usr/bin/snowflake-client
/// obfs4,meek_lite          /usr/bin/lyrebird          # one binary, several protocols
/// ```
///
/// `<protocols>` is one transport name, or several comma-separated (a single binary can provide
/// more than one). The rest of the line is the binary path (may contain spaces). Absent file → no
/// transports (today's behavior). A malformed line is skipped (logged to stderr, never fatal).
/// Returns how many transports were registered.
fn apply_transports(builder: &mut TorClientConfigBuilder, base: &str) -> usize {
    let path = format!("{base}/transports.txt");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let mut added = 0usize;
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("Transport ").unwrap_or(line).trim();
        // Split into "<protocols> <path>" at the first run of whitespace; the path keeps any
        // internal spaces. A line with no path (just a name) is malformed.
        let Some((protos_field, bin)) = line.split_once(char::is_whitespace) else {
            eprintln!("nightdrop: skipping transport line with no binary path: {line}");
            continue;
        };
        let bin = bin.trim();
        // Crate's `Result` alias is single-arg (anyhow); name the std one for the two-arg form.
        let protocols: std::result::Result<Vec<PtTransportName>, _> = protos_field
            .split(',')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .map(|p| p.parse::<PtTransportName>())
            .collect();
        let protocols = match protocols {
            Ok(ps) if !ps.is_empty() => ps,
            Ok(_) => {
                eprintln!("nightdrop: skipping transport line with no protocol: {line}");
                continue;
            }
            Err(e) => {
                eprintln!("nightdrop: skipping transport line with bad protocol name: {e}");
                continue;
            }
        };
        let mut transport = TransportConfigBuilder::default();
        transport
            .protocols(protocols)
            .path(CfgPath::new_literal(std::path::Path::new(bin)))
            // On demand, not at startup: only spawn the PT binary if a bridge actually needs it,
            // so a missing/renamed binary can't fail Tor bootstrap for users who aren't using it.
            .run_on_startup(false);
        builder.bridges().transports().push(transport);
        added += 1;
    }
    added
}

/// Build the onion-service config, enabling **restricted discovery** (onion client authorization,
/// #22) when `client_auth_dir` is set and already holds ≥1 authorized-client key. An empty/absent
/// dir yields a normal **public** onion (today's behavior); we never enable restriction with an
/// empty authorized set, which would make the service unreachable by everyone. The directory is
/// watched (`watch_configuration`), so keys added/removed after launch take effect without a relaunch.
fn onion_service_config(
    nickname: HsNickname,
    client_auth_dir: Option<&str>,
) -> Result<OnionServiceConfig> {
    let mut builder = OnionServiceConfigBuilder::default();
    builder.nickname(nickname);
    // One more introduction point than arti's default of 3: a device onion that loses an intro
    // point (relay churn while the phone is backgrounded/roaming) stays reachable for pairing and
    // first-contact Hellos instead of going briefly dark. Kept small — each IPT is a maintained
    // circuit, and battery matters on mobile — so this trades a little upkeep for reachability.
    builder.num_intro_points(4);
    if let Some(dir) = client_auth_dir {
        if client_auth::authorized_count(std::path::Path::new(dir)) > 0 {
            let mut provider = DirectoryKeyProviderBuilder::default();
            provider
                .path(CfgPath::new_literal(std::path::Path::new(dir)))
                .permissions()
                .dangerously_trust_everyone();
            builder
                .restricted_discovery()
                .enabled(true)
                .watch_configuration(true)
                .key_dirs()
                .access()
                .push(provider);
        }
    }
    builder.build().context("onion service config")
}

/// Write a `u32` big-endian length prefix followed by the frame bytes.
async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, frame: &[u8]) -> Result<()> {
    w.write_all(&(frame.len() as u32).to_be_bytes()).await?;
    w.write_all(frame).await?;
    Ok(())
}

/// Read one length-prefixed frame.
async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod bridge_tests {
    use super::*;

    #[test]
    fn apply_bridges_reads_valid_lines_and_skips_the_rest() {
        let dir = std::env::temp_dir().join(format!(
            "nd-bridges-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let state = dir.join("arti-state");
        let cache = dir.join("arti-cache");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            dir.join("bridges.txt"),
            concat!(
                "# my bridges\n",
                "\n",
                "38.229.33.83:80 0BAC39417268B96B9F514E7F63FA6FBA1A788955\n",
                "Bridge 38.229.33.84:443 0BAC39417268B96B9F514E7F63FA6FBA1A788955\n",
                "this is not a bridge line\n",
            ),
        )
        .unwrap();

        let mut builder = TorClientConfigBuilder::from_directories(&state, &cache);
        let added = apply_bridges(&mut builder, dir.to_str().unwrap());
        assert_eq!(
            added, 2,
            "two valid lines added; comment/blank/junk skipped"
        );

        // Absent file → no bridges, no error.
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let mut b2 = TorClientConfigBuilder::from_directories(&state, &cache);
        assert_eq!(apply_bridges(&mut b2, empty.to_str().unwrap()), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_transports_reads_valid_lines_and_skips_the_rest() {
        let dir = std::env::temp_dir().join(format!(
            "nd-transports-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let state = dir.join("arti-state");
        let cache = dir.join("arti-cache");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            dir.join("transports.txt"),
            concat!(
                "# my pluggable transports\n",
                "\n",
                "obfs4 /usr/bin/lyrebird\n",
                "Transport snowflake /usr/bin/snowflake-client\n",
                "obfs4,meek_lite /usr/bin/lyrebird\n",
                "obfs4-no-path-here\n",  // no binary path → skipped
                ", /usr/bin/whatever\n", // empty protocol list → skipped
            ),
        )
        .unwrap();

        let mut builder = TorClientConfigBuilder::from_directories(&state, &cache);
        let added = apply_transports(&mut builder, dir.to_str().unwrap());
        assert_eq!(
            added, 3,
            "three valid transport lines; comment/blank/no-path/no-protocol skipped"
        );

        // Absent file → no transports, no error.
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let mut b2 = TorClientConfigBuilder::from_directories(&state, &cache);
        assert_eq!(apply_transports(&mut b2, empty.to_str().unwrap()), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
