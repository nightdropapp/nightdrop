//! One HTTPS request carried inside a meek tunnel.
//!
//! The meek reflector forwards a raw byte stream to its fixed destination; we run an ordinary TLS
//! session to that destination over it (rustls driven by hand: `write_tls` goes up in a round
//! trip, the reply is fed to `read_tls`), then one HTTP/1.1 request. The destination's certificate
//! is verified as usual, so the CDN and the reflector see only ciphertext.

use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use crate::http;
use crate::meek::{Reply, RoundTrip, MAX_PAYLOAD};

/// Idle polling, as lyrebird's meek client: start at 100 ms, back off x1.5, cap at 5 s.
const POLL_START: Duration = Duration::from_millis(100);
const POLL_MAX: Duration = Duration::from_secs(5);

/// POST `body` to `https://{host}{path}` through `rt`, and return the response.
pub fn https_post(
    rt: &mut dyn RoundTrip,
    tls: Arc<rustls::ClientConfig>,
    host: &str,
    path: &str,
    content_type: &str,
    body: &[u8],
    timeout: Duration,
) -> Result<http::Response> {
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| anyhow!("{host} is not a valid TLS name"))?;
    let mut conn = rustls::ClientConnection::new(tls, name)?;
    // Buffered by rustls until the handshake completes, then sent encrypted.
    write!(
        conn.writer(),
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    conn.writer().write_all(body)?;

    let deadline = Instant::now() + timeout;
    let mut plain = Vec::new();
    let mut poll = POLL_START;
    let mut closed = false;
    loop {
        if Instant::now() > deadline {
            bail!("no complete answer from {host} within {timeout:?}");
        }
        // What TLS wants to send goes up in this round trip, at most meek's 64 KiB; anything over
        // stays buffered in rustls for the next one (never truncated: that would corrupt TLS).
        let mut up = Capped(Vec::new());
        while conn.wants_write() && up.0.len() < MAX_PAYLOAD {
            if conn.write_tls(&mut up)? == 0 {
                break;
            }
        }
        let up = up.0;
        let had_up = !up.is_empty();
        let down = match rt.round_trip(&up)? {
            Reply::Data(d) => d,
            Reply::Closed => {
                closed = true;
                Vec::new()
            }
        };
        let had_down = !down.is_empty();
        let mut rd = &down[..];
        while !rd.is_empty() {
            conn.read_tls(&mut rd)?;
            conn.process_new_packets()
                .map_err(|e| anyhow!("TLS to {host} failed: {e}"))?;
        }
        read_plaintext(&mut conn, &mut plain)?;
        if plain.len() > http::MAX_BODY {
            bail!("answer from {host} is over the size limit");
        }
        if let Some((resp, _)) = http::parse(&plain)? {
            return Ok(resp);
        }
        if closed {
            bail!("{host} closed the connection before a complete answer");
        }
        if had_up || had_down {
            poll = POLL_START;
        } else {
            std::thread::sleep(poll);
            poll = (poll.mul_f32(1.5)).min(POLL_MAX);
        }
    }
}

/// A writer that accepts at most [`MAX_PAYLOAD`] bytes in total, so rustls keeps the rest.
struct Capped(Vec<u8>);

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = buf.len().min(MAX_PAYLOAD - self.0.len());
        self.0.extend_from_slice(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Drain whatever plaintext rustls has decrypted so far.
fn read_plaintext(conn: &mut rustls::ClientConnection, out: &mut Vec<u8>) -> Result<()> {
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match conn.reader().read(&mut chunk) {
            Ok(0) => return Ok(()), // the server sent close_notify
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
}

/// TLS settings for the destination inside the tunnel: Mozilla roots, HTTP/1.1.
pub fn inner_tls_config() -> Arc<rustls::ClientConfig> {
    crate::meek::front_tls_config()
}
