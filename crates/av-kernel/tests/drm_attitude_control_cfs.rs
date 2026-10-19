//! M23.4 (`docs/sil-plan.md`'s M23 milestone: "cFS (pinned) on OSAL-posix in Docker with the
//! lockstep PSP/scheduler app and the reference ADCS app; loop closed and scored;
//! byte-identical at the port boundary"; `docs/open-questions.md` questions 143, 145, 146, 147,
//! 153, 154, 155): closes `drms/demo_attitude_control.*.yaml`'s attitude control loop with the
//! `"controller"` instance REBOUND from M22.4's native `crate::drm::controller::
//! AttitudeControllerModel` to `BINDING_KIND_CONTAINER`, running the real, compiled
//! `services/cfs/apps/adcs` inside the real `altavista-cfs-lockstep` image (fronted by
//! `crates/av-lockstep-shim` in the same container -- `services/cfs/container-entrypoint.sh`),
//! through [`av_kernel::drm::execute`] exactly like every other binding kind.
//!
//! **Everything else about the loop is unchanged from M22.4**
//! (`crates/av-kernel/tests/drm_attitude_control.rs`): the same truth plant
//! (`drms/demo_attitude_control_truth.system.yaml`, three reaction wheels, Jz=50, 0.2 rad
//! initial pointing error about +z), the same star tracker/IMU sensor models and their same
//! declared noise, the same CCSDS packet codecs (apid 200/201 in, 300 out --
//! `drms/demo_attitude_control_controller_cfs.system.yaml` copies them verbatim). The container-
//! bound `services/cfs/apps/adcs` control law (`adcs_control.c`) uses the identical gains
//! (`ADCS_CONTROLLER_KP_DEFAULT=0.25`, `ADCS_CONTROLLER_KD_DEFAULT=5.0`) and identical target
//! (`ADCS_QuatIdentity()`) as the native fixture's own `controller.kp`/`controller.kd`/
//! `controller.target_q` -- chosen deliberately so this is a fair settling-behaviour comparison,
//! not two different controllers.
//!
//! **Why this test scores pointing error from *truth*, not `output.controller.pointing_error_
//! rad`.** `services/cfs/apps/io_lockstep` never populates `LockstepStepResponse.named_outputs`
//! (`lockstep_messages.h`'s own doc comment) -- there is no C-side scalar to read the way M22.4's
//! native model exposes one. This test instead reads
//! `drms/demo_attitude_control_truth.system.yaml`'s own declared `q_x`/`q_y`/`q_z`/`q_w` state
//! components directly off `RunProducts.trajectories["attitude"]` (mean[0..4]) -- the *physical*
//! pointing error, unaffected by which binding closes the loop, and (being noise-free, unlike
//! the star tracker's own measured value M22.4's own 1.2579e-4 rad figure reports) a strictly
//! *tighter* signal than the native fixture's own noise-dominated measured residual. Since the
//! motion stays (by construction, see `drms/demo_attitude_control.drm.yaml`'s own header
//! derivation) a single-axis rotation about z with q_x=q_y~=0 throughout,
//! `pointing_error_rad = 2*asin(sqrt(qx^2+qy^2+qz^2))` (the exact formula
//! `crate::drm::controller`'s own `pointing_error_rad` output uses, `AttitudeControllerModel`'s
//! `2.0 * vnorm.asin()`) is computed directly in this test's own Rust code -- see
//! [`truth_pointing_error_rad_at_end`].
//!
//! **Why the scenario is shorter than M22.4's 300 s.** M22.4's native run is GMAT-vectorized, no
//! process boundary per step. This run pays one real Docker container `Step` gRPC round trip
//! (kernel -> shim -> Unix socket -> cFS software bus -> ADCS control law -> back) per
//! `default_step_rate_hz` tick -- measured during authoring at low tens of milliseconds per
//! step, real OS task scheduling inside the container, not a network cost this task can remove.
//! 30 s at the fixture's own 10 Hz (300 steps) is chosen: long enough (1.5*tau) to show
//! substantial, unambiguous convergence and to compare cleanly against the closed form, short
//! enough that this test (and the two more full runs [`byte_identical_...`] needs, plus the
//! fault-exercise run) finishes in well under this repository's own tool timeouts.
//!
//! **Expected settling behaviour, stated BEFORE running** (the closed form is
//! `drms/demo_attitude_control.drm.yaml`'s own derivation, unchanged: critically damped,
//! `tau = 2*Jz/kd = 20 s`, `theta(t)/theta(0) = (1 + t/tau)*exp(-t/tau)`): at t=30s=1.5*tau,
//! `theta(30)/theta(0) = 2.5*exp(-1.5) = 0.55783...`, i.e. `theta(30) ~= 0.2 * 0.55783 =
//! 0.111566 rad` from the truth-based, noise-free deterministic transient alone -- see
//! [`expected_theta_ratio`] (the exact function `drm_attitude_control.rs` already uses,
//! independently re-derived here against the *native* run's own truth trajectory, below, so
//! this comparison is not a single hard-coded number this test could get wrong twice the same
//! way).
//!
//! **Expected effect of the container's one-step delivery lag, stated BEFORE measuring.** Per
//! `services/cfs/README.md`'s own protocol account, a `Step(until_tai_ns)` call delivers this
//! step's fresh sensor packets onto cFS's software bus and releases exactly one scheduler tick;
//! `services/cfs/apps/sch_lockstep`'s wakeup fires once per tick, driving one ADCS control-law
//! evaluation per step using whatever the software bus already holds at that moment -- so the
//! wheel-torque command ADCS computes and returns *within* a given `Step` call is commanded
//! using the star tracker/IMU packets *delivered in that same call*, not a stale prior value:
//! there is no extra full-step lag beyond the ordinary zero-order-hold discretization every
//! lockstep-driven control loop already has (the native fixture's own header comment already
//! discloses a "few percent" ZOH effect by t=100s). Expected consequence: the container run's
//! truth-based pointing error at t=30s should track the native run's own truth-based pointing
//! error at t=30s closely -- within the same handful-of-percent ZOH-and-discretization budget
//! the native fixture already discloses for itself, not an order of magnitude apart. See
//! [`the_cfs_bound_loop_settles_and_tracks_the_native_run`] for the measured comparison.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use av_cdm::pb::{Binding, BindingKind, ContainerBinding, DesignReferenceMission, EventKind, Fault, FaultTargetKind, PortDirection, PortTrafficLog, Provenance, SosConfiguration, SystemDefinition};
use prost::Message;
use av_kernel::drm::replay::ReplayConfig;
use av_kernel::drm::{execute, hash, schema, RunConfig, RunProducts};
use av_lockstep::docker::{lock_docker_tests, prune_stale_test_resources, test_run_id, DockerTestLock};
use gmat_sys::Gmat;

// The fixture loaders, the Docker plumbing (throwaway registry on the fixed port, guards, the
// `AV_CFS_TEST_IMAGE` override and its gate) and the truth pointing-error helpers are shared with
// `drm_attitude_control_renode.rs`: see `tests/cfs_common/mod.rs`.
mod cfs_common;
use cfs_common::{container_drm, load_system, push_cfs_image_to_local_registry, read, sha256_hex, skip_if_cfs_image_unavailable, truth_last_mean, truth_pointing_error_rad, TRUTH_QW, TRUTH_QZ};

/// Loads the M22.4 native bundle's truth/startracker/imu `SystemDefinition`s (byte-identical to
/// `drm_attitude_control.rs`'s own fixtures -- this test does not fork them) plus this task's
/// own cFS-bound controller `SystemDefinition`
/// (`drms/demo_attitude_control_controller_cfs.system.yaml` -- ports/packet_codecs copied
/// verbatim from the native one, `container.*` parameters instead of `controller.*`), and the
/// native `SosConfiguration` (`demo_attitude_control.sos.yaml`) as the topology template
/// [`container_sos`] rebinds `"controller"` from.
fn load_systems_and_base_sos() -> (BTreeMap<String, SystemDefinition>, SosConfiguration) {
    let truth = load_system("demo_attitude_control_truth");
    let star = load_system("demo_attitude_control_startracker");
    let imu = load_system("demo_attitude_control_imu");
    let controller_cfs = load_system("demo_attitude_control_controller_cfs");
    let mut systems = BTreeMap::new();
    systems.insert(truth.id.clone(), truth);
    systems.insert(star.id.clone(), star);
    systems.insert(imu.id.clone(), imu);
    systems.insert(controller_cfs.id.clone(), controller_cfs);

    let base_sos = schema::parse_sos_yaml(&read("demo_attitude_control.sos.yaml")).expect("native SosConfiguration parses");
    (systems, base_sos)
}

/// Rebinds `base_sos`'s `"controller"` instance from `BINDING_KIND_MODEL` (M22.4's native
/// `AttitudeControllerModel`) to `BINDING_KIND_CONTAINER` (this task's own `ContainerBinding`,
/// image pulled by digest) -- the exit criterion's own framing: "the controller instance
/// rebound to `BINDING_KIND_CONTAINER`." Every connection/every other instance is untouched
/// (`base_sos.clone()`), so the physical topology genuinely is the same DRM, not a rebuild.
fn container_sos(id: &str, base_sos: &SosConfiguration, image: &str, image_digest: &str) -> SosConfiguration {
    let mut sos = base_sos.clone();
    sos.id = id.to_string();
    sos.name = format!("{} (M23.4: controller rebound to BINDING_KIND_CONTAINER)", base_sos.name);
    let mut found = false;
    for inst in sos.instances.iter_mut() {
        if inst.name == "controller" {
            found = true;
            inst.system_id = "attitude_control_controller_cfs_sys".to_string();
            inst.step_rate_hz = 10.0;
            inst.binding = Some(Binding {
                kind: BindingKind::Container as i32,
                config: Some(av_cdm::pb::binding::Config::Container(ContainerBinding { image: image.to_string(), image_digest: image_digest.to_string(), ..Default::default() })),
            });
        }
    }
    assert!(found, "demo_attitude_control.sos.yaml must declare a \"controller\" instance to rebind");
    sos.hash = hash::canonical_sos_hash(&sos);
    sos
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default() , products_dir: None, replay: None, command_source: None }
}

/// Question 207/156, ported from `drm_attitude_control_renode.rs`'s own `run_byte_identical_
/// port_traffic_between_posix_container_and_renode` preamble (`let _lock = lock_docker_tests();
/// prune_stale_test_resources(&_lock); let run_id = test_run_id();`) -- same three calls, same
/// order (acquire the host-wide `flock` first, sweep whatever a previous, interrupted run left
/// behind BEFORE creating anything, then mint this run's own id). Differs from that sibling file
/// only in being factored into one small helper: that file has exactly one Docker-gated test
/// body to thread this preamble into, this file has FOUR
/// ([`run_the_cfs_bound_loop_settles_and_tracks_the_native_run`],
/// [`run_byte_identical_run_products_across_two_separately_spawned_cfs_containers`],
/// [`run_byte_identical_products_when_the_container_bound_controller_is_replayed_docker_free`],
/// [`run_a_power_cycle_hardware_fault_on_the_container_bound_controller_is_accepted_and_the_run_continues`]),
/// and repeating this exact three-line sequence (plus its rationale) four times over would just
/// be copy-paste, not a different pattern. The lock is still held for each calling test's own
/// WHOLE body, not just this call: returning `DockerTestLock` (not merely dropping it here) lets
/// the caller bind it to a `let _lock = ...` that lives until that `run_...` function returns --
/// `prune_stale_test_resources` deliberately takes `&DockerTestLock` rather than acquiring one
/// itself (see that function's own doc comment on why: a second, independently-opened `flock` in
/// the same process would deadlock against a lock the caller already holds) specifically so a
/// caller can do exactly this. Each of this file's four `#[test]` wrappers calls
/// `skip_if_cfs_image_unavailable` and returns BEFORE ever calling into its own `run_...`
/// function, and this helper is only ever called from inside a `run_...` function (never from a
/// `#[test]` wrapper directly) -- so a skipped test never reaches `lock_docker_tests()` at all,
/// and never serialises against another track's own Docker-gated gates for no reason.
fn lock_and_prune_docker_tests() -> (DockerTestLock, String) {
    let lock = lock_docker_tests();
    prune_stale_test_resources(&lock);
    let run_id = test_run_id();
    (lock, run_id)
}

// ------------------------------------------------------------------------------------------
// Truth-based pointing error (this file's own module doc comment explains why truth, not
// output.controller.pointing_error_rad).
// ------------------------------------------------------------------------------------------

fn truth_pointing_error_rad_at_end(products: &RunProducts) -> f64 {
    let mean = truth_last_mean(products);
    // Internal cross-check, not the number this test scores on: `2*acos(|q_w|)` recovers the
    // same rotation angle from the SCALAR quaternion component (a real unit quaternion has
    // qx^2+qy^2+qz^2+qw^2=1, so asin(|qv|) and acos(|qw|) agree exactly) -- confirms
    // truth_pointing_error_rad's own `asin`-based formula is reading a genuine unit quaternion,
    // not, say, an accidentally transposed state vector where this cross-check would fail.
    let from_asin = truth_pointing_error_rad(mean);
    let from_acos = 2.0 * mean[TRUTH_QW].clamp(-1.0, 1.0).acos();
    assert!((from_asin - from_acos).abs() < 1e-9, "asin- and acos-derived pointing error must agree for a genuine unit quaternion: {from_asin} vs {from_acos} (mean={:?})", mean);
    from_asin
}
fn truth_qz_series(products: &RunProducts) -> Vec<(i64, f64)> {
    let traj = products.trajectories.get("attitude").expect("the \"attitude\" truth instance produced a trajectory");
    traj.samples.iter().map(|s| (s.tai_ns, s.mean[TRUTH_QZ])).collect()
}

/// The closed-form `theta(t)/theta(0)` ratio for a critically damped (`zeta = 1`) second-order
/// system released from rest -- `drms/demo_attitude_control.drm.yaml`'s own derivation,
/// identically re-derived here (not imported from `drm_attitude_control.rs`, a `tests/` binary
/// this crate does not expose as a library) so a change to that file's own copy cannot silently
/// desynchronize from what this file checks against.
const TAU_S: f64 = 20.0;
fn expected_theta_ratio(t_s: f64) -> f64 {
    (1.0 + t_s / TAU_S) * (-t_s / TAU_S).exp()
}
const THETA0_RAD: f64 = 0.2;

// ------------------------------------------------------------------------------------------
// 1. The pointing objective: expected behaviour stated above (module doc comment), measured
//    against both the closed form and a same-duration NATIVE (BINDING_KIND_MODEL) run.
// ------------------------------------------------------------------------------------------

const COMPARISON_DURATION_S: i64 = 30;

#[test]
fn the_cfs_bound_loop_settles_and_tracks_the_native_run() {
    if skip_if_cfs_image_unavailable("the_cfs_bound_loop_settles_and_tracks_the_native_run") {
        return;
    }
    run_the_cfs_bound_loop_settles_and_tracks_the_native_run();
}

fn run_the_cfs_bound_loop_settles_and_tracks_the_native_run() {
    let _engine = gmat_sys::engine_lock();
    // Question 207: held for this whole test body (see lock_and_prune_docker_tests's own doc
    // comment) -- a different worktree's own docker-gated cargo test/pytest process racing this
    // daemon-wide prune sweep is exactly what round 3's own gate measured failing elsewhere in
    // this workspace.
    let (_lock, run_id) = lock_and_prune_docker_tests();
    let (systems, base_sos) = load_systems_and_base_sos();

    // --- Native (BINDING_KIND_MODEL) run over the SAME 30 s window, truth-scored the same way,
    //     for a genuine apples-to-apples comparison (never the pre-existing 300 s/1.2579e-4 rad
    //     number, which is a *measured*, star-tracker-noise-dominated value over a different
    //     duration -- see this file's own module doc comment for why truth is used instead).
    //     `base_sos` (unmodified) still names M22.4's own native controller system_id
    //     ("attitude_control_controller_sys"), which `load_systems_and_base_sos` does not load
    //     (it only loads this task's own cFS-bound "..._cfs_sys" variant) -- loaded here,
    //     separately, into its own systems map so the container run below is unaffected.
    let native_controller = load_system("demo_attitude_control_controller");
    let mut native_systems = systems.clone();
    native_systems.insert(native_controller.id.clone(), native_controller);
    let native_sos = { let mut s = base_sos.clone(); s.hash = hash::canonical_sos_hash(&s); s };
    let native_drm = container_drm("attitude_control_native_30s_drm", &native_sos.id, COMPARISON_DURATION_S, vec![]);
    let gmat_native = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let native_products = execute(run_config(&gmat_native, &native_drm, &native_sos, &native_systems, "test-run-native-30s")).expect("the native 30s run must execute");
    let native_theta = truth_pointing_error_rad_at_end(&native_products);

    let expected_ratio = expected_theta_ratio(COMPARISON_DURATION_S as f64);
    let expected_theta = THETA0_RAD * expected_ratio;
    println!("expected theta({COMPARISON_DURATION_S}s) = {expected_theta:.6} rad (closed form); native truth-based measured = {native_theta:.6} rad");
    // The native run's own truth trajectory should track the closed form tightly (no sensor
    // noise in this path at all -- truth is exact physics) modulo the kernel's own zero-order-
    // hold discretization of the commanded torque at 10 Hz -- the native fixture's header
    // comment discloses "a few percent" by t=100s; at t=30s (an earlier, less-decayed point)
    // this bounds it generously at 10%.
    assert!((native_theta - expected_theta).abs() / expected_theta < 0.10, "native truth-based theta({COMPARISON_DURATION_S}s)={native_theta} rad must be within 10% of the closed-form prediction {expected_theta} rad");

    // --- Container (BINDING_KIND_CONTAINER) run: the same DRM, the "controller" instance
    //     rebound. Exit criterion 2/3.
    let (image, digest, _registry_guards) = push_cfs_image_to_local_registry(&run_id);
    let cfs_sos = container_sos("attitude_control_cfs_30s_sos", &base_sos, &image, &digest);
    let cfs_drm = container_drm("attitude_control_cfs_30s_drm", &cfs_sos.id, COMPARISON_DURATION_S, vec![]);
    let gmat_cfs = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfs_products = execute(run_config(&gmat_cfs, &cfs_drm, &cfs_sos, &systems, "test-run-cfs-30s")).expect("the container-bound 30s run must execute end to end through the real cFS image");
    let cfs_theta = truth_pointing_error_rad_at_end(&cfs_products);

    println!("measured: native theta({COMPARISON_DURATION_S}s)={native_theta:.6} rad, cFS-bound theta({COMPARISON_DURATION_S}s)={cfs_theta:.6} rad, |diff|={:.6} rad", (cfs_theta - native_theta).abs());

    // Exit criterion: "the pointing objective met" -- the loop actually closed and converged,
    // not merely ran. At t=30s=1.5*tau the closed form itself only predicts a ~1.79x reduction
    // (expected_theta_ratio(30) = 0.5578, computed above as `expected_theta`) -- nowhere near an
    // order of magnitude yet (that takes several more tau) -- so the bound below asks for a
    // reduction consistent with that same closed form (at least 1.5x, comfortably under the
    // ~1.79x this run should actually show), not an arbitrary rounder number that would fail a
    // correctly-working controller at this early a checkpoint. A control law that stopped
    // correcting, or a sign error, still fails this outright; so does a container returning a
    // hardcoded/zeroed command (the separate exactly-zero check below).
    assert!(cfs_theta.abs() < THETA0_RAD / 1.5, "the cFS-bound loop's pointing error must have decreased by at least 1.5x from the initial {THETA0_RAD} rad (closed form predicts ~1.79x by t={COMPARISON_DURATION_S}s); got {cfs_theta} rad");
    assert!(cfs_theta.abs() > 1e-9, "the cFS-bound loop's pointing error must not be exactly zero -- a real, physically propagated plant under a real (if perfect) control law never lands on exactly zero");

    // Expected difference from the one-step-delivery-lag question, stated BEFORE this
    // assertion (this file's own module doc comment): the container path's per-step ADCS
    // evaluation uses the same-step delivered packets (no extra full-step lag beyond the
    // shared zero-order-hold every lockstep loop already has), so the two runs' truth-based
    // thetas at the identical t=30s should agree closely -- bounded at 15% of the native value
    // (looser than the native-vs-closed-form 10% bound above, to absorb the container's own
    // additional, real discretization: cFS's own tick/step boundary is a real OS scheduling
    // event, not a mathematically exact instant the way the native in-process path's is).
    let relative_diff = (cfs_theta - native_theta).abs() / native_theta.abs();
    assert!(relative_diff < 0.15, "cFS-bound theta({COMPARISON_DURATION_S}s)={cfs_theta} rad must be within 15% of the native run's own theta({COMPARISON_DURATION_S}s)={native_theta} rad (both truth-based, same DRM, same gains) -- got a {:.1}% relative difference", relative_diff * 100.0);

    // No overshoot: q_z should be monotonically decreasing in magnitude once past the initial
    // transient's own possible small nonlinearity -- checked coarsely (every sample no larger
    // in magnitude than the very first) rather than strictly monotonic, matching this fixture's
    // own critically-damped-so-no-overshoot design intent without over-fitting to ZOH ripple.
    let series = truth_qz_series(&cfs_products);
    let q0 = series.first().expect("at least one sample").1.abs();
    for (t, qz) in &series {
        assert!(qz.abs() <= q0 + 1e-6, "q_z must never exceed its own initial magnitude {q0} (no overshoot expected, critically damped) -- got {qz} at t_ns={t}");
    }
}

// ------------------------------------------------------------------------------------------
// 2. Question 145: byte-identical RunProducts across two runs at the port boundary.
//    "Port-boundary determinism is the bar -- FSW internal thread order is recorded in the run
//    log, not asserted" (this task's own brief) -- the assertion below is on
//    RunProducts.trajectories/events/scores/provenance, exactly the M13.2
//    byte_identical_run_products_across_two_runs test's own shape
//    (crates/av-kernel/tests/drm_container.rs), never on anything internal to the container
//    (cFE task scheduling order is not observed by this test at all).
// ------------------------------------------------------------------------------------------

const DETERMINISM_DURATION_S: i64 = 10;

#[test]
fn byte_identical_run_products_across_two_separately_spawned_cfs_containers() {
    if skip_if_cfs_image_unavailable("byte_identical_run_products_across_two_separately_spawned_cfs_containers") {
        return;
    }
    run_byte_identical_run_products_across_two_separately_spawned_cfs_containers();
}

/// **What this fails against.** Any nondeterminism at the port boundary -- a stray timestamp,
/// an uninitialized buffer byte leaking into a packet, a race in how `io_lockstep` orders
/// `named_outputs`/port replies -- shows up as `products_a != products_b` even though both runs
/// use the identical DRM/SosConfiguration/SystemDefinition (down to the image digest) against
/// two independently `docker run`-started containers (a fresh shim and a fresh cFS process
/// each time, per `services/cfs/README.md`'s own "one shim process serves exactly one peer
/// connection" contract -- this is not the same process answering twice).
///
/// **Nothing is excluded.** The `"controller"` segment's `dynamics_hash` is compared too: the
/// Docker path's `ModelInfo::settings_hash` (`binding::container_model_info`) hashes `image`,
/// `image_digest`, `tls` and the Bind `version`, and not `127.0.0.1:<ephemeral host port>`, so two
/// containers started on two different host ports must hash equal (question 245 (7)). Both values
/// are printed. Every observable byte -- `"attitude"`'s and `"imu"`'s trajectories (samples and
/// the full `port_command:attitude:wheel_torque_in:<t>` event-id list), `products.events` and
/// `.scores` -- is compared unmodified below.
fn run_byte_identical_run_products_across_two_separately_spawned_cfs_containers() {
    let _engine = gmat_sys::engine_lock();
    // Question 207: held for this whole test body -- see lock_and_prune_docker_tests's own doc
    // comment.
    let (_lock, run_id) = lock_and_prune_docker_tests();
    let (systems, base_sos) = load_systems_and_base_sos();
    let (image, digest, _registry_guards) = push_cfs_image_to_local_registry(&run_id);
    let sos = container_sos("attitude_control_cfs_det_sos", &base_sos, &image, &digest);
    let drm = container_drm("attitude_control_cfs_det_drm", &sos.id, DETERMINISM_DURATION_S, vec![]);

    let gmat_a = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products_a = execute(run_config(&gmat_a, &drm, &sos, &systems, "test-run-cfs-det")).expect("run A must execute end to end against a real cFS container");

    let gmat_b = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products_b = execute(run_config(&gmat_b, &drm, &sos, &systems, "test-run-cfs-det")).expect("run B (a separately spawned container) must also execute end to end");

    assert_eq!(products_a.trajectories.keys().collect::<Vec<_>>(), products_b.trajectories.keys().collect::<Vec<_>>(), "the same entities must be present in both runs");
    for (name, traj_a) in &products_a.trajectories {
        let traj_b = &products_b.trajectories[name];
        assert_eq!(traj_a.samples, traj_b.samples, "instance {name:?}: samples must be byte-identical across two independently spawned cFS containers given the identical Bind/Step sequence");
        assert_eq!(traj_a.event_ids, traj_b.event_ids, "instance {name:?}: event_ids must be byte-identical");
        assert_eq!(traj_a.state_space_id, traj_b.state_space_id);
        assert_eq!(traj_a.segments.len(), traj_b.segments.len(), "instance {name:?}: segment count must match");
        for (seg_a, seg_b) in traj_a.segments.iter().zip(traj_b.segments.iter()) {
            assert_eq!(seg_a.start_tai_ns, seg_b.start_tai_ns);
            assert_eq!(seg_a.end_tai_ns, seg_b.end_tai_ns);
            assert_eq!(seg_a.dynamics_model, seg_b.dynamics_model);
            assert_eq!(seg_a.dynamics_depth, seg_b.dynamics_depth);
            if name == "controller" {
                println!("controller dynamics_hash: run A {} / run B {}", seg_a.dynamics_hash, seg_b.dynamics_hash);
                assert!(!seg_a.dynamics_hash.is_empty(), "the container segment must carry a dynamics_hash");
            }
            assert_eq!(seg_a.dynamics_hash, seg_b.dynamics_hash, "instance {name:?}: dynamics_hash must not depend on the ephemeral Docker host port");
        }
    }
    assert_eq!(products_a.events, products_b.events);
    assert_eq!(products_a.scores, products_b.scores);
    assert_eq!(products_a.provenance, products_b.provenance);
}

// ------------------------------------------------------------------------------------------
// T4 (M25.4b, question 175's own follow-on): replaying the container-bound "controller" is
// Docker-free -- record one real run against the cFS container above, then replay it from
// nothing but the recorded PortTrafficLog, and compare.
// ------------------------------------------------------------------------------------------

/// A fresh, empty scratch directory under the OS temp dir, unique per call within this process
/// (mirrors `tests/port_traffic_sidecar.rs::scratch_dir`'s own convention).
fn replay_scratch_dir(label: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("av-kernel-cfs-replay-test-{}-{label}-{n}", std::process::id()));
    assert!(!dir.exists(), "scratch dir {dir:?} must not already exist");
    dir
}

/// `a == b`, except `attributes[key]` is allowed to differ (and be present in only one of the
/// two) -- used below for exactly one key, `"container_binding_hash"`, on exactly one instance's
/// own `Trajectory.provenance` (see [`run_byte_identical_products_when_the_container_bound_
/// controller_is_replayed_docker_free`]'s own doc comment for why that one key cannot survive a
/// Docker-free replay).
fn provenance_matches_except_attribute(a: &Provenance, b: &Provenance, excluded_key: &str) -> bool {
    let mut a2 = a.clone();
    let mut b2 = b.clone();
    a2.attributes.remove(excluded_key);
    b2.attributes.remove(excluded_key);
    a2 == b2
}

const REPLAY_DURATION_S: i64 = 10;

#[test]
fn byte_identical_products_when_the_container_bound_controller_is_replayed_docker_free() {
    if skip_if_cfs_image_unavailable("byte_identical_products_when_the_container_bound_controller_is_replayed_docker_free") {
        return;
    }
    run_byte_identical_products_when_the_container_bound_controller_is_replayed_docker_free();
}

/// **Hypothesis, stated before running:** recording one real run against the cFS container,
/// then replaying `"controller"` (`RunConfig.replay.instances` left EMPTY -- question 175's own
/// "empty means every `BINDING_KIND_CONTAINER` instance" default, exercised here rather than
/// naming `"controller"` explicitly, so this test also proves that default resolves correctly
/// against a real container-bound `SosConfiguration`) from nothing but the recorded
/// `PortTrafficLog`, produces a second `RunProducts` matching the first -- with one
/// exclusion, disclosed here rather than silently dropped:
///
/// `"controller"`'s `Trajectory.provenance.attributes["container_binding_hash"]`: a Docker-free
/// replay never makes the `Bind` call, and the port-traffic log records the Bind `version`
/// (`container_bind_version.<instance>`, question 254 item 8) but not the binding hash. The
/// segment's `dynamics_hash`/`.dynamics_model`/`.dynamics_depth` are NOT excluded any more: they
/// are rebuilt from that version and compared.
///
/// **Nothing else is excluded.** `"attitude"` and `"imu"` are real, unreplayed, deterministic
/// native instances in both runs (identical seeds, identical wheel-torque commands received --
/// the recorded, then replayed, `wheel_torque_out` frames are byte-identical by construction),
/// so their own trajectories, and every `Event`/`Score`, are expected to match exactly.
fn run_byte_identical_products_when_the_container_bound_controller_is_replayed_docker_free() {
    let _engine = gmat_sys::engine_lock();
    // Question 207: held for this whole test body -- see lock_and_prune_docker_tests's own doc
    // comment. Held across BOTH runs below (the real container run and the Docker-free replay),
    // not just the first -- the replay half touches no Docker at all, but releasing the lock
    // between the two runs would defeat the point: another track's prune sweep could race the
    // guards (_registry_guards) that stay alive until this whole function returns.
    let (_lock, run_id) = lock_and_prune_docker_tests();
    let (systems, base_sos) = load_systems_and_base_sos();
    let (image, digest, _registry_guards) = push_cfs_image_to_local_registry(&run_id);
    let sos = container_sos("attitude_control_cfs_replay_sos", &base_sos, &image, &digest);
    let drm = container_drm("attitude_control_cfs_replay_drm", &sos.id, REPLAY_DURATION_S, vec![]);

    let dir = replay_scratch_dir("t4");
    let run_id = "test-run-cfs-replay".to_string();

    // Run 1: the real container. This is the ONLY run in this test that touches Docker at all.
    let gmat_real = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg_real = RunConfig { gmat: &gmat_real, drm: &drm, sos: &sos, systems: &systems, run_id: run_id.clone(), error_mode: Default::default(), products_dir: Some(dir.clone()), replay: None, command_source: None };
    let run_real = execute(cfg_real).expect("real container run must execute end to end");
    assert!(!run_real.port_traffic_hash.is_empty(), "sanity: a sidecar was actually recorded");

    // Run 2: replay, from the recorded log alone. No `_registry_guards`/container/image
    // plumbing is created for this call -- if this run needed Docker, it would have nothing to
    // reach (the registry/image guards above go out of scope only at the very end of this
    // function, but nothing in this second `execute()` call ever touches `image`/`digest`
    // again: `sos`/`drm` are the SAME values, byte-identical `ContainerBinding.image`/
    // `.image_digest` fields included, but `RunConfig.replay` is what makes `execute()` never
    // dial them for the "controller" instance this run).
    let replay_cfg = ReplayConfig { log_path: dir.join("port_traffic.pb"), expected_hash: run_real.port_traffic_hash.clone(), instances: vec![] };
    let gmat_replay = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg_replay = RunConfig { gmat: &gmat_replay, drm: &drm, sos: &sos, systems: &systems, run_id, error_mode: Default::default(), products_dir: None, replay: Some(replay_cfg), command_source: None };
    let run_replayed = execute(cfg_replay).expect("replay run must execute end to end, Docker-free");

    // The declared binding kind is unaffected by replay (question 175's own "the binding kind
    // in the artifact stays as declared") -- pinned directly against the loaded
    // SosConfiguration, not anything a run produced.
    let controller_instance = sos.instances.iter().find(|i| i.name == "controller").expect("controller instance exists");
    assert_eq!(controller_instance.binding.as_ref().unwrap().kind, BindingKind::Container as i32, "the replayed controller instance's own SosConfiguration entry must still declare BINDING_KIND_CONTAINER");

    assert_eq!(run_real.trajectories.keys().collect::<Vec<_>>(), run_replayed.trajectories.keys().collect::<Vec<_>>(), "the same entities must be present in both runs");
    for (name, traj_real) in &run_real.trajectories {
        let traj_replayed = &run_replayed.trajectories[name];
        assert_eq!(traj_real.samples, traj_replayed.samples, "instance {name:?}: samples must be byte-identical");
        assert_eq!(traj_real.event_ids, traj_replayed.event_ids, "instance {name:?}: event_ids must be byte-identical");
        assert_eq!(traj_real.state_space_id, traj_replayed.state_space_id);
        assert_eq!(traj_real.segments.len(), traj_replayed.segments.len(), "instance {name:?}: segment count must match");
        for (seg_real, seg_replayed) in traj_real.segments.iter().zip(traj_replayed.segments.iter()) {
            assert_eq!(seg_real.start_tai_ns, seg_replayed.start_tai_ns);
            assert_eq!(seg_real.end_tai_ns, seg_replayed.end_tai_ns);
            // Nothing excluded (question 254 item 8): the sidecar records the container's Bind version.
            assert_eq!(seg_real.dynamics_model, seg_replayed.dynamics_model, "instance {name:?}");
            assert_eq!(seg_real.dynamics_hash, seg_replayed.dynamics_hash, "instance {name:?}");
            assert_eq!(seg_real.dynamics_depth, seg_replayed.dynamics_depth, "instance {name:?}");
            if name == "controller" {
                assert!(!seg_real.dynamics_hash.is_empty(), "the container segment carries a dynamics_hash");
            }
        }
        if name == "controller" {
            let (Some(prov_real), Some(prov_replayed)) = (&traj_real.provenance, &traj_replayed.provenance) else {
                panic!("instance {name:?}: both runs must set Trajectory.provenance");
            };
            assert!(
                provenance_matches_except_attribute(prov_real, prov_replayed, "container_binding_hash"),
                "instance {name:?}: provenance must match except container_binding_hash (see this test's own doc comment) -- real={prov_real:?} replayed={prov_replayed:?}"
            );
        } else {
            assert_eq!(traj_real.provenance, traj_replayed.provenance, "instance {name:?}");
        }
    }
    assert_eq!(run_real.events, run_replayed.events, "no event should be missing or extra: the container instance never reports its own AppliedCommand or CDM Measurement either way (ContainerModel::step_with_ports's own Vec::new() -- see crates/av-kernel/src/drm/binding.rs), so replay has nothing model-internal to fail to reproduce here, unlike the native controller this task's own module doc comment (tests/replay.rs) explains");
    assert_eq!(run_real.scores, run_replayed.scores, "both empty: container_drm clears objectives/measures the same way the sibling determinism test's own container_drm call does");
    assert_eq!(run_real.dropped_in_flight_messages, run_replayed.dropped_in_flight_messages);

    std::fs::remove_dir_all(&dir).ok();
}

// ------------------------------------------------------------------------------------------
// 3. The power-cycle fault exercised once (FAULT_TARGET_KIND_HARDWARE, kind="power_cycle").
//
// Honesty note (this task's own "no test was orphaned, no wrong-implementation-catching claim
// overstated" standard): `crate::drm::executor::run_shared_group`'s existing, already-tested
// HARDWARE/power_cycle path (crates/av-kernel/tests/drm_container.rs) calls
// `ContainerModel::reset(boundary, format!("fault:{}", f.id))` -- `lockstep.proto`'s own
// `LockstepResetRequest.reason` doc comment names THREE valid forms ("power_cycle", "watchdog",
// "fault:<fault id>"), and a fault-driven reset has always used the third form, not the literal
// string "power_cycle" (that literal is the FAULT's own `kind`, not the wire `reason`). This
// test exercises exactly that existing, unchanged path against the real container (not a bare
// subprocess) and asserts what is actually true: `io_lockstep`'s own `handle_reset`
// (fsw/src/io_lockstep_app.c) re-arms `psp_lockstep_init(req.tai_ns)` and ACKs with the
// request's own echoed sequence -- proven here by the run continuing to completion past the
// fault with no protocol error -- and a FAULT event is recorded. `services/cfs/apps/adcs`
// itself has NO reset handler (`io_lockstep` never forwards RESET onto the software bus -- only
// this app's own PSP clock bookkeeping is affected), so this test does NOT claim (and would be
// wrong to claim) that ADCS's own cached target/measurements are cleared by this fault the way
// the M13.2 lockstep-ref fixture's own accumulated "integral" genuinely zeroes on Reset
// (crates/av-kernel/tests/drm_container.rs's own `a_power_cycle_fault_on_a_container_instance_
// resets_the_integrator_and_the_run_continues`) -- there is no ADCS-side accumulator to reset in
// the first place. That gap (ADCS has no reset handler at all) is disclosed here, not hidden.
//
// hilprep-6 added the assertion that the controller RESUMES: the "run continues" assertion above hid
// a defect for as long as this test existed. `psp_lockstep_init` zeroed the tick count that
// `sch_lockstep` compares with its own last-seen count, so a RESET after N steps silenced the
// scheduler (and with it the ADCS) for N steps -- with the fault at 5 s of 10 s, for the rest of the
// run. The posix image built before the fix (`altavista-cfs-lockstep:local`, `4a9a6aee...`) fails the
// assertion with 0 outputs in the 50 post-RESET steps; an image built from the fixed tree passes with
// 50 of 50 (`AV_CFS_TEST_IMAGE` points the tests at such a test-only image).
// ------------------------------------------------------------------------------------------

const RESET_DURATION_S: i64 = 10;
const RESET_FAULT_TAI_OFFSET_S: i64 = 5;

#[test]
fn a_power_cycle_hardware_fault_on_the_container_bound_controller_is_accepted_and_the_run_continues() {
    if skip_if_cfs_image_unavailable("a_power_cycle_hardware_fault_on_the_container_bound_controller_is_accepted_and_the_run_continues") {
        return;
    }
    run_a_power_cycle_hardware_fault_on_the_container_bound_controller_is_accepted_and_the_run_continues();
}

fn run_a_power_cycle_hardware_fault_on_the_container_bound_controller_is_accepted_and_the_run_continues() {
    let _engine = gmat_sys::engine_lock();
    // Question 207: held for this whole test body -- see lock_and_prune_docker_tests's own doc
    // comment.
    let (_lock, run_id) = lock_and_prune_docker_tests();
    let (systems, base_sos) = load_systems_and_base_sos();
    let (image, digest, _registry_guards) = push_cfs_image_to_local_registry(&run_id);
    let sos = container_sos("attitude_control_cfs_reset_sos", &base_sos, &image, &digest);
    let scenario_start = schema::parse_drm_yaml(&read("demo_attitude_control.drm.yaml")).expect("native DRM parses").scenario.expect("scenario").start_tai_ns;
    let fault_tai_ns = scenario_start + RESET_FAULT_TAI_OFFSET_S * 1_000_000_000;
    let faults = vec![Fault { id: "cfs_power_cycle".to_string(), instance: "controller".to_string(), target_kind: FaultTargetKind::Hardware as i32, tai_ns: fault_tai_ns, kind: "power_cycle".to_string(), ..Default::default() }];
    let drm = container_drm("attitude_control_cfs_reset_drm", &sos.id, RESET_DURATION_S, faults);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = replay_scratch_dir("power-cycle");
    let cfg = RunConfig { products_dir: Some(dir.clone()), ..run_config(&gmat, &drm, &sos, &systems, "test-run-cfs-reset") };
    let products = execute(cfg).expect("a power-cycle HARDWARE fault on the container-bound \"controller\" instance must be accepted, not refused, and the run must complete");
    let sidecar = std::fs::read(dir.join("port_traffic.pb")).unwrap_or_else(|e| panic!("reading port_traffic.pb: {e}"));
    let _ = std::fs::remove_dir_all(&dir);

    let fault_events: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::Fault as i32).collect();
    assert_eq!(fault_events.len(), 1, "exactly one FAULT event expected: {:?}", products.events);
    assert_eq!(fault_events[0].name, "cfs_power_cycle");
    assert_eq!(fault_events[0].entity_id, "controller");
    assert_eq!(fault_events[0].tai_ns, fault_tai_ns);

    // The run really did continue past the fault (Reset's own gRPC round trip against the real
    // container succeeded and the kernel kept stepping): the truth trajectory covers the whole
    // scenario, not just the pre-fault span.
    let traj = products.trajectories.get("attitude").expect("the \"attitude\" instance produced a trajectory");
    assert_eq!(traj.samples.len(), (RESET_DURATION_S + 1) as usize, "expected samples covering the full {RESET_DURATION_S}s scenario (1 Hz output grid), got tai_ns values {:?}", traj.samples.iter().map(|s| s.tai_ns).collect::<Vec<_>>());

    // hilprep-6: the controller RESUMES after the RESET. Before the fix `psp_lockstep_init` zeroed the
    // tick count that `sch_lockstep` compares with its own last-seen count, so after a RESET at step N
    // the scheduler sent no wakeup for N steps and the ADCS published nothing (here: the 50 steps from
    // the fault, i.e. the rest of the run). Read off the port traffic the executor recorded: the
    // `wheel_torque_out` OUT records of "controller" carry the epoch of the STEP that produced them
    // (the STEP's `until_tai_ns`), 10 Hz, one per step. The first one strictly after the fault's epoch
    // must be within TWO steps of it (the first post-RESET step is expected; the second is the slack
    // for a wakeup landing one poll late), and nearly every later step must have one.
    let log = PortTrafficLog::decode(sidecar.as_slice()).expect("port_traffic.pb decodes");
    let step_ns = 100_000_000i64;
    let after: Vec<i64> = log.records.iter().filter(|r| r.instance == "controller" && r.port == "wheel_torque_out" && r.direction == PortDirection::Out as i32 && r.tai_ns > fault_tai_ns).map(|r| r.tai_ns).collect();
    let steps_after = (RESET_DURATION_S - RESET_FAULT_TAI_OFFSET_S) * 10;
    let first = after.first().copied();
    println!("POST-RESET OUTPUT: fault epoch {fault_tai_ns}; first controller output after it {:?} ({} steps later); {} outputs over the {steps_after} post-RESET steps", first, first.map(|t| (t - fault_tai_ns) / step_ns).unwrap_or(-1), after.len());
    assert!(first.is_some_and(|t| t <= fault_tai_ns + 2 * step_ns), "the controller must publish again within two steps of the RESET (first output after the fault: {first:?}, fault epoch {fault_tai_ns}; {} outputs in the {steps_after} post-RESET steps): after a RESET at step N the scheduler stayed silent for N steps (psp_lockstep_init zeroed the tick count sch_lockstep compares against)", after.len());
    assert!(after.len() as i64 >= steps_after - 2, "the controller must keep publishing after the RESET: {} outputs in the {steps_after} post-RESET steps", after.len());
}

// ------------------------------------------------------------------------------------------
// Question 239 / sil-plan "Found, not fixed" 1: a posix run's provenance does not depend on
// which throwaway registry served the image.
// ------------------------------------------------------------------------------------------

const REGISTRY_INDEPENDENCE_DURATION_S: i64 = DETERMINISM_DURATION_S;

#[test]
fn the_port_traffic_hash_is_the_same_across_two_separately_started_registries() {
    if skip_if_cfs_image_unavailable("the_port_traffic_hash_is_the_same_across_two_separately_started_registries") {
        return;
    }
    run_the_port_traffic_hash_is_the_same_across_two_separately_started_registries();
}

/// Whether `docker inspect <id>` still finds a container with this id.
fn docker_container_exists(id: &str) -> bool {
    Command::new("docker").args(["inspect", "--type=container", id]).output().map(|o| o.status.success()).unwrap_or(false)
}

/// **The defect this fails against.** The posix half of the cFS tests binds
/// `ContainerBinding.image = <registry>/altavista-cfs-lockstep`, and the whole `SosConfiguration`
/// is hashed (ADR-005 section 7) into `Provenance.attributes["sos_configuration_hash"]`, which
/// the `port_traffic.pb` sidecar embeds -- so when the registry's address was an ephemeral
/// `127.0.0.1:<port>`, two runs of identical code against two throwaway registries got different
/// whole-file `port_traffic_hash` values (`a514ea92...` against `36329922...`) with every record
/// identical. [`push_cfs_image_to_local_registry`] now publishes each registry on the fixed
/// [`cfs_common::FIXED_REGISTRY_PORT`]; this test starts TWO registries one after the other (distinct
/// containers: different ids, the first removed before the second starts), runs the same DRM and
/// the same run id once against each, and requires the same whole-file `port_traffic_hash`
/// (each the SHA-256 of the file written) and the same `sos_configuration_hash`.
fn run_the_port_traffic_hash_is_the_same_across_two_separately_started_registries() {
    let _engine = gmat_sys::engine_lock();
    let (_lock, run_id) = lock_and_prune_docker_tests();
    let (systems, base_sos) = load_systems_and_base_sos();
    const SHARED_RUN_ID: &str = "test-run-cfs-registry-independence";

    // One full cycle: start a registry, push the image to it, run the DRM against it, write the
    // sidecar, then tear the registry down (the guards drop at the end of this closure).
    let run_against_fresh_registry = |label: &str| -> (String, String, String, RunProducts, Vec<u8>) {
        let (image, digest, registry_guards) = push_cfs_image_to_local_registry(&run_id);
        let registry_id = registry_guards.0 .0.clone();
        let sos = container_sos("attitude_control_cfs_registry_sos", &base_sos, &image, &digest);
        let drm = container_drm("attitude_control_cfs_registry_drm", &sos.id, REGISTRY_INDEPENDENCE_DURATION_S, vec![]);
        let dir = replay_scratch_dir(label);
        let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: SHARED_RUN_ID.to_string(), error_mode: Default::default(), products_dir: Some(dir.clone()), replay: None, command_source: None };
        let products = execute(cfg).unwrap_or_else(|e| panic!("run {label} must execute end to end against its own registry: {e:?}"));
        let sidecar = std::fs::read(dir.join("port_traffic.pb")).unwrap_or_else(|e| panic!("reading run {label}'s port_traffic.pb: {e}"));
        let _ = std::fs::remove_dir_all(&dir);
        (image, digest, registry_id, products, sidecar)
    };

    let (image_a, digest_a, registry_a, products_a, sidecar_a) = run_against_fresh_registry("registry-a");
    assert!(!docker_container_exists(&registry_a), "registry A ({registry_a}) must be gone before registry B starts: the two runs must be served by distinct registry containers");
    let (image_b, digest_b, registry_b, products_b, sidecar_b) = run_against_fresh_registry("registry-b");

    assert_ne!(registry_a, registry_b, "the two runs must be served by two distinct registry containers");
    println!("registry A {registry_a}: image {image_a}@{digest_a}");
    println!("registry B {registry_b}: image {image_b}@{digest_b}");
    assert_eq!(image_a, image_b, "ContainerBinding.image must not depend on which registry container serves it");
    assert_eq!(digest_a, digest_b, "the same local image pushed to two fresh registries must have the same digest (image_digest is part of the hashed binding)");

    let sos_hash_a = products_a.provenance.attributes.get("sos_configuration_hash").expect("run A records sos_configuration_hash");
    let sos_hash_b = products_b.provenance.attributes.get("sos_configuration_hash").expect("run B records sos_configuration_hash");
    println!("sos_configuration_hash: A={sos_hash_a} B={sos_hash_b}");
    println!("port_traffic_hash: A={} B={}", products_a.port_traffic_hash, products_b.port_traffic_hash);
    assert!(!products_a.port_traffic_hash.is_empty() && !products_b.port_traffic_hash.is_empty(), "both runs were given a products_dir, so both report a non-empty port_traffic_hash");
    assert_eq!(products_a.port_traffic_hash, sha256_hex(&sidecar_a), "run A's port_traffic_hash must be the SHA-256 of the port_traffic.pb it wrote");
    assert_eq!(products_b.port_traffic_hash, sha256_hex(&sidecar_b), "run B's port_traffic_hash must be the SHA-256 of the port_traffic.pb it wrote");
    assert_eq!(sos_hash_a, sos_hash_b, "sos_configuration_hash must be the same across the two registries");
    assert_eq!(products_a.port_traffic_hash, products_b.port_traffic_hash, "the whole-file port_traffic_hash must be the same across the two registries");
    assert_eq!(sidecar_a, sidecar_b, "the two port_traffic.pb files must be byte-identical");
}
