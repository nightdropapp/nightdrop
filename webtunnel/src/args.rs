//! Pluggable-transport argument lists: the `key=value;key=value` string a Tor client passes to
//! a transport for each bridge (pt-spec §3.5).
//!
//! Tor (and arti) send it in the SOCKS5 username/password fields: the username holds the first
//! 255 bytes, the password the rest, and a list that fits in the username alone is sent with a
//! password of a single NUL byte. `\`, `;` and (in keys) `=` are backslash-escaped.

use crate::Error;

/// The parsed argument list for one bridge, in the order it was sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PtArgs(Vec<(String, String)>);

impl PtArgs {
    /// Build from already-split pairs (for tests and for callers that parse bridge lines
    /// themselves).
    pub fn from_pairs<K: Into<String>, V: Into<String>>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        PtArgs(
            pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }

    /// Decode the SOCKS5 username/password pair a Tor client sends.
    pub fn from_socks_auth(username: &[u8], password: &[u8]) -> Result<Self, Error> {
        let mut raw = username.to_vec();
        if password != [0] {
            raw.extend_from_slice(password);
        }
        let s = std::str::from_utf8(&raw)
            .map_err(|_| Error::Args("argument list is not UTF-8".into()))?;
        Self::parse(s)
    }

    /// Parse an escaped `k=v;k=v` list.
    pub fn parse(s: &str) -> Result<Self, Error> {
        let mut pairs = Vec::new();
        if s.is_empty() {
            return Ok(PtArgs(pairs));
        }
        let mut key = String::new();
        let mut value = String::new();
        let mut in_value = false;
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    let escaped = chars.next().ok_or_else(|| {
                        Error::Args("argument list ends in a lone backslash".into())
                    })?;
                    if in_value {
                        value.push(escaped)
                    } else {
                        key.push(escaped)
                    }
                }
                '=' if !in_value => in_value = true,
                ';' => {
                    pairs.push(Self::finish(&mut key, &mut value, in_value)?);
                    in_value = false;
                }
                c => {
                    if in_value {
                        value.push(c)
                    } else {
                        key.push(c)
                    }
                }
            }
        }
        pairs.push(Self::finish(&mut key, &mut value, in_value)?);
        Ok(PtArgs(pairs))
    }

    fn finish(
        key: &mut String,
        value: &mut String,
        in_value: bool,
    ) -> Result<(String, String), Error> {
        if !in_value {
            return Err(Error::Args(format!("argument {key:?} has no '='")));
        }
        if key.is_empty() {
            return Err(Error::Args("argument with an empty key".into()));
        }
        Ok((std::mem::take(key), std::mem::take(value)))
    }

    /// The first value for `key`, as goptlib's `Args.Get` returns.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Encode in the escaped wire form (the inverse of [`PtArgs::parse`]).
    pub fn encode(&self) -> String {
        fn esc(out: &mut String, s: &str, in_key: bool) {
            for c in s.chars() {
                if c == '\\' || c == ';' || (in_key && c == '=') {
                    out.push('\\');
                }
                out.push(c);
            }
        }
        let mut out = String::new();
        for (i, (k, v)) in self.0.iter().enumerate() {
            if i > 0 {
                out.push(';');
            }
            esc(&mut out, k, true);
            out.push('=');
            esc(&mut out, v, false);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_list() {
        let a = PtArgs::parse("url=https://example.com/abc;ver=0.0.3").unwrap();
        assert_eq!(a.get("url"), Some("https://example.com/abc"));
        assert_eq!(a.get("ver"), Some("0.0.3"));
        assert_eq!(a.get("missing"), None);
    }

    #[test]
    fn empty_list_is_empty() {
        assert_eq!(PtArgs::parse("").unwrap(), PtArgs::default());
    }

    #[test]
    fn unescapes_like_arti_escapes() {
        // arti escapes `\` and `;` everywhere and `=` in keys; values may hold a bare `=`.
        let a = PtArgs::parse(r"k\=ey=a\;b\\c=d").unwrap();
        assert_eq!(a.get("k=ey"), Some(r"a;b\c=d"));
    }

    #[test]
    fn encode_round_trips() {
        let a = PtArgs::from_pairs([("url", "https://h/p?x=1;y"), ("we\\ird=", "v")]);
        assert_eq!(PtArgs::parse(&a.encode()).unwrap(), a);
    }

    #[test]
    fn first_duplicate_wins() {
        let a = PtArgs::parse("x=1;x=2").unwrap();
        assert_eq!(a.get("x"), Some("1"));
    }

    #[test]
    fn rejects_malformed_lists() {
        assert!(PtArgs::parse("novalue").is_err());
        assert!(PtArgs::parse("=v").is_err());
        assert!(
            PtArgs::parse("k=v;").is_err(),
            "a trailing ';' leaves an empty pair"
        );
        assert!(PtArgs::parse(r"k=v\").is_err());
    }

    #[test]
    fn socks_auth_nul_password_is_ignored() {
        let a = PtArgs::from_socks_auth(b"url=https://h/p", &[0]).unwrap();
        assert_eq!(a.get("url"), Some("https://h/p"));
    }

    #[test]
    fn socks_auth_spills_into_password() {
        // A list over 255 bytes is split across the two fields at byte 255 (arti's
        // `settings_to_protocol`), possibly mid-value.
        let long = format!("url=https://h/{}", "p".repeat(300));
        let (user, pass) = long.as_bytes().split_at(255);
        let a = PtArgs::from_socks_auth(user, pass).unwrap();
        assert_eq!(a.get("url").unwrap().len(), "https://h/".len() + 300);
    }
}
