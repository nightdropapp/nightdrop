//! Opt-in traffic counters for diagnosing drops (`NIGHTDROP_RELAY_STATS=1`).
//!
//! The live relay went unreachable for ~15 minutes three times one night (2026-10-12/13) while a
//! test relay on the same host and binary did not, and nothing it logged could say why. These
//! counters show what the relay was doing around such a spell: how many streams arrived, how many
//! it refused and why, and what was asked of the store.
//!
//! **Counts only.** Nothing here records a mailbox handle, message id, blob or its hash, an address,
//! or anything else about who asked: every counter is a number summed over all clients and reset
//! each interval. That keeps "no logs" true of everything that could tell clients apart
//! (ARCHITECTURE.md §1a); off by default, and printed only when the operator asks for it.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use nightdrop::relay_client::RelayEvent;

/// How often a summary line is printed.
pub const INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// The process-wide counters. Updated always (a relaxed atomic add costs nothing); printed only
/// when enabled.
pub static STATS: Stats = Stats::new();

const OPS: [&str; 8] = [
    "POST",
    "FETCH",
    "TAKE",
    "TAKE_MANY",
    "PEEK",
    "RECALL",
    "DIRECTORY",
    "REAP",
];

pub struct Stats {
    /// Rendezvous streams accepted and served.
    pub accepted: AtomicU64,
    /// `accept` itself failed (the circuit to the client's rendezvous point did not complete).
    pub accept_failed: AtomicU64,
    /// Dropped because `MAX_CONCURRENT_STREAMS` were already open.
    pub dropped_capacity: AtomicU64,
    /// A served stream ended with an I/O error (reset, timeout, closed mid-request).
    pub stream_errors: AtomicU64,
    /// A served stream the client ended with a Tor END cell — how a client normally hangs up, which
    /// the read loop sees as an error. Counted apart so `stream_errors` means real failures.
    pub client_ended: AtomicU64,
    /// A request line over `max_line_bytes`.
    pub line_too_long: AtomicU64,
    /// A request line refused by the relay-wide in-flight line budget.
    pub line_busy: AtomicU64,
    /// A connection closed by the per-connection rate limit.
    pub rate_limited: AtomicU64,
    /// The most streams open at once during the interval.
    pub peak_open: AtomicUsize,
    /// Requests per operation, in [`OPS`] order.
    ops: [AtomicU64; 8],
    /// Posts refused, by reason: relay full, mailbox full, blob too large, anything else.
    rejected_full: AtomicU64,
    rejected_mailbox: AtomicU64,
    rejected_large: AtomicU64,
    rejected_other: AtomicU64,
    /// Bytes accepted by successful posts.
    posted_bytes: AtomicU64,
}

impl Stats {
    const fn new() -> Self {
        Self {
            accepted: AtomicU64::new(0),
            accept_failed: AtomicU64::new(0),
            dropped_capacity: AtomicU64::new(0),
            stream_errors: AtomicU64::new(0),
            client_ended: AtomicU64::new(0),
            line_too_long: AtomicU64::new(0),
            line_busy: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            peak_open: AtomicUsize::new(0),
            ops: [const { AtomicU64::new(0) }; 8],
            rejected_full: AtomicU64::new(0),
            rejected_mailbox: AtomicU64::new(0),
            rejected_large: AtomicU64::new(0),
            rejected_other: AtomicU64::new(0),
            posted_bytes: AtomicU64::new(0),
        }
    }

    /// Count one store request from its event. Reads only the operation, the result text and the
    /// blob length — never the handle, message id or hash the event also carries.
    pub fn record(&self, ev: &RelayEvent) {
        if let Some(i) = OPS.iter().position(|op| *op == ev.op) {
            self.ops[i].fetch_add(1, Ordering::Relaxed);
        }
        if ev.op != "POST" {
            return;
        }
        if ev.result == "ok" {
            self.posted_bytes
                .fetch_add(ev.blob_len as u64, Ordering::Relaxed);
        } else if ev.result.contains("relay full") {
            self.rejected_full.fetch_add(1, Ordering::Relaxed);
        } else if ev.result.contains("mailbox full") {
            self.rejected_mailbox.fetch_add(1, Ordering::Relaxed);
        } else if ev.result.contains("too large") {
            self.rejected_large.fetch_add(1, Ordering::Relaxed);
        } else {
            self.rejected_other.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Note how many streams are open now, for the interval's peak.
    pub fn note_open(&self, open: usize) {
        self.peak_open.fetch_max(open, Ordering::Relaxed);
    }

    /// The summary line for the interval just ended, resetting every counter. `open_now` and the
    /// store totals are levels, not counts, so they are passed in rather than reset.
    pub fn take_line(&self, open_now: usize, stored_blobs: usize, stored_bytes: usize) -> String {
        let t = |c: &AtomicU64| c.swap(0, Ordering::Relaxed);
        let peak = self
            .peak_open
            .swap(open_now, Ordering::Relaxed)
            .max(open_now);
        let ops = OPS
            .iter()
            .zip(&self.ops)
            .map(|(name, c)| format!("{}={}", name.to_ascii_lowercase(), t(c)))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "stats {}m: streams open={open_now} peak={peak} accepted={} accept_failed={} \
             dropped_capacity={} client_ended={} errors={} line_too_long={} line_busy={} rate_limited={} | \
             requests {ops} | post_rejected full={} mailbox={} large={} other={} | \
             posted_bytes={} | store blobs={stored_blobs} bytes={stored_bytes}",
            INTERVAL.as_secs() / 60,
            t(&self.accepted),
            t(&self.accept_failed),
            t(&self.dropped_capacity),
            t(&self.client_ended),
            t(&self.stream_errors),
            t(&self.line_too_long),
            t(&self.line_busy),
            t(&self.rate_limited),
            t(&self.rejected_full),
            t(&self.rejected_mailbox),
            t(&self.rejected_large),
            t(&self.rejected_other),
            t(&self.posted_bytes),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(op: &'static str, result: &str, len: usize) -> RelayEvent {
        RelayEvent {
            op,
            handle: "SECRET-HANDLE".into(),
            msg_id: Some("SECRET-ID".into()),
            blob_len: len,
            blob_hash: "SECRET-HASH".into(),
            ttl_secs: 60,
            queue_depth: 1,
            result: result.into(),
        }
    }

    #[test]
    fn counts_requests_and_rejections_and_resets() {
        let s = Stats::new();
        s.record(&ev("POST", "ok", 100));
        s.record(&ev("POST", "rejected: relay full", 10));
        s.record(&ev("POST", "rejected: mailbox full", 10));
        s.record(&ev("TAKE", "ok", 0));
        s.accepted.fetch_add(3, Ordering::Relaxed);
        s.note_open(7);
        let line = s.take_line(2, 5, 1234);
        assert!(line.contains("post=3"), "{line}");
        assert!(line.contains("take=1"), "{line}");
        assert!(line.contains("full=1 mailbox=1 large=0"), "{line}");
        assert!(line.contains("posted_bytes=100"), "{line}");
        assert!(line.contains("accepted=3"), "{line}");
        assert!(line.contains("open=2 peak=7"), "{line}");
        assert!(line.contains("store blobs=5 bytes=1234"), "{line}");
        let next = s.take_line(0, 0, 0);
        assert!(
            next.contains("post=0") && next.contains("accepted=0"),
            "{next}"
        );
        assert!(
            next.contains("peak=2"),
            "peak restarts from the level at the reset: {next}"
        );
    }

    #[test]
    fn the_line_never_carries_what_identifies_a_request() {
        let s = Stats::new();
        s.record(&ev("POST", "ok", 1));
        let line = s.take_line(0, 0, 0);
        for secret in ["SECRET-HANDLE", "SECRET-ID", "SECRET-HASH"] {
            assert!(!line.contains(secret), "{line}");
        }
    }
}
