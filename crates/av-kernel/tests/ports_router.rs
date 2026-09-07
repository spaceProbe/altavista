//! `docs/open-questions.md` question 108 (M13.1) and question 110 (M14.2)'s required
//! kernel-level tests:
//!
//! 1. Two native models exchanging a SIGNAL, with and without latency
//!    ([`two_native_models_exchange_a_signal_without_latency`]/
//!    [`two_native_models_exchange_a_signal_with_latency`] -- the latter rewritten for M14.2,
//!    see its own doc comment for exactly what changed and why).
//! 2. Order determinism across two runs, comparing the delivered sequence itself
//!    ([`delivery_order_is_deterministic_and_matches_port_then_sender_and_repeats_identically`],
//!    and, added for M14.2, the same comparison with deferral actually in play --
//!    [`delivery_order_is_deterministic_under_deferral_and_repeats_identically`]).
//! 3. **M14.2 (question 110):** a 50 ms latency under a 100 ms receiver step delivers one step
//!    later than zero latency when emission lands mid-step -- counted directly, in native steps
//!    ([`fifty_ms_latency_under_a_hundred_ms_step_delivers_one_step_later_than_zero_latency_when_emission_lands_mid_step`]).
//! 4. **M14.2:** the same step when emission lands exactly on a step boundary with zero latency
//!    ([`zero_latency_delivers_in_the_same_step_when_emission_lands_exactly_on_a_step_boundary`]).
//! 5. **M14.2:** a held message is not lost -- delivered later, and every message emitted early
//!    enough to be deliverable before the run ends actually is
//!    ([`every_deliverable_message_is_delivered_none_lost_over_a_run_long_enough_for_all_of_them_to_land`]).
//! 6. **M16.1 (question 119):** a coarse sender, stepped ahead of the output clock atomically by
//!    `HeteroScheduler::advance_to_with_ports`'s new catch-up, still cannot deliver to a fine
//!    receiver before the receiver's own step reaches the sender's step-end epoch -- counted
//!    directly in receiver steps
//!    ([`a_coarse_sender_stepped_ahead_atomically_does_not_deliver_to_a_fine_receiver_before_its_own_step_end`]).
//! 7. **M16.1:** order determinism holds when that same catch-up loop iterates more than once per
//!    output tick for more than one sender at once
//!    ([`delivery_order_is_deterministic_when_multiple_senders_catch_up_by_different_amounts`]).
//!
//! The four typed refusals (undeclared port, direction mismatch, kind mismatch, non-latency
//! link model) are [`av_kernel::router::Router::build`]'s own unit tests
//! (`crates/av-kernel/src/router.rs`, `#[cfg(test)] mod tests`) -- pure data in, pure data out,
//! no GMAT and nothing this file needs to duplicate. That same module's `#[cfg(test)] mod
//! tests` also carries M14.2's `Router::take_inbox`-level deferral unit tests (holding across
//! multiple calls, the boundary-inclusive epoch, never losing a message across many small
//! `as_of` steps) -- this file's own M14.2 tests exercise the identical rule end to end, through
//! the real `HeteroKernel::run_with_ports` entry point M14.1 wired the router into.
//! `a_connection_to_an_undeclared_port_is_refused_through_the_full_executor`
//! (`tests/drm_executor.rs`) additionally proves the undeclared-port refusal reaches the real
//! `execute()` entry point.
//!
//! No GMAT dependency: every model here is a small synthetic `av_dynamics::DynamicsModel`,
//! following this crate's existing convention (`src/kernel.rs`'s own test module) of never
//! needing a GMAT install to exercise the kernel/scheduler/router plumbing itself.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use av_cdm::pb::{Connection, ModelInfo, Port, PortDirection, PortKind, PortMessage, PortTiming, SosConfiguration, SystemDefinition, SystemInstance};
use av_dynamics::{decode_signal, AppliedCommand, BoxedModel, DynamicsModel, Inbox, ModelError, Outbox, StepResult};
use av_kernel::{HeteroKernel, Router};

/// A model with one port: emits a SIGNAL carrying its own emission epoch (as an `f64` of
/// nanoseconds) on `port` at every native step up to and including `stop_after_tai_ns` (default
/// `i64::MAX`, i.e. every step for the whole run -- [`SignalSender::new`]), and ignores whatever
/// `Inbox` it is handed. Past `stop_after_tai_ns` it still steps (state is a single,
/// never-changing component -- this test suite is about port delivery, not dynamics) but emits
/// nothing, letting a test give a receiver some trailing steps with no new emissions to drain
/// whatever is still held -- needed to demonstrate zero messages lost, not merely "lost less
/// than it used to be".
#[derive(Clone)]
struct SignalSender {
    port: String,
    stop_after_tai_ns: i64,
}
impl SignalSender {
    fn new(port: impl Into<String>) -> Self {
        SignalSender { port: port.into(), stop_after_tai_ns: i64::MAX }
    }
    fn stopping_after(port: impl Into<String>, stop_after_tai_ns: i64) -> Self {
        SignalSender { port: port.into(), stop_after_tai_ns }
    }
}
impl DynamicsModel for SignalSender {
    type Error = ModelError;
    fn state_dim(&self) -> usize {
        1
    }
    fn derivatives(&self, _state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        out[0] = 0.0;
        Ok(())
    }
    fn describe(&self) -> ModelInfo {
        ModelInfo { id: "test.signal_sender".to_string(), ..Default::default() }
    }
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64, _inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        let t1 = t_tai_ns + dt_ns;
        let mut outbox = Outbox::new();
        if t1 <= self.stop_after_tai_ns {
            // The emitted value *is* the emission epoch -- lets every assertion below recompute
            // what should have been sent, at every step, without threading extra state through
            // this model.
            outbox.push_signal(&self.port, t1, t1 as f64);
        }
        Ok((StepResult { state: state.to_vec(), t_tai_ns: t1, outputs: BTreeMap::new() }, outbox, Vec::new()))
    }
}

/// Like [`SignalSender`], but a genuinely physical (`state_dim() == 6`, `[pos x3; vel x3]`,
/// constant zero velocity) model rather than a 1-component placeholder -- needed for M16.1's
/// coarse-sender tests below, where `HeteroKernel::run_with_ports` samples the sender itself
/// (via `HeteroScheduler::sample_kind`) at output ticks strictly between its own native steps
/// once it is coarser than the output rate, which requires `crate::interpolate::hermite_velocity`
/// (a `[pos x3; vel x3, ...]` shape, panicking on anything shorter) rather than [`SignalSender`]'s
/// own single, uninterpolable component.
#[derive(Clone)]
struct PhysicalSignalSender {
    port: String,
}
impl PhysicalSignalSender {
    fn new(port: impl Into<String>) -> Self {
        PhysicalSignalSender { port: port.into() }
    }
}
impl DynamicsModel for PhysicalSignalSender {
    type Error = ModelError;
    fn state_dim(&self) -> usize {
        6
    }
    fn derivatives(&self, _state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        out.fill(0.0);
        Ok(())
    }
    fn describe(&self) -> ModelInfo {
        ModelInfo { id: "test.physical_signal_sender".to_string(), state_space_id: "test.6d".to_string(), frame_id: "test.frame".to_string(), ..Default::default() }
    }
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64, _inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        let t1 = t_tai_ns + dt_ns;
        let mut outbox = Outbox::new();
        // Same convention as `SignalSender`: the emitted value *is* the emission epoch.
        outbox.push_signal(&self.port, t1, t1 as f64);
        Ok((StepResult { state: state.to_vec(), t_tai_ns: t1, outputs: BTreeMap::new() }, outbox, Vec::new()))
    }
}

/// A model with one or more IN ports: logs every delivered `PortMessage` (in the order its own
/// `Inbox` already carries them) into a shared `log`, and never emits anything itself.
#[derive(Clone)]
struct SignalReceiver {
    log: Rc<RefCell<Vec<PortMessage>>>,
}
impl DynamicsModel for SignalReceiver {
    type Error = ModelError;
    fn state_dim(&self) -> usize {
        1
    }
    fn derivatives(&self, _state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        out[0] = 0.0;
        Ok(())
    }
    fn describe(&self) -> ModelInfo {
        ModelInfo { id: "test.signal_receiver".to_string(), ..Default::default() }
    }
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        self.log.borrow_mut().extend(inbox.messages().iter().cloned());
        Ok((StepResult { state: state.to_vec(), t_tai_ns: t_tai_ns + dt_ns, outputs: BTreeMap::new() }, Outbox::new(), Vec::new()))
    }
}

fn signal_port(name: &str, direction: PortDirection, latency_ns: i64) -> Port {
    Port {
        name: name.to_string(),
        kind: PortKind::Signal as i32,
        direction: direction as i32,
        timing: if latency_ns == 0 { None } else { Some(PortTiming { latency_ns, ..Default::default() }) },
        ..Default::default()
    }
}

/// One sender ("sender", OUT port "out") wired to one receiver ("receiver", IN port "in").
fn one_sender_wiring(link_model: &str, sender_latency_ns: i64, receiver_latency_ns: i64) -> (SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let sender_sys = SystemDefinition { id: "sender_sys".to_string(), ports: vec![signal_port("out", PortDirection::Out, sender_latency_ns)], ..Default::default() };
    let receiver_sys = SystemDefinition { id: "receiver_sys".to_string(), ports: vec![signal_port("in", PortDirection::In, receiver_latency_ns)], ..Default::default() };
    let sos = SosConfiguration {
        instances: vec![
            SystemInstance { name: "sender".to_string(), system_id: "sender_sys".to_string(), ..Default::default() },
            SystemInstance { name: "receiver".to_string(), system_id: "receiver_sys".to_string(), ..Default::default() },
        ],
        connections: vec![Connection { from_instance: "sender".to_string(), from_port: "out".to_string(), to_instance: "receiver".to_string(), to_port: "in".to_string(), link_model: link_model.to_string() }],
        ..Default::default()
    };
    let systems = BTreeMap::from([(sender_sys.id.clone(), sender_sys), (receiver_sys.id.clone(), receiver_sys)]);
    (sos, systems)
}

fn run_one_sender(link_model: &str, sender_latency_ns: i64, receiver_latency_ns: i64, period_ns: i64, steps: i64) -> Vec<PortMessage> {
    let (sos, systems) = one_sender_wiring(link_model, sender_latency_ns, receiver_latency_ns);
    let mut router = Router::build(&sos, &systems).expect("valid wiring");

    let log: Rc<RefCell<Vec<PortMessage>>> = Rc::new(RefCell::new(Vec::new()));
    let mut kernel = HeteroKernel::new(period_ns);
    kernel.register_system("sender", period_ns, Box::new(SignalSender::new("out".to_string())) as BoxedModel, 0, vec![0.0]);
    kernel.register_system("receiver", period_ns, Box::new(SignalReceiver { log: log.clone() }) as BoxedModel, 0, vec![0.0]);

    kernel.run_with_ports(0, period_ns * steps, &mut router).expect("run succeeds");
    let received = log.borrow().clone();
    received
}

/// Required test: two native models exchanging a SIGNAL, without a link model (zero added
/// latency). `HeteroScheduler::advance_to_with_ports` steps instances due at the same instant
/// in id-sorted order (`"receiver"` before `"sender"`), so the receiver only ever drains a
/// message at the native step *after* the one that emitted it: over 5 steps, the sender emits
/// at t = 100, 200, 300, 400, 500 ms and the receiver drains at t = 200, 300, 400, 500 ms,
/// seeing the *previous* step's message each time -- 4 delivered messages, the 5th still
/// in flight when the run ends.
#[test]
fn two_native_models_exchange_a_signal_without_latency() {
    let period_ns: i64 = 100_000_000;
    let received = run_one_sender("", 0, 0, period_ns, 5);

    let want_tai_ns: Vec<i64> = (1..=4).map(|k| k * period_ns).collect();
    let got_tai_ns: Vec<i64> = received.iter().map(|m| m.tai_ns).collect();
    assert_eq!(got_tai_ns, want_tai_ns);

    for m in &received {
        assert_eq!(m.port, "in", "a delivered message carries the *receiving* port's own name");
        assert_eq!(decode_signal(&m.payload), Some(m.tai_ns as f64), "no link_model: delivered tai_ns equals the sender's own emission epoch exactly");
    }
}

/// Required test: the same two models, with the `"latency"` link model requested -- the
/// delivered `tai_ns` is the emission epoch plus both ports' own declared `PortTiming.
/// latency_ns`, while the *payload* (the raw emission epoch the sender encoded) is unchanged --
/// proving the router adds latency to the message's own availability timestamp, not to what was
/// actually sent.
///
/// **What changed for M14.2 (question 110) and why.** The M13.1 version of this test used a 50
/// ms total latency (30 ms + 20 ms) under this same 100 ms period. That combination can *never*
/// distinguish "latency merely stamped onto `tai_ns`" (the M13.1 bug, question 110's own
/// framing: "the link model stamps latency onto the message epoch but delivery still happens at
/// the receiver's next step") from "latency actually gates delivery" (this task's fix): both
/// instances share `"sender"`/`"receiver"` as their names, and `"receiver"` sorts before
/// `"sender"`, so a message `"sender"` emits at a tied due epoch is only ever queued *after*
/// `"receiver"`'s own step at that same epoch has already run -- the earliest `"receiver"` can
/// possibly see it is one native step later regardless of latency. With 50 ms < 100 ms
/// (period_ns), availability (`emission + 50 ms`) is always `<=` that one-step-later epoch
/// anyway, so the message arrives exactly where the M13.1 (buggy) code already put it, and this
/// test passed under both the buggy and the fixed router without telling them apart -- exactly
/// what M13.1's own PR called out as its known gap.
///
/// This version raises the total latency to 120 ms, *above* one period, which does distinguish
/// them: a message emitted at step `k` (`t = k * period_ns`) is, by the ordering argument above,
/// queued no earlier than `"receiver"`'s own due epoch `(k+1) * period_ns` -- but its
/// availability is `k * period_ns + 120 ms`, which is **not** `<= (k+1) * period_ns` (120 ms >
/// 100 ms). A router that only stamps the timestamp (M13.1's bug) would still hand the message
/// to `"receiver"` at `(k+1) * period_ns` regardless -- delivered *before* it was actually
/// available, exactly the failure question 110 opened. [`av_kernel::router::Router::take_inbox`]
/// holds it one native step further, to `(k+2) * period_ns`, where `k * period_ns + 120 ms <=
/// (k+2) * period_ns` finally holds -- asserted below directly, alongside a companion,
/// side-by-side count of *native steps elapsed* in
/// [`fifty_ms_latency_under_a_hundred_ms_step_delivers_one_step_later_than_zero_latency_when_emission_lands_mid_step`].
#[test]
fn two_native_models_exchange_a_signal_with_latency() {
    let period_ns: i64 = 100_000_000;
    let sender_latency_ns: i64 = 70_000_000;
    let receiver_latency_ns: i64 = 50_000_000;
    let total_latency_ns = sender_latency_ns + receiver_latency_ns; // 120 ms, > period_ns
    assert!(total_latency_ns > period_ns, "this test only distinguishes stamped-only from actually-deferred latency once it exceeds one period -- see the doc comment above");

    // 7 native steps: emissions at k = 1..=7 are each due for delivery at (k+2)*period_ns, so
    // k = 6 (due at 800 ms) and k = 7 (due at 900 ms, itself the run's very last native step)
    // both fall outside this run's own 700 ms horizon -- still in flight when it ends, exactly
    // like the zero-latency test above (which likewise leaves its own last emission in flight).
    let steps = 7;
    let received = run_one_sender("latency", sender_latency_ns, receiver_latency_ns, period_ns, steps);

    let want_tai_ns: Vec<i64> = (1..=5).map(|k| k * period_ns + total_latency_ns).collect();
    let got_tai_ns: Vec<i64> = received.iter().map(|m| m.tai_ns).collect();
    assert_eq!(got_tai_ns, want_tai_ns);
    assert_eq!(received.len(), 5, "5 of the 7 emitted messages are deliverable within this run's horizon (see the doc comment); 2 are still in flight when it ends");

    for m in &received {
        assert_eq!(m.port, "in", "a delivered message carries the *receiving* port's own name");
        let emission_epoch = decode_signal(&m.payload).expect("SIGNAL payload is 8 bytes");
        assert_eq!(m.tai_ns as f64 - emission_epoch, total_latency_ns as f64, "added latency must be exactly the two ports' summed latency_ns");
    }
}

/// Two senders ("sender_a" on OUT port "beta", "sender_b" on OUT port "alpha") both wired to
/// one receiver's two IN ports of the same names, so the receiver's inbox at every step mixes
/// messages from two different senders on two different ports -- exercising the full four-field
/// delivery order (question 108: receiving instance, then port name, then sender emission
/// epoch, then sender instance id). Port name is deliberately made the *only* way to tell the
/// two apart in sort order that matters here: "alpha" < "beta" while "sender_a" < "sender_b" --
/// i.e. sorting by port name gives the *opposite* pairing sender-instance-name order alone
/// would, proving port name really is the primary key, not a tie-break that happens to agree
/// with it.
fn two_sender_wiring() -> (SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let sender_a_sys = SystemDefinition { id: "sender_a_sys".to_string(), ports: vec![signal_port("beta", PortDirection::Out, 0)], ..Default::default() };
    let sender_b_sys = SystemDefinition { id: "sender_b_sys".to_string(), ports: vec![signal_port("alpha", PortDirection::Out, 0)], ..Default::default() };
    let receiver_sys =
        SystemDefinition { id: "receiver_sys".to_string(), ports: vec![signal_port("alpha", PortDirection::In, 0), signal_port("beta", PortDirection::In, 0)], ..Default::default() };
    let sos = SosConfiguration {
        instances: vec![
            SystemInstance { name: "sender_a".to_string(), system_id: "sender_a_sys".to_string(), ..Default::default() },
            SystemInstance { name: "sender_b".to_string(), system_id: "sender_b_sys".to_string(), ..Default::default() },
            SystemInstance { name: "receiver".to_string(), system_id: "receiver_sys".to_string(), ..Default::default() },
        ],
        connections: vec![
            Connection { from_instance: "sender_a".to_string(), from_port: "beta".to_string(), to_instance: "receiver".to_string(), to_port: "beta".to_string(), link_model: String::new() },
            Connection { from_instance: "sender_b".to_string(), from_port: "alpha".to_string(), to_instance: "receiver".to_string(), to_port: "alpha".to_string(), link_model: String::new() },
        ],
        ..Default::default()
    };
    let systems = BTreeMap::from([(sender_a_sys.id.clone(), sender_a_sys), (sender_b_sys.id.clone(), sender_b_sys), (receiver_sys.id.clone(), receiver_sys)]);
    (sos, systems)
}

fn run_two_senders(period_ns: i64, steps: i64) -> Vec<(String, i64)> {
    let (sos, systems) = two_sender_wiring();
    let mut router = Router::build(&sos, &systems).expect("valid wiring");
    let log: Rc<RefCell<Vec<PortMessage>>> = Rc::new(RefCell::new(Vec::new()));

    let mut kernel = HeteroKernel::new(period_ns);
    kernel.register_system("sender_a", period_ns, Box::new(SignalSender::new("beta".to_string())) as BoxedModel, 0, vec![0.0]);
    kernel.register_system("sender_b", period_ns, Box::new(SignalSender::new("alpha".to_string())) as BoxedModel, 0, vec![0.0]);
    kernel.register_system("receiver", period_ns, Box::new(SignalReceiver { log: log.clone() }) as BoxedModel, 0, vec![0.0]);

    kernel.run_with_ports(0, period_ns * steps, &mut router).expect("run succeeds");
    let received: Vec<(String, i64)> = log.borrow().iter().map(|m| (m.port.clone(), m.tai_ns)).collect();
    received
}

/// Required test: order determinism across two runs, comparing the delivered sequence itself
/// (not a summary such as a count or a hash of it).
#[test]
fn delivery_order_is_deterministic_and_matches_port_then_sender_and_repeats_identically() {
    let period_ns: i64 = 100_000_000;
    let run1 = run_two_senders(period_ns, 5);
    let run2 = run_two_senders(period_ns, 5);

    assert_eq!(run1, run2, "the exact delivered sequence -- port names paired with delivery epochs, in order -- must be bit-identical across two independent runs of the same setup");

    assert_eq!(run1.len(), 8, "4 receiver steps each deliver one message per sender (the 5th emission from each sender is still in flight when the run ends)");
    for pair in run1.chunks(2) {
        assert_eq!(pair[0].0, "alpha", "\"alpha\" (from sender_b) must sort before \"beta\" (from sender_a): port name is the primary key, not sender instance id");
        assert_eq!(pair[1].0, "beta");
        assert_eq!(pair[0].1, pair[1].1, "both messages in a pair were emitted at, and delivered at, the same epoch in this no-latency setup");
    }
}

// ---------------------------------------------------------------------------------------------
// M14.2 (`docs/open-questions.md` question 110): latency actually gates delivery.
// ---------------------------------------------------------------------------------------------

/// Like [`SignalReceiver`], but logs one entry **per own native step** -- `(this step's own
/// result epoch, however many messages its `Inbox` carried, possibly zero)` -- instead of a flat
/// stream of delivered messages. A flat stream (as `SignalReceiver` logs) can only ever answer
/// "what arrived, and with what `tai_ns`"; the tests below need to ask "*which step* first saw
/// anything at all", i.e. how many native steps elapsed before a held message cleared, which
/// requires knowing about the steps that saw nothing too.
#[derive(Clone)]
struct SignalReceiverStepLog {
    log: Rc<RefCell<Vec<(i64, usize)>>>,
}
impl DynamicsModel for SignalReceiverStepLog {
    type Error = ModelError;
    fn state_dim(&self) -> usize {
        1
    }
    fn derivatives(&self, _state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        out[0] = 0.0;
        Ok(())
    }
    fn describe(&self) -> ModelInfo {
        ModelInfo { id: "test.signal_receiver_step_log".to_string(), ..Default::default() }
    }
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        let t1 = t_tai_ns + dt_ns;
        self.log.borrow_mut().push((t1, inbox.messages().len()));
        Ok((StepResult { state: state.to_vec(), t_tai_ns: t1, outputs: BTreeMap::new() }, Outbox::new(), Vec::new()))
    }
}

/// One sender ("sender", OUT port "out") wired to *two* receivers on the same emissions: `"
/// receiver_zero"` over a `""` (unset, always-zero-latency) connection, `"receiver_latent"` over
/// a `"latency"` connection declaring `latency_ns` entirely on the receiving port. Comparing the
/// two receivers' own [`SignalReceiverStepLog`]s inside **one** run gives an apples-to-apples
/// count of native steps elapsed before each first saw a message, off the exact same emissions.
fn one_sender_two_receivers_wiring(latency_ns: i64) -> (SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let sender_sys = SystemDefinition { id: "sender_sys".to_string(), ports: vec![signal_port("out", PortDirection::Out, 0)], ..Default::default() };
    let zero_sys = SystemDefinition { id: "zero_sys".to_string(), ports: vec![signal_port("in", PortDirection::In, 0)], ..Default::default() };
    let latent_sys = SystemDefinition { id: "latent_sys".to_string(), ports: vec![signal_port("in", PortDirection::In, latency_ns)], ..Default::default() };
    let sos = SosConfiguration {
        instances: vec![
            SystemInstance { name: "sender".to_string(), system_id: "sender_sys".to_string(), ..Default::default() },
            SystemInstance { name: "receiver_zero".to_string(), system_id: "zero_sys".to_string(), ..Default::default() },
            SystemInstance { name: "receiver_latent".to_string(), system_id: "latent_sys".to_string(), ..Default::default() },
        ],
        connections: vec![
            Connection { from_instance: "sender".to_string(), from_port: "out".to_string(), to_instance: "receiver_zero".to_string(), to_port: "in".to_string(), link_model: String::new() },
            Connection { from_instance: "sender".to_string(), from_port: "out".to_string(), to_instance: "receiver_latent".to_string(), to_port: "in".to_string(), link_model: "latency".to_string() },
        ],
        ..Default::default()
    };
    let systems = BTreeMap::from([(sender_sys.id.clone(), sender_sys), (zero_sys.id.clone(), zero_sys), (latent_sys.id.clone(), latent_sys)]);
    (sos, systems)
}

/// Required test ("a test that counts steps"): a 50 ms latency under a 100 ms receiver step
/// delivers one step later than zero latency, when emission lands mid-step (the sender's own 80
/// ms period does not align with the receiver's 100 ms one, so its first emission, at t = 80 ms,
/// falls strictly inside the receiver's own first `(0, 100 ms]` window rather than exactly on
/// its boundary -- see the sibling boundary test below for the exactly-on-a-boundary case).
///
/// Both receivers share the sender's one emission at t = 80 ms (availability 80 ms for
/// `"receiver_zero"`, 130 ms for `"receiver_latent"`): `"receiver_zero"`'s first due epoch,
/// 100 ms, is `>= 80 ms`, so it sees the message at its very first step; `"receiver_latent"`'s
/// first due epoch, 100 ms, is *not* `>= 130 ms`, so it is held there and only clears at
/// `"receiver_latent"`'s *second* due epoch, 200 ms -- exactly one native step later.
#[test]
fn fifty_ms_latency_under_a_hundred_ms_step_delivers_one_step_later_than_zero_latency_when_emission_lands_mid_step() {
    let sender_period_ns: i64 = 80_000_000;
    let receiver_period_ns: i64 = 100_000_000;
    let latency_ns: i64 = 50_000_000;
    // `HeteroKernel::run_with_ports` samples every registered system at every output tick, which
    // requires each system's own native period to divide the output period evenly (otherwise a
    // system not yet due for its first native step cannot be sampled at all) -- 400 ms is the
    // smallest common multiple of the sender's 80 ms and the receivers' 100 ms.
    let output_period_ns: i64 = 400_000_000;

    let (sos, systems) = one_sender_two_receivers_wiring(latency_ns);
    let mut router = Router::build(&sos, &systems).expect("valid wiring");

    let log_zero: Rc<RefCell<Vec<(i64, usize)>>> = Rc::new(RefCell::new(Vec::new()));
    let log_latent: Rc<RefCell<Vec<(i64, usize)>>> = Rc::new(RefCell::new(Vec::new()));
    let mut kernel = HeteroKernel::new(output_period_ns);
    kernel.register_system("sender", sender_period_ns, Box::new(SignalSender::new("out".to_string())) as BoxedModel, 0, vec![0.0]);
    kernel.register_system("receiver_zero", receiver_period_ns, Box::new(SignalReceiverStepLog { log: log_zero.clone() }) as BoxedModel, 0, vec![0.0]);
    kernel.register_system("receiver_latent", receiver_period_ns, Box::new(SignalReceiverStepLog { log: log_latent.clone() }) as BoxedModel, 0, vec![0.0]);

    // One output tick (0 to 400 ms) is enough: `advance_to_with_ports` still steps every system
    // through every one of its own native due epochs up to that tick internally (100, 200, 300,
    // 400 ms for the receivers), independent of how coarse the output-sampling grid itself is.
    kernel.run_with_ports(0, output_period_ns, &mut router).expect("run succeeds");

    let zero_log = log_zero.borrow().clone();
    let latent_log = log_latent.borrow().clone();
    assert_eq!(zero_log[0], (100_000_000, 1), "zero latency: the one t=80ms emission is already available at the receiver's first step (100 ms)");
    assert_eq!(latent_log[0], (100_000_000, 0), "50 ms latency: not yet available (130 ms > 100 ms) -- held, not delivered early, at the first step");
    assert_eq!(latent_log[1], (200_000_000, 1), "50 ms latency: available by the second step (130 ms <= 200 ms) -- delivered exactly one native step later than the zero-latency case");

    let zero_step = zero_log.iter().find(|(_, n)| *n > 0).expect("zero-latency receiver eventually sees the message").0;
    let latent_step = latent_log.iter().find(|(_, n)| *n > 0).expect("latent receiver eventually sees the message").0;
    assert_eq!(latent_step, zero_step + receiver_period_ns, "one receiver step later, counted directly in native steps -- question 110's own required assertion");
}

/// Required test: the same step when emission lands exactly on a step boundary with zero
/// latency. `"alpha_sender"` and `"zulu_receiver"` are named so that, at their shared due epoch
/// (both have period 100 ms, `t0 = 0`), `BTreeMap` visits `"alpha_sender"` first -- it emits
/// (queuing its message with the connection's zero added latency) *before* `"zulu_receiver"`
/// asks for its own inbox at that very same epoch, so the message is available (`tai_ns ==
/// as_of_tai_ns`, question 110's inclusive `<=` boundary) at the first opportunity there is: no
/// deferral at all, not even the one-native-step delay every other test in this file sees from
/// `"receiver"` sorting *before* `"sender"`.
#[test]
fn zero_latency_delivers_in_the_same_step_when_emission_lands_exactly_on_a_step_boundary() {
    let period_ns: i64 = 100_000_000;
    let sender_sys = SystemDefinition { id: "sender_sys".to_string(), ports: vec![signal_port("out", PortDirection::Out, 0)], ..Default::default() };
    let receiver_sys = SystemDefinition { id: "receiver_sys".to_string(), ports: vec![signal_port("in", PortDirection::In, 0)], ..Default::default() };
    let sos = SosConfiguration {
        instances: vec![
            SystemInstance { name: "alpha_sender".to_string(), system_id: "sender_sys".to_string(), ..Default::default() },
            SystemInstance { name: "zulu_receiver".to_string(), system_id: "receiver_sys".to_string(), ..Default::default() },
        ],
        connections: vec![Connection { from_instance: "alpha_sender".to_string(), from_port: "out".to_string(), to_instance: "zulu_receiver".to_string(), to_port: "in".to_string(), link_model: String::new() }],
        ..Default::default()
    };
    let systems = BTreeMap::from([(sender_sys.id.clone(), sender_sys), (receiver_sys.id.clone(), receiver_sys)]);
    let mut router = Router::build(&sos, &systems).expect("valid wiring");

    let log: Rc<RefCell<Vec<PortMessage>>> = Rc::new(RefCell::new(Vec::new()));
    let mut kernel = HeteroKernel::new(period_ns);
    kernel.register_system("alpha_sender", period_ns, Box::new(SignalSender::new("out".to_string())) as BoxedModel, 0, vec![0.0]);
    kernel.register_system("zulu_receiver", period_ns, Box::new(SignalReceiver { log: log.clone() }) as BoxedModel, 0, vec![0.0]);

    // A single native step: "alpha_sender" emits at t=100ms (exactly this run's one and only
    // step boundary), "zulu_receiver" -- processed second, per the doc comment -- must already
    // see it in the very same step.
    kernel.run_with_ports(0, period_ns, &mut router).expect("run succeeds");

    let received = log.borrow().clone();
    assert_eq!(received.len(), 1, "delivered within the one and only step this run has, not held for a step that never comes");
    assert_eq!(received[0].tai_ns, period_ns, "delivered tai_ns equals the emission/boundary epoch exactly: zero latency, availability == step epoch");
}

/// One sender ("sender_a", OUT port "beta", zero added latency) and one sender ("sender_b", OUT
/// port "alpha", 150 ms added latency -- above the 100 ms period, so its own messages are
/// genuinely held and cross a receiver step boundary that "sender_a"'s never do) both wired to
/// one receiver's two IN ports of those same names, so the receiver's own inbox mixes an
/// immediately-available message with a held-then-delivered one at different steps of the same
/// run -- unlike [`two_sender_wiring`], where neither connection ever defers anything.
fn two_sender_wiring_with_deferral() -> (SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let sender_a_sys = SystemDefinition { id: "sender_a_sys".to_string(), ports: vec![signal_port("beta", PortDirection::Out, 0)], ..Default::default() };
    let sender_b_sys = SystemDefinition { id: "sender_b_sys".to_string(), ports: vec![signal_port("alpha", PortDirection::Out, 0)], ..Default::default() };
    let receiver_sys = SystemDefinition {
        id: "receiver_sys".to_string(),
        ports: vec![signal_port("alpha", PortDirection::In, 150_000_000), signal_port("beta", PortDirection::In, 0)],
        ..Default::default()
    };
    let sos = SosConfiguration {
        instances: vec![
            SystemInstance { name: "sender_a".to_string(), system_id: "sender_a_sys".to_string(), ..Default::default() },
            SystemInstance { name: "sender_b".to_string(), system_id: "sender_b_sys".to_string(), ..Default::default() },
            SystemInstance { name: "receiver".to_string(), system_id: "receiver_sys".to_string(), ..Default::default() },
        ],
        connections: vec![
            Connection { from_instance: "sender_a".to_string(), from_port: "beta".to_string(), to_instance: "receiver".to_string(), to_port: "beta".to_string(), link_model: String::new() },
            Connection { from_instance: "sender_b".to_string(), from_port: "alpha".to_string(), to_instance: "receiver".to_string(), to_port: "alpha".to_string(), link_model: "latency".to_string() },
        ],
        ..Default::default()
    };
    let systems = BTreeMap::from([(sender_a_sys.id.clone(), sender_a_sys), (sender_b_sys.id.clone(), sender_b_sys), (receiver_sys.id.clone(), receiver_sys)]);
    (sos, systems)
}

fn run_two_senders_with_deferral(period_ns: i64, steps: i64) -> Vec<(String, i64)> {
    let (sos, systems) = two_sender_wiring_with_deferral();
    let mut router = Router::build(&sos, &systems).expect("valid wiring");
    let log: Rc<RefCell<Vec<PortMessage>>> = Rc::new(RefCell::new(Vec::new()));

    let mut kernel = HeteroKernel::new(period_ns);
    kernel.register_system("sender_a", period_ns, Box::new(SignalSender::new("beta".to_string())) as BoxedModel, 0, vec![0.0]);
    kernel.register_system("sender_b", period_ns, Box::new(SignalSender::new("alpha".to_string())) as BoxedModel, 0, vec![0.0]);
    kernel.register_system("receiver", period_ns, Box::new(SignalReceiver { log: log.clone() }) as BoxedModel, 0, vec![0.0]);

    kernel.run_with_ports(0, period_ns * steps, &mut router).expect("run succeeds");
    let received: Vec<(String, i64)> = log.borrow().iter().map(|m| (m.port.clone(), m.tai_ns)).collect();
    received
}

/// Required test: order determinism across two runs, comparing the delivered sequence itself,
/// **with deferral actually in play** -- unlike
/// [`delivery_order_is_deterministic_and_matches_port_then_sender_and_repeats_identically`]
/// above, where every connection clears in exactly one native step every time, so a router that
/// held nothing back at all would pass it too. Here "alpha" (from `"sender_b"`, 150 ms latency)
/// is genuinely held across a step boundary "beta" (from `"sender_a"`, zero latency) never is,
/// so the two ports' own deliveries interleave unevenly across steps -- exactly the shape of
/// output where a non-deterministic hold (e.g. iterating a `HashMap` instead of `BTreeMap`
/// somewhere in the hold-and-reconsider path) would show up as a run-to-run mismatch.
#[test]
fn delivery_order_is_deterministic_under_deferral_and_repeats_identically() {
    let period_ns: i64 = 100_000_000;
    let run1 = run_two_senders_with_deferral(period_ns, 8);
    let run2 = run_two_senders_with_deferral(period_ns, 8);

    assert_eq!(run1, run2, "the exact delivered sequence must be bit-identical across two independent runs of the same setup, even though one port's own connection genuinely defers delivery and the other never does");

    let beta_count = run1.iter().filter(|(p, _)| p == "beta").count();
    let alpha_count = run1.iter().filter(|(p, _)| p == "alpha").count();
    assert!(beta_count > 0 && alpha_count > 0, "both connections must have delivered something in this run for the comparison above to be meaningful");
    assert_ne!(beta_count, alpha_count, "the deferred (\"alpha\") and undeferred (\"beta\") connections must actually clear at a different cadence -- otherwise this test cannot tell deferral-aware ordering from the no-deferral case above");
}

/// Required test: whatever is needed to prove a held message is delivered later and not lost --
/// total messages delivered equals total messages emitted, over a run long enough for every one
/// of them to land. Unlike every other latency test in this file (each of which deliberately
/// leaves at least one trailing emission still in flight when its own run ends, and says so),
/// this run's receiver steps far more often than its sender emits, with enough trailing steps
/// after the sender's very last emission that even *that* message's own 30 ms latency clears
/// well before the run ends -- so there is nothing left in `Router::has_pending` to explain away.
#[test]
fn every_deliverable_message_is_delivered_none_lost_over_a_run_long_enough_for_all_of_them_to_land() {
    let period_ns: i64 = 100_000_000;
    let latency_ns: i64 = 30_000_000;
    // Real emissions at t = 100, 200, 300, 400, 500 ms (5 of them); [`SignalSender::
    // stopping_after`] emits nothing after that, so the run can keep going with `"receiver"`
    // getting extra, message-free steps to drain whatever the last real emission left held --
    // without that, every registered system is due again at this run's own last native step
    // (`HeteroKernel::run_with_ports` requires the horizon to be an exact multiple of the output
    // period, which in turn must be a multiple of every system's own period, so the very last
    // instant is *always* another native step for both), and a step's own emission -- available
    // only strictly after it, given nonzero latency -- can never be drained within that same run
    // (see `crate::router`'s module doc comment's "still pending when the run ends" note, and
    // this file's other latency tests, which each leave exactly that trailing residue and say
    // so). This test exists to show that residue is a property of *how long the run is*, not of
    // deferral itself: give it enough quiet trailing steps and nothing is left over at all.
    let stop_after_tai_ns: i64 = 5 * period_ns;
    let end_tai_ns: i64 = 8 * period_ns; // 3 trailing, emission-free receiver steps after t=500ms

    let sender_sys = SystemDefinition { id: "sender_sys".to_string(), ports: vec![signal_port("out", PortDirection::Out, 0)], ..Default::default() };
    let receiver_sys = SystemDefinition { id: "receiver_sys".to_string(), ports: vec![signal_port("in", PortDirection::In, latency_ns)], ..Default::default() };
    let sos = SosConfiguration {
        instances: vec![
            SystemInstance { name: "sender".to_string(), system_id: "sender_sys".to_string(), ..Default::default() },
            SystemInstance { name: "receiver".to_string(), system_id: "receiver_sys".to_string(), ..Default::default() },
        ],
        connections: vec![Connection { from_instance: "sender".to_string(), from_port: "out".to_string(), to_instance: "receiver".to_string(), to_port: "in".to_string(), link_model: "latency".to_string() }],
        ..Default::default()
    };
    let systems = BTreeMap::from([(sender_sys.id.clone(), sender_sys), (receiver_sys.id.clone(), receiver_sys)]);
    let mut router = Router::build(&sos, &systems).expect("valid wiring");

    let log: Rc<RefCell<Vec<PortMessage>>> = Rc::new(RefCell::new(Vec::new()));
    let mut kernel = HeteroKernel::new(period_ns);
    kernel.register_system("sender", period_ns, Box::new(SignalSender::stopping_after("out", stop_after_tai_ns)) as BoxedModel, 0, vec![0.0]);
    kernel.register_system("receiver", period_ns, Box::new(SignalReceiver { log: log.clone() }) as BoxedModel, 0, vec![0.0]);

    kernel.run_with_ports(0, end_tai_ns, &mut router).expect("run succeeds");

    let total_emitted = 5; // t = 100, 200, 300, 400, 500 ms
    let received = log.borrow().clone();
    assert_eq!(received.len(), total_emitted, "every emitted message must be delivered exactly once: none lost, none duplicated");
    assert!(!router.has_pending(), "nothing left held: this run was long enough for every message to actually land");

    // "receiver" sorts before "sender", so (as in this file's other same-period tests) a message
    // emitted at t=k*period is queued only after the receiver's own tied step already ran, and
    // (latency_ns < period_ns) is picked up at the receiver's very next due epoch regardless.
    let want_tai_ns: Vec<i64> = (1..=5).map(|k| k * period_ns + latency_ns).collect();
    let got_tai_ns: Vec<i64> = received.iter().map(|m| m.tai_ns).collect();
    assert_eq!(got_tai_ns, want_tai_ns, "delivered in emission order, each carrying its own emission epoch plus the connection's latency");
}

// ---------------------------------------------------------------------------------------------
// M16.1 (`docs/open-questions.md` question 119): a coarse system's step is one atomic advance
// past the output clock, but that must never let its port outputs reach a receiver before the
// receiver's own step reaches the sender's step-end epoch -- question 110's existing availability
// gate, applied to a sender that is now sometimes stepped multiple periods ahead of "now".
// ---------------------------------------------------------------------------------------------

/// **Required test (M16.1): a coarse sender's atomic catch-up step delivers nothing early.** A
/// 300 ms sender wired to a 100 ms receiver, zero added latency, run for exactly one sender
/// period (three receiver steps: 100, 200, 300 ms). `HeteroScheduler::advance_to_with_ports`'s
/// new catch-up (`crate::schedule`, M16.1) forces the sender's *entire* first native step (0 ->
/// 300 ms) to happen atomically during the very first output tick's own call (`target_tai_ns` =
/// 100 ms) -- three whole receiver periods before the receiver's own clock naturally gets there --
/// exactly the scenario `docs/open-questions.md` question 119 was decided over: "stepping ahead
/// delivers nothing early" because the message is stamped with the step's own end epoch (300 ms)
/// and [`av_kernel::router::Router::take_inbox`] (question 110, unchanged, not duplicated here)
/// holds it until a receiver step's own epoch reaches that. [`SignalReceiverStepLog`] logs one
/// entry *per receiver native step*, empty or not, so this counts by step, not merely by
/// comparing final `tai_ns` values or a total delivered count.
///
/// **What this test would fail against:**
/// 1. An `advance_to_with_ports` whose catch-up path stamps a stepped-ahead system's outbox with
///    the *query* epoch it is catching up towards (100 ms, this call's own `target_tai_ns`)
///    instead of the step's own true result epoch (300 ms) -- the message would already be
///    available at 100 ms and the receiver's very first step would read `(100_000_000, 1)`, not
///    `(100_000_000, 0)`.
/// 2. Any code path that hands a stepped-ahead system's outbox to a receiver's pending queue
///    without going back through `Router::take_inbox`'s own `as_of_tai_ns` gate (e.g. delivering
///    immediately once the sender's step completes, or gating on the *catch-up loop's* `target_
///    tai_ns` instead of the receiver's own step epoch) -- the receiver's second step (200 ms,
///    still 100 ms short of the sender's 300 ms availability) would already read
///    `(200_000_000, 1)` instead of `(200_000_000, 0)`.
/// 3. A catch-up that never happens at all (M15.2's original bug, `crate::kernel`'s
///    `hetero_kernel_run_with_ports_populates_native_interpolated_and_held_kind_across_a_multi_
///    rate_run` is what catches that) would make this test's own premise moot, not fail it
///    outright -- this test assumes the catch-up itself works and checks only that it does not
///    also leak delivery early.
#[test]
fn a_coarse_sender_stepped_ahead_atomically_does_not_deliver_to_a_fine_receiver_before_its_own_step_end() {
    let sender_period_ns: i64 = 300_000_000;
    let receiver_period_ns: i64 = 100_000_000;
    let output_period_ns: i64 = receiver_period_ns;

    let sender_sys = SystemDefinition { id: "sender_sys".to_string(), ports: vec![signal_port("out", PortDirection::Out, 0)], ..Default::default() };
    let receiver_sys = SystemDefinition { id: "receiver_sys".to_string(), ports: vec![signal_port("in", PortDirection::In, 0)], ..Default::default() };
    let sos = SosConfiguration {
        instances: vec![
            SystemInstance { name: "sender".to_string(), system_id: "sender_sys".to_string(), ..Default::default() },
            SystemInstance { name: "receiver".to_string(), system_id: "receiver_sys".to_string(), ..Default::default() },
        ],
        connections: vec![Connection { from_instance: "sender".to_string(), from_port: "out".to_string(), to_instance: "receiver".to_string(), to_port: "in".to_string(), link_model: String::new() }],
        ..Default::default()
    };
    let systems = BTreeMap::from([(sender_sys.id.clone(), sender_sys), (receiver_sys.id.clone(), receiver_sys)]);
    let mut router = Router::build(&sos, &systems).expect("valid wiring");

    let log: Rc<RefCell<Vec<(i64, usize)>>> = Rc::new(RefCell::new(Vec::new()));
    let mut kernel = HeteroKernel::new(output_period_ns);
    kernel.register_system("sender", sender_period_ns, Box::new(PhysicalSignalSender::new("out".to_string())) as BoxedModel, 0, vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    kernel.register_system("receiver", receiver_period_ns, Box::new(SignalReceiverStepLog { log: log.clone() }) as BoxedModel, 0, vec![0.0]);

    kernel.run_with_ports(0, sender_period_ns, &mut router).expect("run succeeds");

    let received = log.borrow().clone();
    assert_eq!(received.len(), 3, "one entry per receiver native step: 100, 200, 300 ms");
    assert_eq!(
        received[0],
        (100_000_000, 0),
        "receiver's 1st step: the sender's message (available only at 300 ms) must not be visible yet, even though the sender's own atomic catch-up step -- computing that very message -- already ran earlier in this same output tick"
    );
    assert_eq!(received[1], (200_000_000, 0), "receiver's 2nd step: still short of the sender's 300 ms availability -- held, not delivered early");
    assert_eq!(received[2], (300_000_000, 1), "receiver's 3rd step: exactly t + P (300 ms) -- the first receiver step at or after availability, never before, counted by step");
}

/// **Required test (M16.1): order determinism holds when the catch-up loop itself iterates more
/// than once per output tick, for more than one sender at once.** Unlike
/// [`delivery_order_is_deterministic_and_matches_port_then_sender_and_repeats_identically`] and
/// [`delivery_order_is_deterministic_under_deferral_and_repeats_identically`] above (every system
/// in both shares the run's own output period, so `HeteroScheduler::advance_to_with_ports`'s
/// `loop { .. }` body never runs more than once per output tick for any of them), `"sender_a"`
/// (200 ms period) and `"sender_b"` (300 ms period) here are each multiple whole periods behind
/// the output clock the very first time either is ever advanced, forcing that same loop to
/// iterate repeatedly within a single `advance_to_with_ports` call, across two different systems
/// catching up by two different amounts. A non-deterministic ordering bug reachable only through
/// that repeated-iteration path (e.g. a `HashMap` sneaking into the catch-up loop's own min-
/// selection or system iteration, or an off-by-one in how many times a system tied for the
/// minimum gets stepped) would show up as a run-to-run mismatch here even though it cannot show up
/// in either sibling test above, where the loop body only ever runs once.
#[test]
fn delivery_order_is_deterministic_when_multiple_senders_catch_up_by_different_amounts() {
    let receiver_period_ns: i64 = 100_000_000;
    let output_period_ns: i64 = receiver_period_ns;
    let sender_a_period_ns: i64 = 200_000_000; // 2 periods behind the output clock on tick 1
    let sender_b_period_ns: i64 = 300_000_000; // 3 periods behind the output clock on tick 1
    let end_tai_ns: i64 = 600_000_000;

    let sender_a_sys = SystemDefinition { id: "sender_a_sys".to_string(), ports: vec![signal_port("beta", PortDirection::Out, 0)], ..Default::default() };
    let sender_b_sys = SystemDefinition { id: "sender_b_sys".to_string(), ports: vec![signal_port("alpha", PortDirection::Out, 0)], ..Default::default() };
    let receiver_sys = SystemDefinition {
        id: "receiver_sys".to_string(),
        ports: vec![signal_port("alpha", PortDirection::In, 0), signal_port("beta", PortDirection::In, 0)],
        ..Default::default()
    };
    let sos = SosConfiguration {
        instances: vec![
            SystemInstance { name: "sender_a".to_string(), system_id: "sender_a_sys".to_string(), ..Default::default() },
            SystemInstance { name: "sender_b".to_string(), system_id: "sender_b_sys".to_string(), ..Default::default() },
            SystemInstance { name: "receiver".to_string(), system_id: "receiver_sys".to_string(), ..Default::default() },
        ],
        connections: vec![
            Connection { from_instance: "sender_a".to_string(), from_port: "beta".to_string(), to_instance: "receiver".to_string(), to_port: "beta".to_string(), link_model: String::new() },
            Connection { from_instance: "sender_b".to_string(), from_port: "alpha".to_string(), to_instance: "receiver".to_string(), to_port: "alpha".to_string(), link_model: String::new() },
        ],
        ..Default::default()
    };
    let systems = BTreeMap::from([(sender_a_sys.id.clone(), sender_a_sys), (sender_b_sys.id.clone(), sender_b_sys), (receiver_sys.id.clone(), receiver_sys)]);

    let run = || -> Vec<(String, i64)> {
        let mut router = Router::build(&sos, &systems).expect("valid wiring");
        let log: Rc<RefCell<Vec<PortMessage>>> = Rc::new(RefCell::new(Vec::new()));
        let mut kernel = HeteroKernel::new(output_period_ns);
        kernel.register_system("sender_a", sender_a_period_ns, Box::new(PhysicalSignalSender::new("beta".to_string())) as BoxedModel, 0, vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        kernel.register_system("sender_b", sender_b_period_ns, Box::new(PhysicalSignalSender::new("alpha".to_string())) as BoxedModel, 0, vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        kernel.register_system("receiver", receiver_period_ns, Box::new(SignalReceiver { log: log.clone() }) as BoxedModel, 0, vec![0.0]);
        kernel.run_with_ports(0, end_tai_ns, &mut router).expect("run succeeds");
        let received: Vec<(String, i64)> = log.borrow().iter().map(|m| (m.port.clone(), m.tai_ns)).collect();
        received
    };

    let run1 = run();
    let run2 = run();
    assert_eq!(run1, run2, "the exact delivered sequence must be bit-identical across two runs, even though both senders are caught up multiple periods ahead of the output clock the first time either steps at all");
    assert!(
        run1.iter().any(|(p, _)| p == "alpha") && run1.iter().any(|(p, _)| p == "beta"),
        "both senders' own catch-up paths must actually have produced deliveries for the comparison above to be meaningful"
    );
}
