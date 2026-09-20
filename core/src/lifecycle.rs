//! Cooperative shutdown for the core's long-lived background threads (the poller in [`crate::api`]
//! and the Tor transport's idle-stream reaper).
//!
//! Tearing a live core down and immediately building another over the same Tor state directory is
//! a routine operation here — a guard heal, a backup restore, a logout all do it — and arti holds
//! an **exclusive on-disk lock** on that directory. "Stopped" therefore has to mean *the thread has
//! let go of everything it holds*, not *we asked it to stop*. See `ARCHITECTURE.md` §6.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// A stop flag a background thread can **sleep on**, plus an exit signal whoever stopped it can
/// **wait for**.
///
/// Two things a plain `AtomicBool` cannot do, and both are needed to release Tor's state lock:
///
/// * **Wake the thread now.** A poller that only sees the flag on its next tick keeps running for
///   up to a full background interval (2s). Sleeping on the condvar makes a stop immediate without
///   the continuous timer wakeups a short-slice sleep loop would cost on battery.
/// * **Report when the thread is really gone.** The poller snapshots [`RelayClient`]s and drains
///   them *off* the core lock (`ARCHITECTURE.md` §1.5.2); on Tor each of those clones carries an
///   `Arc<TorClient>` **and** the tokio runtime. While that thread is still in a drain, arti's lock
///   is still held, and the replacement client comes up read-only ("Another process has the lock on
///   our state files") — unable to persist the very guards a heal just picked, so the heal repeats
///   forever. Waiting for the exit is what stops that.
///
/// [`RelayClient`]: crate::relay_client::RelayClient
pub(crate) struct StopSignal {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    /// Set by [`StopSignal::stop`]: the thread should wind up at its next opportunity.
    stop: bool,
    /// Set by [`ExitGuard`]'s drop: the thread has run its locals' destructors and is gone.
    exited: bool,
}

impl StopSignal {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        })
    }

    /// Ask the thread to stop and wake it out of any [`sleep`](Self::sleep). Idempotent.
    pub(crate) fn stop(&self) {
        self.lock().stop = true;
        self.changed.notify_all();
    }

    /// Whether a stop has been requested. Cheap; call it wherever a long loop can bail early.
    pub(crate) fn stopped(&self) -> bool {
        self.lock().stop
    }

    /// Sleep up to `dur`, returning early the moment [`stop`](Self::stop) is called. `false` means
    /// "stop requested" — the caller should break out of its loop.
    pub(crate) fn sleep(&self, dur: Duration) -> bool {
        let guard = self.lock();
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, dur, |s| !s.stop)
            .unwrap_or_else(|e| e.into_inner());
        !guard.stop
    }

    /// Block until the thread's [`ExitGuard`] drops, or `bound` elapses. `true` = it exited (so
    /// everything it held is released); `false` = it is still running and the caller must decide
    /// what to do about that.
    ///
    /// The bound is not optional: the poller can be mid-Tor-round-trip, and an unbounded wait here
    /// would hang a logout — or, worse, a duress wipe — for as long as that takes.
    pub(crate) fn wait_for_exit(&self, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        let mut guard = self.lock();
        while !guard.exited {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (next, _) = self
                .changed
                .wait_timeout(guard, left)
                .unwrap_or_else(|e| e.into_inner());
            guard = next;
        }
        true
    }

    /// Whether the thread has exited. Test/diagnostic use.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn exited(&self) -> bool {
        self.lock().exited
    }

    /// Recover from poisoning rather than propagate it (§1.5.3): a thread that panicked while
    /// holding this guard must not make shutdown itself unusable.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Held by a background thread for its whole life; its drop is what marks the thread exited.
///
/// A guard rather than a line at the end of the thread body so that an **unwinding panic** also
/// reports the exit — otherwise one panicking tick would leave every later [`wait_for_exit`] to
/// burn its full timeout waiting for a thread that is already dead.
///
/// [`wait_for_exit`]: StopSignal::wait_for_exit
pub(crate) struct ExitGuard(Arc<StopSignal>);

impl ExitGuard {
    pub(crate) fn new(signal: Arc<StopSignal>) -> Self {
        Self(signal)
    }
}

impl Drop for ExitGuard {
    fn drop(&mut self) {
        self.0.lock().exited = true;
        self.0.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the condvar: a stop must not wait out the thread's sleep interval.
    #[test]
    fn stop_wakes_a_sleeping_thread_immediately() {
        let signal = StopSignal::new();
        let thread_signal = Arc::clone(&signal);
        let handle = std::thread::spawn(move || {
            let _exit = ExitGuard::new(Arc::clone(&thread_signal));
            // Would sleep for a minute if the stop had to wait for the timeout.
            while thread_signal.sleep(Duration::from_secs(60)) {}
        });

        let started = Instant::now();
        signal.stop();
        assert!(
            signal.wait_for_exit(Duration::from_secs(5)),
            "the thread must exit once stopped"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "stop woke the sleeper late: {:?}",
            started.elapsed()
        );
        handle.join().unwrap();
    }

    /// `wait_for_exit` reports honestly rather than blocking forever — this is what keeps a
    /// shutdown (and the duress wipe behind it) bounded when the poller is stuck in a relay dial.
    #[test]
    fn wait_for_exit_is_bounded_when_the_thread_will_not_stop() {
        let signal = StopSignal::new();
        let thread_signal = Arc::clone(&signal);
        let stuck = Arc::new(Mutex::new(true));
        let thread_stuck = Arc::clone(&stuck);
        let handle = std::thread::spawn(move || {
            let _exit = ExitGuard::new(thread_signal);
            // Ignores the stop flag entirely, like a thread blocked in a Tor round-trip.
            while *thread_stuck.lock().unwrap() {
                std::thread::sleep(Duration::from_millis(10));
            }
        });

        signal.stop();
        assert!(
            !signal.wait_for_exit(Duration::from_millis(100)),
            "a thread that has not exited must be reported as such, not waited on forever"
        );
        assert!(!signal.exited());

        *stuck.lock().unwrap() = false;
        assert!(signal.wait_for_exit(Duration::from_secs(5)));
        handle.join().unwrap();
    }

    /// A panicking thread has still released what it held, so it must still count as exited.
    #[test]
    fn a_panicking_thread_still_signals_its_exit() {
        let signal = StopSignal::new();
        let thread_signal = Arc::clone(&signal);
        let handle = std::thread::spawn(move || {
            let _exit = ExitGuard::new(thread_signal);
            panic!("tick blew up");
        });

        assert!(
            signal.wait_for_exit(Duration::from_secs(5)),
            "the exit guard must fire while unwinding"
        );
        assert!(handle.join().is_err(), "the panic itself is not swallowed");
    }

    /// Stopping before the thread ever sleeps must not lose the signal.
    #[test]
    fn a_stop_before_the_first_sleep_is_not_missed() {
        let signal = StopSignal::new();
        signal.stop();
        assert!(signal.stopped());
        assert!(
            !signal.sleep(Duration::from_secs(60)),
            "sleep returns at once"
        );
    }
}
