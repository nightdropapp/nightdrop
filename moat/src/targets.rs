//! The meek `targets` string: where the domain-fronted tunnel goes.
//!
//! Same syntax as Tor Browser's `extensions.torlauncher.bridgedb_targets` and lyrebird's meek
//! `targets=` argument: groups separated by `,`, each `URL|FRONT+FRONT…`. The URL's host is the
//! CDN-hosted meek reflector (sent as the HTTP `Host` header); each front is a domain served by the
//! same CDN, used for DNS, TCP and the TLS SNI — the only name an observer of the connection sees.

use anyhow::{bail, Result};

/// One way into the meek reflector: the TLS/SNI name to connect to, and the reflector host to ask
/// for once inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Connected to, and named in the TLS SNI. What the network sees.
    pub front: String,
    /// The reflector, sent as the HTTP `Host` header inside the encrypted connection.
    pub host: String,
    /// Request path on the reflector (normally `/`).
    pub path: String,
}

/// Parse a targets string into every (front, reflector) pair, in the order given.
pub fn parse(targets: &str) -> Result<Vec<Target>> {
    let mut out = Vec::new();
    for group in targets.split(',').map(str::trim).filter(|g| !g.is_empty()) {
        let Some((url, fronts)) = group.split_once('|') else {
            bail!("meek target {group:?} has no '|' between the URL and its fronts");
        };
        let Some(rest) = url.strip_prefix("https://") else {
            bail!("meek target URL {url:?} must be https://");
        };
        let (host, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if !is_hostname(host) {
            bail!("meek target URL {url:?} has no valid host");
        }
        for front in fronts.split('+').map(str::trim).filter(|f| !f.is_empty()) {
            if !is_hostname(front) {
                bail!("meek front {front:?} is not a hostname");
            }
            out.push(Target {
                front: front.to_string(),
                host: host.to_string(),
                path: path.to_string(),
            });
        }
    }
    if out.is_empty() {
        bail!("no meek targets");
    }
    Ok(out)
}

/// A DNS name: labels of letters, digits and hyphens, with a top-level label that contains a
/// letter. No ports, IP addresses or userinfo — a front is a domain on purpose, and this string
/// ends up in a TLS SNI and an HTTP header.
fn is_hostname(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.contains('.')
        && s.rsplit('.')
            .next()
            .is_some_and(|tld| tld.bytes().any(|b| b.is_ascii_alphabetic()))
        && s.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tor_browser_targets_parse_into_one_pair_per_front() {
        // Tor Browser 16.0's value, 2026-10-03.
        let t = parse("https://1723079976.rsc.cdn77.org|cdn.zk.mk+www.cdn77.com").unwrap();
        assert_eq!(
            t,
            vec![
                Target {
                    front: "cdn.zk.mk".into(),
                    host: "1723079976.rsc.cdn77.org".into(),
                    path: "/".into()
                },
                Target {
                    front: "www.cdn77.com".into(),
                    host: "1723079976.rsc.cdn77.org".into(),
                    path: "/".into()
                },
            ]
        );
    }

    #[test]
    fn several_groups_and_paths() {
        let t = parse(
            "https://a.cdn.example/meek|x.example, https://b.example.net|y.example+z.example",
        )
        .unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(
            (t[0].host.as_str(), t[0].path.as_str()),
            ("a.cdn.example", "/meek")
        );
        assert_eq!(t[2].front, "z.example");
    }

    #[test]
    fn malformed_targets_are_refused() {
        for bad in [
            "",
            "https://x.example",                // no fronts
            "http://x.example|f.example",       // not https
            "https://x.example|",               // empty front list
            "https://x.example|f.example:443",  // port in front
            "https://x.example|1.2.3.4",        // an IP address, not a domain
            "https://user@x.example|f.example", // userinfo
            "https://x.example|f example",      // space
        ] {
            let r = parse(bad);
            assert!(r.is_err(), "{bad:?} parsed as {r:?}");
        }
    }
}
