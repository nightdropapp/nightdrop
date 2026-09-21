//! In-process WebTunnel pluggable-transport client.
//!
//! WebTunnel hides a Tor connection inside what looks like an ordinary HTTPS request to a real
//! website: TLS to the bridge's web server, an HTTP/1.1 `Upgrade: websocket` on a secret path,
//! and — once the server answers `101 Switching Protocols` — raw Tor bytes both ways, with no
//! WebSocket framing. Wire-compatible with the Tor Project's Go implementation.
//!
//! Two layers:
//! - [`connect`] dials one bridge and returns the tunnelled byte stream. It knows nothing about
//!   how Tor reaches it.
//! - [`socks`] serves that to a Tor client as an *unmanaged* pluggable transport: a SOCKS5
//!   listener on loopback, which arti reaches via `proxy_addr`.

pub mod args;
pub mod client;
pub mod config;
pub mod socks;
mod tls;

pub use args::PtArgs;
pub use client::{connect, WebTunnelStream};
pub use config::ClientConfig;
pub use tls::chain_hash;

/// The transport name bridge lines use: `Bridge webtunnel <addr> <fingerprint> url=…`.
pub const TRANSPORT_NAME: &str = "webtunnel";

/// Everything that can go wrong between a bridge line and an open tunnel.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The pluggable-transport argument list could not be parsed.
    #[error("bad transport arguments: {0}")]
    Args(String),
    /// The arguments parsed but do not describe a usable WebTunnel bridge.
    #[error("bad WebTunnel bridge config: {0}")]
    Config(String),
    /// TCP connect, TLS, or I/O failed.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    /// The TLS handshake completed but the server's certificate was not acceptable.
    #[error("TLS: {0}")]
    Tls(String),
    /// The server did not answer the HTTP Upgrade the way a WebTunnel bridge does.
    #[error("HTTP upgrade refused: {0}")]
    Upgrade(String),
    /// A SOCKS client spoke something other than the SOCKS5 subset Tor uses.
    #[error("SOCKS: {0}")]
    Socks(String),
}

impl Error {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }
}
