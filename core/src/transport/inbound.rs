//! A byte budget for inbound frames, shared by every networked transport.
//!
//! Each frame is capped on its own ([`MAX_FRAME_BYTES`](crate::wire::MAX_FRAME_BYTES), sized for
//! the largest attachment), but frames go into a channel the core drains on its own schedule, and
//! any number of connections can be reading at once. Without a budget, whoever can reach the
//! transport could make it hold frame after frame until the app runs out of memory. The budget
//! counts every frame being read or waiting to be taken; a reader that would exceed it waits —
//! which stalls that sender's stream, so the network carries the back-pressure — and gives up the
//! connection if no room appears within [`INBOUND_WAIT`].
//!
//! One frame is always admitted when nothing is held, so an attachment larger than the budget
//! still arrives; what the budget bounds is how many arrive at once.

use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::Result;

/// Bytes of inbound frames a transport holds at once (read, being read, or waiting for the core).
pub(crate) const INBOUND_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// How long a reader waits for room before dropping its connection.
pub(crate) const INBOUND_WAIT: Duration = Duration::from_secs(60);

/// How often a waiting reader checks for room.
const WAIT_STEP: Duration = Duration::from_millis(25);

/// The shared count behind the budget. Cloned into every reader; released by `try_recv`.
#[derive(Clone)]
pub(crate) struct InboundBudget {
    held: Arc<AtomicUsize>,
    limit: usize,
}

impl Default for InboundBudget {
    fn default() -> Self {
        Self::with_limit(INBOUND_BUDGET_BYTES)
    }
}

impl InboundBudget {
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self {
            held: Arc::new(AtomicUsize::new(0)),
            limit,
        }
    }

    /// Reserve `len` bytes if they fit, or if nothing is held at all.
    pub(crate) fn try_reserve(&self, len: usize) -> bool {
        let mut current = self.held.load(Ordering::Acquire);
        loop {
            if current != 0 && current.saturating_add(len) > self.limit {
                return false;
            }
            match self.held.compare_exchange_weak(
                current,
                current + len,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(now) => current = now,
            }
        }
    }

    /// Give back `len` bytes: the core took the frame, or its read failed.
    pub(crate) fn release(&self, len: usize) {
        let _ = self
            .held
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |h| {
                Some(h.saturating_sub(len))
            });
    }

    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.held.load(Ordering::Acquire)
    }

    /// Wait (blocking this thread) until `len` bytes can be reserved, at most [`INBOUND_WAIT`].
    pub(crate) fn reserve_blocking(&self, len: usize) -> Result<()> {
        let deadline = Instant::now() + INBOUND_WAIT;
        while !self.try_reserve(len) {
            anyhow::ensure!(
                Instant::now() < deadline,
                "inbound frames are not being taken; dropping the connection"
            );
            std::thread::sleep(WAIT_STEP);
        }
        Ok(())
    }

    /// [`reserve_blocking`](Self::reserve_blocking) for an async reader (the Tor transport).
    #[cfg(feature = "tor")]
    pub(crate) async fn reserve(&self, len: usize) -> Result<()> {
        let deadline = Instant::now() + INBOUND_WAIT;
        while !self.try_reserve(len) {
            anyhow::ensure!(
                Instant::now() < deadline,
                "inbound frames are not being taken; dropping the connection"
            );
            tokio::time::sleep(WAIT_STEP).await;
        }
        Ok(())
    }
}

/// Read one length-prefixed frame, reserving its length in `budget` before reading the body. The
/// reservation stays with the frame until whoever receives it calls
/// [`release`](InboundBudget::release); on a failed read it is given back here.
pub(crate) fn read_frame(r: &mut impl Read, budget: &InboundBudget) -> Result<Vec<u8>> {
    let mut prefix = [0u8; 4];
    r.read_exact(&mut prefix)?;
    let len = crate::wire::frame_len(prefix)?;
    budget.reserve_blocking(len)?;
    // Grow as bytes arrive rather than allocating the announced length up front.
    let mut buf = Vec::new();
    let read = r.take(len as u64).read_to_end(&mut buf);
    if read.is_err() || buf.len() != len {
        budget.release(len);
        read?;
        anyhow::bail!("connection closed mid-frame");
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framed(body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn a_frame_is_admitted_within_the_budget_and_released_by_the_receiver() {
        let budget = InboundBudget::with_limit(100);
        let frame = read_frame(&mut std::io::Cursor::new(framed(&[1; 40])), &budget).unwrap();
        assert_eq!(frame.len(), 40);
        assert_eq!(budget.held(), 40, "held until the core takes it");
        budget.release(frame.len());
        assert_eq!(budget.held(), 0);
    }

    #[test]
    fn a_frame_larger_than_the_budget_still_arrives_when_nothing_is_held() {
        let budget = InboundBudget::with_limit(10);
        assert!(budget.try_reserve(500), "one oversized frame at a time");
        assert!(!budget.try_reserve(1), "but nothing beside it");
        budget.release(500);
        assert!(budget.try_reserve(1));
    }

    #[test]
    fn frames_past_the_budget_wait_for_the_receiver() {
        let budget = InboundBudget::with_limit(100);
        assert!(budget.try_reserve(80));
        assert!(!budget.try_reserve(30), "would exceed the budget");
        assert!(budget.try_reserve(20), "exactly fills it");
        // A reader waiting on room proceeds once the receiver takes a frame.
        let waiting = {
            let budget = budget.clone();
            std::thread::spawn(move || budget.reserve_blocking(30))
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !waiting.is_finished(),
            "still waiting while the budget is full"
        );
        budget.release(80);
        waiting.join().unwrap().unwrap();
        assert_eq!(budget.held(), 50);
    }

    #[test]
    fn a_failed_read_gives_its_reservation_back() {
        let budget = InboundBudget::with_limit(100);
        let mut short = framed(&[1; 40]);
        short.truncate(20);
        assert!(read_frame(&mut std::io::Cursor::new(short), &budget).is_err());
        assert_eq!(budget.held(), 0);
    }

    #[test]
    fn an_over_long_prefix_is_refused_before_any_reservation() {
        let budget = InboundBudget::with_limit(100);
        let prefix = u32::MAX.to_be_bytes();
        assert!(read_frame(&mut std::io::Cursor::new(prefix), &budget).is_err());
        assert_eq!(budget.held(), 0);
    }
}
