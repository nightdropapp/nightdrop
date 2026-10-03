//! Fetch Tor bridge lines from the Tor Project's **moat** "Circumvention Settings" API, for a
//! client that cannot reach Tor at all (docs/design/android-bridges.md §7a.1).
//!
//! **Not routed through Tor, by design**: it exists for the moment Tor does not work. The request
//! is domain-fronted (meek): the network sees TLS to a CDN front, the CDN forwards it to the
//! meek reflector, which forwards it to `bridges.torproject.org`, where a second TLS session ends.
//! The CDN and the Tor Project see the user's IP address (moat uses it to pick the country); the
//! request carries nothing about the user. It contacts nothing else — no Night Drop server ever.
//! The app sends it only when the user asks, after saying exactly that.

pub mod http;
pub mod meek;
pub mod moat;
pub mod targets;
pub mod tunnel;

use anyhow::Result;

pub use moat::{Fetched, MOAT_HOST};

/// Where the domain-fronted tunnel goes: Tor Browser 16.0's `extensions.torlauncher.bridgedb_targets`
/// (checked 2026-10-03). CDN fronts get retired from time to time; when Tor Browser changes this,
/// change it here too (docs/design/android-bridges.md §7a.1).
pub const TARGETS: &str = "https://1723079976.rsc.cdn77.org|cdn.zk.mk+www.cdn77.com";

/// The pluggable transports Night Drop can use, and so the only bridge types worth asking for.
pub const TRANSPORTS: &[&str] = &["webtunnel"];

/// Fetch WebTunnel bridge lines from the Tor Project, **directly, not through Tor**, for `country`
/// (two-letter lowercase code) or, when `None`, for wherever this device's IP address appears to
/// be. Blocking, and can take a minute on a bad network: call it off the UI thread.
pub fn fetch_bridges(country: Option<&str>) -> Result<Fetched> {
    let targets = targets::parse(TARGETS)?;
    let front_tls = meek::front_tls_config();
    let mut new_tunnel = move || -> Result<Box<dyn meek::RoundTrip>> {
        Ok(Box::new(meek::FrontedMeek::new(
            targets.clone(),
            front_tls.clone(),
            meek::FrontedMeek::default_dialer(),
        )?))
    };
    moat::fetch(
        &mut new_tunnel,
        tunnel::inner_tls_config(),
        country,
        TRANSPORTS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shawn's requirement, 2026-10-03: fetching bridges must not involve any Night Drop server.
    /// The only names this crate can contact are the fronts and reflector in [`TARGETS`] (the CDN)
    /// and [`MOAT_HOST`] (the Tor Project), so pin exactly those.
    #[test]
    fn the_only_hosts_contacted_are_the_cdn_and_the_tor_project() {
        let t = targets::parse(TARGETS).unwrap();
        let mut names: Vec<&str> = t
            .iter()
            .flat_map(|t| [t.front.as_str(), t.host.as_str()])
            .collect();
        names.push(MOAT_HOST);
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names,
            [
                "1723079976.rsc.cdn77.org",
                "bridges.torproject.org",
                "cdn.zk.mk",
                "www.cdn77.com"
            ]
        );
        assert!(names.iter().all(|n| !n.contains("nightdrop")));
    }

    /// The real thing, over the network. Off by default; run by hand with
    /// `cargo test -p moat live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_fetch_for_a_censored_country() {
        let f = fetch_bridges(Some("cn")).unwrap();
        println!(
            "country {:?}, {} line(s), defaults: {}",
            f.country,
            f.lines.len(),
            f.from_defaults
        );
        assert!(!f.lines.is_empty());
        assert!(f.lines.iter().all(|l| l.starts_with("webtunnel ")));
    }
}
