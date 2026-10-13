//! Bounds on what a contact's frames may carry (self-audit 2026-10-13, trust-boundary inventory;
//! `docs/trust-boundaries.md`).
//!
//! Frames from a paired contact are authenticated, but nothing bounded their size: an address, a
//! message id or a text could be as large as a frame (~134 MB), stored in the sealed state and
//! rewritten on every save, which a hostile contact could use to make the app crawl or fill the disk.
//! These are the limits; every value that arrives from outside and is kept goes through one.

/// Longest message text kept (text, edit, burn), in bytes. Longer arrives truncated, marked; this
/// side refuses to send longer, so a real message is never cut silently.
pub(crate) const MAX_TEXT_BYTES: usize = 64 * 1024;

/// Longest id a contact may name (message ids are 16 characters, transfer ids 23).
pub(crate) const MAX_ID_BYTES: usize = 128;

/// Longest peer address kept (a v3 onion is 62 characters).
pub(crate) const MAX_ADDRESS_BYTES: usize = 128;

/// A received text, cut to [`MAX_TEXT_BYTES`] at a character boundary and marked if it was cut.
pub(crate) fn clamp_text(text: String) -> String {
    if text.len() <= MAX_TEXT_BYTES {
        return text;
    }
    let mut end = MAX_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} […]", &text[..end])
}

/// Whether `id` can be a message or transfer id: 1..=[`MAX_ID_BYTES`] printable ASCII, no spaces.
pub(crate) fn sane_id(id: &str) -> bool {
    (1..=MAX_ID_BYTES).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_graphic())
}

/// Whether `addr` can be a peer address: 1..=[`MAX_ADDRESS_BYTES`] of `A-Z a-z 0-9 . : - _`. An onion,
/// and the names and `host:port` forms of the test and LAN transports, all fit; the Tor transport
/// checks for a real v3 onion before dialing anything.
pub(crate) fn sane_address(addr: &str) -> bool {
    (1..=MAX_ADDRESS_BYTES).contains(&addr.len())
        && addr
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cut_at_a_character_boundary_and_marked() {
        let short = "hello".to_string();
        assert_eq!(clamp_text(short.clone()), short);
        let long = "é".repeat(MAX_TEXT_BYTES); // 2 bytes each
        let cut = clamp_text(long);
        assert!(cut.len() <= MAX_TEXT_BYTES + 8);
        assert!(cut.ends_with(" […]"));
    }

    #[test]
    fn ids_and_addresses_are_bounded() {
        assert!(sane_id("AbC-_123"));
        assert!(sane_id(&"x".repeat(MAX_ID_BYTES)));
        for bad in ["", "a b", "a\nb", "é", &"x".repeat(MAX_ID_BYTES + 1)] {
            assert!(!sane_id(bad), "{bad:?}");
        }
        for ok in [
            "bzcqxuxwvtmrmvprsoscnronkjf5wknfuj5ozxiq5fr6qowvnkwrwwad.onion",
            "alice",
            "192.168.1.5:9000",
        ] {
            assert!(sane_address(ok), "{ok}");
        }
        for bad in [
            "",
            "a b",
            "evil/x",
            "http://x",
            &"a".repeat(MAX_ADDRESS_BYTES + 1),
        ] {
            assert!(!sane_address(bad), "{bad:?}");
        }
    }
}
