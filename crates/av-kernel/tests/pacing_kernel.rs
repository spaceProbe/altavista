//! Real-time pacing through `HeteroKernel::run_with_ports_paced` with the production clock
//! (question 242, ADR-005 section 2). No GMAT, no board: the "board" here is a zero-dimensional
//! stand-in whose step records when it was released and can sleep a chosen time, which is all
//! the pacing contract is about. Nothing here claims a board result.
//!
//! Wall-clock tolerances, stated once. The schedule is held by `thread::sleep` plus a 300 us
//! spin (`SystemClock`), so a tick is released *at* its scheduled instant or later, never
//! earlier; "later" is the OS scheduler. These tests run on a host that shares two cargo jobs
//! and a docker lock with other work, so every upper bound is generous (tens of milliseconds on
//! 100 ms ticks) and every lower bound is exact up to the few microseconds between the pacer's
//! anchor and the first step the model observes (`ANCHOR_SLACK`).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::rc::Rc;
use std::time::{Duration, Instant};

use av_cdm::pb::{ModelInfo, SosConfiguration};
use av_dynamics::{erase_with_id, BoxedModel, DecodeErrorOccurrence, DynamicsModel, SensorFaultEffectDrain, StepResult};
use av_kernel::pacing::{overrun_events, Pacer};
use av_kernel::router::Router;
use av_kernel::HeteroKernel;

const MS: i64 = 1_000_000;
const T0: i64 = 1_000_000_000_000;
/// The pacer anchors a few microseconds before the first step is observed; offsets are measured
/// from the first observed step.
const ANCHOR_SLACK: Duration = Duration::from_millis(2);

type StepLog = Rc<RefCell<Vec<(i64, Instant)>>>;

/// A zero-dimensional "board": records `(step end epoch, instant the step began)` and sleeps
/// `slow[k]` on its k-th step (1-based) when asked.
struct Board {
    t0: i64,
    slow: BTreeMap<i64, Duration>,
    log: StepLog,
}

impl DynamicsModel for Board {
    type Error = Infallible;
    fn state_dim(&self) -> usize {
        0
    }
    fn derivatives(&self, _s: &[f64], _t: i64, _c: &[f64], _o: &mut [f64]) -> Result<(), Self::Error> {
        Ok(())
    }
    fn describe(&self) -> ModelInfo {
        ModelInfo { id: "test.board".to_string(), ..Default::default() }
    }
    fn step(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        let end = t_tai_ns + dt_ns;
        self.log.borrow_mut().push((end, Instant::now()));
        if let Some(d) = self.slow.get(&((end - self.t0) / dt_ns)) {
            std::thread::sleep(*d);
        }
        Ok(StepResult { state: state.to_vec(), t_tai_ns: end, outputs: BTreeMap::new() })
    }
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        Vec::new()
    }
    fn drain_sensor_fault_effect(&self) -> Option<SensorFaultEffectDrain> {
        None
    }
    fn drain_decode_errors(&self) -> Vec<DecodeErrorOccurrence> {
        Vec::new()
    }
}

/// A physical (6-component) constant-velocity stand-in that records when its steps began.
struct Mover {
    log: StepLog,
}

impl DynamicsModel for Mover {
    type Error = Infallible;
    fn state_dim(&self) -> usize {
        6
    }
    fn derivatives(&self, state: &[f64], _t: i64, _c: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        out[0..3].copy_from_slice(&state[3..6]);
        out[3..6].fill(0.0);
        Ok(())
    }
    fn describe(&self) -> ModelInfo {
        ModelInfo { id: "test.mover".to_string(), ..Default::default() }
    }
    fn step(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        self.log.borrow_mut().push((t_tai_ns + dt_ns, Instant::now()));
        let dt_s = dt_ns as f64 * 1e-9;
        let mut next = state.to_vec();
        for i in 0..3 {
            next[i] += state[3 + i] * dt_s;
        }
        Ok(StepResult { state: next, t_tai_ns: t_tai_ns + dt_ns, outputs: BTreeMap::new() })
    }
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        Vec::new()
    }
    fn drain_sensor_fault_effect(&self) -> Option<SensorFaultEffectDrain> {
        None
    }
    fn drain_decode_errors(&self) -> Vec<DecodeErrorOccurrence> {
        Vec::new()
    }
}

struct Rig {
    kernel: HeteroKernel,
    router: Router,
    board_log: StepLog,
    mover_log: StepLog,
}

fn rig(output_period_ns: i64, board_period_ns: i64, mover_period_ns: i64, slow: &[(i64, u64)]) -> Rig {
    let board_log: StepLog = Rc::default();
    let mover_log: StepLog = Rc::default();
    let mut kernel = HeteroKernel::new(output_period_ns);
    let board: BoxedModel =
        erase_with_id("test.board", Board { t0: T0, slow: slow.iter().map(|&(k, ms)| (k, Duration::from_millis(ms))).collect(), log: board_log.clone() }, |_id, never: Infallible| match never {});
    kernel.register_system("board", board_period_ns, board, T0, vec![]);
    let mover: BoxedModel = erase_with_id("test.mover", Mover { log: mover_log.clone() }, |_id, never: Infallible| match never {});
    kernel.register_system("mover", mover_period_ns, mover, T0, vec![7.0e6, 0.0, 0.0, 0.0, 7.5e3, 0.0]);
    let router = Router::build(&SosConfiguration::default(), &BTreeMap::new()).expect("no connections declared");
    Rig { kernel, router, board_log, mover_log }
}

/// Offsets (from the first logged step) of the steps with end epoch `T0 + k * 100 ms`.
fn offsets_ms(log: &StepLog) -> Vec<(i64, f64)> {
    let log = log.borrow();
    let first = log.first().expect("at least one step").1;
    log.iter().map(|&(end, at)| ((end - T0) / MS, at.duration_since(first).as_secs_f64() * 1e3)).collect()
}

/// A 1 s arc at 10 Hz with a fast model takes at least 1 s of wall time and not much more, every
/// step is released one period after the previous (never earlier than its schedule), no overrun
/// is recorded, and the products are exactly those of the lockstep run.
#[test]
fn a_one_second_arc_at_10_hz_takes_one_second_of_wall_time_and_is_otherwise_the_lockstep_run() {
    let end = T0 + 1_000 * MS;
    let mut r = rig(100 * MS, 100 * MS, 100 * MS, &[]);
    let mut pacer = Pacer::real_time(vec!["board".to_string()]);
    let started = Instant::now();
    let paced = r.kernel.run_with_ports_paced(T0, end, &mut r.router, &mut pacer).expect("paced run");
    let stats = pacer.finish_run(end);
    let wall = started.elapsed();

    // At least the arc (exact: finish_run holds to wall(T_end), and W0 is taken after `started`);
    // at most +200 ms: a fast model adds only sleep overshoot, so 20% of the arc is a loose
    // bound for a loaded host.
    assert!(wall >= Duration::from_millis(1_000), "the arc took {wall:?}, less than its 1 s of simulated time");
    assert!(wall <= Duration::from_millis(1_200), "the arc took {wall:?}, more than 200 ms over");
    println!("one_second_arc: wall = {:.3} ms, ticks = {}, overruns = {}, worst work = {} ns, final lateness = {} ns", wall.as_secs_f64() * 1e3, stats.ticks_paced, stats.overrun_count, stats.worst_work_ns, stats.final_lateness_ns);

    assert_eq!(stats.ticks_paced, 10);
    assert_eq!(stats.overrun_count, 0, "a model that does no work cannot overrun a 100 ms tick (host stalls above 100 ms aside)");
    assert!(pacer.overruns().is_empty());
    assert_eq!(stats.final_lateness_ns, 0);
    assert_eq!(stats.forcing_instances, vec!["board".to_string()]);

    // Release times: step k begins at W0 + (k-1) * 100 ms, never earlier.
    let offs = offsets_ms(&r.board_log);
    assert_eq!(offs.len(), 10);
    for (end_ms, off) in &offs {
        let k = end_ms / 100;
        let scheduled = (k - 1) as f64 * 100.0;
        assert!(*off >= scheduled - ANCHOR_SLACK.as_secs_f64() * 1e3, "step {k} began at {off:.3} ms, before its scheduled {scheduled} ms");
        assert!(*off <= scheduled + 50.0, "step {k} began at {off:.3} ms, more than 50 ms after its scheduled {scheduled} ms");
    }

    // The clock is the only difference: the same run, unpaced, gives identical trajectories.
    let mut lockstep = rig(100 * MS, 100 * MS, 100 * MS, &[]);
    let reference = lockstep.kernel.run_with_ports(T0, end, &mut lockstep.router).expect("lockstep run");
    assert_eq!(paced, reference, "pacing must not change any product but the clock's");
}

/// The board's steps are released one *step* period apart even when the output period is coarser
/// (here 500 ms output, 100 ms board step): pacing is at the scheduler's tick, so the five steps
/// inside one output tick are not sent in a burst.
#[test]
fn an_output_period_coarser_than_the_step_period_does_not_bunch_the_steps() {
    let end = T0 + 1_000 * MS;
    let mut r = rig(500 * MS, 100 * MS, 100 * MS, &[]);
    let mut pacer = Pacer::real_time(vec!["board".to_string()]);
    let started = Instant::now();
    r.kernel.run_with_ports_paced(T0, end, &mut r.router, &mut pacer).expect("paced run");
    let stats = pacer.finish_run(end);
    assert!(started.elapsed() >= Duration::from_millis(1_000));
    assert_eq!(stats.ticks_paced, 10, "ten 100 ms ticks, not two 500 ms output ticks");

    let offs = offsets_ms(&r.board_log);
    assert_eq!(offs.len(), 10);
    println!("coarse_output: release offsets (ms) = {:?}", offs.iter().map(|(_, o)| (o * 10.0).round() / 10.0).collect::<Vec<_>>());
    for w in offs.windows(2) {
        let gap = w[1].1 - w[0].1;
        // Each release is 100 ms after the previous one, within the overshoot of one sleep; a
        // bunched implementation would show gaps of ~0 ms inside each output tick.
        assert!(gap >= 95.0, "steps {} and {} were released {gap:.3} ms apart: bunched", w[0].0 / 100, w[1].0 / 100);
        assert!(gap <= 150.0, "steps {} and {} were released {gap:.3} ms apart", w[0].0 / 100, w[1].0 / 100);
    }
}

/// A model that sleeps longer than the period on chosen steps: the overruns are counted with the
/// right size, the run catches up without skipping a step, and the schedule is never re-anchored
/// (later steps are still released at their original instants).
///
/// Steps 3 and 7 sleep 250 ms on a 100 ms tick. Step k is released at (k-1) * 100 ms and due at
/// k * 100 ms. Step 3 begins at 200 ms and ends at 450 ms: overrun 150 ms. Step 4 is released at
/// once (its 300 ms release has passed), does no work, and ends at 450 ms against a 400 ms
/// deadline: overrun 50 ms. Step 5 (release 400 ms) ends at 450 ms against 500 ms: back on time.
/// Likewise 7 and 8: four overruns, 150 + 50 + 150 + 50 = 400 ms in all.
#[test]
fn steps_that_sleep_past_the_period_are_counted_and_the_run_catches_up_without_skipping() {
    let end = T0 + 1_000 * MS;
    let mut r = rig(100 * MS, 100 * MS, 100 * MS, &[(3, 250), (7, 250)]);
    let mut pacer = Pacer::real_time(vec!["board".to_string()]);
    let started = Instant::now();
    r.kernel.run_with_ports_paced(T0, end, &mut r.router, &mut pacer).expect("paced run");
    let stats = pacer.finish_run(end);
    let wall = started.elapsed();
    println!(
        "overruns: wall = {:.1} ms, ticks = {}, overruns = {}, worst = {} ns at {} ms, total = {} ns, histogram = {:?}, final lateness = {} ns",
        wall.as_secs_f64() * 1e3,
        stats.ticks_paced,
        stats.overrun_count,
        stats.worst_overrun_ns,
        (stats.worst_overrun_tai_ns - T0) / MS,
        stats.total_overrun_ns,
        stats.overrun_histogram,
        stats.final_lateness_ns
    );

    // No step skipped, none repeated, in epoch order.
    let epochs: Vec<i64> = r.board_log.borrow().iter().map(|&(e, _)| (e - T0) / MS).collect();
    assert_eq!(epochs, (1..=10).map(|k| k * 100).collect::<Vec<_>>());
    assert_eq!(stats.ticks_paced, 10);

    // Counts are exact; sizes are the arithmetic above plus sleep overshoot (never negative), so
    // each is checked as "at least that, and at most 40 ms more".
    assert_eq!(stats.overrun_count, 4, "steps 3, 4, 7 and 8");
    let by_epoch: BTreeMap<i64, i64> = pacer.overruns().iter().map(|o| ((o.epoch_tai_ns - T0) / MS, o.overrun_ns)).collect();
    assert_eq!(by_epoch.keys().copied().collect::<Vec<_>>(), vec![300, 400, 700, 800]);
    for (epoch_ms, want_ms) in [(300, 150), (400, 50), (700, 150), (800, 50)] {
        let got = by_epoch[&epoch_ms];
        assert!(got >= want_ms * MS - MS && got <= (want_ms + 40) * MS, "overrun at {epoch_ms} ms was {} ms, expected {want_ms}..{} ms", got as f64 / 1e6, want_ms + 40);
    }
    assert!(stats.worst_overrun_ns >= 149 * MS && stats.worst_overrun_ns <= 190 * MS, "worst overrun {} ns", stats.worst_overrun_ns);
    assert!(stats.worst_overrun_tai_ns == T0 + 300 * MS || stats.worst_overrun_tai_ns == T0 + 700 * MS);
    assert!(stats.total_overrun_ns >= 396 * MS && stats.total_overrun_ns <= 560 * MS);
    assert_eq!(stats.overrun_histogram.iter().sum::<u64>(), 4);
    assert_eq!(stats.overrun_histogram[4] + stats.overrun_histogram[5], 4, "every overrun is between 10 ms and 1 s");
    assert!(stats.worst_work_ns >= 249 * MS, "the slow steps' work time is measured: {} ns", stats.worst_work_ns);

    // The schedule was never re-anchored: step 10 (release 900 ms) still begins at ~900 ms, and
    // the run ends on its wall-clock schedule, with the catch-up complete.
    let offs = offsets_ms(&r.board_log);
    let last = offs.last().unwrap().1;
    assert!((899.0..=960.0).contains(&last), "step 10 began at {last:.1} ms, expected ~900 ms: a re-anchored schedule would be ~100-200 ms late");
    assert_eq!(stats.final_lateness_ns, 0);
    assert!(wall >= Duration::from_millis(1_000) && wall <= Duration::from_millis(1_250), "wall {wall:?}");

    // One event per overrun, at the tick's epoch, carrying the numbers.
    let events = overrun_events(pacer.overruns(), "sos", "pack", "run-x");
    assert_eq!(events.len(), 4);
    for (e, o) in events.iter().zip(pacer.overruns()) {
        assert_eq!(e.tai_ns, o.epoch_tai_ns);
        assert_eq!(e.values["overrun_ns"], o.overrun_ns as f64);
        assert_eq!(e.values["work_ns"], o.work_ns as f64);
    }

    // Slowness changes only the clock: trajectories equal the lockstep run's.
    let mut lockstep = rig(100 * MS, 100 * MS, 100 * MS, &[]);
    let reference = lockstep.kernel.run_with_ports(T0, end, &mut lockstep.router).expect("lockstep run");
    let mut again = rig(100 * MS, 100 * MS, 100 * MS, &[(3, 250), (7, 250)]);
    let mut pacer2 = Pacer::real_time(vec!["board".to_string()]);
    let paced = again.kernel.run_with_ports_paced(T0, end, &mut again.router, &mut pacer2).expect("paced run");
    assert_eq!(paced, reference);
}

/// A physical system whose period (1 s) exceeds the output tick steps once, *past* the target
/// (`catch_up_eligible`). That step belongs to the tick that needs it: it must be released with
/// the first tick, not held until `wall(1 s - base period)` as its own end epoch would require
/// (which would also stall every board step behind it).
#[test]
fn a_physical_catch_up_step_ahead_of_the_target_belongs_to_the_tick_that_needs_it() {
    let end = T0 + 1_000 * MS;
    let mut r = rig(100 * MS, 100 * MS, 1_000 * MS, &[]);
    let mut pacer = Pacer::real_time(vec!["board".to_string()]);
    let started = Instant::now();
    let paced = r.kernel.run_with_ports_paced(T0, end, &mut r.router, &mut pacer).expect("paced run");
    let stats = pacer.finish_run(end);
    assert!(started.elapsed() >= Duration::from_millis(1_000));

    let first_board_step = r.board_log.borrow().first().expect("board stepped").1;
    let mover = r.mover_log.borrow().clone();
    assert_eq!(mover.len(), 1, "one 1 s step");
    let mover_at_ms = mover[0].1.duration_since(first_board_step).as_secs_f64() * 1e3;
    println!("catch_up: mover step (end epoch {} ms) began {:.3} ms after the first board step", (mover[0].0 - T0) / MS, mover_at_ms);
    assert!(mover_at_ms < 50.0, "the physical step began at {mover_at_ms:.1} ms; paced on its own end epoch it would wait until ~900 ms");
    let board = offsets_ms(&r.board_log);
    assert_eq!(board.len(), 10);
    assert!(board[1].1 >= 98.0, "board step 2 at {:.1} ms: still one period after step 1", board[1].1);
    assert_eq!(stats.ticks_paced, 10, "the catch-up step merged into tick 1 and made no tick of its own");
    assert_eq!(stats.overrun_count, 0);

    let mut lockstep = rig(100 * MS, 100 * MS, 1_000 * MS, &[]);
    let reference = lockstep.kernel.run_with_ports(T0, end, &mut lockstep.router).expect("lockstep run");
    assert_eq!(paced, reference);
}

/// Two spans of one run (the executor's boundary-bounded `run_with_ports` calls) share one
/// pacer: the schedule is anchored once, the second span continues it, the span-boundary epoch is
/// paced once, and the time spent between spans (the executor re-binds models there) is not
/// hidden: it shows up as the lateness of the first tick after it.
///
/// Span 1 covers 0..500 ms and returns at ~400 ms (the last tick is released at 400 ms). The gap
/// sleeps 250 ms, so span 2 starts at ~650 ms: its first tick (epoch 600 ms, released at 500 ms,
/// due at 600 ms) is released at once and finishes at ~650 ms, an overrun of ~50 ms. The next
/// tick (release 600 ms) is already back on its schedule.
#[test]
fn two_spans_of_one_run_share_one_anchor_and_the_gap_between_them_is_counted() {
    let mid = T0 + 500 * MS;
    let end = T0 + 1_000 * MS;
    let mut pacer = Pacer::real_time(vec!["board".to_string()]);
    let started = Instant::now();
    let mut first = rig(100 * MS, 100 * MS, 100 * MS, &[]);
    first.kernel.run_with_ports_paced(T0, mid, &mut first.router, &mut pacer).expect("span 1");
    std::thread::sleep(Duration::from_millis(250));
    // A fresh kernel per span, systems registered at the span start, as `run_one_span` does.
    let board_log: StepLog = Rc::default();
    let mut kernel = HeteroKernel::new(100 * MS);
    kernel.register_system(
        "board",
        100 * MS,
        erase_with_id("test.board", Board { t0: mid, slow: BTreeMap::new(), log: board_log.clone() }, |_id, never: Infallible| match never {}),
        mid,
        vec![],
    );
    let mut router = Router::build(&SosConfiguration::default(), &BTreeMap::new()).expect("no connections declared");
    kernel.run_with_ports_paced(mid, end, &mut router, &mut pacer).expect("span 2");
    let stats = pacer.finish_run(end);
    assert!(started.elapsed() >= Duration::from_millis(1_000));
    assert_eq!(stats.ticks_paced, 10, "five ticks per span; the epoch ending span 1 is not paced again by span 2");
    let starts: Vec<(i64, f64)> = board_log.borrow().iter().map(|&(e, at)| ((e - T0) / MS, at.duration_since(started).as_secs_f64() * 1e3)).collect();
    println!("two_spans: span-2 step starts (ms after run start) = {:?}; overruns = {:?}", starts.iter().map(|(e, o)| (*e, o.round())).collect::<Vec<_>>(), pacer.overruns());
    assert_eq!(stats.overrun_count, 1, "only the first tick after the gap is late");
    let o = pacer.overruns()[0];
    assert_eq!(o.epoch_tai_ns, T0 + 600 * MS);
    assert!(o.overrun_ns >= 45 * MS && o.overrun_ns <= 110 * MS, "overrun {} ms, expected ~50", o.overrun_ns as f64 / 1e6);
    // The anchor never moved: the last step still begins at ~900 ms after the run's start.
    let last = starts.last().unwrap().1;
    assert!((898.0..=960.0).contains(&last), "last step began at {last:.1} ms, expected ~900");
}
