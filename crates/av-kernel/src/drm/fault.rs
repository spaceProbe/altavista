//! Injecting `Scenario.faults` (ADR-005 section 5, `docs/adr/005-simulation-kernel.md`) at
//! their declared epochs, for the kinds this crate can act on: `FAULT_TARGET_KIND_DYNAMICS`
//! (fully applied), `_PORT` and `_SENSOR` (validated, seeded, and explicitly refused -- see
//! "PORT and SENSOR" below), and `_HARDWARE` (a container power cycle only -- see "Container
//! power-cycle (HARDWARE)" below; any other `_HARDWARE` use is a typed refusal, not silently
//! dropped).
//!
//! ## DYNAMICS (question 87's "What to build" item 5) -- applied
//!
//! **What a DYNAMICS fault can change.** Only `kind == "parameter"` faults are supported (the
//! `Fault.kind` field's own doc comment names several kinds -- `"drop"`, `"delay"`, ... --
//! that apply to ports/sensors/hardware, not dynamics; a DYNAMICS fault with any other `kind`
//! is a typed error, not silently ignored). `Fault.target` names one parameter using the same
//! vocabulary `binding`'s module doc comment documents (`force_model.gravity_degree`,
//! `force_model.gravity_order`, `force_model.relativistic_correction`, any
//! `spacecraft.<GmatFieldName>`, or `accel.{x,y,z}` for a native binding); `Fault.params
//! ["value"]` is the new value. `executor::execute` splits the run into one
//! [`av_cdm::pb::TrajectorySegment`] per fault-bounded span (a "contiguous span produced by
//! one propagation with one dynamics configuration" -- exactly what `TrajectorySegment`'s own
//! doc comment describes), applying this function at each fault's epoch and re-binding from
//! the previous segment's own final physical state -- so position/velocity are continuous
//! across the boundary; only the named parameter changes.
//!
//! **GMAT re-binding.** A GMAT-bound instance's Spacecraft is *rebuilt* at the fault epoch
//! (GMAT holds no "change one field on a live, already-`Initialize`d Spacecraft mid-flight"
//! operation this crate uses elsewhere) with `DisplayStateType = "Cartesian"` and the
//! X/Y/Z/VX/VY/VZ fields set from the segment's own final state (km, `av_cdm::units`) --
//! *not* by re-deriving Keplerian elements, which would need an osculating-element
//! computation this crate does not have and does not need: the physical SI state at the
//! boundary is already exact. Any previously-declared Keplerian element parameters
//! (`spacecraft.SMA`/`ECC`/`INC`/`RAAN`/`AOP`/`TA`) are dropped from the rebuilt spec so GMAT
//! is never handed a `DisplayStateType` that disagrees with the fields actually present.
//!
//! **Why DYNAMICS needs no RNG.** ADR-005 section 5's seeded-PCG64 rule ("every random element
//! draws from a seeded PCG64 keyed by `Scenario.seeds[<fault id>]`") covers faults that have a
//! *random* element. A DYNAMICS/`"parameter"` fault does not: it "sets a model parameter"
//! (section 5's own wording) to `Fault.params["value"]`, a single declared number -- there is
//! nothing to draw. [`apply_dynamics_fault`] is unchanged by this task for exactly that reason.
//!
//! ## Container power-cycle (M16.2, `docs/open-questions.md` question 120) -- HARDWARE
//!
//! A `FAULT_TARGET_KIND_HARDWARE` fault of `kind == "power_cycle"` naming a
//! `BINDING_KIND_CONTAINER` instance is the one `_HARDWARE` shape this crate applies today:
//! there is no `BindingPlan` to rebind (a container instance has none --
//! `binding::ContainerSpec`/`ContainerModel` are a parallel track, see `binding`'s own module
//! doc comment), so [`apply_dynamics_fault`] is never called for it. Instead
//! `executor::run_shared_group` calls `binding::ContainerModel::reset` at the fault's own epoch,
//! with `reason = "fault:<fault id>"` (`lockstep.proto`'s own `LockstepResetRequest.reason` doc
//! comment names exactly this form) -- see that function's own doc comment for the boundary
//! mechanics. [`is_container_power_cycle`] is the one predicate both `executor::execute`'s
//! load-time validation and `run_shared_group`'s own boundary-collection pass use, so the two
//! decisions can never disagree.
//!
//! **Why HARDWARE, and why this was not DYNAMICS to begin with.** `FAULT_TARGET_KIND_HARDWARE`
//! already exists in `proto/altavista/v1/system.proto` (`= 3`), and its own doc comment reads
//! "Hardware: Renode peripheral fault, board reset, power cycle" -- it names a power cycle
//! directly. M15.3 wired this to `FAULT_TARGET_KIND_DYNAMICS` instead, on the mistaken premise
//! that only PORT, SENSOR and DYNAMICS were available to this crate; that premise was the
//! brief's own error, not a real constraint (HARDWARE was never proto-unavailable, only
//! unmentioned). **The lead decided (question 120): HARDWARE, for containers now and Renode and
//! boards later.** DYNAMICS's own doc comment ("a dynamics or environment parameter change")
//! never really described a power cycle in the first place -- a power cycle does not change a
//! *parameter* of the running dynamics, it resets the instance's own accumulated state back to a
//! known baseline, which is exactly the "board reset, power cycle" HARDWARE's doc comment
//! already names. PORT still does not fit: `Fault.target` would have to name a port, but a power
//! cycle is not "acts in the router" (drop/delay/corrupt/duplicate) -- it never touches an
//! in-flight message.
//!
//! **The interim DYNAMICS/`"power_cycle"` shape is now a typed load error, not a silently
//! accepted alternative.** [`is_legacy_dynamics_power_cycle`] recognizes exactly the shape
//! M15.3 used; `executor::execute` refuses it at load with
//! `DrmError::PowerCycleFaultMustTargetHardware`, before Pass 2 ever runs, so no DRM can rely on
//! the old shape still working (`is_container_power_cycle` no longer matches it at all -- it
//! only ever matches `FAULT_TARGET_KIND_HARDWARE` now, see below).
//!
//! **HARDWARE naming anything other than a container's own power cycle is also refused, not
//! silently dropped.** A `FAULT_TARGET_KIND_HARDWARE` fault naming a `BINDING_KIND_MODEL`
//! instance has no meaning yet (`DrmError::HardwareFaultNotSupportedOnInstance`) -- HARDWARE's
//! other named uses (`RenodeBinding`, `BoardBinding.power_control`) are Renode/board specific,
//! and Renode work is deferred until a host with Renode exists; a `BINDING_KIND_MODEL` instance
//! is neither a container, a Renode target, nor a board. A `FAULT_TARGET_KIND_HARDWARE` fault
//! naming a container instance with any `kind` other than `"power_cycle"` is refused the same
//! way (`DrmError::HardwareFaultKindNotSupported`) -- a container has no Renode peripheral or
//! board to reset, only its own process to power-cycle. See `executor::execute`'s own load-time
//! validation pass for both checks.
//!
//! ## PORT and SENSOR -- seeded, validated, and explicitly refused
//!
//! Section 5 also names `FAULT_TARGET_KIND_PORT` ("acts in the router: drop, delay, corrupt,
//! duplicate") and `FAULT_TARGET_KIND_SENSOR` ("acts in sensor models: bias, noise, dropout,
//! misalignment"). Neither has a runtime this crate can act on yet: there is no port router
//! (`binding`'s module doc comment: `CONTAINER`/`RENODE`/`BOARD` bindings, and the router that
//! would sit between them, are all still Planned/P2) and no sensor-model binding kind exists
//! at all. Applying either kind "end to end" is therefore impossible today, and this module
//! never claims otherwise: [`realize_unapplied_fault`] validates the fault's `kind` against
//! the names section 5 documents for its `target_kind`, resolves its seed from
//! `Scenario.seeds[fault.id]` (via [`crate::rng::seed_for`]), draws the first `u64` from that
//! fault's own [`crate::rng::Pcg64`] stream -- proving the seeded stream is correctly keyed and
//! reproducible, exactly what section 5 promises -- and then **always** returns
//! [`DrmError::FaultTargetKindNotSupported`], carrying that draw so it stays visible to the
//! caller even though nothing consumes it. This is a typed, explicit refusal, not a silent
//! skip; see this module's own tests, and the crate's `tests/faults_seeded.rs`, for exactly
//! what each failure mode looks like.
//!
//! **What this module does *not* invent.** Neither the proto nor ADR-005 defines a per-kind
//! parameter schema for a PORT/SENSOR fault's actual effect (e.g. what a `"bias"` fault's
//! magnitude/units would be, or a `"drop"` fault's probability parameter name) -- that belongs
//! to the router/sensor-model bindings themselves, which are still Planned. Inventing one here,
//! ahead of those bindings, would be scope this task does not have the authority to set (it
//! would become part of ADR-005's own contract the moment a caller started relying on it).
//! [`realize_unapplied_fault`] therefore draws one canonical value per fault (proof of a
//! correctly-keyed, reproducible stream) rather than a kind-specific realization.
//!
//! **Integration note (still open, out of scope for M16.2).** [`realize_unapplied_fault`] is
//! not yet called from `executor::execute`: that function's own fault-collecting loops only
//! ever look at DYNAMICS and (as of M16.2) HARDWARE faults, so a PORT/SENSOR fault declared in
//! a real DRM today is still silently dropped before this module ever sees it. `executor.rs` is
//! this task's own file to edit (unlike at M15.3), but M16.2's scope is the HARDWARE move
//! (question 120) alone -- wiring PORT/SENSOR realization into `execute()` is unrelated feature
//! work this task does not extend to, and is left exactly as escalated before. The suggested
//! integration point is unchanged: the same place `DrmError::FaultEpochNotOnSampleGrid` is
//! already checked, before any binding/GMAT call.

use std::collections::BTreeMap;

use av_cdm::pb::{Fault, FaultTargetKind};
use av_cdm::units;

use super::attitude::{parse_index_and_field, AttitudeWheelsSpec};
use super::binding::{BindingPlan, ConstantAccelSpec, GmatSystemSpec};
use super::controller::AttitudeControllerSpec;
use super::ground::GroundStationSpec;
use super::sensors::{ImuSpec, StarTrackerSpec};
use super::DrmError;
use crate::rng::{seed_for, Pcg64};

/// `Fault.kind` values ADR-005 section 5 documents for `FAULT_TARGET_KIND_PORT`: "acts in the
/// router (drop, delay, corrupt, duplicate)". Matches `proto/altavista/v1/system.proto`'s own
/// `Fault.kind` doc comment.
const PORT_KINDS: [&str; 4] = ["drop", "delay", "corrupt", "duplicate"];
/// `Fault.kind` values ADR-005 section 5 documents for `FAULT_TARGET_KIND_SENSOR`: "acts in
/// sensor models (bias, noise, dropout, misalignment)". `"misalign"`, not `"misalignment"`,
/// per `proto/altavista/v1/system.proto`'s `Fault.kind` doc comment, which spells out the
/// literal string values this field actually takes.
const SENSOR_KINDS: [&str; 4] = ["bias", "noise", "dropout", "misalign"];

/// Comparison key for ADR-005 section 5's required fault-application order: "injected at their
/// epochs in sorted `(epoch, id)` order". Exposed so a caller sorting `&[Fault]` (today,
/// `executor::run_plain_instance`'s DYNAMICS-only `instance_faults`, which currently sorts by
/// `tai_ns` alone -- see this module's own doc comment's "Integration note") can use the exact
/// tie-break section 5 specifies: `faults.sort_by_key(fault::epoch_id_order)`.
pub fn epoch_id_order(fault: &Fault) -> (i64, String) {
    (fault.tai_ns, fault.id.clone())
}

/// `Fault.kind`'s own doc comment's literal string for a power-cycle fault (grouped there
/// alongside `"reset"`, `"parameter"`, ...). M16.2 (question 120): the one `kind` value a
/// `FAULT_TARGET_KIND_HARDWARE` fault may carry when it names a `BINDING_KIND_CONTAINER`
/// instance -- see this module's own doc comment's "Container power-cycle (HARDWARE)" section.
/// (Through M15.3 this lived under `FAULT_TARGET_KIND_DYNAMICS` instead; superseded -- see
/// [`is_legacy_dynamics_power_cycle`].)
pub const POWER_CYCLE_KIND: &str = "power_cycle";

/// `true` iff `fault` is a `FAULT_TARGET_KIND_HARDWARE`/`"power_cycle"` fault -- the one shape
/// `executor::run_shared_group` calls `binding::ContainerModel::reset` for. Does **not** check
/// whether `fault.instance` actually names a `BINDING_KIND_CONTAINER` instance (this module has
/// no `SosConfiguration`/classification in scope) -- every caller already checks that
/// separately (`executor::execute`'s load-time validation against `container_plans`;
/// `run_shared_group`'s own boundary-collection pass against the same map), so this predicate is
/// only ever evaluated for a fault already known to name one.
pub fn is_container_power_cycle(fault: &Fault) -> bool {
    fault.target_kind == FaultTargetKind::Hardware as i32 && fault.kind == POWER_CYCLE_KIND
}

/// `true` iff `fault` is the interim, M15.3-era shape a container power cycle used before
/// question 120 moved it to `FAULT_TARGET_KIND_HARDWARE` -- `FAULT_TARGET_KIND_DYNAMICS` with
/// `kind == "power_cycle"`. `executor::execute` refuses this at load
/// (`DrmError::PowerCycleFaultMustTargetHardware`) rather than let it fall through to an
/// ordinary DYNAMICS boundary (which would try `apply_dynamics_fault` and reject it there
/// anyway, for the wrong, generic reason -- `kind != "parameter"`) or to the generic
/// container-fault refusal (which would not name the specific mistake). Deliberately checked
/// independent of what `fault.instance` names: the shape itself, not just its effect on a
/// container, is what M16.2 retires.
pub fn is_legacy_dynamics_power_cycle(fault: &Fault) -> bool {
    fault.target_kind == FaultTargetKind::Dynamics as i32 && fault.kind == POWER_CYCLE_KIND
}

/// The named `target_kind`'s ADR-005 section 5 kind vocabulary (see [`PORT_KINDS`]/
/// [`SENSOR_KINDS`]), or `None` for any `target_kind` other than PORT/SENSOR (a caller error --
/// [`realize_unapplied_fault`] is only meant to be called for those two).
fn kinds_for(target_kind: FaultTargetKind) -> Option<&'static [&'static str]> {
    match target_kind {
        FaultTargetKind::Port => Some(&PORT_KINDS),
        FaultTargetKind::Sensor => Some(&SENSOR_KINDS),
        _ => None,
    }
}

/// Validate, seed, and deterministically (but never actually) realize a
/// `FAULT_TARGET_KIND_PORT` or `FAULT_TARGET_KIND_SENSOR` fault -- see this module's own doc
/// comment's "PORT and SENSOR" section for exactly what this does and does not claim.
///
/// # Panics
///
/// If `fault.target_kind` decodes to anything other than `FAULT_TARGET_KIND_PORT`/`_SENSOR`
/// (including an unrecognized wire value or `_UNSPECIFIED`) -- a caller bug, not a data
/// problem: every caller of this function must already have dispatched on `target_kind` to
/// reach it (mirroring `apply_dynamics_fault`'s own implicit assumption, via `BindingPlan`,
/// that its caller already resolved which fault kind it is looking at).
pub fn realize_unapplied_fault(fault: &Fault, seeds: &BTreeMap<String, u64>) -> DrmError {
    let target_kind = FaultTargetKind::try_from(fault.target_kind).unwrap_or(FaultTargetKind::Unspecified);
    let allowed = kinds_for(target_kind).unwrap_or_else(|| panic!("realize_unapplied_fault called with target_kind {target_kind:?}; only PORT/SENSOR are supported"));

    if !allowed.contains(&fault.kind.as_str()) {
        return DrmError::UnknownParameter { context: format!("fault {:?}", fault.id), name: format!("kind={:?} (expected one of {allowed:?} for {})", fault.kind, target_kind.as_str_name()) };
    }
    let Some(seed) = seed_for(seeds, &fault.id) else {
        return DrmError::MissingFaultSeed { fault_id: fault.id.clone() };
    };
    let realized_draw = Pcg64::new(seed).next_u64();
    DrmError::FaultTargetKindNotSupported { fault_id: fault.id.clone(), target_kind: target_kind.as_str_name().to_string(), realized_draw }
}

/// The six Keplerian `spacecraft.*` element names [`rebind_gmat_spec_at_state`] unconditionally
/// *removes* from `spacecraft_real` at every re-materialization after the first (whether or not
/// the DRM originally declared the instance's initial state this way) -- see that function's own
/// doc comment. `pub(crate)` (M18.4, `docs/open-questions.md` question 127): `super::binding::
/// gmat_settings` excludes this set from the `BTreeMap` `dynamics_hash` is computed over, for the
/// same reason as [`CARTESIAN_FIELDS`] below -- these fields describe an *initial state
/// representation*, present only in a segment that has never been rebound, never a
/// force-model/ballistic configuration value a later re-materialization could meaningfully
/// disagree with. A `spacecraft.SMA`/`ECC`/... DYNAMICS fault (question 82's own vocabulary
/// technically allows one) has in fact never had any propagated effect past the segment it fires
/// in either: [`apply_dynamics_fault`] writes it into `spacecraft_real`, but the very next
/// [`rebind_gmat_spec_at_state`] this crate always calls before actually reconstructing GMAT's
/// `Spacecraft` (`executor::materialize_plan_at_boundary`, unconditionally, for every GMAT-bound
/// re-materialization) strips it straight back out again -- so excluding these fields from the
/// hash does not hide a real effect this crate could otherwise observe; there was never one to
/// observe. `docs/open-questions.md` and this crate's own fault vocabulary never claimed
/// otherwise; this doc comment is the first place it is stated plainly.
pub(crate) const KEPLERIAN_FIELDS: [&str; 6] = ["SMA", "ECC", "INC", "RAAN", "AOP", "TA"];
/// The six `spacecraft.*` fields [`rebind_gmat_spec_at_state`] writes from an instance's own
/// *instantaneous* physical state at every re-materialization (fault-bounded or not) -- never a
/// declared configuration value. `pub(crate)` (M18.4, `docs/open-questions.md` question 127):
/// `super::binding::gmat_settings` excludes exactly this set from the `BTreeMap` `dynamics_hash`
/// is computed over, so two GMAT-bound segments with identical configuration but different
/// position/velocity hash equal -- see that function's own doc comment for why state, not just
/// this field list, must never leak into a *configuration* hash. Single source of truth shared
/// with [`rebind_gmat_spec_at_state`] itself, so the two can never silently disagree about which
/// six fields are "state".
pub(crate) const CARTESIAN_FIELDS: [&str; 6] = ["X", "Y", "Z", "VX", "VY", "VZ"];
/// The `spacecraft.*` string field [`rebind_gmat_spec_at_state`] always overwrites to
/// `"Cartesian"` at every re-materialization after the first, regardless of what the DRM
/// originally declared (`"Keplerian"`, most commonly). `pub(crate)` (M18.4, question 127):
/// `super::binding::gmat_settings` excludes this one key too, for exactly the reason
/// [`KEPLERIAN_FIELDS`]/[`CARTESIAN_FIELDS`] are excluded -- it records *which representation this
/// particular re-materialization's own state happened to use*, not a declared configuration
/// choice a caller could meaningfully change to get different physics. Without excluding it, a
/// GMAT-bound instance's segment 0 (still `"Keplerian"`, never rebound) would never hash equal to
/// segment 1 (always `"Cartesian"`, the moment ANY re-materialization -- fault, maneuver, or a
/// boundary belonging entirely to some OTHER instance -- happens even once), which defeated
/// question 115's bystander merge for exactly the shape `tests/demo_two_instance.rs::
/// demo_two_instance_bystander_invariance_against_real_single_instance_gmat_runs` measures: a
/// bystander's own FIRST-EVER re-materialization is necessarily this Keplerian -> Cartesian
/// transition, whether or not anything about its own dynamics configuration changed.
pub(crate) const DISPLAY_STATE_TYPE_FIELD: &str = "DisplayStateType";

fn fault_value(fault: &Fault) -> Result<f64, DrmError> {
    fault.params.get("value").copied().ok_or_else(|| DrmError::MissingParameter { context: format!("fault {:?}", fault.id), name: "params[\"value\"]".to_string() })
}

fn apply_gmat_target(spec: &mut GmatSystemSpec, target: &str, value: f64, fault_id: &str) -> Result<(), DrmError> {
    if let Some(field) = target.strip_prefix("force_model.") {
        match field {
            "gravity_degree" => spec.gravity_degree = value.round() as i32,
            "gravity_order" => spec.gravity_order = value.round() as i32,
            "relativistic_correction" => spec.relativistic_correction = value != 0.0,
            other => return Err(DrmError::UnknownParameter { context: format!("fault {fault_id:?}"), name: format!("force_model.{other}") }),
        }
        Ok(())
    } else if let Some(field) = target.strip_prefix("spacecraft.") {
        spec.spacecraft_real.insert(field.to_string(), value);
        Ok(())
    } else {
        Err(DrmError::UnknownParameter { context: format!("fault {fault_id:?}"), name: target.to_string() })
    }
}

/// M22.1b (`docs/open-questions.md` question 152, decided by the lead): "a DYNAMICS fault on an
/// attitude instance changes inertia, wheel limits, or a wheel's availability, through declared
/// parameters" -- exactly the three targets this function recognizes, each a deliberate,
/// separately-tested arm (never a catch-all). Reuses `crate::drm::attitude::
/// parse_index_and_field` for the `"attitude.wheel.<k>.<field>"` pattern rather than
/// re-implementing it -- the same helper `parse_attitude_spec` itself uses, so the two never
/// silently disagree about what counts as a valid wheel index/field split.
///
/// `attitude.inertia.jxy`/`.jxz`/`.jyz` write **both** off-diagonal positions -- the tensor stays
/// symmetric by construction, the same invariant `crate::drm::attitude::parse_attitude_spec`
/// already assembles at load time (`AttitudeWheelsSpec::inertia`'s own doc comment). Unlike
/// [`super::binding::parse_constant_accel_spec`]'s own load-time physical checks (unit norm,
/// positive-definiteness, ...), this function does **not** re-run them -- a fault can drive the
/// tensor non-positive-definite, and that failure surfaces, typed, at the *next*
/// re-materialization (`crate::drm::attitude::AttitudeWheelsModel::new`'s own singularity guard,
/// via `crate::registry::ModelRegistry::construct_attitude` -> `DrmError::Model`), exactly the
/// same "a fault's own value is not re-validated against the spec's original load-time checks"
/// precedent [`apply_gmat_target`] already sets for a GMAT-bound instance's own fields.
fn apply_attitude_target(spec: &mut AttitudeWheelsSpec, target: &str, value: f64, fault_id: &str) -> Result<(), DrmError> {
    match target {
        "attitude.inertia.jxx" => spec.inertia[0][0] = value,
        "attitude.inertia.jyy" => spec.inertia[1][1] = value,
        "attitude.inertia.jzz" => spec.inertia[2][2] = value,
        "attitude.inertia.jxy" => {
            spec.inertia[0][1] = value;
            spec.inertia[1][0] = value;
        }
        "attitude.inertia.jxz" => {
            spec.inertia[0][2] = value;
            spec.inertia[2][0] = value;
        }
        "attitude.inertia.jyz" => {
            spec.inertia[1][2] = value;
            spec.inertia[2][1] = value;
        }
        other => {
            let rest = other.strip_prefix("attitude.wheel.").ok_or_else(|| DrmError::UnknownParameter { context: format!("fault {fault_id:?}"), name: target.to_string() })?;
            let (idx, field) = parse_index_and_field(rest).ok_or_else(|| DrmError::UnknownParameter { context: format!("fault {fault_id:?}"), name: target.to_string() })?;
            let n_wheels = spec.wheel_axes.len();
            if idx == 0 || idx > n_wheels {
                return Err(DrmError::UnknownParameter { context: format!("fault {fault_id:?}"), name: format!("{target} (this instance declares {n_wheels} wheel(s), indexed 1..={n_wheels})") });
            }
            match field {
                // "wheel limits" (question 152's own wording): the declared saturation
                // momentum limit -- see AttitudeWheelsSpec::wheel_momentum_limits's own doc
                // comment; exit criterion 4's own fault (halving a wheel's limit) targets
                // exactly this.
                "momentum_limit" => spec.wheel_momentum_limits[idx - 1] = value,
                // "a wheel's availability" (question 152's own wording): see
                // AttitudeWheelsSpec::wheel_available's own doc comment for exactly what
                // `false` means physically (no torque authority, momentum already held is
                // unaffected).
                "available" => spec.wheel_available[idx - 1] = value != 0.0,
                _ => return Err(DrmError::UnknownParameter { context: format!("fault {fault_id:?}"), name: target.to_string() }),
            }
        }
    }
    Ok(())
}

/// M22.2b (`docs/open-questions.md` questions 142/149, decided by the lead): "a DYNAMICS fault
/// on a sensor instance perturbs its declared noise parameters" -- for a star tracker, that is
/// exactly one field: `startracker.noise_sigma_rad`, the declared 1-sigma boresight error a
/// degraded star tracker (dust on the baffle, a radiation-damaged detector, ...) would report as
/// larger. `startracker.update_rate_hz`/`.seed`/`.mount_q` are declared hardware/software
/// configuration this batch does not model a fault changing mid-run (an update rate or mounting
/// angle does not drift; a seed is not a physical quantity at all) -- naming any of those three
/// is a typed [`DrmError::UnknownParameter`], the same "not every declared parameter is a valid
/// fault target" rule [`apply_attitude_target`] already applies to `attitude.q0.*`/`.omega0.*`
/// (declared only at classification, never a fault target).
fn apply_star_tracker_target(spec: &mut StarTrackerSpec, target: &str, value: f64, fault_id: &str) -> Result<(), DrmError> {
    match target {
        "startracker.noise_sigma_rad" => spec.noise_sigma_rad = value,
        other => return Err(DrmError::UnknownParameter { context: format!("fault {fault_id:?}"), name: other.to_string() }),
    }
    Ok(())
}

/// The IMU counterpart of [`apply_star_tracker_target`]: a DYNAMICS fault perturbs one of the
/// four declared noise/bias-random-walk sigmas (`imu.gyro_noise_sigma`/`.accel_noise_sigma`/
/// `.gyro_bias_rw_sigma`/`.accel_bias_rw_sigma`) -- a degraded IMU reporting more measurement
/// noise, or a bias that random-walks faster. `imu.update_rate_hz`/`.seed`/`.mount_q`/
/// `.true_specific_force` are refused for the identical reason [`apply_star_tracker_target`]
/// refuses the star tracker's own non-noise fields.
fn apply_imu_target(spec: &mut ImuSpec, target: &str, value: f64, fault_id: &str) -> Result<(), DrmError> {
    match target {
        "imu.gyro_noise_sigma" => spec.gyro_noise_sigma = value,
        "imu.accel_noise_sigma" => spec.accel_noise_sigma = value,
        "imu.gyro_bias_rw_sigma" => spec.gyro_bias_rw_sigma = value,
        "imu.accel_bias_rw_sigma" => spec.accel_bias_rw_sigma = value,
        other => return Err(DrmError::UnknownParameter { context: format!("fault {fault_id:?}"), name: other.to_string() }),
    }
    Ok(())
}

/// M22.4 (`docs/sil-plan.md`'s M22 milestone paragraph): "a DYNAMICS fault on a controller
/// instance degrades its declared gains" -- `controller.kp`/`controller.kd`, a control-loop
/// analogue of a physically degraded actuator authority or a re-tuned (deliberately or by
/// fault) control law. `controller.target_q.*`/`.update_rate_hz` are declared configuration this
/// batch does not model a fault changing mid-run -- naming either is a typed [`DrmError::
/// UnknownParameter`], the same "not every declared parameter is a valid fault target" rule
/// [`apply_star_tracker_target`]/[`apply_imu_target`] already apply to their own non-noise
/// fields.
fn apply_controller_target(spec: &mut AttitudeControllerSpec, target: &str, value: f64, fault_id: &str) -> Result<(), DrmError> {
    match target {
        "controller.kp" => spec.kp = value,
        "controller.kd" => spec.kd = value,
        other => return Err(DrmError::UnknownParameter { context: format!("fault {fault_id:?}"), name: other.to_string() }),
    }
    Ok(())
}

/// M25.1 (ADR-005 sec 5's general rule, "DYNAMICS sets a model parameter through the model's own
/// declared parameter interface" -- see `crate::drm::ground`'s own module doc comment, "Fault /
/// maneuver / covariance"): every `"station.*"` numeric field [`GroundStationSpec`] declares is a
/// valid DYNAMICS fault target -- `station.elevation_mask_rad` (an antenna outage or degraded
/// horizon raising/lowering the usable mask), `station.latitude_rad`/`.longitude_rad`/`.height_m`
/// (a relocated or mobile ground asset). `station.body` is not a numeric field (`Fault.params
/// ["value"]` is always a single `f64`, per this module's own doc comment) and is refused, the
/// same "not every declared parameter is a valid fault target" rule [`apply_star_tracker_target`]/
/// [`apply_controller_target`] already apply to their own non-numeric or configuration-only
/// fields.
fn apply_ground_station_target(spec: &mut GroundStationSpec, target: &str, value: f64, fault_id: &str) -> Result<(), DrmError> {
    match target {
        "station.latitude_rad" => spec.latitude_rad = value,
        "station.longitude_rad" => spec.longitude_rad = value,
        "station.height_m" => spec.height_m = value,
        "station.elevation_mask_rad" => spec.elevation_mask_rad = value,
        other => return Err(DrmError::UnknownParameter { context: format!("fault {fault_id:?}"), name: other.to_string() }),
    }
    Ok(())
}

/// Rebuild `spec` around a Cartesian state at the segment boundary (see the module doc
/// comment) -- called for every GMAT-bound fault-split segment, including one with no
/// parameter change of its own kind (i.e. every segment after the first uses this, since the
/// spacecraft must be reconstructed at its new epoch regardless of what changed).
pub fn rebind_gmat_spec_at_state(spec: &GmatSystemSpec, state_si: [f64; 6]) -> GmatSystemSpec {
    let mut spec = spec.clone();
    for f in KEPLERIAN_FIELDS {
        spec.spacecraft_real.remove(f);
    }
    spec.spacecraft_str.insert("DisplayStateType".to_string(), "Cartesian".to_string());
    let state_km = units::state_m_to_km(state_si);
    for (field, value) in CARTESIAN_FIELDS.into_iter().zip(state_km) {
        spec.spacecraft_real.insert(field.to_string(), value);
    }
    spec
}

/// Apply one `FAULT_TARGET_KIND_DYNAMICS`/`"parameter"` fault to `plan`, returning the new
/// plan a fresh binding should be materialized from. Does **not** itself touch GMAT --
/// `executor::execute` calls [`super::binding::materialize_gmat`]/`materialize_constant_accel`
/// on the result, exactly as it does for the instance's initial binding.
pub fn apply_dynamics_fault(plan: &BindingPlan, fault: &Fault) -> Result<BindingPlan, DrmError> {
    if fault.kind != "parameter" {
        return Err(DrmError::UnknownParameter { context: format!("fault {:?}", fault.id), name: format!("kind={:?} (only \"parameter\" is supported for FAULT_TARGET_KIND_DYNAMICS)", fault.kind) });
    }
    let value = fault_value(fault)?;
    match plan {
        BindingPlan::Gmat(spec) => {
            let mut spec = spec.clone();
            apply_gmat_target(&mut spec, &fault.target, value, &fault.id)?;
            Ok(BindingPlan::Gmat(spec))
        }
        BindingPlan::ConstantAccel(spec) => {
            let mut spec: ConstantAccelSpec = spec.clone();
            match fault.target.as_str() {
                "accel.x" => spec.a[0] = value,
                "accel.y" => spec.a[1] = value,
                "accel.z" => spec.a[2] = value,
                other => return Err(DrmError::UnknownParameter { context: format!("fault {:?}", fault.id), name: other.to_string() }),
            }
            Ok(BindingPlan::ConstantAccel(spec))
        }
        BindingPlan::Attitude(spec) => {
            let mut spec: AttitudeWheelsSpec = spec.clone();
            apply_attitude_target(&mut spec, &fault.target, value, &fault.id)?;
            Ok(BindingPlan::Attitude(spec))
        }
        BindingPlan::StarTracker(spec) => {
            let mut spec: StarTrackerSpec = spec.clone();
            apply_star_tracker_target(&mut spec, &fault.target, value, &fault.id)?;
            Ok(BindingPlan::StarTracker(spec))
        }
        BindingPlan::Imu(spec) => {
            let mut spec: ImuSpec = spec.clone();
            apply_imu_target(&mut spec, &fault.target, value, &fault.id)?;
            Ok(BindingPlan::Imu(spec))
        }
        BindingPlan::Controller(spec) => {
            let mut spec: AttitudeControllerSpec = spec.clone();
            apply_controller_target(&mut spec, &fault.target, value, &fault.id)?;
            Ok(BindingPlan::Controller(spec))
        }
        BindingPlan::GroundStation(spec) => {
            let mut spec: GroundStationSpec = spec.clone();
            apply_ground_station_target(&mut spec, &fault.target, value, &fault.id)?;
            Ok(BindingPlan::GroundStation(spec))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parameter_fault_on_a_native_binding_changes_only_the_named_axis() {
        let plan = BindingPlan::ConstantAccel(ConstantAccelSpec { a: [1.0, 2.0, 3.0], frame_id: "test.frame".to_string(), x0_si: vec![0.0; 6], ..Default::default() });
        let fault = Fault { id: "f1".to_string(), target: "accel.y".to_string(), kind: "parameter".to_string(), params: std::collections::BTreeMap::from([("value".to_string(), 99.0)]), ..Default::default() };
        let BindingPlan::ConstantAccel(spec) = apply_dynamics_fault(&plan, &fault).unwrap() else { panic!("expected ConstantAccel") };
        assert_eq!(spec.a, [1.0, 99.0, 3.0]);
    }

    fn attitude_spec(n_wheels: usize) -> AttitudeWheelsSpec {
        AttitudeWheelsSpec {
            inertia: [[10.0, 1.0, 2.0], [1.0, 20.0, 3.0], [2.0, 3.0, 30.0]],
            wheel_axes: (0..n_wheels).map(|_| [1.0, 0.0, 0.0]).collect(),
            wheel_momentum_limits: vec![1.0; n_wheels],
            q0: [0.0, 0.0, 0.0, 1.0],
            omega0: [0.0, 0.0, 0.0],
            wheel_available: vec![true; n_wheels],
            wheel_commanded_torque: vec![0.0; n_wheels],
        }
    }

    fn attitude_fault(target: &str, value: f64) -> Fault {
        Fault { id: "f1".to_string(), target: target.to_string(), kind: "parameter".to_string(), target_kind: FaultTargetKind::Dynamics as i32, params: std::collections::BTreeMap::from([("value".to_string(), value)]), ..Default::default() }
    }

    /// M22.1b (question 152): "a DYNAMICS fault on an attitude instance changes inertia" --
    /// a diagonal inertia component. Fails against an implementation missing the
    /// `BindingPlan::Attitude` arm entirely (a compile error) or one that silently no-ops it.
    #[test]
    fn a_dynamics_fault_on_an_attitude_instance_changes_a_diagonal_inertia_component() {
        let plan = BindingPlan::Attitude(attitude_spec(0));
        let BindingPlan::Attitude(spec) = apply_dynamics_fault(&plan, &attitude_fault("attitude.inertia.jxx", 99.0)).unwrap() else { panic!("expected Attitude") };
        assert_eq!(spec.inertia[0][0], 99.0);
        // Every other entry (including the other two diagonal terms and every off-diagonal
        // term) must be untouched -- only the named parameter changes.
        assert_eq!(spec.inertia[1][1], 20.0);
        assert_eq!(spec.inertia[2][2], 30.0);
        assert_eq!(spec.inertia[0][1], 1.0);
    }

    /// An off-diagonal inertia fault (`jxy`/`jxz`/`jyz`) must write **both** symmetric
    /// positions -- fails against an implementation that writes only `inertia[i][j]`, leaving
    /// the tensor asymmetric (a real bug: `AttitudeWheelsModel::new`'s own `mat3_inverse_
    /// symmetric` assumes symmetry and would silently compute a wrong inverse from an
    /// asymmetric tensor, never refusing it).
    #[test]
    fn a_dynamics_fault_on_an_attitude_instance_changes_an_off_diagonal_inertia_component_symmetrically() {
        let plan = BindingPlan::Attitude(attitude_spec(0));
        let BindingPlan::Attitude(spec) = apply_dynamics_fault(&plan, &attitude_fault("attitude.inertia.jxy", 7.0)).unwrap() else { panic!("expected Attitude") };
        assert_eq!(spec.inertia[0][1], 7.0);
        assert_eq!(spec.inertia[1][0], 7.0, "jxy must write both symmetric positions, not only inertia[0][1]");
    }

    /// M22.1b (question 152): "...or wheel limits" -- exit criterion 4's own fault shape
    /// (halving a wheel's momentum limit). Fails against an implementation that maps
    /// `momentum_limit` onto the wrong index (off-by-one on the 1-based wheel index) or onto
    /// `wheel_available` instead.
    #[test]
    fn a_dynamics_fault_on_an_attitude_instance_halves_a_wheel_momentum_limit() {
        let mut spec0 = attitude_spec(2);
        spec0.wheel_momentum_limits = vec![10.0, 20.0];
        let plan = BindingPlan::Attitude(spec0);
        let BindingPlan::Attitude(spec) = apply_dynamics_fault(&plan, &attitude_fault("attitude.wheel.2.momentum_limit", 10.0)).unwrap() else { panic!("expected Attitude") };
        assert_eq!(spec.wheel_momentum_limits, vec![10.0, 10.0], "only wheel 2's own limit must change");
        assert!(spec.wheel_available.iter().all(|a| *a), "a momentum_limit fault must not touch wheel_available");
    }

    /// M22.1b (question 152): "...or a wheel's own availability" -- the third, deliberate
    /// DYNAMICS-fault arm. Fails against an implementation that never added `"available"` to
    /// `apply_attitude_target`'s own match (would fall through to `UnknownParameter` instead of
    /// `Ok`) or one that inverts the `value != 0.0` sense.
    #[test]
    fn a_dynamics_fault_on_an_attitude_instance_disables_a_wheel() {
        let plan = BindingPlan::Attitude(attitude_spec(2));
        let BindingPlan::Attitude(spec) = apply_dynamics_fault(&plan, &attitude_fault("attitude.wheel.1.available", 0.0)).unwrap() else { panic!("expected Attitude") };
        assert_eq!(spec.wheel_available, vec![false, true], "only wheel 1 must be disabled");
        assert_eq!(spec.wheel_momentum_limits, vec![1.0, 1.0], "an availability fault must not touch wheel_momentum_limits");
    }

    #[test]
    fn a_dynamics_fault_on_an_attitude_instance_naming_an_out_of_range_wheel_index_is_a_typed_error() {
        let plan = BindingPlan::Attitude(attitude_spec(1));
        let err = apply_dynamics_fault(&plan, &attitude_fault("attitude.wheel.2.momentum_limit", 5.0)).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { .. }), "{err:?}");
    }

    #[test]
    fn a_dynamics_fault_on_an_attitude_instance_naming_an_unrecognized_target_is_a_typed_error() {
        let plan = BindingPlan::Attitude(attitude_spec(1));
        let err = apply_dynamics_fault(&plan, &attitude_fault("attitude.nonsense", 1.0)).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { .. }), "{err:?}");
        let err2 = apply_dynamics_fault(&plan, &attitude_fault("attitude.wheel.1.nonsense", 1.0)).unwrap_err();
        assert!(matches!(err2, DrmError::UnknownParameter { .. }), "{err2:?}");
    }

    // -- M22.2b (`docs/open-questions.md` questions 142/149/151/152, decided by the lead): "a
    // DYNAMICS fault on a sensor instance perturbs its declared noise parameters" -- the sensor
    // counterpart of the attitude fault tests immediately above. `attitude_fault` is reused
    // unchanged despite its name: it is a generic "build a FAULT_TARGET_KIND_DYNAMICS/
    // \"parameter\" fault naming `target` with `value`" constructor that `apply_dynamics_fault`
    // never inspects `Fault.instance` from, so it is exactly as valid for a star tracker/IMU
    // target as for an attitude one.

    fn star_tracker_spec() -> StarTrackerSpec {
        StarTrackerSpec { update_rate_hz: 2.0, seed: 7, noise_sigma_rad: 1e-5, mount_q: [0.1, 0.2, 0.3, 0.4] }
    }

    fn ground_station_spec() -> GroundStationSpec {
        GroundStationSpec { body: "Earth".to_string(), latitude_rad: 0.5, longitude_rad: -1.4, height_m: 10.0, elevation_mask_rad: 0.1745 }
    }

    /// M25.1: each declared `"station.*"` numeric field is a valid DYNAMICS-fault target,
    /// changing only its own field -- fails against an implementation that maps one target onto
    /// the wrong field, mirroring `a_dynamics_fault_on_an_imu_instance_changes_each_declared_
    /// noise_sigma_independently`'s own per-field proof.
    #[test]
    fn a_dynamics_fault_on_a_ground_station_instance_changes_each_declared_field_independently() {
        fn check(base: &GroundStationSpec, target: &str, get: impl Fn(&GroundStationSpec) -> f64) {
            let plan = BindingPlan::GroundStation(base.clone());
            let BindingPlan::GroundStation(spec) = apply_dynamics_fault(&plan, &attitude_fault(target, 0.2)).unwrap() else { panic!("expected GroundStation") };
            assert_eq!(get(&spec), 0.2, "target {target} must change only its own field");
        }
        let base = ground_station_spec();
        check(&base, "station.latitude_rad", |s| s.latitude_rad);
        check(&base, "station.longitude_rad", |s| s.longitude_rad);
        check(&base, "station.height_m", |s| s.height_m);
        check(&base, "station.elevation_mask_rad", |s| s.elevation_mask_rad);
    }

    /// `station.body` is not a numeric field (`Fault.params["value"]` is always `f64`) -- naming
    /// it, or an outright unrecognized target, is a typed `DrmError::UnknownParameter`, never
    /// silently accepted. Mirrors `a_dynamics_fault_on_a_star_tracker_instance_naming_a_non_
    /// noise_field_is_a_typed_error`'s own proof.
    #[test]
    fn a_dynamics_fault_on_a_ground_station_instance_naming_a_non_numeric_field_is_a_typed_error() {
        let plan = BindingPlan::GroundStation(ground_station_spec());
        for target in ["station.body", "station.nonsense"] {
            let err = apply_dynamics_fault(&plan, &attitude_fault(target, 1.0)).unwrap_err();
            assert!(matches!(err, DrmError::UnknownParameter { ref name, .. } if name == target), "target {target:?}: {err:?}");
        }
    }

    fn imu_spec() -> ImuSpec {
        ImuSpec {
            update_rate_hz: 2.0,
            seed: 9,
            gyro_noise_sigma: 1e-4,
            gyro_bias_rw_sigma: 1e-6,
            accel_noise_sigma: 1e-3,
            accel_bias_rw_sigma: 1e-5,
            mount_q: [0.1, 0.2, 0.3, 0.4],
            true_specific_force: [0.0, 0.0, 9.81],
        }
    }

    /// "...perturbs its declared noise parameters" -- for a star tracker, the one declared
    /// noise field, `startracker.noise_sigma_rad`. Fails against an implementation missing the
    /// `BindingPlan::StarTracker` arm entirely (a compile error, since this match is exhaustive)
    /// or one that silently no-ops it (this test's `unwrap()` would panic on `Err`, or the
    /// equality below would fail against the untouched default).
    #[test]
    fn a_dynamics_fault_on_a_star_tracker_instance_changes_its_declared_noise_sigma() {
        let plan = BindingPlan::StarTracker(star_tracker_spec());
        let BindingPlan::StarTracker(spec) = apply_dynamics_fault(&plan, &attitude_fault("startracker.noise_sigma_rad", 0.01)).unwrap() else { panic!("expected StarTracker") };
        assert_eq!(spec.noise_sigma_rad, 0.01);
        // Every other declared field -- hardware/software configuration, not a physical
        // quantity a DYNAMICS fault perturbs -- must be untouched.
        assert_eq!(spec.update_rate_hz, 2.0);
        assert_eq!(spec.seed, 7);
        assert_eq!(spec.mount_q, [0.1, 0.2, 0.3, 0.4]);
    }

    /// `startracker.update_rate_hz`/`.seed`/`.mount_q` are declared hardware/software
    /// configuration `apply_star_tracker_target`'s own doc comment says this batch does not
    /// model a fault changing mid-run (an update rate does not drift, a mounting angle does not
    /// drift, a seed is not a physical quantity at all) -- naming any of them, or an outright
    /// unrecognized target, is a typed [`DrmError::UnknownParameter`], never silently accepted.
    /// Fails against an implementation that accidentally widens `apply_star_tracker_target`'s
    /// match beyond `"startracker.noise_sigma_rad"`.
    #[test]
    fn a_dynamics_fault_on_a_star_tracker_instance_naming_a_non_noise_field_is_a_typed_error() {
        let plan = BindingPlan::StarTracker(star_tracker_spec());
        for target in ["startracker.update_rate_hz", "startracker.seed", "startracker.mount_q.x", "startracker.nonsense"] {
            let err = apply_dynamics_fault(&plan, &attitude_fault(target, 1.0)).unwrap_err();
            assert!(matches!(err, DrmError::UnknownParameter { ref name, .. } if name == target), "target {target:?}: {err:?}");
        }
    }

    /// The IMU counterpart of the star tracker test above: all four declared noise/bias-
    /// random-walk sigmas (`imu.gyro_noise_sigma`/`.accel_noise_sigma`/`.gyro_bias_rw_sigma`/
    /// `.accel_bias_rw_sigma`) are valid DYNAMICS-fault targets, each changing only its own
    /// field -- fails against an implementation that maps one target onto the wrong field (e.g.
    /// `imu.accel_noise_sigma` silently writing `gyro_noise_sigma` instead).
    #[test]
    fn a_dynamics_fault_on_an_imu_instance_changes_each_declared_noise_sigma_independently() {
        fn check(base: &ImuSpec, target: &str, get: impl Fn(&ImuSpec) -> f64) {
            let plan = BindingPlan::Imu(base.clone());
            let BindingPlan::Imu(spec) = apply_dynamics_fault(&plan, &attitude_fault(target, 42.0)).unwrap() else { panic!("expected Imu") };
            assert_eq!(get(&spec), 42.0, "target {target} must change only its own field");
            // The other three noise sigmas, and every non-noise field, must be untouched.
            assert_eq!(spec.mount_q, base.mount_q);
            assert_eq!(spec.true_specific_force, base.true_specific_force);
        }
        let base = imu_spec();
        check(&base, "imu.gyro_noise_sigma", |s| s.gyro_noise_sigma);
        check(&base, "imu.accel_noise_sigma", |s| s.accel_noise_sigma);
        check(&base, "imu.gyro_bias_rw_sigma", |s| s.gyro_bias_rw_sigma);
        check(&base, "imu.accel_bias_rw_sigma", |s| s.accel_bias_rw_sigma);
    }

    /// The IMU counterpart of the star tracker "non-noise field" refusal test: its own update
    /// rate/seed/mounting quaternion/true specific force are declared configuration, not a
    /// DYNAMICS-fault target (`apply_imu_target`'s own doc comment) -- a typed
    /// `DrmError::UnknownParameter`, never silently accepted.
    #[test]
    fn a_dynamics_fault_on_an_imu_instance_naming_a_non_noise_field_is_a_typed_error() {
        let plan = BindingPlan::Imu(imu_spec());
        for target in ["imu.update_rate_hz", "imu.seed", "imu.mount_q.x", "imu.true_specific_force.z", "imu.nonsense"] {
            let err = apply_dynamics_fault(&plan, &attitude_fault(target, 1.0)).unwrap_err();
            assert!(matches!(err, DrmError::UnknownParameter { ref name, .. } if name == target), "target {target:?}: {err:?}");
        }
    }

    fn controller_spec() -> AttitudeControllerSpec {
        AttitudeControllerSpec { kp: 0.5, kd: 5.0, target_q: [0.0, 0.0, 0.0, 1.0], update_rate_hz: 2.0 }
    }

    /// M22.4: a DYNAMICS fault on a controller instance changes `controller.kp`/`.kd`
    /// independently -- fails against an implementation missing the `BindingPlan::Controller`
    /// arm entirely (a compile error, since `apply_dynamics_fault`'s own match is exhaustive)
    /// or one that silently no-ops it.
    #[test]
    fn a_dynamics_fault_on_a_controller_instance_changes_each_declared_gain_independently() {
        fn check(base: &AttitudeControllerSpec, target: &str, get: impl Fn(&AttitudeControllerSpec) -> f64) {
            let plan = BindingPlan::Controller(base.clone());
            let BindingPlan::Controller(spec) = apply_dynamics_fault(&plan, &attitude_fault(target, 42.0)).unwrap() else { panic!("expected Controller") };
            assert_eq!(get(&spec), 42.0, "target {target:?}");
        }
        let base = controller_spec();
        check(&base, "controller.kp", |s| s.kp);
        check(&base, "controller.kd", |s| s.kd);
    }

    /// `controller.target_q.*`/`.update_rate_hz` are declared configuration, not a fault target
    /// -- mirrors `a_dynamics_fault_on_an_imu_instance_naming_a_non_noise_field_is_a_typed_error`.
    #[test]
    fn a_dynamics_fault_on_a_controller_instance_naming_a_non_gain_field_is_a_typed_error() {
        let plan = BindingPlan::Controller(controller_spec());
        for target in ["controller.target_q.w", "controller.update_rate_hz", "controller.nonsense"] {
            let err = apply_dynamics_fault(&plan, &attitude_fault(target, 1.0)).unwrap_err();
            assert!(matches!(err, DrmError::UnknownParameter { ref name, .. } if name == target), "target {target:?}: {err:?}");
        }
    }

    #[test]
    fn a_non_parameter_fault_kind_is_a_typed_error() {
        let plan = BindingPlan::ConstantAccel(ConstantAccelSpec::default());
        let fault = Fault { id: "f1".to_string(), target: "accel.x".to_string(), kind: "bias".to_string(), params: std::collections::BTreeMap::from([("value".to_string(), 1.0)]), ..Default::default() };
        assert!(apply_dynamics_fault(&plan, &fault).is_err());
    }

    #[test]
    fn rebind_at_state_drops_keplerian_fields_and_sets_cartesian_km() {
        let mut spec = GmatSystemSpec::default();
        spec.spacecraft_real.insert("SMA".to_string(), 6878.0);
        spec.spacecraft_real.insert("ECC".to_string(), 0.001);
        let state_si = [7_000_000.0, 0.0, 0.0, 0.0, 7_500.0, 0.0];
        let rebound = rebind_gmat_spec_at_state(&spec, state_si);
        assert!(!rebound.spacecraft_real.contains_key("SMA"));
        assert!(!rebound.spacecraft_real.contains_key("ECC"));
        assert_eq!(rebound.spacecraft_real["X"], 7000.0);
        assert_eq!(rebound.spacecraft_real["VY"], 7.5);
        assert_eq!(rebound.spacecraft_str["DisplayStateType"], "Cartesian");
    }

    fn port_fault(id: &str, kind: &str) -> Fault {
        Fault { id: id.to_string(), target_kind: FaultTargetKind::Port as i32, instance: "veh".to_string(), target: "link.veh_gs".to_string(), kind: kind.to_string(), ..Default::default() }
    }
    fn sensor_fault(id: &str, kind: &str) -> Fault {
        Fault { id: id.to_string(), target_kind: FaultTargetKind::Sensor as i32, instance: "veh".to_string(), target: "sensor.imu".to_string(), kind: kind.to_string(), ..Default::default() }
    }

    #[test]
    fn a_seeded_port_fault_is_refused_explicitly_not_silently_skipped() {
        let seeds = BTreeMap::from([("f_port".to_string(), 42u64)]);
        let err = realize_unapplied_fault(&port_fault("f_port", "drop"), &seeds);
        assert!(matches!(err, DrmError::FaultTargetKindNotSupported { ref fault_id, ref target_kind, .. } if fault_id == "f_port" && target_kind == "FAULT_TARGET_KIND_PORT"), "{err:?}");
    }

    #[test]
    fn a_seeded_sensor_fault_is_refused_explicitly_not_silently_skipped() {
        let seeds = BTreeMap::from([("f_sensor".to_string(), 7u64)]);
        let err = realize_unapplied_fault(&sensor_fault("f_sensor", "bias"), &seeds);
        assert!(matches!(err, DrmError::FaultTargetKindNotSupported { ref fault_id, ref target_kind, .. } if fault_id == "f_sensor" && target_kind == "FAULT_TARGET_KIND_SENSOR"), "{err:?}");
    }

    #[test]
    fn the_refusal_carries_the_deterministic_draw_reproducibly() {
        let seeds = BTreeMap::from([("f1".to_string(), 123u64)]);
        let (DrmError::FaultTargetKindNotSupported { realized_draw: d1, .. }, DrmError::FaultTargetKindNotSupported { realized_draw: d2, .. }) =
            (realize_unapplied_fault(&port_fault("f1", "corrupt"), &seeds), realize_unapplied_fault(&port_fault("f1", "corrupt"), &seeds))
        else {
            panic!("expected FaultTargetKindNotSupported both times");
        };
        assert_eq!(d1, d2, "the same fault id/seed must draw the same value every time");
        assert_eq!(d1, Pcg64::new(123).next_u64(), "must be exactly the fault's own seeded stream's first draw");
    }

    #[test]
    fn an_unrecognized_port_kind_is_a_typed_error_before_any_seed_lookup() {
        let seeds = BTreeMap::new(); // no seed for "f1" either -- kind is checked first.
        let err = realize_unapplied_fault(&port_fault("f1", "not_a_real_kind"), &seeds);
        assert!(matches!(err, DrmError::UnknownParameter { .. }), "{err:?}");
    }

    #[test]
    fn a_port_fault_missing_its_scenario_seed_is_a_typed_error() {
        let seeds = BTreeMap::new();
        let err = realize_unapplied_fault(&port_fault("f_unseeded", "drop"), &seeds);
        assert!(matches!(err, DrmError::MissingFaultSeed { ref fault_id } if fault_id == "f_unseeded"), "{err:?}");
    }

    #[test]
    #[should_panic(expected = "only PORT/SENSOR are supported")]
    fn calling_realize_unapplied_fault_on_a_dynamics_fault_panics() {
        let fault = Fault { id: "f1".to_string(), target_kind: FaultTargetKind::Dynamics as i32, kind: "parameter".to_string(), ..Default::default() };
        let _ = realize_unapplied_fault(&fault, &BTreeMap::new());
    }

    #[test]
    fn epoch_id_order_sorts_by_epoch_then_by_id() {
        let mut faults = [
            Fault { id: "b".to_string(), tai_ns: 100, ..Default::default() },
            Fault { id: "a".to_string(), tai_ns: 100, ..Default::default() },
            Fault { id: "z".to_string(), tai_ns: 50, ..Default::default() },
        ];
        faults.sort_by_key(epoch_id_order);
        let order: Vec<&str> = faults.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(order, ["z", "a", "b"], "earlier epoch first; a same-epoch tie breaks by id");
    }

    /// M16.2 (question 120): `is_container_power_cycle` must key off `FAULT_TARGET_KIND_HARDWARE`
    /// now, not `_DYNAMICS` -- would fail against the pre-M16.2 implementation (which returned
    /// `true` for a DYNAMICS/"power_cycle" fault and `false` here for the HARDWARE one, exactly
    /// backwards from what this asserts).
    #[test]
    fn is_container_power_cycle_matches_hardware_power_cycle_only() {
        let hardware_power_cycle = Fault { id: "f1".to_string(), target_kind: FaultTargetKind::Hardware as i32, kind: "power_cycle".to_string(), ..Default::default() };
        assert!(is_container_power_cycle(&hardware_power_cycle));

        let dynamics_power_cycle = Fault { id: "f2".to_string(), target_kind: FaultTargetKind::Dynamics as i32, kind: "power_cycle".to_string(), ..Default::default() };
        assert!(!is_container_power_cycle(&dynamics_power_cycle), "the interim DYNAMICS shape must no longer match");

        let hardware_other_kind = Fault { id: "f3".to_string(), target_kind: FaultTargetKind::Hardware as i32, kind: "board_reset".to_string(), ..Default::default() };
        assert!(!is_container_power_cycle(&hardware_other_kind), "only kind == \"power_cycle\" matches");
    }

    /// The predicate `executor::execute` uses to recognize and refuse the retired M15.3 shape --
    /// would fail against an implementation that never added this function (nothing to import)
    /// or one that aliased it to `is_container_power_cycle` post-swap (which would then be
    /// `false` here, since that predicate now requires HARDWARE).
    #[test]
    fn is_legacy_dynamics_power_cycle_matches_only_the_retired_shape() {
        let legacy = Fault { id: "f1".to_string(), target_kind: FaultTargetKind::Dynamics as i32, kind: "power_cycle".to_string(), ..Default::default() };
        assert!(is_legacy_dynamics_power_cycle(&legacy));

        let current = Fault { id: "f2".to_string(), target_kind: FaultTargetKind::Hardware as i32, kind: "power_cycle".to_string(), ..Default::default() };
        assert!(!is_legacy_dynamics_power_cycle(&current), "the new HARDWARE shape must not be flagged as the legacy one");

        let ordinary_parameter_fault = Fault { id: "f3".to_string(), target_kind: FaultTargetKind::Dynamics as i32, kind: "parameter".to_string(), ..Default::default() };
        assert!(!is_legacy_dynamics_power_cycle(&ordinary_parameter_fault));
    }
}
