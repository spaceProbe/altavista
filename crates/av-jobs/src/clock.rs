//! The injected TAI-nanosecond clock, following `crates/av-command/src/clock.rs`'s own
//! `Clock`/`SystemClock`/`TestClock` shape **without depending on that crate**: `av-command`
//! is a hot-path crate (that module's own doc comment: "every epoch this crate writes...
//! comes from a `Clock` passed in by the caller, never from reading the wall clock
//! directly"), and `av-jobs` -- a job-queue/runner crate with no reason to sit anywhere near
//! the hot path -- has no business entering its dependency tree. So this module defines its
//! own two-method `Clock` trait and its own `SystemClock`/`TestClock`, mirroring the
//! convention rather than reusing the implementation.
//!
//! No function anywhere else in this crate reads the wall clock or sleeps: every timestamp
//! this crate writes (`JobSpec.requested_tai_ns`, `JobCompletion.started_tai_ns`/
//! `finished_tai_ns`) comes from a `&dyn Clock` parameter, and every test in this crate
//! advances a [`TestClock`] explicitly rather than calling `std::thread::sleep`.

use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use av_cdm::time::Tai;

/// Nanoseconds on TAI, from whatever source a caller wires up. The only two implementations
/// in this crate are [`SystemClock`] (the real wall clock) and [`TestClock`] (every test).
pub trait Clock: fmt::Debug {
    fn now_tai_ns(&self) -> i64;
}

/// The real clock -- the only place in this crate allowed to call `SystemTime::now()`.
/// Converts through `av_cdm::time::Tai`, the same UTC/TAI boundary
/// `crates/av-command/src/clock.rs::SystemClock` uses.
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
/// at whatever [`TestClock::new`] was given; [`TestClock::set`]/[`TestClock::advance`] are
/// the only ways its value changes -- the deterministic substitute for "wait a bit and check
/// again", never a real sleep.
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

    /// Moves the clock forward (or back, for a negative `delta_ns`) by an exact amount.
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
