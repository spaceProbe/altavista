//! Pure edge-to-core latency arithmetic (min/max/p50/p99) over caller-supplied instant
//! pairs -- never a clock read itself (question 199's rule, restated for this crate by
//! `crate`'s own module doc: only the *harness binary*, `src/bin/av-edge-latency.rs`, may
//! read a real monotonic clock).
//!
//! # The two instants, named precisely
//!
//! "Edge-to-core latency (plugin emit to engine accept)" (`docs/edge-plan.md` milestone
//! E5) needs two concrete instants per batch, and this module names them exactly:
//!
//! - **Emit**: the `std::time::Instant` `src/bin/av-edge-latency.rs` records immediately
//!   before that binary hands one signed `MeasurementBatch` to its own gRPC client's
//!   `Submit` call (i.e. immediately before the batch leaves this process on the wire --
//!   the plugin-side analogue of `av_edge::plugin::Pacing::due_at`'s own "whenever the
//!   caller considers replay to have started" instant, but per batch rather than once for
//!   the whole run). This is the closest observable point to "the plugin emitted this
//!   batch" available without instrumenting `av-ingest-client`'s own generated tonic
//!   client code.
//! - **Accept**: the `std::time::Instant` the same binary records immediately after that
//!   `Submit` call's response future resolves with an *accepted* `BatchVerdict` for that
//!   batch (never a rejected one -- a rejected batch was never durably appended, so
//!   "accept" has no meaning for it; `src/bin/av-edge-latency.rs` fails loudly rather than
//!   silently excluding a rejection from the sample set). This is the closest observable
//!   point to "the engine can now read this batch" available from the client side: the
//!   server's own `EdgeIngestService::submit` handler (`crates/av-ingest/src/service.rs`)
//!   appends to the durable log and returns the verdict in the same synchronous call path
//!   (no queue, no async hand-off between "appended" and "response sent"), so the moment
//!   the client sees an accepted verdict is also the moment `crate::consumer::
//!   LogPartitionConsumer::open`, reading the same file, would already see that record.
//!
//! **Why the client side, not instrumenting the server.** The alternative -- timestamping
//! inside `EdgeIngestService::submit` itself -- would need `av-ingest` to read a clock
//! (that crate is clock-injected by design: `crates/av-ingest/src/service.rs`'s own module
//! doc, "the clock this crate is injected with rather than ever reading live") and would
//! measure a *different* quantity (server-side processing time only, excluding the network
//! hop each direction) than what "edge-to-core latency" actually means operationally: the
//! time from the edge deciding to send a batch to the moment that batch is durably part of
//! what the engine can consume. Measuring both endpoints from the one process that already
//! owns both instants (the harness binary drives the plugin-side send *and* waits on the
//! response) is simpler and answers the operationally meaningful question directly, at the
//! cost of also counting each round trip's own network/scheduling overhead -- which for a
//! real deployment (plugin and ingest on different hosts) is exactly the overhead this
//! number is supposed to include, not exclude.
//!
//! Reported as **nanoseconds since some caller-chosen zero instant** (`u64`) rather than
//! as `std::time::Instant`/`Duration` directly in this module's own public types, so this
//! module itself never touches `std::time` at all -- the harness binary converts its own
//! `Instant`s to nanosecond offsets before calling [`summarize`], keeping every clock read
//! in the one place question 199 requires.

/// One batch's own `(emit, accept)` pair, both nanosecond offsets from the same
/// caller-chosen zero instant (so subtracting them is well-defined regardless of what that
/// zero instant was).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencySample {
    pub emit_ns: u64,
    pub accept_ns: u64,
}

impl LatencySample {
    /// `accept_ns - emit_ns`, saturating at zero rather than underflowing/panicking on a
    /// caller's own clock-ordering bug (a typed report is more useful than a panic here --
    /// this module's standing convention).
    pub fn latency_ns(&self) -> u64 {
        self.accept_ns.saturating_sub(self.emit_ns)
    }
}

/// min/max/p50/p99 over a set of [`LatencySample`]s -- every field a plain `u64`
/// nanosecond count, ready for `src/bin/av-edge-latency.rs`'s own machine-readable JSON
/// report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencyReport {
    pub count: usize,
    pub min_ns: u64,
    pub max_ns: u64,
    pub p50_ns: u64,
    pub p99_ns: u64,
}

/// Nearest-rank percentile over an already-sorted-ascending slice, restated from
/// `crate::compare`'s identical helper (both files independently need it over a different
/// element type -- `u64` here, `f64` there -- and neither is a generic library this crate
/// otherwise needs; duplicated deliberately rather than introducing a shared-generics
/// module for two ten-line functions).
fn percentile(sorted_ascending: &[u64], p: f64) -> u64 {
    let n = sorted_ascending.len();
    let rank = ((p * n as f64).ceil() as usize).clamp(1, n) - 1;
    sorted_ascending[rank]
}

/// Summarizes `samples` into a [`LatencyReport`], or `None` for an empty set (never a
/// fabricated all-zero report -- a caller with zero samples has a different problem than a
/// caller with a genuinely-zero latency, and this function does not conflate the two).
pub fn summarize(samples: &[LatencySample]) -> Option<LatencyReport> {
    if samples.is_empty() {
        return None;
    }
    let mut latencies: Vec<u64> = samples.iter().map(LatencySample::latency_ns).collect();
    latencies.sort_unstable();
    Some(LatencyReport {
        count: latencies.len(),
        min_ns: *latencies.first().expect("non-empty"),
        max_ns: *latencies.last().expect("non-empty"),
        p50_ns: percentile(&latencies, 0.50),
        p99_ns: percentile(&latencies, 0.99),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_samples_summarize_to_none() {
        assert!(summarize(&[]).is_none());
    }

    #[test]
    fn a_single_sample_is_every_statistic() {
        let report = summarize(&[LatencySample { emit_ns: 100, accept_ns: 150 }]).unwrap();
        assert_eq!(report.count, 1);
        assert_eq!(report.min_ns, 50);
        assert_eq!(report.max_ns, 50);
        assert_eq!(report.p50_ns, 50);
        assert_eq!(report.p99_ns, 50);
    }

    #[test]
    fn min_max_p50_p99_over_a_known_distribution() {
        let samples: Vec<LatencySample> = (1..=100u64).map(|i| LatencySample { emit_ns: 0, accept_ns: i }).collect();
        let report = summarize(&samples).unwrap();
        assert_eq!(report.count, 100);
        assert_eq!(report.min_ns, 1);
        assert_eq!(report.max_ns, 100);
        assert_eq!(report.p50_ns, 50);
        assert_eq!(report.p99_ns, 99);
    }

    #[test]
    fn latency_ns_saturates_rather_than_underflows_on_a_clock_ordering_bug() {
        let sample = LatencySample { emit_ns: 200, accept_ns: 100 };
        assert_eq!(sample.latency_ns(), 0);
    }
}
