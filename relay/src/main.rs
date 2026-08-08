//! Night Drop minimal relay (`ARCHITECTURE.md` §5c + §6 + §11).
//!
//! An untrusted box that holds only opaque, already-encrypted blobs under ephemeral handles
//! with short TTLs — **no keys, no logs, minimal metadata**. It publishes **its own Tor onion
//! service** (its own keystore, separate from any chat identity), so it is reachable from any
//! network (LTE, NAT) with no LAN/port-forwarding. The protocol + store (`RelayCore`) are
//! shared with the client/tests in `nightdrop::relay_client`; this binary bootstraps Tor,
//! accepts rendezvous streams, and serves the newline-JSON protocol over them.
//!
//! Env:
//! - `NIGHTDROP_RELAY_STATE` — base dir for arti state/keystore (default `relay-state`); persisting
//!   it keeps the `.onion` **stable** across restarts.
//! - `NIGHTDROP_RELAY_DEV` — enable the **dev flow-log** (§11.9): one metadata-only line per
//!   operation to stdout + `relay.log` (never blob bytes). Leave unset in production.
//!
//! Operator subcommands (no Tor bootstrap):
//! - `gen-directory-key` / `sign-directory` — the signed relay directory (§3.1).
//! - `authorize-client` / `revoke-client` / `list-clients` — private-relay client authorization
//!   (§3.2). Any authorized client (a `.auth` file under `<state>/authorized-clients/`) flips the
//!   relay's onion to **restricted discovery**: only those clients can fetch its descriptor.

mod tui;

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Most rendezvous streams served at once. Past this, new streams are refused (the client retries)
/// so a connection flood can't exhaust file descriptors / memory by spawning unbounded tasks.
const MAX_CONCURRENT_STREAMS: usize = 512;
/// Per-connection request rate limit (token bucket): allow a short burst, then a sustained rate;
/// a connection that exceeds it is closed. Bounds a single client's request flood — which, without
/// this, drives the store/flush work — while leaving normal drain/pair traffic untouched.
const REQ_BURST: f64 = 60.0;
const REQ_RATE_PER_SEC: f64 = 30.0;

/// Decrements the live-stream counter when a connection task ends (RAII).
struct ConnGuard(Arc<AtomicUsize>);
impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

use anyhow::Context as _;
use arti_client::config::TorClientConfigBuilder;
use arti_client::{DataStream, TorClient, TorClientConfig};
use futures::{AsyncBufReadExt, AsyncWriteExt, StreamExt};
use nightdrop::relay_client::{RelayCore, RelayEvent, RelayLimits, RelayLogger, Request};
use safelog::DisplayRedacted as _;
use tokio::runtime::Runtime;
use tor_cell::relaycell::msg::Connected;
use tor_config_path::CfgPath;
use tor_hsservice::config::restricted_discovery::DirectoryKeyProviderBuilder;
use tor_hsservice::config::{OnionServiceConfig, OnionServiceConfigBuilder};
use tor_hsservice::{handle_rend_requests, HsNickname};

fn main() -> anyhow::Result<()> {
    // Operator tooling. Directory (§3.1 — rotate the relay set without an app update) and
    // private-relay client authorization (§3.2 — restrict the onion to authorized clients).
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen-directory-key") => return gen_directory_key(),
        Some("sign-directory") => return sign_directory(&args[2..]),
        Some("authorize-client") => return authorize_client(&args[2..]),
        Some("revoke-client") => return revoke_client(&args[2..]),
        Some("list-clients") => return list_clients(),
        _ => {}
    }

    // rustls 0.23 needs a process-default crypto provider before any TLS config is built.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Diagnostics only: with RUST_LOG set, surface arti's tracing (onion-service publish,
    // descriptor upload, circuit/IPT health). No subscriber → no logs by default.
    if std::env::var("RUST_LOG").is_ok() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }

    let state_dir = state_dir();
    let tui = std::env::var("NIGHTDROP_RELAY_TUI").is_ok();
    let dev = std::env::var("NIGHTDROP_RELAY_DEV").is_ok();
    // TUI → events to the dashboard (+ relay.log). Plain dev → stdout + relay.log.
    let (logger, mut rx): (Option<RelayLogger>, Option<Receiver<RelayEvent>>) = if tui {
        let (tx, rx) = std::sync::mpsc::channel();
        (Some(tui_logger(tx)), Some(rx))
    } else if dev {
        (Some(make_logger()), None)
    } else {
        (None, None)
    };
    // Persist the store-and-forward queue to <state>/queue.json by default, so a relay
    // restart or crash doesn't drop queued mail (only opaque, already-encrypted, time-boxed
    // blobs under unlinkable handles are written; anything past its TTL is dropped on load).
    // Set NIGHTDROP_RELAY_EPHEMERAL=1 for strict RAM-only (nothing but the onion key on disk).
    // Serve the operator-signed relay directory if one is present (§3.1). Drop the output of
    // `nightdrop-relay sign-directory` as <state>/relay-list.json; clients fetch + verify it.
    let directory = std::fs::read_to_string(format!("{state_dir}/relay-list.json"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let base = if std::env::var("NIGHTDROP_RELAY_EPHEMERAL").is_ok() {
        RelayCore::new(logger)
    } else {
        RelayCore::with_persistence(
            logger,
            RelayLimits::default(),
            std::path::PathBuf::from(format!("{state_dir}/queue.json")),
        )
    };
    let core = Arc::new(base.with_directory(directory));
    let start = Instant::now();

    // If prior runs kept ending unreachable, plain restarts aren't helping — the guard set is
    // wedged. Reset it before bootstrapping so this start picks fresh entry guards (§6).
    if read_unhealthy(&state_dir) >= UNHEALTHY_RESET_THRESHOLD {
        reset_tor_guards(&state_dir);
    }

    let config = tor_config(&state_dir)?;
    let runtime = Runtime::new().context("tokio runtime")?;
    runtime.block_on(async move {
        if !tui {
            eprintln!("nightdrop-relay: bootstrapping Tor (first run can take a minute)…");
        }
        let client = TorClient::create_bootstrapped(config)
            .await
            .context("bootstrap Tor")?;
        let nickname: HsNickname = "nightdroprelay".parse().context("onion service nickname")?;
        // Private relay (§3.2): if any client has been authorized, gate the onion to them via
        // Tor restricted discovery — unauthorized clients can't even fetch our descriptor.
        let auth = auth_dir();
        let private = nightdrop::transport::client_auth::authorized_count(Path::new(&auth)) > 0;
        let svc_config = relay_onion_config(nickname, &auth)?;
        let (service, rend_requests) = client
            .launch_onion_service(svc_config)
            .context("launch onion service")?
            .context("onion service did not start")?;
        let onion = service
            .onion_address()
            .context("onion service has no address yet")?
            .display_unredacted()
            .to_string();

        // Write the address to `<state>/onion` so dev tooling can read it without scraping logs.
        let _ = std::fs::write(format!("{state_dir}/onion"), &onion);

        // Seconds since `start` at which we last served a real client's rendezvous stream. Written
        // by the accept loop below, read by the watchdog as independent proof of reachability.
        let last_served = Arc::new(AtomicU64::new(0));

        // Self-healing watchdog. arti keeps this process alive even when its descriptor publisher
        // or introduction points wedge (observed after multi-day uptime: the process runs, the
        // onion goes dark, clients get "could not reach relay"). `Restart=always` can't recover
        // that — nothing crashed. So we prove reachability ourselves and exit when the onion has
        // been *demonstrably* dark long enough to warrant a fresh start; systemd then restarts us,
        // re-establishing intro points and republishing the descriptor.
        //
        // Reachability is proved TWO ways each cycle, because each one is blind to a failure the
        // other catches (the reasoning lives on `Cycle::verdict`):
        //   1. an end-to-end self-dial over Tor — the only thing that actually answers "can a
        //      client reach this relay", but it resolves us under a single time period, so it is
        //      blind to a descriptor that made it onto one HsDir ring and not the other;
        //   2. the publisher's own `DegradedUnreachable`, which is exactly that ring check.
        //
        // What this watchdog no longer asks is `is_fully_reachable()`. That is arti's summary of
        // its bootstrap PROGRESS and is wrong in both directions: on 2026-08-02 this relay read
        // healthy for ~90 minutes while every client timed out, and it reads false on services
        // that are serving perfectly — which is what put the systemd restart counter in the
        // thirties. But it was also carrying (2) inside it, so replacing it with (1) alone would
        // have quietly dropped a true signal; hence both, with the aggregate itself read narrowly.
        {
            let probe_client = client.clone();
            let probe_onion = onion.clone();
            let watched = Arc::clone(&service);
            let max_line = core.max_line_bytes();
            let served = Arc::clone(&last_served);
            let sd = state_dir.clone();
            tokio::spawn(async move {
                let mut clocks = DarkClocks::default();
                let mut cleared = false;
                loop {
                    tokio::time::sleep(PROBE_INTERVAL).await;
                    let began = Instant::now();
                    let dial = probe_self(&probe_client, &probe_onion, max_line).await;
                    // `{e:#}`, not `{e}`: anyhow's plain Display shows only the outermost context,
                    // which here is the bare label "self-dial connect" — it cannot tell a
                    // descriptor that was never found from intro points that are gone from a dial
                    // that timed out, and that distinction is the entire diagnostic value of the
                    // line. The alternate form prints the whole cause chain.
                    let how = match &dial {
                        Ok(()) => format!("reached in {}ms", began.elapsed().as_millis()),
                        Err(e) => format!("self-dial failed: {e:#}"),
                    };
                    let publisher_dark = watched.status().state()
                        == tor_hsservice::status::State::DegradedUnreachable;

                    let now = Instant::now();
                    clocks.observe(publisher_dark, dial.is_err(), now);
                    // The served-clients veto is about the dial, so the dial's clock decides what
                    // "since the darkness began" means for it.
                    let since = clocks.dial.unwrap_or(now);
                    let cycle = Cycle {
                        dial_failed: dial.is_err(),
                        publisher_dark,
                        served_since_dark: served.load(Ordering::Relaxed)
                            > since.duration_since(start).as_secs(),
                        client_unusable: tor_client_unusable(&probe_client),
                        dark_for: clocks.dark_for(publisher_dark, now),
                    };
                    let dark_for = cycle.dark_for;
                    match cycle.verdict() {
                        Verdict::Healthy => {
                            clocks = DarkClocks::default();
                            // A heartbeat, deliberately logged every time. It is the only line that
                            // proves this relay was reachable at a given moment, and the counters
                            // around it are worthless without evidence that the log is still
                            // growing — an idle onion service leaves every other number frozen.
                            // Silent under the TUI, which owns the terminal (the dev dashboard
                            // routes its own output to relay.log for exactly this reason); the
                            // failure lines below still print, being rare and worth the mess.
                            if !tui {
                                eprintln!("nightdrop-relay: self-dial {how}");
                            }
                            // Reached: clear the escalation counter once, so a *later* independent
                            // outage starts its own plain-restart-then-guard-reset sequence from
                            // zero.
                            if !cleared {
                                write_unhealthy(&sd, 0);
                                cleared = true;
                            }
                        }
                        Verdict::Inconclusive(why) => {
                            // arti's own words for the blockage, when it is the blockage we're
                            // deferring to — "Offline" and "Filtering" call for very different
                            // follow-up from whoever reads this.
                            let detail = cycle
                                .client_unusable
                                .as_deref()
                                .map(|d| format!(" ({d})"))
                                .unwrap_or_default();
                            eprintln!(
                                "nightdrop-relay: {how} — {why}{detail}, so this says nothing \
                                 about the onion; not restarting"
                            );
                            clocks.excuse(cycle.client_unusable.is_some());
                        }
                        Verdict::Dark(why) => eprintln!(
                            "nightdrop-relay: {how} — {why}; dark for {}s of {}s before a restart",
                            dark_for.as_secs(),
                            WATCHDOG_MAX_DARK.as_secs()
                        ),
                        Verdict::Restart(why) => {
                            // Record this unhealthy cycle so the next start can escalate to a guard
                            // reset if plain restarts keep failing, then exit for systemd to
                            // restart us.
                            let n = read_unhealthy(&sd) + 1;
                            write_unhealthy(&sd, n);
                            eprintln!(
                                "nightdrop-relay: {how} — {why}, sustained for {}s (unhealthy \
                                 cycle {n}) — exiting for a fresh restart (systemd Restart=always)",
                                dark_for.as_secs()
                            );
                            std::process::exit(1);
                        }
                    }
                }
            });
        }
        if tui {
            // Hand the terminal to the dashboard on its own thread; the accept loop keeps the
            // main thread.
            if let Some(rx) = rx.take() {
                let core = Arc::clone(&core);
                let onion = onion.clone();
                std::thread::spawn(move || tui::run_tui(onion, core, rx, start));
            }
        } else {
            eprintln!("nightdrop-relay onion: {onion}");
            eprintln!(
                "  mode: {}",
                if private {
                    "PRIVATE — restricted to authorized clients (restricted discovery)"
                } else {
                    "PUBLIC — reachable by anyone with the address"
                }
            );
            eprintln!(
                "(opaque encrypted blobs only; no keys, no logs{})",
                if dev {
                    "; DEV flow-log -> stdout + relay.log"
                } else {
                    ""
                }
            );
        }

        // One task per rendezvous stream; each serves the newline-JSON relay protocol. A bounded
        // live-stream counter sheds load past MAX_CONCURRENT_STREAMS so a connection flood can't
        // spawn unbounded tasks.
        let active = Arc::new(AtomicUsize::new(0));
        let mut streams = handle_rend_requests(rend_requests);
        while let Some(request) = streams.next().await {
            if active.fetch_add(1, Ordering::Relaxed) >= MAX_CONCURRENT_STREAMS {
                active.fetch_sub(1, Ordering::Relaxed);
                continue; // at capacity — drop the rendezvous; the client retries
            }
            let core = Arc::clone(&core);
            let guard = ConnGuard(Arc::clone(&active));
            let served = Arc::clone(&last_served);
            tokio::spawn(async move {
                let _guard = guard; // decrements the counter when this task ends
                if let Ok(stream) = request.accept(Connected::new_empty()).await {
                    // Someone reached this onion. The watchdog reads this as proof that the
                    // service is live even when its own self-dial is failing. Our own probe lands
                    // here too, which is harmless: the watchdog only consults this while a dark
                    // spell is running, and every probe during one has failed — a probe that
                    // reached us would have ended the spell instead.
                    served.store(start.elapsed().as_secs(), Ordering::Relaxed);
                    let _ = serve_stream(stream, core).await;
                }
            });
        }
        drop(service); // keep the service alive for the whole (endless) accept loop
        Ok::<_, anyhow::Error>(())
    })
}

/// Serve the relay protocol over one rendezvous stream: read newline-delimited JSON requests,
/// dispatch through the shared [`RelayCore`], and write back each response line.
async fn serve_stream(stream: DataStream, core: Arc<RelayCore>) -> anyhow::Result<()> {
    let (read, mut write) = stream.split();
    let mut reader = futures::io::BufReader::new(read);
    // Cap each request line so a hostile peer streaming endless bytes with no newline can't OOM the
    // relay before the storage limits apply (see RelayLimits::max_line_bytes).
    let max = core.max_line_bytes();
    // Per-connection token bucket: a short burst, then a sustained rate; a client that floods past
    // it gets closed. Throttles single-connection request floods without penalising normal traffic.
    let mut tokens = REQ_BURST;
    let mut last_refill = Instant::now();
    loop {
        match read_line_capped(&mut reader, max).await {
            Ok(None) => break, // clean EOF
            Ok(Some(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let now = Instant::now();
                tokens = (tokens
                    + now.duration_since(last_refill).as_secs_f64() * REQ_RATE_PER_SEC)
                    .min(REQ_BURST);
                last_refill = now;
                if tokens < 1.0 {
                    let _ = write
                        .write_all(RelayCore::error_line("rate limit exceeded").as_bytes())
                        .await;
                    let _ = write.flush().await;
                    break; // close the connection; a fresh one over Tor is costly for the flooder
                }
                tokens -= 1.0;
                write.write_all(core.handle_line(line).as_bytes()).await?;
                write.flush().await?;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::InvalidData => {
                let _ = write
                    .write_all(RelayCore::error_line("line too long").as_bytes())
                    .await;
                let _ = write.flush().await;
                break;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// How long each watchdog signal has been continuously bad — **one clock per signal**.
///
/// [`Cycle::verdict`] is careful that a veto may only be spent against the signal it actually
/// explains: `served_since_dark` is checked *after* `publisher_dark` so that clients reaching us
/// over the ring that IS published can never excuse the ring that is not. A single shared clock
/// defeated that from outside the function — an `Inconclusive` cycle cleared the accumulated
/// darkness no matter which signal had established it.
///
/// Observed on the dev relay 2026-08-08: the publisher's darkness climbed 0s → 302s → 604s and was
/// reset to 0s by a "clients are still being served" cycle, over and over, for six hours. The
/// threshold was never reachable, so the restart the watchdog exists to perform could not happen
/// while any client was still being served — which is exactly the state a one-sided outage
/// produces, since everyone on the good ring is served throughout.
#[derive(Default)]
struct DarkClocks {
    /// Since when the descriptor has been missing from one of the two HsDir rings.
    publisher: Option<Instant>,
    /// Since when the end-to-end self-dial has been failing.
    dial: Option<Instant>,
}

impl DarkClocks {
    /// Start each clock when its own signal first goes bad; stop it the moment that signal is good
    /// again. The publisher's clock does not care that a dial failed, and vice versa.
    fn observe(&mut self, publisher_dark: bool, dial_failed: bool, now: Instant) {
        if publisher_dark {
            self.publisher.get_or_insert(now);
        } else {
            self.publisher = None;
        }
        if dial_failed {
            self.dial.get_or_insert(now);
        } else {
            self.dial = None;
        }
    }

    /// How long the outage *being judged* has lasted. `verdict` reads `publisher_dark` first, so
    /// when it is set that is the outage on trial and its clock is the one that counts.
    fn dark_for(&self, publisher_dark: bool, now: Instant) -> Duration {
        let clock = if publisher_dark {
            self.publisher
        } else {
            self.dial
        };
        clock.map(|t| now.duration_since(t)).unwrap_or_default()
    }

    /// Spend a veto against only what it is entitled to explain.
    ///
    /// An unusable Tor client explains both signals — nothing can dial or upload without one — so
    /// it clears both. "Clients are still being served" explains the dial and nothing else, so the
    /// publisher's clock keeps running.
    fn excuse(&mut self, client_unusable: bool) {
        self.dial = None;
        if client_unusable {
            self.publisher = None;
        }
    }
}

/// What one watchdog cycle observed.
struct Cycle {
    /// The end-to-end self-dial did not reach us this cycle.
    dial_failed: bool,
    /// arti's descriptor publisher reports the descriptor present on one of the two HsDir rings
    /// and absent from the other — `State::DegradedUnreachable`, in its own words "definitely not
    /// reachable by all clients".
    publisher_dark: bool,
    /// A real client's rendezvous stream was served *after* we started counting this dark spell.
    served_since_dark: bool,
    /// arti's reason, if any, that our own Tor client cannot currently work.
    client_unusable: Option<String>,
    /// How long the onion has been continuously dark.
    dark_for: Duration,
}

/// What to do about it.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Reached, and publishing to both rings. Nothing to do.
    Healthy,
    /// The probe failed for a reason that is not the onion being dark; carries the reason. The
    /// dark spell is abandoned rather than extended — the clock restarts if it fails again.
    Inconclusive(&'static str),
    /// The onion really does look dark, but not yet for long enough to restart over.
    Dark(&'static str),
    /// Sustained, corroborated darkness: exit and let systemd bring us back.
    Restart(&'static str),
}

impl Cycle {
    /// A restart is destructive — it rotates the introduction points, so every client holding the
    /// current descriptor fails until it refetches. That is worth doing to a relay that is dark
    /// (those clients are already stranded) and never worth doing to one that is merely failing our
    /// own probe. So each piece of counter-evidence is spent before the restart is.
    ///
    /// Two independent things can make us dark, and they catch different failures. The order below
    /// is load-bearing: a veto may only be spent against a signal it actually explains.
    fn verdict(&self) -> Verdict {
        // Nothing is wrong, so nothing needs excusing. Checked first so that a transient reading
        // from `client_unusable` can never downgrade a cycle that both reached us and published.
        if !self.dial_failed && !self.publisher_dark {
            return Verdict::Healthy;
        }
        // Something IS wrong — but if our own Tor client cannot use the network, that one fact
        // explains BOTH of the signals below: a dial cannot complete without a working client, and
        // neither can a descriptor upload. Restarting cannot fix someone else's outage, and doing
        // it in a loop while the box is offline is the "reset because the device is OFFLINE"
        // mistake that cost real anonymity margin the last time it was made. So this veto outranks
        // both signals, and is the only one that does.
        if self.client_unusable.is_some() {
            return Verdict::Inconclusive("our own Tor client is unusable");
        }
        // arti's own account of its own descriptor uploads. Not a guess about the network — the
        // publisher reporting which of the two HsDir rings it managed to upload to — so the
        // served-clients veto below does NOT apply to it: clients reaching us over the ring that IS
        // published is exactly what a one-sided outage looks like from the inside, which is why the
        // self-dial cannot see this case (it resolves us under one time period, its own).
        //
        // This is the signal the old `is_fully_reachable()` check was accidentally carrying, and
        // losing it would have been a real regression: it is what fired on this relay at 09:46 on
        // 2026-08-05, when it sat at 8/8 HSDirs on period 20670 and 0/2 on 20669. Read narrowly —
        // only `DegradedUnreachable`, never the aggregate's opinion of "fully reachable" — it has
        // no false positives from IPT churn, because `Bootstrapping` and `Recovering` (the states
        // that produced the old thrashing) are matched *ahead* of it and can only ever mask it.
        if self.publisher_dark {
            return self.escalate("the descriptor is missing from one of the two HsDir rings");
        }
        // The end-to-end self-dial — a guess about the network, so it has to survive counter-
        // evidence. Someone reached this onion while we could not: the service is live and the
        // fault is in our probe, so restarting would strand clients that are working right now.
        // Our own probes cannot forge this — every probe since the dark spell began has failed, and
        // a success would have ended the spell outright.
        if self.served_since_dark {
            return Verdict::Inconclusive("clients are still being served");
        }
        self.escalate("the self-dial could not reach this relay")
    }

    /// Darkness is only ever acted on once it has lasted; a single bad cycle is not an outage.
    fn escalate(&self, why: &'static str) -> Verdict {
        if self.dark_for < WATCHDOG_MAX_DARK {
            Verdict::Dark(why)
        } else {
            Verdict::Restart(why)
        }
    }
}

/// Prove this relay is reachable the way a client would: dial our own `.onion` over Tor and run one
/// real request through the accept loop. Returns `Ok(())` only if a response line came back.
///
/// `GetDirectory` is the request because it is read-only and storeless — it touches no mailbox and
/// leaves nothing behind — while still exercising the whole path a client depends on: HSDir lookup,
/// introduction, rendezvous, our accept loop, [`RelayCore::handle_line`], and the response.
async fn probe_self<R: tor_rtcompat::Runtime>(
    client: &TorClient<R>,
    onion: &str,
    max_line: usize,
) -> anyhow::Result<()> {
    // A freshly isolated client per probe, because arti's onion-service client caches a fetched
    // descriptor per (service, client isolation). Reusing the main client would let probe after
    // probe be answered out of that cache — and "clients can no longer look us up" is exactly the
    // failure being hunted, so a probe that skips the lookup cannot see it. A new isolation gets a
    // new cache entry, which forces a real HSDir fetch. It shares the bootstrapped internals, so
    // this costs a circuit, not a second Tor client. (Confirmed by tracing: four probes produced
    // four `HS desc fetch` lines, one apiece.)
    //
    // KNOWN LIMIT, worth stating rather than overclaiming: a probe looks the service up under the
    // ONE time period its own consensus says is current, exactly as a co-located client would. A
    // service published for one period but not the other is therefore only caught while the
    // *broken* period is the one being looked up — which is the case that matters (that is what
    // "clients found nothing" means), but a distant client whose consensus points at the other
    // period can be stranded during a rollover window without this seeing it. arti exposes no
    // per-period publication state to check instead, and the daily rollover bounds how long such a
    // one-sided outage can hide from a probe running every few minutes.
    let probe = client.isolated_client();
    let exchange = async {
        let mut stream = probe
            .connect((onion, nightdrop::relay_client::RELAY_PORT))
            .await
            .context("self-dial connect")?;
        let line = nightdrop::relay_client::request_line(&Request::GetDirectory)?;
        stream.write_all(line.as_bytes()).await?;
        stream.flush().await?;
        let mut reader = futures::io::BufReader::new(stream);
        let response = read_line_capped(&mut reader, max_line)
            .await?
            .ok_or_else(|| anyhow::anyhow!("relay closed the probe stream without answering"))?;
        let response = nightdrop::relay_client::parse_response_line(&response)?;
        if !response.ok {
            anyhow::bail!("relay refused the probe: {:?}", response.error);
        }
        Ok(())
    };
    tokio::time::timeout(PROBE_TIMEOUT, exchange)
        .await
        .map_err(|_| anyhow::anyhow!("self-dial timed out after {}s", PROBE_TIMEOUT.as_secs()))?
}

/// Why arti says our own Tor client cannot currently work, if it says so at all.
///
/// Used only to *suppress* a restart: a failed self-dial made through a client that is offline,
/// filtered, or still bootstrapping says nothing about whether our onion is published. The
/// signal is imprecise (`ready_for_traffic` reports that arti cannot act without saying why), which
/// is why it is read in this direction only — as a reason to do nothing, never as a reason to act.
fn tor_client_unusable<R: tor_rtcompat::Runtime>(client: &TorClient<R>) -> Option<String> {
    let status = client.bootstrap_status();
    if let Some(blockage) = status.blocked() {
        return Some(blockage.to_string());
    }
    (!status.ready_for_traffic()).then(|| "not ready for traffic".to_string())
}

/// Async twin of the core's line-capped reader: read one newline-terminated line, bounding the
/// buffer at `max` bytes. `Ok(None)` at EOF; `Err(InvalidData)` if a line exceeds `max`.
async fn read_line_capped<R>(reader: &mut R, max: usize) -> std::io::Result<Option<String>>
where
    R: futures::io::AsyncBufRead + Unpin,
{
    // `AsyncBufReadExt` (fill_buf / consume_unpin) is imported at the top of the file.
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let (consumed, done) = {
            let chunk = reader.fill_buf().await?;
            if chunk.is_empty() {
                (0usize, true)
            } else if let Some(i) = chunk.iter().position(|&b| b == b'\n') {
                buf.extend_from_slice(&chunk[..i]);
                (i + 1, true)
            } else {
                buf.extend_from_slice(chunk);
                (chunk.len(), false)
            }
        };
        reader.consume_unpin(consumed);
        if done {
            if buf.is_empty() && consumed == 0 {
                return Ok(None);
            }
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
        if buf.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too long",
            ));
        }
    }
}

/// `gen-directory-key`: mint a fresh directory-signing keypair. Prints the secret private key (to
/// save) and the public key as a Rust array literal (to paste into `directory::DIRECTORY_PUBKEY`
/// and rebuild the app). Do this ONCE per deployment.
fn gen_directory_key() -> anyhow::Result<()> {
    let (privkey_b64, pubkey) = nightdrop::directory::generate_signing_key();
    let arr = pubkey
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!("Directory signing keypair generated.\n");
    eprintln!("1) SAVE this PRIVATE key somewhere safe — it signs relay lists (never publish it):");
    eprintln!("     {privkey_b64}\n");
    eprintln!("2) BAKE this PUBLIC key into the app (core/src/directory.rs) and rebuild:");
    eprintln!("     pub const DIRECTORY_PUBKEY: [u8; 32] = [{arr}];");
    Ok(())
}

/// `sign-directory <privkey-b64> <version> <relay.onion> [more...]`: sign a relay list and print
/// the one-line JSON. Drop it as `<state>/relay-list.json` on each relay you run; apps fetch it,
/// verify it against the baked-in key, and adopt the relays. Bump `version` on every change.
fn sign_directory(args: &[String]) -> anyhow::Result<()> {
    if args.len() < 3 {
        anyhow::bail!(
            "usage: nightdrop-relay sign-directory <privkey-b64> <version> <relay.onion> [more...]"
        );
    }
    let version: u64 = args[1].parse().context("version must be a number")?;
    let relays: Vec<String> = args[2..].to_vec();
    let issued_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let signed = nightdrop::directory::sign_list(&args[0], version, issued_at, relays)
        .context("invalid private key")?;
    println!("{signed}");
    Ok(())
}

/// The relay's state/keystore base dir (stable onion lives here). `NIGHTDROP_RELAY_STATE`, else
/// `relay-state`.
fn state_dir() -> String {
    std::env::var("NIGHTDROP_RELAY_STATE").unwrap_or_else(|_| "relay-state".into())
}

/// Directory of authorized-client descriptor keys backing restricted discovery (§3.2). Non-empty
/// ⇒ the relay's onion runs PRIVATE (only these clients can fetch its descriptor / connect).
fn auth_dir() -> String {
    format!("{}/authorized-clients", state_dir())
}

/// Build the relay's onion-service config, enabling **restricted discovery** (private relay, §3.2)
/// when the authorized-clients dir holds ≥1 key. An empty/absent dir yields a normal PUBLIC onion —
/// we never restrict with an empty set (that would make the relay unreachable by everyone). The
/// directory is watched, so authorizing/revoking after launch takes effect without a relaunch.
/// Mirrors `nightdrop::transport::tor::onion_service_config` (kept in sync by construction).
/// Introduction points for the relay's onion. arti defaults to 3; the relay is always-on
/// infrastructure whose reachability everyone depends on, so it runs more — losing a couple of
/// intro points (relay churn, transient failures) then still leaves the service reachable instead
/// of dark. Well under arti's max of 20.
const RELAY_INTRO_POINTS: u8 = 6;

/// How often the watchdog proves reachability with an end-to-end self-dial. Each probe costs one
/// descriptor fetch and one rendezvous circuit, so this is deliberately minutes rather than the
/// 30s of the old status poll — a status read was free, evidence is not.
const PROBE_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Cap on a single self-dial, and deliberately far larger than a probe should ever need.
///
/// Measured on a healthy relay over six consecutive probes: 3.4s, 3.5s, 14.1s, 20.3s, 40.6s,
/// 56.8s. A full HSDir lookup plus introduction and rendezvous is simply high-variance, and each
/// probe pays for all of it (see [`probe_self`] on why it must). A tight cap here would not detect
/// a real outage any sooner — `WATCHDOG_MAX_DARK` sets that — it would only manufacture failures on
/// a relay that is fine, and each one is a step toward a restart that strands clients. So the cap
/// exists to stop a hung dial from wedging the loop, nothing more; it stays well under
/// `PROBE_INTERVAL` so cycles cannot overlap.
const PROBE_TIMEOUT: Duration = Duration::from_secs(180);
/// How long the onion may stay *demonstrably* dark before the watchdog exits for a fresh restart.
/// Several probes' worth, so a single unlucky circuit can't trigger it. A restart rotates the
/// introduction points and strands clients holding the old descriptor, so it is worth being slow
/// and sure: the cost of restarting a relay that was fine is real, while a relay that is genuinely
/// dark is already stranding everyone.
///
/// The margin is not theoretical. A freshly launched relay was measured failing its very first
/// probe outright — a full 180s timeout, five minutes after launch, with the descriptor evidently
/// still settling — and then reaching itself in 4.8s and 3.9s on the two cycles that followed. One
/// early failure like that is normal and must not cost a restart; only a spell that outlives
/// several probes means anything.
const WATCHDOG_MAX_DARK: Duration = Duration::from_secs(15 * 60);

/// Consecutive unhealthy (sustained-unreachable) restart cycles after which the guard state is
/// reset on the next start. The first restart is plain — it re-establishes introduction points and
/// republishes the descriptor, which fixes an intro-point wedge and keeps arti's entry guards
/// sticky (a privacy property). Only if that doesn't restore reachability do we escalate to a guard
/// reset, since the remaining cause is a wedged guard set (guards churned out of the network — a
/// plain restart reuses them, so it can't recover; this is exactly the state that had to be cleared
/// by hand). So: plain restart, then guard reset.
const UNHEALTHY_RESET_THRESHOLD: u32 = 2;

/// The escalation counter: consecutive unhealthy restart cycles, persisted so it survives the exit.
fn unhealthy_marker(state_dir: &str) -> std::path::PathBuf {
    Path::new(state_dir).join("unhealthy-restarts")
}
fn read_unhealthy(state_dir: &str) -> u32 {
    std::fs::read_to_string(unhealthy_marker(state_dir))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}
fn write_unhealthy(state_dir: &str, n: u32) {
    let _ = std::fs::write(unhealthy_marker(state_dir), n.to_string());
}

/// Delete arti's entry-guard + circuit-timing state — but NOT the onion keystore, so the stable
/// `.onion` address is preserved. The next bootstrap then picks fresh entry guards, recovering from
/// a wedged guard set that a plain restart (which reuses the same guards) cannot.
fn reset_tor_guards(state_dir: &str) {
    let dir = Path::new(state_dir).join("arti-state").join("state");
    for f in ["guards.json", "circuit_timeouts.json"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    eprintln!(
        "nightdrop-relay: reset entry-guard state (onion identity kept) to recover reachability"
    );
}

fn relay_onion_config(nickname: HsNickname, auth_dir: &str) -> anyhow::Result<OnionServiceConfig> {
    let mut builder = OnionServiceConfigBuilder::default();
    builder.nickname(nickname);
    builder.num_intro_points(RELAY_INTRO_POINTS);
    if nightdrop::transport::client_auth::authorized_count(Path::new(auth_dir)) > 0 {
        let mut provider = DirectoryKeyProviderBuilder::default();
        provider
            .path(CfgPath::new_literal(Path::new(auth_dir)))
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
    builder.build().context("onion service config")
}

/// `authorize-client <name> <descriptor:x25519:…>`: authorize one client to reach this (private)
/// relay. `<name>` is any memorable label (revoke with the same one); `<key>` is the value the
/// client's app shows under "relay access key". Restart the relay if this is the first client
/// (that flips the onion from PUBLIC to PRIVATE); later authorizations are picked up live.
fn authorize_client(args: &[String]) -> anyhow::Result<()> {
    if args.len() < 2 {
        anyhow::bail!("usage: nightdrop-relay authorize-client <name> <descriptor:x25519:...>");
    }
    let dir = auth_dir();
    let first = nightdrop::transport::client_auth::authorized_count(Path::new(&dir)) == 0;
    nightdrop::transport::client_auth::authorize(Path::new(&dir), &args[0], &args[1])
        .context("authorize client")?;
    let n = nightdrop::transport::client_auth::authorized_count(Path::new(&dir));
    eprintln!("authorized '{}' — {n} client(s) now authorized.", args[0]);
    if first {
        eprintln!("This is the FIRST client: restart the relay to switch it to PRIVATE mode.");
    } else {
        eprintln!("Picked up live (the relay watches the directory) — no restart needed.");
    }
    Ok(())
}

/// `revoke-client <name>`: remove a client's authorization (by the label used to authorize it).
/// Picked up live. If it was the last client, restart to return the relay to PUBLIC mode.
fn revoke_client(args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() {
        anyhow::bail!("usage: nightdrop-relay revoke-client <name>");
    }
    let dir = auth_dir();
    nightdrop::transport::client_auth::revoke(Path::new(&dir), &args[0])
        .context("revoke client")?;
    let n = nightdrop::transport::client_auth::authorized_count(Path::new(&dir));
    eprintln!("revoked '{}' — {n} client(s) still authorized.", args[0]);
    if n == 0 {
        eprintln!("No clients remain: restart the relay to return it to PUBLIC mode.");
    }
    Ok(())
}

/// `list-clients`: how many clients are authorized + the on-disk key files. Names are stored hashed
/// (Tor requires slug filenames), so revoke by the label you authorized with, not the filename.
fn list_clients() -> anyhow::Result<()> {
    let dir = auth_dir();
    let n = nightdrop::transport::client_auth::authorized_count(Path::new(&dir));
    eprintln!(
        "{n} authorized client(s){}",
        if n == 0 {
            " — relay is PUBLIC (anyone can use it)"
        } else {
            " — relay is PRIVATE when running"
        }
    );
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) == Some("auth") {
                eprintln!("  {}", e.file_name().to_string_lossy());
            }
        }
    }
    Ok(())
}

/// Build the arti config rooted at `base` (so the relay's onion key persists → stable address).
fn tor_config(base: &str) -> anyhow::Result<TorClientConfig> {
    let state = format!("{base}/arti-state");
    let cache = format!("{base}/arti-cache");
    std::fs::create_dir_all(&state).ok();
    std::fs::create_dir_all(&cache).ok();
    let mut builder = TorClientConfigBuilder::from_directories(&state, &cache);
    builder.storage().permissions().dangerously_trust_everyone();
    builder.build().context("build Tor config")
}

/// TUI sink: forward each event to the dashboard channel and also append to `relay.log` (but
/// NOT stdout — that would corrupt the alternate-screen TUI).
fn tui_logger(tx: std::sync::mpsc::Sender<RelayEvent>) -> RelayLogger {
    use std::fs::OpenOptions;
    use std::io::Write as _;
    let tx = Arc::new(std::sync::Mutex::new(tx));
    let file = Arc::new(std::sync::Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open("relay.log")
            .ok(),
    ));
    Arc::new(move |ev: RelayEvent| {
        if let Ok(mut guard) = file.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{}  {}", now_hms(), ev.summary());
            }
        }
        if let Ok(guard) = tx.lock() {
            let _ = guard.send(ev);
        }
    })
}

/// The dev flow-log sink: one metadata-only line per op to stdout + `relay.log` (§11.9).
fn make_logger() -> RelayLogger {
    use std::fs::OpenOptions;
    use std::io::Write as _;
    let file = Arc::new(std::sync::Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open("relay.log")
            .ok(),
    ));
    Arc::new(move |ev: RelayEvent| {
        let line = format!("{}  {}  src=anonymous(Tor)", now_hms(), ev.summary());
        println!("{line}");
        if let Ok(mut guard) = file.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{line}");
            }
        }
    })
}

/// UTC `HH:MM:SS` without pulling a date crate (dev log only).
fn now_hms() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cycle in which everything is fine.
    fn healthy(dark_for: Duration) -> Cycle {
        Cycle {
            dial_failed: false,
            publisher_dark: false,
            served_since_dark: false,
            client_unusable: None,
            dark_for,
        }
    }

    /// A failed self-dial with no counter-evidence at all.
    fn uncorroborated(dark_for: Duration) -> Cycle {
        Cycle {
            dial_failed: true,
            ..healthy(dark_for)
        }
    }

    #[test]
    fn a_reached_relay_that_is_publishing_to_both_rings_is_healthy() {
        assert_eq!(healthy(Duration::ZERO).verdict(), Verdict::Healthy);
        assert_eq!(
            healthy(WATCHDOG_MAX_DARK * 10).verdict(),
            Verdict::Healthy,
            "a successful cycle is healthy regardless of how long a previous spell ran"
        );
    }

    #[test]
    fn a_sustained_dark_spell_with_no_counter_evidence_restarts() {
        // Below the threshold the watchdog keeps watching rather than acting: one unlucky circuit
        // is not an outage.
        assert!(matches!(
            uncorroborated(WATCHDOG_MAX_DARK - Duration::from_secs(1)).verdict(),
            Verdict::Dark(_)
        ));
        assert!(matches!(
            uncorroborated(WATCHDOG_MAX_DARK).verdict(),
            Verdict::Restart(_)
        ));
    }

    #[test]
    fn a_relay_that_is_still_serving_clients_is_never_restarted() {
        // The regression that matters most: a restart rotates the introduction points and strands
        // every client holding the current descriptor. If even one client got through since the
        // dark spell started, the service is live and our probe is the thing that is broken — so no
        // length of dark spell may justify restarting on top of working clients.
        let cycle = Cycle {
            served_since_dark: true,
            ..uncorroborated(WATCHDOG_MAX_DARK * 10)
        };
        assert_eq!(
            cycle.verdict(),
            Verdict::Inconclusive("clients are still being served")
        );
    }

    #[test]
    fn a_dial_made_through_a_broken_tor_client_is_not_evidence_about_the_onion() {
        // Our client being offline/filtered says nothing about whether our descriptor is published,
        // and a restart cannot fix someone else's outage.
        let cycle = Cycle {
            client_unusable: Some("offline".into()),
            ..uncorroborated(WATCHDOG_MAX_DARK * 10)
        };
        assert_eq!(
            cycle.verdict(),
            Verdict::Inconclusive("our own Tor client is unusable")
        );
    }

    #[test]
    fn a_descriptor_missing_from_one_hsdir_ring_is_dark_even_though_the_self_dial_succeeds() {
        // The case the self-dial structurally cannot see, and the one that actually restarted this
        // relay at 09:46 on 2026-08-05 (8/8 HSDirs on one period, 0/2 on the other). The probe
        // looks the service up under its own time period, so it reaches us over the ring that IS
        // published while clients on the other ring find nothing. Losing this signal when the old
        // is_fully_reachable() check was removed would have been a silent regression.
        let cycle = Cycle {
            publisher_dark: true,
            ..healthy(WATCHDOG_MAX_DARK)
        };
        assert!(matches!(cycle.verdict(), Verdict::Restart(_)));
    }

    #[test]
    fn serving_clients_does_not_excuse_a_descriptor_missing_from_a_ring() {
        // The served-clients veto second-guesses the self-dial, which is a guess about the network.
        // It must not apply to the publisher, which is arti reporting its own uploads: clients
        // reaching us over the published ring is exactly what a one-sided outage looks like from
        // in here, so treating them as counter-evidence would mask the failure permanently.
        let cycle = Cycle {
            publisher_dark: true,
            served_since_dark: true,
            ..healthy(WATCHDOG_MAX_DARK)
        };
        assert!(matches!(cycle.verdict(), Verdict::Restart(_)));
    }

    #[test]
    fn a_served_client_cannot_reset_the_clock_on_a_half_published_descriptor() {
        // The bug this split fixes, and it was live on the dev relay for six hours on 2026-08-08.
        // `verdict` already refuses to let the served-clients veto excuse `publisher_dark` — but
        // the clock lived outside `verdict`, and an Inconclusive cycle wiped it whatever had set
        // it. So a relay alternating between "missing from one ring" and "a client got served"
        // reset to 0s every other cycle and could never reach the restart threshold. Which is not
        // an edge case: a one-sided outage serves everyone on the good ring the whole time, so
        // that alternation IS the failure, not a distraction from it.
        let start = Instant::now();
        let mut clocks = DarkClocks::default();

        // Cycle 1: half-published, and the dial fails too.
        clocks.observe(true, true, start);
        // Cycle 2: the publisher is still dark; a client was served, so the dial is excused.
        clocks.excuse(false);
        let later = start + WATCHDOG_MAX_DARK;
        clocks.observe(true, true, later);

        assert_eq!(
            clocks.dark_for(true, later),
            WATCHDOG_MAX_DARK,
            "the publisher's darkness must survive a veto that only explains the dial"
        );
        assert_eq!(
            clocks.dark_for(false, later),
            Duration::ZERO,
            "the dial's own clock is the one that veto resets"
        );
    }

    #[test]
    fn an_unusable_tor_client_clears_both_clocks() {
        // The counterpart: this veto really does explain both signals, so it must clear both or a
        // box that was merely offline would come back to a pre-loaded restart timer and reset its
        // introduction points over an outage no restart could fix.
        let start = Instant::now();
        let mut clocks = DarkClocks::default();
        clocks.observe(true, true, start);
        clocks.excuse(true);
        let later = start + WATCHDOG_MAX_DARK;
        assert_eq!(clocks.dark_for(true, later), Duration::ZERO);
        assert_eq!(clocks.dark_for(false, later), Duration::ZERO);
    }

    #[test]
    fn a_signal_going_good_stops_its_own_clock_and_leaves_the_other_running() {
        let start = Instant::now();
        let mut clocks = DarkClocks::default();
        clocks.observe(true, true, start);
        // The dial comes back; the descriptor is still missing from a ring.
        let later = start + WATCHDOG_MAX_DARK;
        clocks.observe(true, false, later);
        assert_eq!(clocks.dark_for(true, later), WATCHDOG_MAX_DARK);
        assert_eq!(clocks.dark_for(false, later), Duration::ZERO);
    }

    #[test]
    fn an_unusable_tor_client_excuses_the_ring_check_too_and_never_restarts() {
        // The one veto that outranks BOTH signals, because it explains both: a descriptor upload
        // needs a working Tor client every bit as much as a dial does. Getting this order wrong
        // means a box that is merely offline restarts itself in a loop — repeatedly rotating its
        // introduction points, and on the third cycle wiping its entry guards — over an outage no
        // restart can fix. That is the "reset because the device is OFFLINE" mistake, and it is
        // worth a test precisely because the code reads fine either way.
        let offline = Cycle {
            publisher_dark: true,
            dial_failed: true,
            client_unusable: Some("Offline: we seem to be offline".into()),
            ..healthy(WATCHDOG_MAX_DARK * 10)
        };
        assert_eq!(
            offline.verdict(),
            Verdict::Inconclusive("our own Tor client is unusable")
        );
    }

    #[test]
    fn a_transient_client_complaint_cannot_downgrade_a_cycle_that_is_actually_fine() {
        // `client_unusable` is imprecise by design (`ready_for_traffic` reports that arti cannot
        // act without saying why). If it were consulted before the healthy check, an arti that
        // grumbles while everything works would suppress the heartbeat and the counter reset.
        let fine = Cycle {
            client_unusable: Some("not ready for traffic".into()),
            ..healthy(Duration::ZERO)
        };
        assert_eq!(fine.verdict(), Verdict::Healthy);
    }

    #[test]
    fn the_probe_asks_the_relay_a_question_that_stores_nothing() {
        // The self-dial has to exercise the real path without leaving anything behind on a relay
        // whose whole point is holding as little as possible. GetDirectory is read-only and
        // storeless, and a healthy relay answers ok even when it serves no directory.
        let core = RelayCore::new(None);
        let line = nightdrop::relay_client::request_line(&Request::GetDirectory).unwrap();
        let response =
            nightdrop::relay_client::parse_response_line(&core.handle_line(line.trim())).unwrap();
        assert!(response.ok);
        assert!(core.snapshot().is_empty());
    }
}
