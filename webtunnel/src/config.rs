//! Turning a bridge's argument list into a dialable WebTunnel config.
//!
//! Options follow lyrebird's WebTunnel client, which is what Tor Browser and Orbot ship, so a
//! bridge line from bridges.torproject.org means the same thing here as there:
//!
//! | option         | meaning |
//! |----------------|---------|
//! | `url`          | **required.** `https://host[:port]/secret-path` — HTTP host, path, TLS on/off |
//! | `addr`         | TCP endpoint to dial instead of resolving the URL's host |
//! | `servername`   | TLS SNI to send; a comma-separated list means "pick one, move on if it fails" |
//! | `sni-imitation`| like `servername`, but the certificate is still checked against the URL's host |
//! | `cert-domain`  | name to check the certificate against, when it differs from the SNI |
//! | `cert`         | base64 SHA-256 certificate-chain pin; replaces CA validation |
//! | `utls`         | accepted, not yet honoured (the TLS fingerprint is step 2 of the design) |
//! | `ver`          | server version; informational |

use crate::{Error, PtArgs};
use base64::Engine as _;

/// Where the TCP connection goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Remote {
    /// `addr=` was given: dial exactly this `host:port`, no filtering.
    Explicit(String),
    /// Resolve the URL's host. Private, loopback and other non-public results are dropped, so
    /// a bridge hostname can't be pointed at the device's own network.
    Resolve { host: String, port: u16 },
}

/// The TLS layer, present for `https://` URLs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsConfig {
    /// Candidate SNI values. One is used per attempt, rotating to the next only when an
    /// upgrade through it fails (lyrebird's behaviour).
    pub server_names: Vec<String>,
    /// Name the certificate must be valid for. `None` means "whatever SNI was sent".
    pub verify_name: Option<String>,
    /// SHA-256 certificate-chain pin. When set, CA validation is replaced by the pin.
    pub pinned_chain_hash: Option<[u8; 32]>,
}

/// One WebTunnel bridge, ready to dial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientConfig {
    pub remote: Remote,
    /// `Host:` header, and the default SNI.
    pub http_host: String,
    /// The secret path, without its leading `/`, percent-encoded as it appeared in the URL.
    pub path: String,
    /// `None` for a plain `http://` URL.
    pub tls: Option<TlsConfig>,
}

impl ClientConfig {
    pub fn from_args(args: &PtArgs) -> Result<Self, Error> {
        let url_str = args
            .get("url")
            .ok_or_else(|| Error::Config("missing url".into()))?;
        let url =
            url::Url::parse(url_str).map_err(|e| Error::Config(format!("url {url_str:?}: {e}")))?;
        let (tls_on, default_port) = match url.scheme() {
            "https" => (true, 443),
            "http" => (false, 80),
            other => {
                return Err(Error::Config(format!(
                    "url scheme {other:?} is not http or https"
                )))
            }
        };
        let host = match url.host() {
            // Bracketless, as goptlib's url.Hostname() gives it and as SNI/Host want it.
            Some(url::Host::Ipv6(ip)) => ip.to_string(),
            Some(h) => h.to_string(),
            None => return Err(Error::Config("url has no host".into())),
        };
        let port = url.port().unwrap_or(default_port);
        // Go's url.EscapedPath(): the query and fragment are not part of the upgrade request.
        let path = url.path().trim_start_matches('/').to_string();

        let remote = match args.get("addr") {
            Some(a) if !a.is_empty() => Remote::Explicit(a.to_string()),
            _ => Remote::Resolve {
                host: host.clone(),
                port,
            },
        };

        let tls = if tls_on {
            let mut server_names = vec![host.clone()];
            let mut verify_name = None;
            if let Some(spec) = args.get("sni-imitation") {
                server_names = split_names(spec)?;
                verify_name = Some(host.clone());
            }
            if let Some(spec) = args.get("servername") {
                server_names = split_names(spec)?;
            }
            if let Some(name) = args.get("cert-domain") {
                verify_name = Some(name.to_string());
            }
            let pinned_chain_hash = match args.get("cert") {
                None => None,
                Some(b64) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(b64)
                        .map_err(|e| Error::Config(format!("cert is not base64: {e}")))?;
                    let hash: [u8; 32] = bytes.try_into().map_err(|_| {
                        Error::Config("cert is not a 32-byte SHA-256 chain hash".into())
                    })?;
                    Some(hash)
                }
            };
            Some(TlsConfig {
                server_names,
                verify_name,
                pinned_chain_hash,
            })
        } else {
            None
        };

        Ok(ClientConfig {
            remote,
            http_host: host,
            path,
            tls,
        })
    }
}

/// lyrebird's servername spec: comma-separated, whitespace-trimmed, empties dropped.
fn split_names(spec: &str) -> Result<Vec<String>, Error> {
    let names: Vec<String> = spec
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if names.is_empty() {
        return Err(Error::Config(format!("server name list {spec:?} is empty")));
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(s: &str) -> Result<ClientConfig, Error> {
        ClientConfig::from_args(&PtArgs::parse(s).unwrap())
    }

    #[test]
    fn plain_https_bridge_line() {
        // The shape bridges.torproject.org hands out.
        let c = cfg("url=https://cdn.example.org/x9ab0cd1e2;ver=0.0.3").unwrap();
        assert_eq!(
            c.remote,
            Remote::Resolve {
                host: "cdn.example.org".into(),
                port: 443
            }
        );
        assert_eq!(c.http_host, "cdn.example.org");
        assert_eq!(c.path, "x9ab0cd1e2");
        let tls = c.tls.unwrap();
        assert_eq!(tls.server_names, ["cdn.example.org"]);
        assert_eq!(tls.verify_name, None);
        assert_eq!(tls.pinned_chain_hash, None);
    }

    #[test]
    fn http_url_has_no_tls_and_port_80() {
        let c = cfg("url=http://h.example/p").unwrap();
        assert!(c.tls.is_none());
        assert_eq!(
            c.remote,
            Remote::Resolve {
                host: "h.example".into(),
                port: 80
            }
        );
    }

    #[test]
    fn explicit_port_and_addr() {
        let c = cfg("url=https://h.example:8443/a/b;addr=192.0.2.7:443").unwrap();
        assert_eq!(c.remote, Remote::Explicit("192.0.2.7:443".into()));
        assert_eq!(c.path, "a/b");
        let c = cfg("url=https://h.example:8443/a").unwrap();
        assert_eq!(
            c.remote,
            Remote::Resolve {
                host: "h.example".into(),
                port: 8443
            }
        );
    }

    #[test]
    fn query_is_not_part_of_the_path() {
        assert_eq!(cfg("url=https://h/p%20q?x=1#f").unwrap().path, "p%20q");
    }

    #[test]
    fn ipv6_host_is_unbracketed() {
        let c = cfg("url=https://[2001:db8::1]/p").unwrap();
        assert_eq!(c.http_host, "2001:db8::1");
    }

    #[test]
    fn servername_list_and_sni_imitation() {
        let c = cfg("url=https://real.example/p;servername= a.example, ,b.example").unwrap();
        let tls = c.tls.unwrap();
        assert_eq!(tls.server_names, ["a.example", "b.example"]);
        assert_eq!(tls.verify_name, None);

        let c = cfg("url=https://real.example/p;sni-imitation=cover.example").unwrap();
        let tls = c.tls.unwrap();
        assert_eq!(tls.server_names, ["cover.example"]);
        assert_eq!(tls.verify_name.as_deref(), Some("real.example"));

        let c =
            cfg("url=https://real.example/p;servername=s.example;cert-domain=v.example").unwrap();
        assert_eq!(c.tls.unwrap().verify_name.as_deref(), Some("v.example"));
    }

    #[test]
    fn cert_pin_must_be_a_sha256() {
        let pin = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        let c = cfg(&format!("url=https://h/p;cert={pin}")).unwrap();
        assert_eq!(c.tls.unwrap().pinned_chain_hash, Some([7u8; 32]));
        assert!(cfg("url=https://h/p;cert=AAAA").is_err());
        assert!(cfg("url=https://h/p;cert=not*base64").is_err());
    }

    #[test]
    fn rejects_unusable_lines() {
        assert!(cfg("ver=0.0.3").is_err(), "url is required");
        assert!(cfg("url=ftp://h/p").is_err());
        assert!(cfg("url=not a url").is_err());
        assert!(cfg("url=https://h/p;servername=, ,").is_err());
    }
}
