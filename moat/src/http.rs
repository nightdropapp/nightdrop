//! Just enough HTTP/1.1 to read one response: a status line, headers, and a body framed by
//! `Content-Length` or chunked transfer encoding. Used twice — for each meek round trip on the
//! outer (fronted) connection, and for the moat response inside the tunnel.
//!
//! Incremental: [`parse`] returns `Ok(None)` until the buffer holds a whole response, so callers
//! can keep reading. Bounded: headers and bodies over the limits are errors, so a hostile or broken
//! server cannot make us buffer without end.

use anyhow::{anyhow, bail, Result};

/// Larger than any moat answer (a few KiB) or meek payload (64 KiB), small enough to be harmless.
pub const MAX_BODY: usize = 1 << 20;
const MAX_HEAD: usize = 16 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Parse one response from the start of `buf`. `Ok(None)` = incomplete; `Ok(Some((r, n)))` = a
/// whole response that used the first `n` bytes.
pub fn parse(buf: &[u8]) -> Result<Option<(Response, usize)>> {
    let Some(head_end) = find(buf, b"\r\n\r\n") else {
        if buf.len() > MAX_HEAD {
            bail!("HTTP header section over {MAX_HEAD} bytes");
        }
        return Ok(None);
    };
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| anyhow!("non-UTF-8 HTTP head"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let (Some(version), Some(code)) = (parts.next(), parts.next()) else {
        bail!("bad HTTP status line {status_line:?}");
    };
    if !version.starts_with("HTTP/1.") {
        bail!("not an HTTP/1.x response: {status_line:?}");
    }
    let status: u16 = code
        .parse()
        .map_err(|_| anyhow!("bad HTTP status {code:?}"))?;

    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            bail!("bad HTTP header line {line:?}");
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            let n: usize = value
                .parse()
                .map_err(|_| anyhow!("bad Content-Length {value:?}"))?;
            if n > MAX_BODY {
                bail!("HTTP body of {n} bytes is over the {MAX_BODY}-byte limit");
            }
            content_length = Some(n);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value
                .to_ascii_lowercase()
                .split(',')
                .any(|t| t.trim() == "chunked");
        }
    }

    let body_start = head_end + 4;
    let rest = &buf[body_start..];
    if chunked {
        return Ok(
            parse_chunked(rest)?.map(|(body, n)| (Response { status, body }, body_start + n))
        );
    }
    // No framing at all: an empty body (meek's idle replies, and every 1xx/204/304).
    let len = content_length.unwrap_or(0);
    if rest.len() < len {
        return Ok(None);
    }
    Ok(Some((
        Response {
            status,
            body: rest[..len].to_vec(),
        },
        body_start + len,
    )))
}

/// Decode a chunked body. `Ok(None)` until the terminating zero-size chunk (and its blank line)
/// has arrived. Trailers are skipped.
fn parse_chunked(buf: &[u8]) -> Result<Option<(Vec<u8>, usize)>> {
    let mut body = Vec::new();
    let mut i = 0;
    loop {
        let Some(line_end) = find(&buf[i..], b"\r\n") else {
            return Ok(None);
        };
        let size_line =
            std::str::from_utf8(&buf[i..i + line_end]).map_err(|_| anyhow!("bad chunk size"))?;
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| anyhow!("bad chunk size {size_hex:?}"))?;
        i += line_end + 2;
        if size == 0 {
            // Trailers (none expected), then the final blank line.
            loop {
                let Some(end) = find(&buf[i..], b"\r\n") else {
                    return Ok(None);
                };
                i += end + 2;
                if end == 0 {
                    return Ok(Some((body, i)));
                }
            }
        }
        if body.len() + size > MAX_BODY {
            bail!("chunked HTTP body over the {MAX_BODY}-byte limit");
        }
        if buf.len() < i + size + 2 {
            return Ok(None);
        }
        body.extend_from_slice(&buf[i..i + size]);
        if &buf[i + size..i + size + 2] != b"\r\n" {
            bail!("chunk not followed by CRLF");
        }
        i += size + 2;
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_body_arrives_whole_or_not_at_all() {
        let full = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhelloEXTRA";
        for cut in 0..(full.len() - 5) {
            assert_eq!(parse(&full[..cut]).unwrap(), None, "cut at {cut}");
        }
        let (r, n) = parse(full).unwrap().unwrap();
        assert_eq!(
            (r.status, r.body.as_slice(), n),
            (200, &b"hello"[..], full.len() - 5)
        );
    }

    #[test]
    fn an_unframed_reply_has_an_empty_body() {
        // meek's idle poll answer.
        let idle = b"HTTP/1.1 200 OK\r\nDate: x\r\n\r\n";
        let (r, n) = parse(idle).unwrap().unwrap();
        assert_eq!((r.status, r.body.len(), n), (200, 0, idle.len()));
    }

    #[test]
    fn chunked_bodies_decode_at_every_boundary() {
        // moat's reply shape (Transfer-Encoding: chunked), with a chunk extension thrown in.
        let full = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3;x=y\r\n:1}\r\n0\r\n\r\n";
        for cut in 0..full.len() {
            assert_eq!(parse(&full[..cut]).unwrap(), None, "cut at {cut}");
        }
        let (r, n) = parse(full).unwrap().unwrap();
        assert_eq!((r.body.as_slice(), n), (&b"{\"a\":1}"[..], full.len()));
    }

    #[test]
    fn hostile_or_broken_responses_are_errors() {
        assert!(parse(b"HTTP/2 200\r\n\r\n").is_err());
        assert!(parse(b"HTTP/1.1 abc OK\r\n\r\n").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nContent-Length: 99999999\r\n\r\n").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nabXX").is_err());
        let huge_head = vec![b'a'; MAX_HEAD + 1];
        assert!(parse(&huge_head).is_err());
    }
}
