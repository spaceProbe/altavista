//! Real-time pacing: the kernel clock slaved to wall time at 1:1, with every deviation counted
//! (ADR-005 section 2, "Real-time pacing is entered only when the configuration binds a board";
//! architecture section 3; `docs/open-questions.md` question 242).
//!
//! Real-time pacing is a mode of the same kernel, not a second kernel: the stepping loop, the
//! router and every model are untouched, and the only difference is **who owns the clock**
//! (ADR-005 closing paragraph). A [`Pacer`] sits beside [`crate::schedule::HeteroScheduler::
//! advance_to_with_ports_paced`]; a lockstep run passes no pacer, reads no clock and allocates
//! nothing on the per-tick path.
//!
//! # The schedule
//!
//! * **Anchored once per run.** [`Pacer::begin_run`] (first call only) records wall `W0` = the
//!   monotonic instant the first tick is about to be released, mapped to sim `T0` = the scenario
//!   start. Wall time for sim time `t` is `W0 + (t - T0)` at 1:1 ([`Pacer::wall_for`]). The
//!   executor calls `run_with_ports` once per boundary-bounded span (faults, maneuvers); later
//!   spans find the anchor in place and never re-anchor, so the time spent re-binding models at
//!   a boundary is **not** hidden: the first tick after it is released late and counted.
//! * **Pace at the scheduler's tick, not the output tick.** `schedule.rs`'s
//!   `advance_to_with_ports` loop is one iteration per distinct step-end epoch `t`
//!   (`let next_time = ... .map(|s| s.next_due_ns).min()`), every system whose `next_due_ns == t`
//!   steps together, and `next_due_ns` is a step's **end** epoch (`debug_assert_eq!(result
//!   .t_tai_ns, t_ns + sys.period_ns)`: the step covers `[t - period, t]`). The work of the
//!   iteration at epoch `t` is *released* no earlier than `wall(t - base_period)` and is *due*
//!   at `wall(t)`. An output period coarser than the board's step period therefore still
//!   releases the board's steps one period apart, not in a burst at the output tick.
//! * **Catch-up steps ahead of the target.** A physical system whose period exceeds the output
//!   tick steps once, *past* the target (`catch_up_eligible`: `history.curr.0 < target_tai_ns`
//!   for `state_dim() != 0`), so its iteration's epoch `t` can exceed the target. That step is
//!   work the output tick needs now; pacing it at its own end epoch would stall the output tick
//!   (and with it every board step due before `t`) until `wall(t)`. The scheduler therefore
//!   paces an iteration at `min(t, target)`: the catch-up step belongs to the tick that needs it
//!   and merges into it. A zero-dimensional system (container, board) steps only when
//!   `next_due_ns <= target`, so for it `t <= target` always holds and nothing is clamped.
//! * **Ticks are keyed by sim epoch.** A tick is opened by [`Pacer::begin_tick`] and stays open,
//!   accumulating work over any number of segments, until a strictly later epoch begins or the
//!   run finishes. A call for an epoch at or before the open tick's (the same epoch ending one
//!   `run_with_ports` call and starting the next; two iterations clamped to one target) resumes
//!   it: it is not released again, not counted again, and its work is added to the same tick.
//!
//! # Overrun
//!
//! A tick whose work finishes after its deadline is an overrun, `overrun_ns = finish -
//! deadline`. The next tick is released at its own scheduled time, or **immediately** if that
//! has passed: the run catches up, never skips a step (the sim result of everything but the
//! clock is unchanged) and never re-anchors the schedule. "The run does not silently fall
//! behind": every overrun is counted ([`PacingStats`]), recorded ([`OverrunRecord`], one
//! `Event` each, [`overrun_events`]) and the run's lateness at its end is reported
//! ([`PacingStats::final_lateness_ns`]). [`Pacer::finish_run`] then holds the run until
//! `wall(T_end)` so a run occupies at least its simulated duration in wall time (the last tick
//! is released at `wall(T_end - base_period)`; its work is measured before the hold).
//!
//! # Wall-clock-dependent products
//!
//! The pacing report and the overrun events depend on the wall clock and on nothing else; every
//! other product of a run is unchanged by pacing. [`WALL_CLOCK_DEPENDENT`] names exactly them so
//! a replay can exclude exactly them from its comparison, and [`is_overrun_event`] recognises
//! the events.
//!
//! # Clock
//!
//! Time is read through [`PacingClock`] so the pacer is testable with a fake that advances on
//! sleep. [`SystemClock`] is the production implementation (`std::time::Instant` and
//! `std::thread::sleep`, finished with a short spin because an OS sleep overshoots by up to a
//! millisecond or more on this host's kernels).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use av_cdm::pb;

/// The run's wall-clock-dependent products, by name, for a replay to exclude (and only these):
/// `RunProducts.pacing`, and every `RunProducts.events` entry for which [`is_overrun_event`] is
/// true. Everything else in `RunProducts` and the port-traffic sidecar is independent of the
/// wall clock.
pub const WALL_CLOCK_DEPENDENT: [&str; 2] = ["RunProducts.pacing", "RunProducts.events[id starts with OVERRUN_EVENT_ID_PREFIX]"];

/// The `Event.name` of a per-overrun event.
pub const OVERRUN_EVENT_NAME: &str = "pacing_overrun";
/// The `Event.id` prefix of a per-overrun event; the suffix is the tick's TAI epoch in ns.
pub const OVERRUN_EVENT_ID_PREFIX: &str = "marker:pacing:overrun:";

/// Upper edges (ns) of the overrun histogram's buckets: 10 us, 100 us, 1 ms, 10 ms, 100 ms, 1 s.
/// Bucket `i` counts overruns in `(edge[i-1], edge[i]]` with `edge[-1] = 0`; the final bucket
/// (index `EDGES.len()`) counts overruns above the last edge. An overrun is strictly positive,
/// so zero is in no bucket. Fixed here, and sent on the wire with the counts.
pub const OVERRUN_HISTOGRAM_UPPER_EDGES_NS: [i64; 6] = [10_000, 100_000, 1_000_000, 10_000_000, 100_000_000, 1_000_000_000];

/// A monotonic clock the [`Pacer`] reads and sleeps on.
pub trait PacingClock {
    /// Monotonic nanoseconds since an arbitrary, fixed origin.
    fn now_ns(&self) -> i64;
    /// Block until `now_ns() >= deadline_ns`; return at once if it already is.
    fn sleep_until_ns(&self, deadline_ns: i64);
    /// Wall-clock UNIX nanoseconds now. Recorded for forensics only; never used for pacing.
    fn unix_ns(&self) -> i64;
}

/// The production [`PacingClock`]: `Instant` for monotonic time, `thread::sleep` for waiting.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// Sleep until this far before the deadline, then spin: an OS sleep can overshoot by a
    /// millisecond, which would be most of a 1 kHz board tick.
    const SPIN_MARGIN_NS: i64 = 300_000;

    pub fn new() -> Self {
        Self { origin: Instant::now() }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl PacingClock for SystemClock {
    fn now_ns(&self) -> i64 {
        i64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(i64::MAX)
    }

    fn sleep_until_ns(&self, deadline_ns: i64) {
        loop {
            let remaining = deadline_ns - self.now_ns();
            if remaining <= 0 {
                return;
            }
            if remaining > Self::SPIN_MARGIN_NS {
                std::thread::sleep(Duration::from_nanos((remaining - Self::SPIN_MARGIN_NS) as u64));
            } else {
                std::hint::spin_loop();
            }
        }
    }

    fn unix_ns(&self) -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX)).unwrap_or(0)
    }
}

/// Why a [`Pacer`] refused a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacingError {
    /// The base period handed to [`Pacer::begin_run`] was not positive.
    NonPositiveBasePeriod { base_period_ns: i64 },
    /// A later span of the run computed a different base period than the one the schedule was
    /// anchored with: the instance set or rates changed mid-run, which pacing cannot honour.
    BasePeriodChanged { anchored_ns: i64, got_ns: i64 },
}

impl std::fmt::Display for PacingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PacingError::NonPositiveBasePeriod { base_period_ns } => write!(f, "real-time pacing needs a positive base period, got {base_period_ns} ns"),
            PacingError::BasePeriodChanged { anchored_ns, got_ns } => {
                write!(f, "real-time pacing was anchored at a base period of {anchored_ns} ns but a later span computed {got_ns} ns")
            }
        }
    }
}
impl std::error::Error for PacingError {}

/// One overrun: a tick whose work finished after its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverrunRecord {
    /// The tick's sim epoch (TAI ns): the end epoch of its step(s).
    pub epoch_tai_ns: i64,
    /// `finish - deadline`, strictly positive.
    pub overrun_ns: i64,
    /// The tick's work time (release to finish, summed over its segments).
    pub work_ns: i64,
}

/// What pacing measured over a run. Ticks counted here are committed ticks (a tick is committed
/// when a later one begins or the run finishes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacingStats {
    pub base_period_ns: i64,
    /// The instances that forced real-time pacing, sorted.
    pub forcing_instances: Vec<String>,
    pub wall_start_unix_ns: i64,
    pub sim_start_tai_ns: i64,
    pub ticks_paced: u64,
    pub overrun_count: u64,
    pub worst_overrun_ns: i64,
    /// The sim epoch of the worst overrun's tick (the earliest, on a tie); 0 with no overrun.
    pub worst_overrun_tai_ns: i64,
    pub total_overrun_ns: i64,
    /// `OVERRUN_HISTOGRAM_UPPER_EDGES_NS.len() + 1` counts; see that constant.
    pub overrun_histogram: Vec<u64>,
    pub worst_work_ns: i64,
    pub total_work_ns: i64,
    /// The last tick's finish against `wall(T_end)`, floored at 0. Set by [`Pacer::finish_run`].
    pub final_lateness_ns: i64,
}

impl PacingStats {
    /// Mean work per tick (integer division); 0 with no ticks.
    pub fn mean_work_ns(&self) -> i64 {
        if self.ticks_paced == 0 {
            0
        } else {
            self.total_work_ns / self.ticks_paced as i64
        }
    }

    /// The wire form (`altavista.v1.PacingReport`, mode `PACING_MODE_REAL_TIME`).
    pub fn to_proto(&self) -> pb::PacingReport {
        pb::PacingReport {
            mode: pb::PacingMode::RealTime as i32,
            base_period_ns: self.base_period_ns,
            forcing_instances: self.forcing_instances.clone(),
            wall_start_unix_ns: self.wall_start_unix_ns,
            sim_start_tai_ns: self.sim_start_tai_ns,
            ticks_paced: self.ticks_paced,
            overrun_count: self.overrun_count,
            worst_overrun_ns: self.worst_overrun_ns,
            worst_overrun_tai_ns: self.worst_overrun_tai_ns,
            total_overrun_ns: self.total_overrun_ns as u64,
            overrun_histogram_upper_edges_ns: OVERRUN_HISTOGRAM_UPPER_EDGES_NS.to_vec(),
            overrun_histogram: self.overrun_histogram.clone(),
            worst_work_ns: self.worst_work_ns,
            mean_work_ns: self.mean_work_ns(),
            total_work_ns: self.total_work_ns as u64,
            final_lateness_ns: self.final_lateness_ns,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Anchor {
    /// Monotonic ns of `W0`.
    wall_ns: i64,
    /// Sim `T0` (TAI ns).
    sim_ns: i64,
    unix_ns: i64,
    base_period_ns: i64,
}

#[derive(Debug, Clone, Copy)]
struct OpenTick {
    epoch_tai_ns: i64,
    deadline_wall_ns: i64,
    /// Start of the segment in progress, if one is.
    segment_start_wall_ns: Option<i64>,
    work_ns: i64,
    finish_wall_ns: i64,
}

/// The real-time schedule of one run. See the module documentation.
pub struct Pacer {
    clock: Box<dyn PacingClock>,
    forcing_instances: Vec<String>,
    anchor: Option<Anchor>,
    open: Option<OpenTick>,
    stats: PacingStats,
    overruns: Vec<OverrunRecord>,
    last_finish_wall_ns: Option<i64>,
    finished: bool,
}

impl Pacer {
    /// A pacer on `clock`; `forcing_instances` (the board-bound instances) are sorted and
    /// reported.
    pub fn new(clock: Box<dyn PacingClock>, mut forcing_instances: Vec<String>) -> Self {
        forcing_instances.sort();
        forcing_instances.dedup();
        Self {
            clock,
            stats: PacingStats {
                base_period_ns: 0,
                forcing_instances: forcing_instances.clone(),
                wall_start_unix_ns: 0,
                sim_start_tai_ns: 0,
                ticks_paced: 0,
                overrun_count: 0,
                worst_overrun_ns: 0,
                worst_overrun_tai_ns: 0,
                total_overrun_ns: 0,
                overrun_histogram: vec![0; OVERRUN_HISTOGRAM_UPPER_EDGES_NS.len() + 1],
                worst_work_ns: 0,
                total_work_ns: 0,
                final_lateness_ns: 0,
            },
            forcing_instances,
            anchor: None,
            open: None,
            overruns: Vec::new(),
            last_finish_wall_ns: None,
            finished: false,
        }
    }

    /// A pacer on the production [`SystemClock`].
    pub fn real_time(forcing_instances: Vec<String>) -> Self {
        Self::new(Box::new(SystemClock::new()), forcing_instances)
    }

    /// Start (or continue) the run: the first call anchors the schedule (`W0` = now, `T0` =
    /// `start_tai_ns`); later calls (one per span) only check that the base period is the one the
    /// schedule was anchored with.
    pub fn begin_run(&mut self, start_tai_ns: i64, base_period_ns: i64) -> Result<(), PacingError> {
        if base_period_ns <= 0 {
            return Err(PacingError::NonPositiveBasePeriod { base_period_ns });
        }
        match self.anchor {
            Some(a) if a.base_period_ns != base_period_ns => Err(PacingError::BasePeriodChanged { anchored_ns: a.base_period_ns, got_ns: base_period_ns }),
            Some(_) => Ok(()),
            None => {
                let anchor = Anchor { wall_ns: self.clock.now_ns(), sim_ns: start_tai_ns, unix_ns: self.clock.unix_ns(), base_period_ns };
                self.stats.base_period_ns = base_period_ns;
                self.stats.wall_start_unix_ns = anchor.unix_ns;
                self.stats.sim_start_tai_ns = start_tai_ns;
                self.anchor = Some(anchor);
                Ok(())
            }
        }
    }

    /// Monotonic wall time for sim time `t_tai_ns`: `W0 + (t - T0)`. `None` before the anchor.
    pub fn wall_for(&self, t_tai_ns: i64) -> Option<i64> {
        self.anchor.map(|a| a.wall_ns + (t_tai_ns - a.sim_ns))
    }

    /// Release the work of the tick ending at `epoch_tai_ns`: sleep until `wall(epoch -
    /// base_period)` if that is still ahead, then open a segment. An epoch at or before the
    /// open tick's resumes that tick instead (no release wait, not counted again).
    ///
    /// # Panics
    ///
    /// If [`Pacer::begin_run`] has not been called: the kernel always calls it first.
    pub fn begin_tick(&mut self, epoch_tai_ns: i64) {
        let anchor = self.anchor.expect("Pacer::begin_run must precede Pacer::begin_tick");
        if let Some(open) = self.open.as_mut() {
            if epoch_tai_ns <= open.epoch_tai_ns {
                if open.segment_start_wall_ns.is_none() {
                    open.segment_start_wall_ns = Some(self.clock.now_ns());
                }
                return;
            }
        }
        self.close_segment();
        self.commit_open();
        let release = anchor.wall_ns + (epoch_tai_ns - anchor.base_period_ns - anchor.sim_ns);
        if self.clock.now_ns() < release {
            self.clock.sleep_until_ns(release);
        }
        let now = self.clock.now_ns();
        self.open = Some(OpenTick {
            epoch_tai_ns,
            deadline_wall_ns: anchor.wall_ns + (epoch_tai_ns - anchor.sim_ns),
            segment_start_wall_ns: Some(now),
            work_ns: 0,
            finish_wall_ns: now,
        });
    }

    /// The work of the current segment is done: stamp its finish. The tick stays open (a later
    /// call for the same epoch resumes it) until a later epoch begins or the run finishes.
    pub fn end_tick(&mut self) {
        self.close_segment();
    }

    fn close_segment(&mut self) {
        let now = self.clock.now_ns();
        if let Some(open) = self.open.as_mut() {
            if let Some(start) = open.segment_start_wall_ns.take() {
                open.work_ns += now - start;
                open.finish_wall_ns = now;
            }
        }
    }

    fn commit_open(&mut self) {
        let Some(tick) = self.open.take() else { return };
        let s = &mut self.stats;
        s.ticks_paced += 1;
        s.total_work_ns += tick.work_ns;
        s.worst_work_ns = s.worst_work_ns.max(tick.work_ns);
        let overrun = tick.finish_wall_ns - tick.deadline_wall_ns;
        if overrun > 0 {
            s.overrun_count += 1;
            s.total_overrun_ns += overrun;
            let bucket = OVERRUN_HISTOGRAM_UPPER_EDGES_NS.iter().position(|&edge| overrun <= edge).unwrap_or(OVERRUN_HISTOGRAM_UPPER_EDGES_NS.len());
            s.overrun_histogram[bucket] += 1;
            if overrun > s.worst_overrun_ns {
                s.worst_overrun_ns = overrun;
                s.worst_overrun_tai_ns = tick.epoch_tai_ns;
            }
            self.overruns.push(OverrunRecord { epoch_tai_ns: tick.epoch_tai_ns, overrun_ns: overrun, work_ns: tick.work_ns });
        }
        self.last_finish_wall_ns = Some(tick.finish_wall_ns);
    }

    /// Finish the run at sim time `end_tai_ns`: commit the open tick, record the final lateness
    /// (the last tick's finish against `wall(end)`, floored at 0), then hold until `wall(end)`.
    /// Idempotent: a second call returns the same statistics without waiting again.
    ///
    /// # Panics
    ///
    /// If the run was never begun.
    pub fn finish_run(&mut self, end_tai_ns: i64) -> PacingStats {
        let anchor = self.anchor.expect("Pacer::begin_run must precede Pacer::finish_run");
        if !self.finished {
            self.close_segment();
            self.commit_open();
            let end_wall = anchor.wall_ns + (end_tai_ns - anchor.sim_ns);
            self.stats.final_lateness_ns = self.last_finish_wall_ns.map_or(0, |f| (f - end_wall).max(0));
            if self.clock.now_ns() < end_wall {
                self.clock.sleep_until_ns(end_wall);
            }
            self.finished = true;
        }
        self.stats.clone()
    }

    /// The statistics so far (committed ticks only).
    pub fn stats(&self) -> &PacingStats {
        &self.stats
    }

    /// Every overrun recorded so far, in tick order.
    pub fn overruns(&self) -> &[OverrunRecord] {
        &self.overruns
    }

    /// The instances that forced pacing, sorted.
    pub fn forcing_instances(&self) -> &[String] {
        &self.forcing_instances
    }
}

/// Whether `event` is a per-overrun event built by [`overrun_events`].
pub fn is_overrun_event(event: &pb::Event) -> bool {
    event.id.starts_with(OVERRUN_EVENT_ID_PREFIX)
}

/// One event per overrun. `EVENT_KIND_MARKER`: an overrun is a point-in-time annotation of the
/// run itself, not a declared fault (`EVENT_KIND_FAULT` carries a DRM fault id), not tied to one
/// instance (`entity_id` is empty, so no trajectory lists it), and not a lifecycle transition;
/// `EVENT_KIND_CUSTOM` is the Python side's catch-all for typed producer events, where a marker
/// is the viewer's generic annotation. `tai_ns` is the tick's epoch; `values` carry
/// `overrun_ns` and `work_ns`. Wall-clock dependent: see [`WALL_CLOCK_DEPENDENT`].
pub fn overrun_events(overruns: &[OverrunRecord], sos_hash: &str, data_pack_hash: &str, run_id: &str) -> Vec<pb::Event> {
    overruns
        .iter()
        .map(|o| pb::Event {
            id: format!("{OVERRUN_EVENT_ID_PREFIX}{}", o.epoch_tai_ns),
            entity_id: String::new(),
            tai_ns: o.epoch_tai_ns,
            kind: pb::EventKind::Marker as i32,
            name: OVERRUN_EVENT_NAME.to_string(),
            detail: format!("real-time tick at {} ns finished {} ns after its deadline (work {} ns)", o.epoch_tai_ns, o.overrun_ns, o.work_ns),
            values: std::collections::BTreeMap::from([("overrun_ns".to_string(), o.overrun_ns as f64), ("work_ns".to_string(), o.work_ns as f64)]),
            provenance: Some(pb::Provenance {
                author_kind: pb::AuthorKind::Service as i32,
                tool: "av-kernel::pacing".to_string(),
                config_hash: sos_hash.to_string(),
                data_pack_hash: data_pack_hash.to_string(),
                run_id: run_id.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod testing {
    //! A fake [`PacingClock`] that advances only when a test says so (or when slept on).
    use super::PacingClock;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Clone, Default)]
    pub struct FakeClock {
        now: Rc<Cell<i64>>,
        /// Every `sleep_until_ns` target that was actually waited for.
        pub sleeps: Rc<RefCell<Vec<i64>>>,
    }

    impl FakeClock {
        pub fn new(start_ns: i64) -> Self {
            Self { now: Rc::new(Cell::new(start_ns)), sleeps: Rc::default() }
        }
        /// Make `ns` of "work" take wall time.
        pub fn work(&self, ns: i64) {
            self.now.set(self.now.get() + ns);
        }
        pub fn now(&self) -> i64 {
            self.now.get()
        }
    }

    impl PacingClock for FakeClock {
        fn now_ns(&self) -> i64 {
            self.now.get()
        }
        fn sleep_until_ns(&self, deadline_ns: i64) {
            if deadline_ns > self.now.get() {
                self.sleeps.borrow_mut().push(deadline_ns);
                self.now.set(deadline_ns);
            }
        }
        fn unix_ns(&self) -> i64 {
            1_700_000_000_000_000_000 + self.now.get()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeClock;
    use super::*;

    const MS: i64 = 1_000_000;
    const T0: i64 = 1_000_000_000_000;

    fn pacer(clock: &FakeClock) -> Pacer {
        Pacer::new(Box::new(clock.clone()), vec!["board_b".to_string(), "board_a".to_string()])
    }

    /// Run one tick at `epoch` whose work takes `work_ns`.
    fn tick(p: &mut Pacer, c: &FakeClock, epoch: i64, work_ns: i64) {
        p.begin_tick(epoch);
        c.work(work_ns);
        p.end_tick();
    }

    #[test]
    fn work_that_fits_is_never_an_overrun_and_ticks_are_released_one_period_ahead_of_their_deadline() {
        let c = FakeClock::new(5_000);
        let mut p = pacer(&c);
        p.begin_run(T0, 100 * MS).unwrap();
        let w0 = c.now();
        for k in 1..=10 {
            tick(&mut p, &c, T0 + k * 100 * MS, 30 * MS);
        }
        let s = p.finish_run(T0 + 1000 * MS);
        assert_eq!(s.ticks_paced, 10);
        assert_eq!(s.overrun_count, 0);
        assert_eq!(s.worst_overrun_ns, 0);
        assert_eq!(s.total_overrun_ns, 0);
        assert!(s.overrun_histogram.iter().all(|&n| n == 0));
        assert_eq!(s.worst_work_ns, 30 * MS);
        assert_eq!(s.mean_work_ns(), 30 * MS);
        assert_eq!(s.final_lateness_ns, 0);
        assert!(p.overruns().is_empty());
        // The first tick (epoch T0 + 100 ms) is released at W0 = now (T0 + 100 ms - 100 ms), the
        // k-th at W0 + (k-1) * 100 ms: the sleeps are exactly that.
        let sleeps = c.sleeps.borrow().clone();
        assert_eq!(sleeps.first().copied(), Some(w0 + 100 * MS), "tick 2 waits for its release at W0 + 100 ms (tick 1 is released at once)");
        // The run is held to wall(T_end): a 1 s run occupies 1 s of wall time.
        assert_eq!(c.now(), w0 + 1000 * MS);
        assert_eq!(s.sim_start_tai_ns, T0);
        assert_eq!(s.base_period_ns, 100 * MS);
        assert_eq!(s.forcing_instances, vec!["board_a".to_string(), "board_b".to_string()]);
        assert_eq!(s.wall_start_unix_ns, 1_700_000_000_000_000_000 + 5_000);
    }

    #[test]
    fn one_late_tick_is_counted_with_its_overrun_and_the_next_is_released_immediately() {
        let c = FakeClock::new(0);
        let mut p = pacer(&c);
        p.begin_run(T0, 100 * MS).unwrap();
        let w0 = c.now();
        tick(&mut p, &c, T0 + 100 * MS, 10 * MS); // on time
        // Tick 2: released at W0 + 100 ms, due W0 + 200 ms, takes 130 ms -> finishes W0 + 230 ms.
        tick(&mut p, &c, T0 + 200 * MS, 130 * MS);
        assert_eq!(c.now(), w0 + 230 * MS);
        let sleeps_before = c.sleeps.borrow().len();
        // Tick 3 is scheduled for release at W0 + 200 ms: already past, so no wait at all.
        tick(&mut p, &c, T0 + 300 * MS, 10 * MS);
        assert_eq!(c.sleeps.borrow().len(), sleeps_before, "a late run does not sleep: tick 3 is released immediately");
        assert_eq!(c.now(), w0 + 240 * MS);
        // Tick 4 (release W0 + 300 ms) is back on schedule: it waits.
        tick(&mut p, &c, T0 + 400 * MS, 10 * MS);
        assert_eq!(c.sleeps.borrow().len(), sleeps_before + 1);
        let s = p.finish_run(T0 + 400 * MS);
        assert_eq!(s.ticks_paced, 4);
        assert_eq!(s.overrun_count, 1);
        assert_eq!(s.worst_overrun_ns, 30 * MS);
        assert_eq!(s.worst_overrun_tai_ns, T0 + 200 * MS);
        assert_eq!(s.total_overrun_ns, 30 * MS);
        assert_eq!(p.overruns(), &[OverrunRecord { epoch_tai_ns: T0 + 200 * MS, overrun_ns: 30 * MS, work_ns: 130 * MS }]);
        assert_eq!(s.worst_work_ns, 130 * MS);
        assert_eq!(s.final_lateness_ns, 0, "the run caught up before its end");
    }

    #[test]
    fn a_run_of_late_ticks_catches_up_without_skipping_or_re_anchoring() {
        let c = FakeClock::new(0);
        let mut p = pacer(&c);
        p.begin_run(T0, 100 * MS).unwrap();
        let w0 = c.now();
        // Ten ticks of 150 ms work on a 100 ms schedule: every tick is released the instant the
        // previous one finishes (never earlier than its schedule, here always later), none
        // skipped, and the lateness grows by 50 ms per tick.
        for k in 1..=10 {
            tick(&mut p, &c, T0 + k * 100 * MS, 150 * MS);
        }
        let s = p.finish_run(T0 + 1000 * MS);
        assert_eq!(s.ticks_paced, 10, "no step skipped");
        assert_eq!(s.overrun_count, 10);
        let overruns: Vec<i64> = p.overruns().iter().map(|o| o.overrun_ns).collect();
        assert_eq!(overruns, (1..=10).map(|k| 50 * MS * k).collect::<Vec<_>>(), "the schedule is never re-anchored: overrun k is k * 50 ms");
        assert_eq!(s.worst_overrun_ns, 500 * MS);
        assert_eq!(s.worst_overrun_tai_ns, T0 + 1000 * MS);
        assert_eq!(s.total_overrun_ns, 50 * MS * 55);
        // Wall end = W0 + 10 * 150 ms; schedule end = W0 + 1000 ms; late by 500 ms, and finish_run
        // does not wait (it is already late).
        assert_eq!(s.final_lateness_ns, 500 * MS);
        assert_eq!(c.now(), w0 + 1500 * MS);
        assert!(c.sleeps.borrow().is_empty(), "a permanently late run never sleeps");
    }

    #[test]
    fn the_histogram_has_fixed_edges_and_puts_each_overrun_in_its_bucket() {
        assert_eq!(OVERRUN_HISTOGRAM_UPPER_EDGES_NS, [10_000, 100_000, 1_000_000, 10_000_000, 100_000_000, 1_000_000_000]);
        let c = FakeClock::new(0);
        let mut p = pacer(&c);
        p.begin_run(0, 1_000).unwrap();
        // Overruns of exactly each edge (inclusive upper bound), one past it, and 5 s. Ticks are
        // 100 s apart in sim time so each is released on its own schedule (never behind the
        // previous one), and the work is set so that finish = deadline + the wanted overrun.
        let targets = [1, 10_000, 10_001, 100_000, 999_999, 1_000_000, 1_000_001, 10_000_000, 100_000_000, 100_000_001, 1_000_000_000, 5_000_000_000i64];
        let mut epoch = 10_000_000_000i64;
        for overrun in targets {
            epoch += 100_000_000_000; // 100 s apart: always back on schedule
            p.begin_tick(epoch);
            // The tick was released at wall(epoch - 1000); deadline wall(epoch): finish at
            // deadline + overrun.
            let deadline = p.wall_for(epoch).unwrap();
            c.work(deadline + overrun - c.now());
            p.end_tick();
        }
        let s = p.finish_run(epoch);
        assert_eq!(s.overrun_count, targets.len() as u64);
        // buckets: <=10us, <=100us, <=1ms, <=10ms, <=100ms, <=1s, >1s
        assert_eq!(s.overrun_histogram, vec![2u64, 2, 2, 2, 1, 2, 1]);
        assert_eq!(s.overrun_histogram.iter().sum::<u64>(), s.overrun_count);
        assert_eq!(s.worst_overrun_ns, 5_000_000_000);
    }

    #[test]
    fn a_span_boundary_epoch_is_paced_once_and_its_work_is_one_tick() {
        let c = FakeClock::new(0);
        let mut p = pacer(&c);
        p.begin_run(T0, 100 * MS).unwrap();
        tick(&mut p, &c, T0 + 100 * MS, 10 * MS);
        // Span 1 ends at the epoch it last paced; span 2 begins (begin_run again, same base) and
        // its first iteration paces the same epoch (a catch-up step clamped to the target, say).
        p.begin_run(T0 + 100 * MS, 100 * MS).unwrap();
        let sleeps = c.sleeps.borrow().len();
        tick(&mut p, &c, T0 + 100 * MS, 5 * MS);
        assert_eq!(c.sleeps.borrow().len(), sleeps, "a resumed tick is not released again");
        // An earlier epoch also folds into the open tick.
        tick(&mut p, &c, T0 + 50 * MS, 5 * MS);
        tick(&mut p, &c, T0 + 200 * MS, 10 * MS);
        let s = p.finish_run(T0 + 200 * MS);
        assert_eq!(s.ticks_paced, 2, "the epoch T0+100ms is one tick however many segments paced it");
        assert_eq!(s.worst_work_ns, 20 * MS, "10 + 5 + 5 ms of work, summed over the tick's segments");
        assert_eq!(s.total_work_ns, 30 * MS);
    }

    #[test]
    fn a_later_span_with_a_different_base_period_is_refused() {
        let c = FakeClock::new(0);
        let mut p = pacer(&c);
        p.begin_run(T0, 100 * MS).unwrap();
        assert_eq!(p.begin_run(T0, 50 * MS), Err(PacingError::BasePeriodChanged { anchored_ns: 100 * MS, got_ns: 50 * MS }));
        assert_eq!(p.begin_run(T0, 0), Err(PacingError::NonPositiveBasePeriod { base_period_ns: 0 }));
    }

    #[test]
    fn the_anchor_is_taken_once_and_wall_time_is_one_to_one() {
        let c = FakeClock::new(777);
        let mut p = pacer(&c);
        assert_eq!(p.wall_for(T0), None);
        p.begin_run(T0, 100 * MS).unwrap();
        c.work(5 * MS);
        p.begin_run(T0, 100 * MS).unwrap(); // a second span does not move the anchor
        assert_eq!(p.wall_for(T0), Some(777));
        assert_eq!(p.wall_for(T0 + 3 * MS), Some(777 + 3 * MS));
    }

    #[test]
    fn a_run_with_no_ticks_reports_zeroes_and_still_holds_to_the_end() {
        let c = FakeClock::new(0);
        let mut p = pacer(&c);
        p.begin_run(T0, 100 * MS).unwrap();
        let s = p.finish_run(T0 + 300 * MS);
        assert_eq!((s.ticks_paced, s.overrun_count, s.mean_work_ns(), s.final_lateness_ns), (0, 0, 0, 0));
        assert_eq!(c.now(), 300 * MS);
        // Idempotent.
        assert_eq!(p.finish_run(T0 + 300 * MS), s);
        assert_eq!(c.now(), 300 * MS);
    }

    #[test]
    fn overrun_events_are_markers_keyed_by_epoch_with_overrun_and_work() {
        let events = overrun_events(&[OverrunRecord { epoch_tai_ns: T0 + 200 * MS, overrun_ns: 30 * MS, work_ns: 130 * MS }], "sos", "pack", "run-1");
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert!(is_overrun_event(e));
        assert_eq!(e.id, format!("marker:pacing:overrun:{}", T0 + 200 * MS));
        assert_eq!(e.tai_ns, T0 + 200 * MS);
        assert_eq!(e.kind, pb::EventKind::Marker as i32);
        assert_eq!(e.name, OVERRUN_EVENT_NAME);
        assert_eq!(e.values["overrun_ns"], 30e6);
        assert_eq!(e.values["work_ns"], 130e6);
        assert!(e.entity_id.is_empty());
        let prov = e.provenance.as_ref().unwrap();
        assert_eq!((prov.config_hash.as_str(), prov.run_id.as_str()), ("sos", "run-1"));
        assert!(!is_overrun_event(&pb::Event { id: "lifecycle:run:dropped_in_flight_messages".to_string(), ..Default::default() }));
    }

    #[test]
    fn the_wire_report_carries_every_statistic_and_the_fixed_edges() {
        let c = FakeClock::new(0);
        let mut p = pacer(&c);
        p.begin_run(T0, 100 * MS).unwrap();
        tick(&mut p, &c, T0 + 100 * MS, 150 * MS);
        let r = p.finish_run(T0 + 100 * MS).to_proto();
        assert_eq!(r.mode, pb::PacingMode::RealTime as i32);
        assert_eq!(r.base_period_ns, 100 * MS);
        assert_eq!(r.forcing_instances, vec!["board_a", "board_b"]);
        assert_eq!((r.ticks_paced, r.overrun_count), (1, 1));
        assert_eq!((r.worst_overrun_ns, r.worst_overrun_tai_ns, r.total_overrun_ns), (50 * MS, T0 + 100 * MS, 50_000_000));
        assert_eq!(r.overrun_histogram_upper_edges_ns, OVERRUN_HISTOGRAM_UPPER_EDGES_NS.to_vec());
        assert_eq!(r.overrun_histogram.len(), r.overrun_histogram_upper_edges_ns.len() + 1);
        assert_eq!(r.overrun_histogram[4], 1, "50 ms is in (10 ms, 100 ms]");
        assert_eq!((r.worst_work_ns, r.mean_work_ns, r.total_work_ns), (150 * MS, 150 * MS, 150_000_000));
        assert_eq!(r.final_lateness_ns, 50 * MS);
        assert_eq!(r.sim_start_tai_ns, T0);
    }
}
