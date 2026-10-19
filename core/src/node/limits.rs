//! Bounds on what a contact's frames may carry (`docs/trust-boundaries.md`).
//!
//! Frames from a paired contact are authenticated, but that does not bound their size: an address,
//! a message id or a text could be as large as a frame (~134 MB), stored in the sealed state and
//! rewritten on every save, which a hostile contact could use to make the app crawl or fill the
//! disk. These are the limits; every value that arrives from outside and is kept goes through one.

/// Longest message text kept (text, edit, burn), in bytes. Longer arrives truncated, marked; this
/// side refuses to send longer, so a real message is never cut silently.
pub(crate) const MAX_TEXT_BYTES: usize = 64 * 1024;

/// Largest preview image a contact may send ahead of a video (theirs are 320 px JPEGs, a few KB).
pub(crate) const MAX_THUMB_BYTES: usize = 256 * 1024;

/// Longest id a contact may name (message ids are 16 characters, transfer ids 23).
pub(crate) const MAX_ID_BYTES: usize = 128;

/// Longest peer address kept (a v3 onion is 62 characters).
pub(crate) const MAX_ADDRESS_BYTES: usize = 128;

/// Messages one contact may add to this device in a rolling day, and attachment bytes. Each item is
/// already bounded; this bounds how many, so a hostile contact cannot fill the disk or make every
/// save re-seal an ever larger state. Far above real use: ten largest-size videos a day still fit.
pub(crate) const MAX_MESSAGES_PER_DAY: u32 = 5_000;
pub(crate) const MAX_MEDIA_BYTES_PER_DAY: u64 = 1024 * 1024 * 1024;
const DAY_SECS: u64 = 24 * 60 * 60;

/// One contact's use of the daily allowance. In memory only: a restart starts a fresh window,
/// which is fine for a brake on floods.
#[derive(Debug, Default, Clone)]
pub(crate) struct Allowance {
    window_start: u64,
    messages: u32,
    media_bytes: u64,
    noticed: bool,
}

/// What [`Allowance::admit`] decided.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admit {
    /// Keep it.
    Yes,
    /// Over the allowance: drop it, and show the one notice for this window.
    DropAndNotice,
    /// Over the allowance, notice already shown: drop it quietly.
    Drop,
}

impl Allowance {
    /// Count one received message carrying `media_bytes` of attachment at `now` (unix seconds).
    pub(crate) fn admit(&mut self, now: u64, media_bytes: u64) -> Admit {
        if now < self.window_start || now >= self.window_start.saturating_add(DAY_SECS) {
            *self = Allowance {
                window_start: now,
                ..Default::default()
            };
        }
        if self.messages >= MAX_MESSAGES_PER_DAY
            || self.media_bytes.saturating_add(media_bytes) > MAX_MEDIA_BYTES_PER_DAY
        {
            if self.noticed {
                return Admit::Drop;
            }
            self.noticed = true;
            return Admit::DropAndNotice;
        }
        self.messages += 1;
        self.media_bytes += media_bytes;
        Admit::Yes
    }
}

/// The notice a chat shows once when its contact runs past the daily allowance.
pub(crate) const OVER_ALLOWANCE_NOTICE: &str = "This contact has sent more than Night Drop keeps \
     in a day. Further messages from them are being dropped until tomorrow.";

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
    fn a_contact_past_the_daily_allowance_is_dropped_with_one_notice_until_the_day_turns() {
        let mut a = Allowance::default();
        let t = 1_000_000;
        for _ in 0..MAX_MESSAGES_PER_DAY {
            assert_eq!(a.admit(t, 0), Admit::Yes);
        }
        assert_eq!(a.admit(t + 5, 0), Admit::DropAndNotice);
        assert_eq!(a.admit(t + 6, 0), Admit::Drop);
        assert_eq!(
            a.admit(t + DAY_SECS, 0),
            Admit::Yes,
            "a new day, a new allowance"
        );

        let mut b = Allowance::default();
        assert_eq!(b.admit(t, MAX_MEDIA_BYTES_PER_DAY), Admit::Yes);
        assert_eq!(b.admit(t, 1), Admit::DropAndNotice, "bytes are capped too");
        // A clock moved back starts a fresh window rather than freezing this one.
        assert_eq!(b.admit(t - 10, 1), Admit::Yes);
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
