//! The injected TAI-nanosecond clock (ADR-004's rule for this crate: "Clocks are injected,
//! never slept").
//!
//! Every epoch this crate writes anywhere -- a `CommandTransition.tai_ns`
//! ([`crate::state`]), a `LedgerRecord.tai_ns` / `PolicyDecision.evaluated_tai_ns`
//! ([`crate::ledger`]) -- comes from a [`Clock`] passed in by the caller, never from reading
//! the wall clock directly. [`SystemClock`] is the one place in this crate allowed to call
//! `SystemTime::now()`; every test in this crate uses [`TestClock`] instead, which a test
//! sets and advances explicitly. **No test in this crate calls `std::thread::sleep`** --
//! a test that needs "some time later" advances a `TestClock`, it never waits for real time
//! to pass.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use av_cdm::time::Tai;

/// Nanoseconds on TAI, from whatever source a caller wires up. The only two
/// implementations in this crate are [`SystemClock`] (the real wall clock) and [`TestClock`]
/// (every test).
pub trait Clock: Send + Sync {
    fn now_tai_ns(&self) -> i64;
}

/// The real clock. Converts the OS wall clock to TAI nanoseconds the same way
/// `av-dynamics-service`'s `evidence::now_tai_ns` does (`av_cdm::time::Tai::from_utc_nanos`
/// over `SystemTime::now()`). This function is the *only* place in this crate that reads
/// the wall clock -- every other epoch in this crate is a parameter, not a read.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_tai_ns(&self) -> i64 {
        let utc_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock reports a time before the Unix epoch")
            .as_nanos() as i64;
        Tai::from_utc_nanos(utc_ns).as_nanos()
    }
}

/// A clock a test sets and advances explicitly, never reading the wall clock at all. Starts
/// at whatever [`TestClock::new`] was given; `set`/`advance` are the only ways its value
/// changes.
#[derive(Debug)]
pub struct TestClock {
    now_tai_ns: AtomicI64,
}

impl TestClock {
    pub fn new(start_tai_ns: i64) -> Self {
        Self { now_tai_ns: AtomicI64::new(start_tai_ns) }
    }

    /// Sets the clock to an exact epoch.
    pub fn set(&self, tai_ns: i64) {
        self.now_tai_ns.store(tai_ns, Ordering::SeqCst);
    }

    /// Moves the clock forward (or back, for a negative `delta_ns`) by an exact amount --
    /// the deterministic substitute for "wait a bit and check again".
    pub fn advance(&self, delta_ns: i64) {
        self.now_tai_ns.fetch_add(delta_ns, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_tai_ns(&self) -> i64 {
        self.now_tai_ns.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_reports_exactly_what_was_set() {
        let clock = TestClock::new(1_000);
        assert_eq!(clock.now_tai_ns(), 1_000);
        clock.set(5_000);
        assert_eq!(clock.now_tai_ns(), 5_000);
    }

    #[test]
    fn test_clock_advance_adds_the_exact_delta_forward_and_backward() {
        let clock = TestClock::new(1_000);
        clock.advance(250);
        assert_eq!(clock.now_tai_ns(), 1_250);
        clock.advance(-1_250);
        assert_eq!(clock.now_tai_ns(), 0);
    }

    /// Not a sleep-based test (this crate never sleeps in a test): one single sample of the
    /// real clock, checked against a wide, static bound -- proves `SystemClock` is wired to
    /// a genuine wall-clock read and the TAI conversion without depending on wall-clock
    /// timing at all.
    #[test]
    fn system_clock_reports_a_plausible_epoch() {
        let ns = SystemClock.now_tai_ns();
        let year_2020_tai_ns: i64 = 1_577_836_800_000_000_000;
        let year_2100_tai_ns: i64 = 4_102_444_800_000_000_000;
        assert!(ns > year_2020_tai_ns && ns < year_2100_tai_ns, "{ns}");
    }
}
