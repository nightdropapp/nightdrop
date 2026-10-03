//! The moat "Circumvention Settings" API (rdsys `doc/moat.md`): ask for bridge lines suited to a
//! country, through any [`RoundTrip`].
//!
//! `/circumvention/settings` answers for a country (geolocated from the request's IP when none is
//! given). An empty answer means "Tor works there without help" and a 406 means "country unknown";
//! the user is asking because Tor is *not* working, so both fall back to `/circumvention/defaults`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

use crate::meek::RoundTrip;
use crate::tunnel;

/// The only host this crate ever talks to inside the tunnel.
pub const MOAT_HOST: &str = "bridges.torproject.org";
const SETTINGS: &str = "/moat/circumvention/settings";
const DEFAULTS: &str = "/moat/circumvention/defaults";
/// What Tor Browser sends; moat accepts plain JSON bodies under it.
const CONTENT_TYPE: &str = "application/vnd.api+json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
/// A bridge line longer than this is not one.
const MAX_LINE: usize = 2048;

#[derive(Serialize)]
struct Request<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    country: Option<&'a str>,
    transports: &'a [&'a str],
}

#[derive(Deserialize, Default)]
struct Response {
    #[serde(default)]
    settings: Option<Vec<Setting>>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    errors: Option<Vec<ApiError>>,
}

#[derive(Deserialize)]
struct Setting {
    bridges: Bridges,
}

#[derive(Deserialize)]
struct Bridges {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    bridge_strings: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ApiError {
    code: i64,
    #[serde(default)]
    detail: String,
}

/// What a fetch found.
#[derive(Debug, PartialEq, Eq)]
pub struct Fetched {
    /// The country moat answered for (the one asked, or the geolocated one).
    pub country: Option<String>,
    /// Usable bridge lines, best first, each starting with one of the requested transports.
    pub lines: Vec<String>,
    /// Whether these came from the generic defaults rather than a country recommendation.
    pub from_defaults: bool,
}

/// Opens a fresh tunnel. Each moat call needs its own: the server closes its connection after
/// answering, and meek then reports that session as finished for good.
pub type NewTunnel<'a> = dyn FnMut() -> Result<Box<dyn RoundTrip>> + 'a;

/// Ask moat for bridges of the given `transports` (e.g. `["webtunnel"]`), for `country` (an ISO
/// code) or, when `None`, for wherever the request appears to come from.
pub fn fetch(
    new_tunnel: &mut NewTunnel,
    tls: Arc<rustls::ClientConfig>,
    country: Option<&str>,
    transports: &[&str],
) -> Result<Fetched> {
    if transports.is_empty() {
        bail!("no transports to ask for");
    }
    if let Some(c) = country {
        if c.len() != 2 || !c.bytes().all(|b| b.is_ascii_lowercase()) {
            bail!("country must be a two-letter lowercase code, not {c:?}");
        }
    }
    let settings = call(
        new_tunnel()?.as_mut(),
        tls.clone(),
        SETTINGS,
        &Request {
            country,
            transports,
        },
    )?;
    let answered_country = settings.country.clone();
    match api_error(&settings) {
        None => {
            let lines = usable_lines(&settings, transports);
            if !lines.is_empty() {
                return Ok(Fetched {
                    country: answered_country,
                    lines,
                    from_defaults: false,
                });
            }
        }
        // Country unknown: the defaults are the right answer.
        Some((406, _)) => {}
        Some((404, _)) => bail!(
            "the Tor Project has no {} bridges that work in this country",
            transports.join("/")
        ),
        Some((code, detail)) => bail!("bridge service error {code}: {detail}"),
    }
    let defaults = call(
        new_tunnel()?.as_mut(),
        tls,
        DEFAULTS,
        &Request {
            country: None,
            transports,
        },
    )?;
    if let Some((code, detail)) = api_error(&defaults) {
        bail!("bridge service error {code}: {detail}");
    }
    let lines = usable_lines(&defaults, transports);
    if lines.is_empty() {
        bail!(
            "the bridge service returned no {} bridges",
            transports.join("/")
        );
    }
    Ok(Fetched {
        country: answered_country,
        lines,
        from_defaults: true,
    })
}

fn call(
    rt: &mut dyn RoundTrip,
    tls: Arc<rustls::ClientConfig>,
    path: &str,
    req: &Request,
) -> Result<Response> {
    let body = serde_json::to_vec(req)?;
    let resp = tunnel::https_post(
        rt,
        tls,
        MOAT_HOST,
        path,
        CONTENT_TYPE,
        &body,
        REQUEST_TIMEOUT,
    )?;
    if resp.status != 200 {
        bail!("bridge service answered HTTP {}", resp.status);
    }
    serde_json::from_slice(&resp.body)
        .map_err(|e| anyhow!("bridge service answer is not the expected JSON: {e}"))
}

fn api_error(r: &Response) -> Option<(i64, String)> {
    r.errors
        .as_ref()?
        .first()
        .map(|e| (e.code, e.detail.clone()))
}

/// Bridge lines of a requested type, in moat's order, deduplicated, each a single sane line.
fn usable_lines(r: &Response, transports: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in r.settings.iter().flatten() {
        if !transports.contains(&s.bridges.kind.as_str()) {
            continue;
        }
        for line in s.bridges.bridge_strings.iter().flatten() {
            let line = line.trim();
            let starts_right = line
                .split_whitespace()
                .next()
                .is_some_and(|t| t == s.bridges.kind);
            if starts_right
                && line.len() <= MAX_LINE
                && !line.contains(['\n', '\r'])
                && !out.iter().any(|l| l == line)
            {
                out.push(line.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meek::tests::test_pki;
    use crate::meek::Reply;
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::rc::Rc;

    /// What the fake moat server was asked.
    #[derive(Debug, Clone)]
    struct Asked {
        head: String,
        body: serde_json::Value,
    }

    /// A meek tunnel that ends in a fake moat server: a real TLS server for bridges.torproject.org
    /// (test certificate) that answers one request, chunked, then closes - as the real one does.
    struct FakeMoat {
        tls: rustls::ServerConnection,
        inbox: Vec<u8>,
        answered: bool,
        handler: Rc<dyn Fn(&Asked) -> String>,
        log: Rc<RefCell<Vec<Asked>>>,
    }

    impl RoundTrip for FakeMoat {
        fn round_trip(&mut self, up: &[u8]) -> Result<Reply> {
            let mut rd = up;
            while !rd.is_empty() {
                self.tls.read_tls(&mut rd)?;
                self.tls.process_new_packets()?;
            }
            let mut chunk = [0u8; 8192];
            loop {
                match self.tls.reader().read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => self.inbox.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            if !self.answered {
                if let Some(end) = self.inbox.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&self.inbox[..end]).to_string();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .map(|v| v.parse().unwrap())
                        .unwrap_or(0);
                    if self.inbox.len() >= end + 4 + len {
                        let body =
                            serde_json::from_slice(&self.inbox[end + 4..end + 4 + len]).unwrap();
                        let asked = Asked { head, body };
                        let json = (self.handler)(&asked);
                        self.log.borrow_mut().push(asked);
                        // Chunked, in two chunks, then close - the shape of the real server.
                        let (a, b) = json.split_at(json.len() / 2);
                        write!(
                            self.tls.writer(),
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n",
                            a.len(),
                            b.len()
                        )?;
                        self.tls.send_close_notify();
                        self.answered = true;
                    }
                }
            }
            let mut down = Vec::new();
            while self.tls.wants_write() {
                self.tls.write_tls(&mut down)?;
            }
            if down.is_empty() && self.answered {
                return Ok(Reply::Closed);
            }
            Ok(Reply::Data(down))
        }
    }

    /// Run `fetch` against fake moat servers that answer with `handler`. Returns the result and
    /// every request the servers saw.
    fn run(
        country: Option<&str>,
        transports: &[&str],
        handler: impl Fn(&Asked) -> String + 'static,
    ) -> (Result<Fetched>, Vec<Asked>) {
        let (server_cfg, client_cfg) = test_pki(&[MOAT_HOST]);
        let log = Rc::new(RefCell::new(Vec::new()));
        let handler: Rc<dyn Fn(&Asked) -> String> = Rc::new(handler);
        let log2 = log.clone();
        let mut new_tunnel = move || -> Result<Box<dyn RoundTrip>> {
            Ok(Box::new(FakeMoat {
                tls: rustls::ServerConnection::new(server_cfg.clone())?,
                inbox: Vec::new(),
                answered: false,
                handler: handler.clone(),
                log: log2.clone(),
            }))
        };
        let r = fetch(&mut new_tunnel, client_cfg, country, transports);
        let asked = log.borrow().clone();
        (r, asked)
    }

    const CN: &str = r#"{"settings":[
        {"bridges":{"type":"webtunnel","source":"bridgedb","bridge_strings":[
            "webtunnel [2001:db8::1]:443 AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA url=https://a.example/x ver=0.0.3",
            "webtunnel [2001:db8::2]:443 BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB url=https://b.example/y ver=0.0.3"]}},
        {"bridges":{"type":"obfs4","source":"bridgedb","bridge_strings":["obfs4 192.0.2.1:443 CCCC cert=x iat-mode=0"]}}
        ],"country":"cn"}"#;

    #[test]
    fn a_country_answer_gives_the_webtunnel_lines_only() {
        let (r, asked) = run(Some("cn"), &["webtunnel"], |_| CN.to_string());
        let f = r.unwrap();
        assert_eq!(f.country.as_deref(), Some("cn"));
        assert!(!f.from_defaults);
        assert_eq!(f.lines.len(), 2);
        assert!(f.lines.iter().all(|l| l.starts_with("webtunnel ")));
        // One request, to moat's settings endpoint, on bridges.torproject.org, saying what we want.
        assert_eq!(asked.len(), 1);
        assert!(asked[0]
            .head
            .starts_with("POST /moat/circumvention/settings HTTP/1.1\r\n"));
        assert!(asked[0]
            .head
            .contains("\r\nHost: bridges.torproject.org\r\n"));
        assert_eq!(
            asked[0].body,
            serde_json::json!({"country": "cn", "transports": ["webtunnel"]})
        );
    }

    #[test]
    fn no_recommendation_falls_back_to_the_defaults() {
        // Geolocated somewhere Tor "should" work - but the user is asking because it does not.
        let (r, asked) = run(None, &["webtunnel"], |a| {
            if a.head.contains("/settings ") {
                r#"{"settings":[],"country":"ca"}"#.into()
            } else {
                CN.replace(r#""country":"cn""#, r#""country":null"#)
            }
        });
        let f = r.unwrap();
        assert!(f.from_defaults);
        assert_eq!(f.country.as_deref(), Some("ca"));
        assert_eq!(f.lines.len(), 2);
        assert_eq!(asked.len(), 2);
        assert_eq!(
            asked[0].body,
            serde_json::json!({"transports": ["webtunnel"]}),
            "no country sent unless chosen"
        );
        assert!(asked[1]
            .head
            .starts_with("POST /moat/circumvention/defaults "));
    }

    #[test]
    fn an_unknown_country_falls_back_and_other_errors_are_reported() {
        let (r, _) = run(None, &["webtunnel"], |a| {
            if a.head.contains("/settings ") {
                r#"{"errors":[{"code":406,"detail":"Could not find country code"}]}"#.into()
            } else {
                CN.into()
            }
        });
        assert!(r.unwrap().from_defaults);

        let (r, asked) = run(Some("cn"), &["webtunnel"], |_| {
            r#"{"errors":[{"code":404,"detail":"No provided transport is available for this country"}]}"#.into()
        });
        assert!(format!("{:#}", r.unwrap_err()).contains("no webtunnel bridges that work"));
        assert_eq!(
            asked.len(),
            1,
            "a 404 is an answer, not a reason to ask the defaults"
        );
    }

    #[test]
    fn junk_lines_and_wrong_types_never_reach_the_user() {
        let (r, _) = run(Some("cn"), &["webtunnel"], |_| {
            r#"{"settings":[{"bridges":{"type":"webtunnel","bridge_strings":[
                "obfs4 192.0.2.1:443 X cert=y",
                "webtunnel ok.example:443 DDDD url=https://ok.example ver=1\nBridge evil",
                "webtunnel good.example:443 EEEE url=https://good.example ver=1",
                "webtunnel good.example:443 EEEE url=https://good.example ver=1"]}}],"country":"cn"}"#
                .into()
        });
        assert_eq!(
            r.unwrap().lines,
            vec!["webtunnel good.example:443 EEEE url=https://good.example ver=1".to_string()]
        );
    }

    #[test]
    fn a_bad_country_code_is_refused_before_anything_is_sent() {
        let (r, asked) = run(Some("CN; DROP"), &["webtunnel"], |_| CN.into());
        assert!(r.is_err());
        assert!(asked.is_empty());
    }
}
