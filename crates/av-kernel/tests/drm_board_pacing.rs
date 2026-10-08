//! A `BINDING_KIND_BOARD` instance bound and paced end to end (question 242, hilprep-3a), **against
//! stand-ins only**: the real `av-edge-board` binary in front of a fake lockstep-local guest on a
//! loopback UDP socket. No board, no Renode, no Docker. The fake guest answers every STEP with an
//! empty STEP_DONE, so the board-bound controller is silent and the truth drifts (the stand-in
//! does not run the control law); what is proven is the binding, the Bind-time check, the
//! per-step protocol and the pacing, not any flight-software behaviour.
//!
//! Runs in both feature states of `av-kernel` (nothing here needs GMAT).
mod drm_board_common;

use std::time::{Duration, Instant};

use av_kernel::pacing::{is_overrun_event, OVERRUN_EVENT_ID_PREFIX};
use drm_board_common::*;

const ARC_S: i64 = 3;
const STEPS: u64 = 30;

fn overrun_events(products: &av_kernel::drm::RunProducts) -> Vec<(i64, f64)> {
    products.events.iter().filter(|e| is_overrun_event(e)).map(|e| (e.tai_ns, e.values["overrun_ns"])).collect()
}

/// The decisive end-to-end proof: the run is paced (wall time at least the arc), the report counts
/// the ticks, the guest saw BIND without the kernel-side `board.*` parameters and every STEP in
/// order, and the board instance reports `BINDING_KIND_BOARD` in its provenance.
#[test]
fn a_board_bound_run_is_paced_end_to_end_through_the_real_edge_service() {
    let _serial = serial();
    let guest = FakeGuest::spawn(GuestPlan::default());
    let device = udp_device(&guest);
    let service = Service::start("paced", &device, "zcu104-test");
    let scene = Scene::new(&device, "zcu104-test", &service.grpc_addr, &[], ARC_S, vec![]);

    let t0 = Instant::now();
    let products = scene.run().unwrap_or_else(|e| panic!("the board-bound run: {e}\nservice stderr:\n{}", service.stderr()));
    let wall = t0.elapsed();

    let pacing = products.pacing.as_ref().expect("a board-bound run carries a PacingReport");
    println!(
        "PACING wall={:?} ticks_paced={} overrun_count={} worst_overrun_ns={} total_overrun_ns={} worst_work_ns={} mean_work_ns={} final_lateness_ns={} base_period_ns={} forcing={:?}",
        wall, pacing.ticks_paced, pacing.overrun_count, pacing.worst_overrun_ns, pacing.total_overrun_ns, pacing.worst_work_ns, pacing.mean_work_ns, pacing.final_lateness_ns, pacing.base_period_ns, pacing.forcing_instances
    );
    assert_eq!(pacing.mode, av_cdm::pb::PacingMode::RealTime as i32);
    assert_eq!(pacing.forcing_instances, vec![CONTROLLER.to_string()]);
    assert_eq!(pacing.base_period_ns, PERIOD_NS);
    assert_eq!(pacing.ticks_paced, STEPS, "a {ARC_S} s arc at 10 Hz is {STEPS} scheduler ticks");
    assert!(wall >= Duration::from_secs(ARC_S as u64), "a paced run occupies at least its simulated duration, took {wall:?}");
    assert!(wall < Duration::from_secs(ARC_S as u64 + 3), "...and not much more (the fake answers at once): {wall:?}");
    assert_eq!(pacing.overrun_count as usize, overrun_events(&products).len(), "every overrun is one event");

    // The fake guest: HELLO once, BIND once, then every STEP in order, then SHUTDOWN.
    {
        let seen = guest.seen.lock().unwrap();
        assert_eq!(seen.step_sequences, (1..=STEPS).collect::<Vec<_>>(), "every step reached the guest, in order");
        let expect_until: Vec<i64> = (1..=STEPS as i64).map(|k| scene.start_tai_ns() + k * PERIOD_NS).collect();
        assert_eq!(seen.step_until_tai_ns, expect_until);
        assert_eq!(seen.binds.len(), 1);
        let bind = &seen.binds[0];
        println!("GUEST BIND parameters = {:?}", bind.parameters);
        assert!(bind.parameters.keys().all(|k| !k.starts_with("board.")), "the guest's BIND carries no board.* parameter: {:?}", bind.parameters);
        assert_eq!(bind.instance, CONTROLLER);
        assert_eq!(bind.seed, 42);
        assert_eq!(bind.step_period_ns, PERIOD_NS);
        assert_eq!(bind.ports.len(), 3, "the controller's three declared ports");
        assert_eq!(seen.frames.iter().filter(|f| **f == av_lockstep_shim::framing::FrameType::Hello).count(), 1, "HELLO once");
    }

    // The edge service did the Bind-time check (the kernel sent board.edge_node_id and
    // board.port_device) and stripped both before forwarding: an unchecked Bind is only a warning.
    let stderr = service.stderr();
    assert!(stderr.contains("Bind board parameters match this link"), "the service verified the kernel's board parameters:\n{stderr}");
    assert!(!stderr.contains("forwarding unchecked"), "{stderr}");

    // The board instance reports BINDING_KIND_BOARD and its own hashes, never the container's.
    let traj = products.trajectories.get(CONTROLLER).expect("the controller's trajectory");
    let attrs = &traj.provenance.as_ref().expect("provenance").attributes;
    assert_eq!(attrs.get("binding_kind").map(String::as_str), Some("BINDING_KIND_BOARD"));
    assert_eq!(attrs.get("board_binding_hash"), Some(&FAKE_BINDING_HASH_BYTE.repeat(32)));
    let link = av_edge::board::BoardLink::from_binding_checked(
        match scene.sos.instances.iter().find(|i| i.name == CONTROLLER).unwrap().binding.as_ref().unwrap().config.as_ref().unwrap() {
            av_cdm::pb::binding::Config::Board(b) => b,
            other => panic!("{other:?}"),
        },
        scene.systems["attitude_control_controller_board_sys"].ports.iter().map(|p| p.name.as_str()),
    )
    .unwrap();
    assert_eq!(attrs.get("board_link_hash"), Some(&link.config_hash_hex()));
    assert!(!attrs.contains_key("container_binding_hash"));
    assert_eq!(traj.segments[0].dynamics_model, "board.controller");
    assert_eq!(traj.segments[0].dynamics_depth, "board-lockstep");
    let others = products.trajectories.iter().filter(|(n, _)| n.as_str() != CONTROLLER);
    for (name, t) in others {
        let a = &t.provenance.as_ref().unwrap().attributes;
        assert!(!a.contains_key("binding_kind") && !a.contains_key("board_binding_hash"), "{name} is a model instance and reports no board attributes");
    }
    guest.finish();
}

/// A guest that answers two chosen steps 130 ms late (the period is 100 ms) makes exactly those
/// ticks overruns, counted with the right epochs and an `overrun_ns` near `delay - period` (30 ms;
/// the tolerance is -10 ms / +50 ms because the delay is measured through a loopback UDP hop, the
/// edge service and a gRPC call, which add 10-25 ms of scheduling jitter on this host: 62 and 73 ms
/// were seen at a 150 ms delay); the run still completes every step (no step skipped, none
/// repeated). The delay is chosen so the tick after a late one is not itself late: it is released
/// at once on finishing and is late only if the first overran by more than one period.
#[test]
fn late_steps_are_counted_as_overruns_at_their_epochs_and_the_run_completes_every_step() {
    let _serial = serial();
    const DELAY_MS: u64 = 130;
    let plan = GuestPlan { delay_for_step: [(5u64, Duration::from_millis(DELAY_MS)), (12, Duration::from_millis(DELAY_MS))].into_iter().collect(), ..GuestPlan::default() };
    let guest = FakeGuest::spawn(plan);
    let device = udp_device(&guest);
    let service = Service::start("overrun", &device, "zcu104-test");
    let scene = Scene::new(&device, "zcu104-test", &service.grpc_addr, &[], ARC_S, vec![]);

    let t0 = Instant::now();
    let products = scene.run().unwrap_or_else(|e| panic!("the run: {e}\nservice stderr:\n{}", service.stderr()));
    let wall = t0.elapsed();
    let pacing = products.pacing.as_ref().expect("PacingReport");
    let overruns = overrun_events(&products);
    println!("OVERRUNS wall={wall:?} count={} events={overruns:?} worst_overrun_ns={} worst_overrun_tai_ns={} total_overrun_ns={}", pacing.overrun_count, pacing.worst_overrun_ns, pacing.worst_overrun_tai_ns, pacing.total_overrun_ns);

    assert_eq!(pacing.ticks_paced, STEPS);
    assert_eq!(pacing.overrun_count, 2, "exactly the two delayed steps overran; events: {overruns:?}");
    let expected_epochs: Vec<i64> = [5i64, 12].iter().map(|k| scene.start_tai_ns() + k * PERIOD_NS).collect();
    assert_eq!(overruns.iter().map(|(t, _)| *t).collect::<Vec<_>>(), expected_epochs, "the overruns carry the epochs of the delayed ticks");
    let expected_overrun_ns = (DELAY_MS as f64 - 100.0) * 1e6;
    for (epoch, overrun_ns) in &overruns {
        assert!((expected_overrun_ns - 10e6..=expected_overrun_ns + 50e6).contains(overrun_ns), "tick at {epoch}: overrun {overrun_ns} ns, expected {expected_overrun_ns} ns, -10 ms / +50 ms");
    }
    assert!(overruns.iter().all(|(t, _)| products.events.iter().any(|e| e.tai_ns == *t && e.id == format!("{OVERRUN_EVENT_ID_PREFIX}{t}"))));
    assert!(expected_epochs.contains(&pacing.worst_overrun_tai_ns));
    // The run caught up without skipping a step: every one of the 30 steps reached the guest once, in order.
    assert_eq!(guest.seen.lock().unwrap().step_sequences, (1..=STEPS).collect::<Vec<_>>());
    // Two late answers (30 ms over their deadline) are caught up by the end: the run still ends on its schedule
    // (it holds to wall(T_end)), so it did not run long by the lateness.
    assert!(wall < Duration::from_secs(ARC_S as u64 + 3), "{wall:?}");
    guest.finish();
}
