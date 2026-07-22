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
use std::sync::atomic::{AtomicUsize, Ordering};
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
use nightdrop::relay_client::{RelayCore, RelayEvent, RelayLimits, RelayLogger};
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

        // Self-healing watchdog. arti keeps this process alive even when its descriptor publisher
        // or introduction points wedge (observed after multi-day uptime: the process runs, the
        // onion goes dark, clients get "could not reach relay"). `Restart=always` can't recover
        // that — nothing crashed. So we watch the service's own reachability and exit when it has
        // been unreachable long enough to warrant a fresh start; systemd then restarts us, which
        // re-establishes intro points and republishes the descriptor. A brief unreachable window
        // is normal during startup and intro-point rotation, so we only act on a sustained outage.
        {
            let watched = Arc::clone(&service);
            let sd = state_dir.clone();
            tokio::spawn(async move {
                let mut unreachable_since: Option<Instant> = None;
                let mut cleared = false;
                loop {
                    tokio::time::sleep(WATCHDOG_INTERVAL).await;
                    if watched.status().state().is_fully_reachable() {
                        unreachable_since = None;
                        // Reachable: clear the escalation counter once, so a *later* independent
                        // outage starts its own plain-restart-then-guard-reset sequence from zero.
                        if !cleared {
                            write_unhealthy(&sd, 0);
                            cleared = true;
                        }
                        continue;
                    }
                    let since = *unreachable_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= WATCHDOG_MAX_UNREACHABLE {
                        // Record this unhealthy cycle so the next start can escalate to a guard
                        // reset if plain restarts keep failing, then exit for systemd to restart us.
                        let n = read_unhealthy(&sd) + 1;
                        write_unhealthy(&sd, n);
                        eprintln!(
                            "nightdrop-relay: onion unreachable for {}s (unhealthy cycle {n}) — \
                             exiting for a fresh restart (systemd Restart=always)",
                            since.elapsed().as_secs()
                        );
                        std::process::exit(1);
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
            tokio::spawn(async move {
                let _guard = guard; // decrements the counter when this task ends
                if let Ok(stream) = request.accept(Connected::new_empty()).await {
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

/// How often the self-healing watchdog samples the onion's reachability.
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(30);
/// How long the onion may stay unreachable before the watchdog exits for a fresh restart. Long
/// enough to ride out normal startup bootstrapping (~1–3 min) and intro-point rotation without a
/// spurious restart; short enough that a real wedge self-corrects in minutes, not days.
const WATCHDOG_MAX_UNREACHABLE: Duration = Duration::from_secs(8 * 60);

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
