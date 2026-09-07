//! The kernel's clock authority (ADR-002 "Rates and pacing"). Lockstep only, as ADR-005
//! (the container/scheduler runtime) is still Planned -- nothing else of ADR-005 is assumed
//! or invented here; this is only the integer time source every system's stepping is driven
//! from.
//!
//! Simulated time is TAI nanoseconds, a signed 64-bit integer -- **never a float** -- so a
//! long run accumulates no rounding error and two runs from the same seed and inputs produce
//! the exact same sequence of instants (ADR-002 / ADR-004 determinism). Nothing here reads
//! the wall clock; the only way `Clock` advances is an explicit call to [`Clock::tick`].

/// The kernel's own simulated-time authority: an integer TAI-nanosecond counter that only
/// advances when told to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    now_tai_ns: i64,
    period_ns: i64,
}

impl Clock {
    /// `start_tai_ns` is the epoch the clock starts at; `period_ns` is the kernel's own base
    /// step period (ADR-002's default is 10 Hz kernel dynamics, i.e. `period_ns =
    /// 100_000_000`). Individual systems may be scheduled faster or slower than this base
    /// period (see `crate::schedule::Scheduler`) -- `period_ns` here is only the increment
    /// [`Clock::tick`] advances by, not a ceiling on any system's own rate.
    pub fn new(start_tai_ns: i64, period_ns: i64) -> Self {
        assert!(period_ns > 0, "clock period must be positive, got {period_ns} ns");
        Self { now_tai_ns: start_tai_ns, period_ns }
    }

    /// The current simulated instant, TAI nanoseconds.
    pub fn now(&self) -> i64 {
        self.now_tai_ns
    }

    /// The clock's own base period, TAI nanoseconds.
    pub fn period_ns(&self) -> i64 {
        self.period_ns
    }

    /// Advance by exactly one base period and return the new time.
    pub fn tick(&mut self) -> i64 {
        self.now_tai_ns += self.period_ns;
        self.now_tai_ns
    }
}

// -------------------------------------------------------------------------------------------
// The base period and its integer-multiple check (ADR-005 sec 2).
// -------------------------------------------------------------------------------------------
//
// "The base period is the greatest common divisor of every instance's step_rate_hz period and
// the DRM's output period; every instance period must be an integer multiple of the base
// period, checked at load, refused otherwise with the offending instance named." Two separate
// pieces, both real code below, not just documentation of an invariant: [`base_period_ns`]
// *computes* the ADR's definition (a GCD of positive periods always divides every one of its
// inputs, so a base period computed this way can never itself fail the check below -- that is
// the point of choosing it, not a gap in the check); [`check_integer_multiples`] is the
// general-purpose refusal, independently useful and independently tested against a
// deliberately inconsistent `base_period_ns` (e.g. one supplied from elsewhere, not computed
// by this module) so the "refused otherwise, with the offending instance named" half of the
// rule is exercised by a case that actually fails, not only by the tautological
// always-passes case `base_period_ns`'s own callers hit.

use std::collections::BTreeMap;

/// Every way computing or checking a base period can be refused (ADR-005 sec 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BasePeriodError {
    /// A period (an instance's own, or the DRM's output period) was not strictly positive --
    /// `gcd` and "integer multiple of" are both meaningless for a zero or negative period.
    NonPositivePeriod { instance: String, period_ns: i64 },
    /// No periods were supplied at all to [`base_period_ns`] (not even an output period) --
    /// there is nothing to take a GCD of.
    NoPeriods,
    /// An instance's declared step period is not an exact integer multiple of the base period
    /// -- named explicitly (ADR-005 sec 2: "refused otherwise with the offending instance
    /// named") so a mission author can fix the declared rate rather than discover a silent
    /// scheduling drift at run time.
    NotAnIntegerMultiple { instance: String, period_ns: i64, base_period_ns: i64 },
}

impl std::fmt::Display for BasePeriodError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BasePeriodError::NonPositivePeriod { instance, period_ns } => {
                write!(f, "instance {instance:?}: period must be positive, got {period_ns} ns")
            }
            BasePeriodError::NoPeriods => write!(f, "base_period_ns: no periods supplied (need at least the DRM output period)"),
            BasePeriodError::NotAnIntegerMultiple { instance, period_ns, base_period_ns } => {
                write!(f, "instance {instance:?}: period {period_ns} ns is not an integer multiple of the base period {base_period_ns} ns")
            }
        }
    }
}
impl std::error::Error for BasePeriodError {}

/// Greatest common divisor of two strictly positive integers (Euclidean algorithm).
fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The kernel's own base period (ADR-005 sec 2): the greatest common divisor of `output_period_ns`
/// (the DRM's own sampling period) and every instance's own step period in `instance_periods_ns`
/// (keyed by instance name, `BTreeMap` so a `NonPositivePeriod` refusal is deterministic about
/// which offending instance it reports first when more than one qualifies). A GCD of strictly
/// positive integers always evenly divides every one of its inputs, so the base period this
/// returns is *guaranteed* to pass [`check_integer_multiples`] against the same inputs --
/// that is the reason ADR-005 defines it this way (a single tick rate the kernel can drive
/// every instance and the output sampling grid from, hitting every one of their native step
/// boundaries exactly, never a fraction of one).
///
/// # Errors
///
/// [`BasePeriodError::NonPositivePeriod`] if `output_period_ns` or any instance period is not
/// strictly positive; [`BasePeriodError::NoPeriods`] is unreachable through this signature
/// (`output_period_ns` is always supplied) and exists for [`check_integer_multiples`]'s sake.
pub fn base_period_ns(output_period_ns: i64, instance_periods_ns: &BTreeMap<String, i64>) -> Result<i64, BasePeriodError> {
    if output_period_ns <= 0 {
        return Err(BasePeriodError::NonPositivePeriod { instance: "<output>".to_string(), period_ns: output_period_ns });
    }
    let mut base = output_period_ns;
    for (instance, period_ns) in instance_periods_ns {
        if *period_ns <= 0 {
            return Err(BasePeriodError::NonPositivePeriod { instance: instance.clone(), period_ns: *period_ns });
        }
        base = gcd(base, *period_ns);
    }
    Ok(base)
}

/// The general-purpose refusal half of ADR-005 sec 2: every instance period in
/// `instance_periods_ns`, and `output_period_ns` itself, must be an exact integer multiple of
/// `base_period_ns` (visited in `BTreeMap`/sorted-name order, so the *first* offending
/// instance reported is deterministic). Callable with any `base_period_ns` -- not only one
/// computed by [`base_period_ns`] itself (which can never fail this check against the same
/// inputs it was derived from) -- so a caller that fixes the base period some other way (a
/// declared configuration value, a coarser tick chosen for performance) still gets the typed,
/// named refusal ADR-005 sec 2 asks for.
///
/// # Errors
///
/// [`BasePeriodError::NonPositivePeriod`] if `base_period_ns` itself is not positive;
/// [`BasePeriodError::NotAnIntegerMultiple`] naming the first (sorted-order) instance, or the
/// synthetic instance name `"<output>"`, whose period does not divide evenly.
pub fn check_integer_multiples(base_period_ns: i64, output_period_ns: i64, instance_periods_ns: &BTreeMap<String, i64>) -> Result<(), BasePeriodError> {
    if base_period_ns <= 0 {
        return Err(BasePeriodError::NonPositivePeriod { instance: "<base>".to_string(), period_ns: base_period_ns });
    }
    if output_period_ns % base_period_ns != 0 {
        return Err(BasePeriodError::NotAnIntegerMultiple { instance: "<output>".to_string(), period_ns: output_period_ns, base_period_ns });
    }
    for (instance, period_ns) in instance_periods_ns {
        if period_ns % base_period_ns != 0 {
            return Err(BasePeriodError::NotAnIntegerMultiple { instance: instance.clone(), period_ns: *period_ns, base_period_ns });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_advances_by_exactly_one_period_and_is_exact_over_many_ticks() {
        let mut clock = Clock::new(1_700_000_000_000_000_000, 100_000_000); // 10 Hz
        for i in 1..=100 {
            let now = clock.tick();
            assert_eq!(now, 1_700_000_000_000_000_000 + i * 100_000_000);
        }
        // 100 ticks at 10 Hz is exactly 10 s -- integer arithmetic, no drift to check for.
        assert_eq!(clock.now(), 1_700_000_010_000_000_000);
    }

    #[test]
    #[should_panic(expected = "clock period must be positive")]
    fn zero_period_panics() {
        Clock::new(0, 0);
    }

    // -- base_period_ns / check_integer_multiples (ADR-005 sec 2) --------------------------

    #[test]
    fn base_period_is_the_gcd_of_the_output_period_and_every_instance_period() {
        let mut periods = BTreeMap::new();
        periods.insert("fast".to_string(), 20_000_000i64); // 50 Hz
        periods.insert("slow".to_string(), 500_000_000i64); // 2 Hz
        // output at 10 Hz (100_000_000 ns): gcd(100_000_000, 20_000_000, 500_000_000) = 20_000_000.
        let base = base_period_ns(100_000_000, &periods).unwrap();
        assert_eq!(base, 20_000_000);
    }

    #[test]
    fn a_base_period_computed_by_this_module_always_passes_its_own_integer_multiple_check() {
        // A deliberately "unfriendly" set of rates (not all harmonics of one another): the
        // computed base period must still divide every one of them exactly, by construction
        // (gcd), which is exactly what makes it safe for the kernel to tick at.
        let mut periods = BTreeMap::new();
        periods.insert("a".to_string(), 30_303_030i64); // ~33 Hz
        periods.insert("b".to_string(), 142_857_143i64); // ~7 Hz
        let output_period_ns = 100_000_000i64;
        let base = base_period_ns(output_period_ns, &periods).unwrap();
        check_integer_multiples(base, output_period_ns, &periods).expect("gcd-derived base period must satisfy its own check");
    }

    #[test]
    fn base_period_refuses_a_non_positive_instance_period_naming_it() {
        let mut periods = BTreeMap::new();
        periods.insert("bad".to_string(), 0i64);
        let err = base_period_ns(100_000_000, &periods).unwrap_err();
        assert_eq!(err, BasePeriodError::NonPositivePeriod { instance: "bad".to_string(), period_ns: 0 });
    }

    #[test]
    fn check_integer_multiples_refuses_an_instance_whose_period_does_not_divide_the_base_period_naming_it() {
        // A base period picked from somewhere other than base_period_ns's own gcd computation
        // (here: the DRM's own output period, a common but not always safe simplification) --
        // this is the case that actually exercises the refusal, since a gcd-derived base can
        // never fail this check against the inputs it was derived from.
        let base = 100_000_000i64; // 10 Hz
        let mut periods = BTreeMap::new();
        periods.insert("ok".to_string(), 200_000_000i64); // 5 Hz: 200_000_000 % 100_000_000 == 0
        periods.insert("bad".to_string(), 30_000_000i64); // ~33 Hz: does not divide evenly
        let err = check_integer_multiples(base, base, &periods).unwrap_err();
        assert_eq!(err, BasePeriodError::NotAnIntegerMultiple { instance: "bad".to_string(), period_ns: 30_000_000, base_period_ns: base });
    }

    #[test]
    fn check_integer_multiples_refuses_an_output_period_that_does_not_divide_the_base_period() {
        let base = 30_000_000i64;
        let output_period_ns = 100_000_000i64; // not a multiple of 30_000_000
        let err = check_integer_multiples(base, output_period_ns, &BTreeMap::new()).unwrap_err();
        assert_eq!(err, BasePeriodError::NotAnIntegerMultiple { instance: "<output>".to_string(), period_ns: output_period_ns, base_period_ns: base });
    }

    #[test]
    fn base_period_of_a_single_output_period_with_no_instances_is_the_output_period_itself() {
        let base = base_period_ns(250_000_000, &BTreeMap::new()).unwrap();
        assert_eq!(base, 250_000_000);
        check_integer_multiples(base, 250_000_000, &BTreeMap::new()).unwrap();
    }
}
