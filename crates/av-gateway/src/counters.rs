//! ADR-004: "Everything rejected is counted." One shared counting primitive so every
//! refusal in this crate (D1's identity refusals, D2's label refusals, D3's tool/frame
//! refusals, D4's crafted-authorize refusals) increments through the same mechanism,
//! never a bespoke `AtomicU64` per call site that a future refusal path could forget to
//! wire up.
//!
//! Keyed by a stable `&'static str` code (each typed refusal enum names its own code via a
//! `code()` method) rather than the enum type itself, so [`Counters`] stays one simple type
//! usable from every module in this crate without a generic parameter per refusal kind.
//! `BTreeMap`, not `HashMap` (ADR-004's determinism rule: [`Counters::snapshot`] is a
//! deterministic, sorted report, never insertion- or hash-order-dependent).

use std::collections::BTreeMap;
use std::sync::Mutex;

/// A deny-by-default refusal reason that can be counted. Implemented by every typed
/// refusal enum in this crate ([`crate::catalogue::ResolveError`],
/// [`crate::labels::LabelRefusal`], [`crate::mcp::McpRefusal`], [`crate::propose_only::
/// ProposeRefusal`]) so [`Counters::record`] takes any of them uniformly.
pub trait Counted {
    /// A stable, `snake_case` identifier for this exact refusal kind -- never the
    /// `Display`/`Debug` text (which may carry caller-supplied, non-deterministic detail),
    /// so two refusals of the same kind always increment the same counter key.
    fn code(&self) -> &'static str;
}

/// Every refusal this process has counted so far, keyed by [`Counted::code`].
#[derive(Debug, Default)]
pub struct Counters {
    counts: Mutex<BTreeMap<&'static str, u64>>,
}

impl Counters {
    pub fn new() -> Self {
        Self::default()
    }

    /// Increments the counter for `reason`'s code by exactly 1. Called at the point a
    /// refusal is decided, never batched or deferred -- so a refusal that returns early
    /// (an `Err` a caller never inspects) still leaves a trace.
    pub fn record(&self, reason: &dyn Counted) {
        let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        *counts.entry(reason.code()).or_insert(0) += 1;
    }

    /// The current count for one code, `0` if it has never been recorded.
    pub fn get(&self, code: &str) -> u64 {
        self.counts.lock().unwrap_or_else(|p| p.into_inner()).get(code).copied().unwrap_or(0)
    }

    /// Every code recorded so far, sorted by code (a `BTreeMap`'s own iteration order --
    /// ADR-004's determinism rule, never a `HashMap`'s unspecified order).
    pub fn snapshot(&self) -> BTreeMap<&'static str, u64> {
        self.counts.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(&'static str);
    impl Counted for Fixture {
        fn code(&self) -> &'static str {
            self.0
        }
    }

    #[test]
    fn record_increments_the_exact_code_and_leaves_others_at_zero() {
        let counters = Counters::new();
        assert_eq!(counters.get("a"), 0);
        counters.record(&Fixture("a"));
        counters.record(&Fixture("a"));
        counters.record(&Fixture("b"));
        assert_eq!(counters.get("a"), 2);
        assert_eq!(counters.get("b"), 1);
        assert_eq!(counters.get("c"), 0);
        assert_eq!(counters.snapshot(), BTreeMap::from([("a", 2), ("b", 1)]));
    }
}
