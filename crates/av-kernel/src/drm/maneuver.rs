//! Applying `Scenario.events` of kind `"maneuver"` (`ScenarioEvent`, `docs/open-questions.md`
//! question 97): an impulsive delta-v applied to one `SystemInstance`'s velocity at an exact
//! epoch, between propagation segments -- the same "split the run at this epoch, re-bind from
//! the segment's own final physical state" shape `super::fault` already gives
//! `FAULT_TARGET_KIND_DYNAMICS` faults (see that module's doc comment), except a maneuver
//! changes the *state* (a velocity jump), never the dynamics configuration itself, so no
//! `BindingPlan` change accompanies it -- `executor::materialize_plan_at_boundary` re-binds the
//! same, unmodified plan at the new (post-burn) state.
//!
//! ## Schema (question 97's "typed, not opaque")
//!
//! `ScenarioEvent` (`proto/altavista/v1/system.proto`) is generic (`kind` string, `values`/
//! `attributes` maps) -- there is no dedicated maneuver message, and none is authorized
//! (`docs/open-questions.md` question 97's own note: no proto change is needed and none is
//! authorized). [`parse`] is this crate's one place that gives that generic shape a typed
//! maneuver contract, called both by `crate::drm::schema` (load time, before any propagation --
//! a malformed event is a typed [`DrmError`], never silently dropped) and by [`super::executor`]
//! (run time, to extract the actual delta-v/frame to apply, so the contract is enforced exactly
//! once either way). A `maneuver` event must have:
//!
//! - `kind == "maneuver"` (any other value is [`DrmError::UnsupportedScenarioEventKind`] --
//!   `"mode"`/`"contact"`/`"custom"`, `ScenarioEvent.kind`'s own doc comment's other named
//!   values, are not modeled by this task and are refused, not silently accepted as opaque).
//! - `instance` non-empty (which `SystemInstance` the burn applies to -- mirrors `Fault.
//!   instance`).
//! - `values` containing *exactly* three keys, `"dv_x"`/`"dv_y"`/`"dv_z"`: the delta-v's three
//!   components, SI m/s, in the declared frame's own basis order (VNB: V, N, B; RIC: R, I, C;
//!   VVLH: X, Y, Z per `AxesKind::Vvlh`'s own doc comment; inertial: the trajectory's own frame
//!   axes directly). Any other key present is [`DrmError::UnknownParameter`]; any of the three
//!   missing is [`DrmError::MissingParameter`].
//! - `attributes` containing *exactly* one key, `"frame_id"`, whose value is one of
//!   `AXES_KIND_RIC`/`AXES_KIND_VNB`/`AXES_KIND_VVLH`/`AXES_KIND_ICRF`/`AXES_KIND_MJ2000_EQ`
//!   (the proto enum's own name -- `crate::drm::schema`'s existing convention for every other
//!   enum field this loader parses -- [`DrmError::InvalidEnumValue`] for an unrecognized name,
//!   [`DrmError::ManeuverFrameNotSupported`] for a real `AxesKind` this executor has no burn
//!   realization for, e.g. `AXES_KIND_BODY_FIXED`). Any other attribute key present is
//!   [`DrmError::UnknownParameter`].
//!
//! `tai_ns` and `id` need no extra validation here beyond the struct's own bare fields --
//! `executor::execute` checks `tai_ns` against the instance's own output sampling grid
//! (`DrmError::ManeuverEpochNotOnSampleGrid`, modeled on `DrmError::FaultEpochNotOnSampleGrid`)
//! and `instance` against the real `SosConfiguration.instances` list
//! (`DrmError::UnknownManeuverInstance`), neither of which this module -- which never sees a
//! `Scenario` or `SosConfiguration` as a whole -- can check.
//!
//! ## Frame realization (pinned against `altavista.scenario.Scenario.maneuver`, question 97)
//!
//! [`dv_to_inertial`] builds the V/N/B, R/I/C, or VVLH basis from the instance's own physical
//! state (SI m, m/s) at the burn epoch and expresses the declared `dv` in the trajectory's own
//! (inertial) frame -- see that function's own doc comment for the exact basis construction,
//! pinned field-for-field against `altavista/scenario.py::Scenario.maneuver`'s VNB path (the
//! reference implementation `goldens/gen_leo_1day_maneuver_vnb.py` runs, through the real
//! `altavista.Scenario.maneuver` call, to generate this task's golden) and
//! `proto/altavista/v1/core.proto`'s `AxesKind` doc comment (RIC/VVLH, ratified question 73).
//! Only VNB is empirically pinned against altavista's own reference implementation -- altavista's
//! `Scenario.maneuver` itself only ever realizes `"VNB"` or `"inertial"` (see that method's own
//! docstring); RIC and VVLH are implemented from the same ratified `AxesKind` convention VNB
//! reuses (`R`/`N`/`B` are shared building blocks) and covered by this module's own orthonormal-
//! basis unit tests, not by an external golden -- see this crate's own task report for the
//! honest statement of that scope limit.
//!
//! ## Burn execution error (question 100, M11.4): the Gates model
//!
//! `docs/open-questions.md` question 97 scoped burn execution error out of that task, leaving
//! `[`dv_to_inertial`]` applying exactly the declared `dv` -- no dispersion, no pointing/
//! magnitude error, `Phi = I` and `P` unchanged across every burn (`executor`'s own "Covariance
//! across a burn" doc comment section). Question 100 (user decision 2026-09-03) closes that gap
//! with the **Gates model** (S. Gates, *"A Simplified Model of Midcourse Maneuver Execution
//! Errors,"* JPL Technical Report 32-1234, 1963): the classic four-sigma impulsive-burn
//! dispersion model that splits execution error into a *magnitude* term (along the commanded
//! `dv`'s own direction) and a *pointing* term (in the plane transverse to it), each with a
//! fixed and a proportional-to-`|dv|` component. `ScenarioEvent.execution_error`
//! (`ManeuverExecutionError`, `proto/altavista/v1/system.proto` field 7, added by the lead) is
//! this crate's typed carrier for the model's four sigmas plus a `seed` key into
//! `Scenario.seeds`; **absent means a perfect burn and is never defaulted** -- the zero-sigma
//! case (an explicit, present block whose four sigmas are all `0.0`) is different from an
//! absent block only in that it is present at all; both leave the applied `dv` numerically
//! identical to the commanded one (see [`gates_sigmas`]'s doc comment for why: a variance of
//! exactly `0.0` draws `0.0` every time, and the analytic injection adds a zero matrix).
//!
//! **The model, exactly.** For a commanded `dv` of magnitude `v = |dv|` and unit direction
//! `u = dv / v`, completed into an orthonormal triad `(u, p1, p2)` by [`burn_triad`]:
//!
//! - magnitude variance `sigma_m^2 = sigma_magnitude_fixed_mps^2 + (sigma_magnitude_proportional
//!   * v)^2`
//! - per-axis pointing variance `sigma_p^2 = sigma_pointing_fixed_mps^2 +
//!   (sigma_pointing_proportional_rad * v)^2` (the same `sigma_p` for both `p1` and `p2` --
//!   the model has no separate "up"/"down" pointing sigma)
//!
//! computed by [`gates_sigmas`]. **Two paths, selected by [`ExecutionErrorMode`] on
//! `executor::RunConfig`** (`docs/open-questions.md` question 103, lead review of M11.4): **not**
//! inferred from whether `DrmOptions.covariance` happens to be set. M11.4's first cut got this
//! wrong -- it picked the path by the run's *shape* (plain run samples, covariance run injects),
//! so a designer's nominal single run that happened to carry an execution-error block silently
//! got a dispersed burn, and a Monte Carlo sweep run against a covariance instance would have
//! gotten an analytic injection instead of a realized draw. [`ExecutionErrorMode`] closes that
//! gap: the caller states the mode explicitly, and [`dv_to_apply`] (called identically by
//! `executor::run_plain_instance` and `executor::run_covariance_instance`) is the one place that
//! branches on it, so neither executor function infers anything from its own shape.
//!
//! - **(a) [`ExecutionErrorMode::Sampled`]** ([`sample_execution_error`] via [`dv_to_apply`] --
//!   a single realized trajectory, exactly the shape a Monte Carlo sweep draw needs, on *either*
//!   the plain or the covariance path): draw three standard normals `z0, z1, z2` from
//!   `crate::rng::event_rng(base_seed, event_id)` (`base_seed` = `Scenario.seeds[execution_error
//!   .seed]`, `event_id` = the `ScenarioEvent.id` -- **a fresh substream per event id**, so
//!   adding or removing an unrelated event, even one sharing the same `Scenario.seeds` key,
//!   never shifts this event's own draws; see `crate::rng::event_rng`'s own doc comment and
//!   `crate::rng::tests::event_substream_is_independent_of_other_events` /
//!   `crates/av-kernel/tests/gates_execution_error.rs`'s end-to-end proof through the full
//!   executor). The applied `dv` is `dv + z0*sigma_m*u + z1*sigma_p*p1 + z2*sigma_p*p2`
//!   (declared-frame basis, the same order as `dv` itself) -- the caller then runs
//!   [`dv_to_inertial`] on *that* perturbed vector, exactly as it would the bare commanded one.
//!   **Nothing is injected into `P`** on this path, even when covariance is on: the dispersion
//!   is already realized in the applied `dv`/mean, so an additional `G Q G^T` term would
//!   double-count the same uncertainty. The MANEUVER event's `values` record both the commanded
//!   and applied vectors and the three raw draws (`events::maneuver_event`'s `sampled`
//!   parameter).
//! - **(b) [`ExecutionErrorMode::Nominal`]** (the default): the commanded `dv` is applied
//!   **exactly** on *either* path (no perturbation -- [`dv_to_apply`] returns `m.dv` unchanged
//!   regardless of whether `execution_error` is declared). On the covariance path only,
//!   `P+ = P- + G Q G^T` is additionally injected into the covariance at the burn epoch when
//!   `execution_error` is declared ([`inject_gates_covariance`]), where `G` maps `(u, p1, p2)`
//!   (rotated into the trajectory's own inertial frame by [`inertial_triad`], the same rotation
//!   [`dv_to_inertial`] itself applies) into the velocity block of the state and
//!   `Q = diag(sigma_m^2, sigma_p^2, sigma_p^2)`. Because `(u, p1, p2)` is orthonormal,
//!   `G Q G^T`'s only nonzero block is `sigma_m^2 (u u^T) + sigma_p^2 (p1 p1^T + p2 p2^T)`,
//!   added into the velocity-velocity 3x3 block (state indices 3..6, this crate's fixed
//!   Cartesian convention -- see [`inject_gates_covariance`]'s own doc comment). A `Nominal`
//!   *plain* run therefore has no `P` to inject into, so a declared `execution_error` block has
//!   **no effect at all** on a `Nominal` plain run's trajectory -- it must reproduce the
//!   perfect-burn golden bit-for-bit (`tests/gates_execution_error.rs`'s own required test).
//!   **`Phi` is not modified** by the injection -- only `P` changes at the boundary, exactly as
//!   the no-execution-error case already established (`executor::run_covariance_instance`'s
//!   "Covariance across a burn" doc comment).
//!
//! **Loader (`schema.rs`).** `RawManeuverExecutionError` mirrors `ManeuverExecutionError`
//! field-for-field. All four sigmas are required and checked finite when the block is present
//! ([`parse`]'s `execution_error` handling, [`DrmError::InvalidManeuverExecutionError`]) -- a
//! zero is an explicit zero (parses and runs as the zero-sigma case above), never an implicit
//! default (the block is `Option<ManeuverExecutionError>`; `None` is the only way to declare a
//! perfect burn). `seed` must name a real key in `Scenario.seeds`
//! ([`validate_execution_error_seed`], [`DrmError::UnknownManeuverSeed`]) -- checked at load
//! (`schema::RawScenario::into_pb`, which has `Scenario.seeds` in scope) and again at run time
//! (`executor::execute`, for a `Scenario` built directly, bypassing the loader), the same
//! "validated once, callable from either entry point" shape [`parse`] itself already has.

use std::collections::BTreeMap;

use av_cdm::pb::{AxesKind, ManeuverExecutionError, ScenarioEvent};

use super::DrmError;

/// `ScenarioEvent.kind`'s only value this module models (see the module doc comment).
pub const MANEUVER_KIND: &str = "maneuver";

const DV_KEYS: [&str; 3] = ["dv_x", "dv_y", "dv_z"];
const FRAME_ATTR_KEY: &str = "frame_id";

/// Frames this executor can actually realize a burn in -- RIC/VNB/VVLH (`AxesKind`'s own
/// `ObjectReferenced` trio, question 10) plus the two inertial axes kinds a CDM v1 trajectory's
/// state is ever actually expressed in (`ICRF`, `MJ2000Eq` -- every `"gmat."`/native binding in
/// this crate propagates in an inertial frame; see `binding`'s module doc comment), realized as
/// a no-op transform. Any other `AxesKind` (body-fixed, ENU/NED, platform body, local cartesian)
/// is a typed [`DrmError::ManeuverFrameNotSupported`] -- this task's own scope ("RIC / VNB /
/// VVLH or the inertial frame"), not a silent fallback to one of these five.
const SUPPORTED_FRAMES: [AxesKind; 5] = [AxesKind::Icrf, AxesKind::Mj2000Eq, AxesKind::Ric, AxesKind::Vnb, AxesKind::Vvlh];

/// A validated `ManeuverExecutionError` (question 100) -- see the module doc comment's "Burn
/// execution error" section. All four sigmas have already been checked finite by [`parse`];
/// `seed`'s existence in `Scenario.seeds` is checked separately by
/// [`validate_execution_error_seed`] (which needs `Scenario.seeds` in scope, unlike `parse`,
/// which only ever sees one bare `ScenarioEvent`).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedExecutionError {
    pub sigma_magnitude_fixed_mps: f64,
    pub sigma_magnitude_proportional: f64,
    pub sigma_pointing_fixed_mps: f64,
    pub sigma_pointing_proportional_rad: f64,
    /// Key into `Scenario.seeds`.
    pub seed: String,
}

/// A `maneuver` `ScenarioEvent`, typed (see the module doc comment's "Schema" section).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedManeuver {
    pub id: String,
    pub tai_ns: i64,
    pub instance: String,
    /// The three declared delta-v components, SI m/s, in `axes`'s own basis order.
    pub dv: [f64; 3],
    pub axes: AxesKind,
    /// Question 100: `None` is a perfect burn (never defaulted -- see the module doc comment).
    pub execution_error: Option<ParsedExecutionError>,
}

fn context(id: &str) -> String {
    format!("scenario event {id:?}")
}

/// Parse and validate `event` as a `maneuver` `ScenarioEvent` -- see the module doc comment's
/// "Schema" section for the exact contract. Called from `crate::drm::schema` (load time) and
/// `super::executor` (run time), so the check happens exactly once in one place either way.
pub fn parse(event: &ScenarioEvent) -> Result<ParsedManeuver, DrmError> {
    if event.kind != MANEUVER_KIND {
        return Err(DrmError::UnsupportedScenarioEventKind { id: event.id.clone(), kind: event.kind.clone() });
    }
    if event.instance.is_empty() {
        return Err(DrmError::MissingParameter { context: context(&event.id), name: "instance".to_string() });
    }

    let mut dv = [0.0; 3];
    for (i, key) in DV_KEYS.iter().enumerate() {
        dv[i] = *event
            .values
            .get(*key)
            .ok_or_else(|| DrmError::MissingParameter { context: format!("{} values", context(&event.id)), name: (*key).to_string() })?;
    }
    if let Some(unknown) = event.values.keys().find(|k| !DV_KEYS.contains(&k.as_str())) {
        return Err(DrmError::UnknownParameter { context: format!("{} values", context(&event.id)), name: unknown.clone() });
    }

    let frame_name = event
        .attributes
        .get(FRAME_ATTR_KEY)
        .ok_or_else(|| DrmError::MissingParameter { context: context(&event.id), name: format!("attributes[{FRAME_ATTR_KEY:?}]") })?;
    if let Some(unknown) = event.attributes.keys().find(|k| k.as_str() != FRAME_ATTR_KEY) {
        return Err(DrmError::UnknownParameter { context: format!("{} attributes", context(&event.id)), name: unknown.clone() });
    }
    let axes = AxesKind::from_str_name(frame_name).ok_or_else(|| DrmError::InvalidEnumValue { field: "scenario_event.attributes[frame_id]", value: frame_name.clone() })?;
    if !SUPPORTED_FRAMES.contains(&axes) {
        return Err(DrmError::ManeuverFrameNotSupported { id: event.id.clone(), frame: frame_name.clone() });
    }

    let execution_error = event.execution_error.as_ref().map(|ee| parse_execution_error(&event.id, ee)).transpose()?;

    Ok(ParsedManeuver { id: event.id.clone(), tai_ns: event.tai_ns, instance: event.instance.clone(), dv, axes, execution_error })
}

/// Validate one declared `ManeuverExecutionError` block (question 100's "all four sigmas
/// required and finite when the block is present" -- see the module doc comment's "Loader"
/// section). Does not check `seed` against `Scenario.seeds` -- that needs the surrounding
/// `Scenario` in scope; see [`validate_execution_error_seed`].
fn parse_execution_error(id: &str, ee: &ManeuverExecutionError) -> Result<ParsedExecutionError, DrmError> {
    for (name, v) in [
        ("sigma_magnitude_fixed_mps", ee.sigma_magnitude_fixed_mps),
        ("sigma_magnitude_proportional", ee.sigma_magnitude_proportional),
        ("sigma_pointing_fixed_mps", ee.sigma_pointing_fixed_mps),
        ("sigma_pointing_proportional_rad", ee.sigma_pointing_proportional_rad),
    ] {
        if !v.is_finite() {
            return Err(DrmError::InvalidManeuverExecutionError { id: id.to_string(), reason: format!("{name} must be finite, got {v}") });
        }
    }
    Ok(ParsedExecutionError {
        sigma_magnitude_fixed_mps: ee.sigma_magnitude_fixed_mps,
        sigma_magnitude_proportional: ee.sigma_magnitude_proportional,
        sigma_pointing_fixed_mps: ee.sigma_pointing_fixed_mps,
        sigma_pointing_proportional_rad: ee.sigma_pointing_proportional_rad,
        seed: ee.seed.clone(),
    })
}

/// Question 100's "`seed` must name a key in `Scenario.seeds`, else a typed load error" --
/// unlike a PORT/SENSOR fault's missing seed (only ever discovered at `fault::
/// realize_unapplied_fault`'s own run time, because that module never sees a whole `Scenario`),
/// a maneuver's `Scenario.seeds` is fully known at load time, so this is checked there, not
/// deferred. Called from `schema::RawScenario::into_pb` (load time) and `executor::execute`
/// (run time, for a `Scenario` built directly, bypassing the loader) -- see the module doc
/// comment's "Loader" section.
pub fn validate_execution_error_seed(m: &ParsedManeuver, seeds: &BTreeMap<String, u64>) -> Result<(), DrmError> {
    if let Some(ee) = &m.execution_error {
        if !seeds.contains_key(&ee.seed) {
            return Err(DrmError::UnknownManeuverSeed { id: m.id.clone(), seed: ee.seed.clone() });
        }
    }
    Ok(())
}

/// Comparison key for ADR-005 section 5's `(epoch, id)` fault-application order, applied to
/// maneuvers too (`super::executor`'s boundary-merging loop uses this and
/// `super::fault::epoch_id_order` together to sort faults and maneuvers into one list).
pub fn epoch_id_order(m: &ParsedManeuver) -> (i64, String) {
    (m.tai_ns, m.id.clone())
}

fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}
fn scale(v: [f64; 3], s: f64) -> [f64; 3] {
    [v[0] * s, v[1] * s, v[2] * s]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn unit(v: [f64; 3]) -> [f64; 3] {
    scale(v, 1.0 / norm(v))
}
fn add3(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0] + c[0], a[1] + b[1] + c[1], a[2] + b[2] + c[2]]
}

/// Express `dv` (declared in `axes`'s own basis order -- see the module doc comment) in the
/// inertial frame `r`/`v` (SI m, m/s) are themselves expressed in.
///
/// **VNB** (`AxesKind::Vnb`, GMAT `ObjectReferenced` `X = V, Y = N`): `V = v/|v|`,
/// `N = (r x v)/|r x v|`, `B = V x N` -- pinned field-for-field against
/// `altavista/scenario.py::Scenario.maneuver`'s own `frame.upper() == "VNB"` branch (`vn`/`V`/`h`/
/// `hn`/`N`/`B` there are exactly this function's `norm(v)`/`V`/`cross(r,v)`/`norm(h)`/`N`/`B`).
///
/// **RIC** (`AxesKind::Ric`, `X = R, Z = N`): `R = r/|r|`, `N` as above, `Y = N x R` (the
/// in-track direction that completes the right-handed triad -- `core.proto`'s own
/// `AxesKind::Ric` doc comment).
///
/// **VVLH** (`AxesKind::Vvlh`, `Z = -R, Y = -N, X = N x R`, ratified question 73):
/// `dv = dv_x (N x R) + dv_y (-N) + dv_z (-R)`.
///
/// **ICRF/MJ2000Eq**: identity -- `r`/`v` are already expressed in one of these two inertial
/// axes kinds, so a `dv` declared in either is applied unrotated.
///
/// # Panics
///
/// If `axes` is not one of the five frames [`parse`] accepts -- every caller reaches this
/// function only with an `axes` `parse` already validated.
pub fn dv_to_inertial(axes: AxesKind, dv: [f64; 3], r: [f64; 3], v: [f64; 3]) -> [f64; 3] {
    match axes {
        AxesKind::Icrf | AxesKind::Mj2000Eq => dv,
        AxesKind::Vnb => {
            let vhat = unit(v);
            let n = unit(cross(r, v));
            let b = cross(vhat, n);
            add3(scale(vhat, dv[0]), scale(n, dv[1]), scale(b, dv[2]))
        }
        AxesKind::Ric => {
            let rhat = unit(r);
            let n = unit(cross(r, v));
            let in_track = cross(n, rhat);
            add3(scale(rhat, dv[0]), scale(in_track, dv[1]), scale(n, dv[2]))
        }
        AxesKind::Vvlh => {
            let rhat = unit(r);
            let n = unit(cross(r, v));
            let in_track = cross(n, rhat);
            add3(scale(in_track, dv[0]), scale(n, -dv[1]), scale(rhat, -dv[2]))
        }
        other => panic!("dv_to_inertial called with unsupported AxesKind {other:?}; callers must validate with maneuver::parse first"),
    }
}

// ============================================================================================
// Gates model (question 100) -- see the module doc comment's "Burn execution error" section.
// ============================================================================================

/// The Gates model's magnitude and pointing 1-sigmas for a commanded burn of magnitude `v =
/// |dv|` (module doc comment's "The model, exactly"): `sigma_m^2 = sigma_magnitude_fixed_mps^2 +
/// (sigma_magnitude_proportional * v)^2`, `sigma_p^2 = sigma_pointing_fixed_mps^2 +
/// (sigma_pointing_proportional_rad * v)^2`. A zero-sigma block (all four fields `0.0`) returns
/// `(0.0, 0.0)` regardless of `v` -- the zero-sigma case that must reproduce a perfect burn
/// exactly.
pub fn gates_sigmas(ee: &ParsedExecutionError, v: f64) -> (f64, f64) {
    let sigma_m = ee.sigma_magnitude_fixed_mps.hypot(ee.sigma_magnitude_proportional * v);
    let sigma_p = ee.sigma_pointing_fixed_mps.hypot(ee.sigma_pointing_proportional_rad * v);
    (sigma_m, sigma_p)
}

/// Complete `dv` into a right-handed orthonormal triad `(u, p1, p2)` with `u = dv / |dv|` (the
/// module doc comment's "The model, exactly"). `p1`/`p2` are an arbitrary (but deterministic,
/// reproducible) completion of the plane transverse to `u` -- the Gates model's pointing sigma
/// is isotropic in that plane (the same `sigma_p` for both axes), so which specific orthonormal
/// pair spans it does not matter physically, only that the same pair is used consistently
/// between the sampled path ([`sample_execution_error`]) and the analytic injection
/// ([`inject_gates_covariance`] via [`inertial_triad`]).
///
/// **Degenerate `dv = 0` case** (an explicit, stated approximation -- not expected in a real
/// DRM, since a zero-magnitude commanded burn has no "along" direction to build `u` from):
/// falls back to the fixed canonical triad `([1,0,0], [0,1,0], [0,0,1])` rather than panicking
/// or dividing by zero, so the model stays total and deterministic even for this edge case.
///
/// Robust basis completion: pick whichever standard basis vector is *least* aligned with `u`
/// (so the Gram-Schmidt step below is never near-degenerate), then orthonormalize against `u`.
fn burn_triad(dv: [f64; 3]) -> ([f64; 3], [f64; 3], [f64; 3]) {
    let v = norm(dv);
    if v == 0.0 {
        return ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
    }
    let u = scale(dv, 1.0 / v);
    let seed = if u[0].abs() <= u[1].abs() && u[0].abs() <= u[2].abs() {
        [1.0, 0.0, 0.0]
    } else if u[1].abs() <= u[2].abs() {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, 1.0]
    };
    let p1 = unit(cross(u, seed));
    let p2 = cross(u, p1);
    (u, p1, p2)
}

/// [`burn_triad`]'s `(u, p1, p2)`, built from the *declared*-frame `dv` and then rotated into
/// the same inertial frame [`dv_to_inertial`] itself expresses `r`/`v` in -- `dv_to_inertial` is
/// a linear rotation (no translation, no rescaling by `dv`'s own magnitude: it is exactly
/// `dv[0]*e1 + dv[1]*e2 + dv[2]*e3` for the frame's own basis vectors `e1,e2,e3`), so applying
/// it to each of `u`/`p1`/`p2` individually gives their correct inertial-frame images while
/// preserving orthonormality (a rotation preserves inner products). Used by
/// [`super::executor::run_covariance_instance`] to build `G` for the analytic injection
/// (module doc comment's path (b)) -- `P` is expressed in the trajectory's own inertial frame,
/// so the triad multiplying `Q` into it must be too.
pub fn inertial_triad(axes: AxesKind, dv: [f64; 3], r: [f64; 3], v: [f64; 3]) -> ([f64; 3], [f64; 3], [f64; 3]) {
    let (u, p1, p2) = burn_triad(dv);
    (dv_to_inertial(axes, u, r, v), dv_to_inertial(axes, p1, r, v), dv_to_inertial(axes, p2, r, v))
}

/// The result of sampling the Gates model once for one maneuver -- module doc comment's path
/// (a). `applied_dv` is in the same declared-frame basis/order as `ParsedManeuver.dv`, ready for
/// the caller's own [`dv_to_inertial`] call; `draws` are the three raw standard normals
/// (`z0` magnitude, `z1`/`z2` pointing), recorded verbatim on the MANEUVER event
/// (`events::maneuver_event`'s `sampled` parameter) so a sweep's realized dispersion is
/// auditable, not just its numeric effect.
pub struct SampledDv {
    pub applied_dv: [f64; 3],
    pub draws: [f64; 3],
}

/// Sample one Gates-model realization of `m`'s commanded `dv` (module doc comment's path (a)),
/// drawing three standard normals from `crate::rng::event_rng(base_seed, &m.id)` -- see that
/// function's own doc comment for why this is "a fresh substream per event id." `base_seed` is
/// the caller's own `Scenario.seeds[ee.seed]` lookup (already validated to exist by
/// [`validate_execution_error_seed`] before any instance runs); this function does not repeat
/// that lookup.
pub fn sample_execution_error(m: &ParsedManeuver, ee: &ParsedExecutionError, base_seed: u64) -> SampledDv {
    let mut rng = crate::rng::event_rng(base_seed, &m.id);
    let v = norm(m.dv);
    let (sigma_m, sigma_p) = gates_sigmas(ee, v);
    let draws = [rng.standard_normal(), rng.standard_normal(), rng.standard_normal()];
    let (u, p1, p2) = burn_triad(m.dv);
    let delta = add3(scale(u, draws[0] * sigma_m), scale(p1, draws[1] * sigma_p), scale(p2, draws[2] * sigma_p));
    SampledDv { applied_dv: [m.dv[0] + delta[0], m.dv[1] + delta[1], m.dv[2] + delta[2]], draws }
}

/// `docs/open-questions.md` question 103: which of the Gates model's two paths (module doc
/// comment's "Burn execution error" section) an executor run realizes a declared
/// `execution_error` block through -- explicit on `executor::RunConfig`, defaulting to
/// [`ExecutionErrorMode::Nominal`], and **never inferred from whether `DrmOptions.covariance`
/// happens to be set**. That inference is exactly the M11.4 bug this field exists to close: the
/// first cut picked the path by the run's own shape (plain run samples, covariance run injects),
/// so a designer's nominal single run that happened to carry an `execution_error` block silently
/// got a dispersed burn. [`dv_to_apply`] is the one place that branches on this value; both
/// `executor::run_plain_instance` and `executor::run_covariance_instance` call it identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionErrorMode {
    /// The commanded `dv` is applied exactly, on either path. A covariance run additionally
    /// injects `P+ = P- + G Q G^T` at the burn boundary when `execution_error` is declared; a
    /// plain (non-covariance) run has no `P` to inject into, so a declared block has **no
    /// effect at all** on a `Nominal` plain run's trajectory.
    #[default]
    Nominal,
    /// One Gates-model realization is drawn ([`sample_execution_error`]) and the *drawn* `dv` is
    /// applied, on either path, and **nothing is injected into `P`** on the covariance path --
    /// the dispersion this draw represents is already realized in the applied `dv`/mean, so
    /// injecting `G Q G^T` on top would double-count the same uncertainty. A `Sampled` run with
    /// no declared `execution_error` behaves exactly like `Nominal` (nothing to sample).
    Sampled,
}

impl ExecutionErrorMode {
    /// The proto-enum-style name recorded on `Event`/`Provenance` (question 103's "provenance
    /// records the mode") -- not a real proto enum (no schema change is authorized for this
    /// task), just a stable string label mirroring this crate's existing `"AXES_KIND_..."`/
    /// `"FAULT_TARGET_KIND_..."` naming convention.
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionErrorMode::Nominal => "EXECUTION_ERROR_MODE_NOMINAL",
            ExecutionErrorMode::Sampled => "EXECUTION_ERROR_MODE_SAMPLED",
        }
    }
}

/// The dv `mode` selects to actually apply for `m`'s burn (question 103), and -- only under
/// [`ExecutionErrorMode::Sampled`] with a declared `execution_error` -- the sampled realization
/// for the caller to record on the MANEUVER event. Shared by `executor::run_plain_instance` and
/// `executor::run_covariance_instance` so the same selection logic runs on both paths rather
/// than either one inferring its own mode from the run's shape (see [`ExecutionErrorMode`]'s own
/// doc comment for the M11.4 bug this closes).
///
/// `Nominal` always returns `(m.dv, None)`, even when `m.execution_error` is declared -- a
/// nominal run never perturbs the applied dv; only the covariance path's own separate injection
/// step (`inject_gates_covariance`, called by the caller when `mode == Nominal` and covariance is
/// on) reacts to the declared block. `Sampled` draws one Gates-model realization
/// ([`sample_execution_error`]) when `m.execution_error` is `Some`, from the fresh-per-event
/// substream `crate::rng::event_rng` derives from `seeds[execution_error.seed]` and `m.id` --
/// looked up here by `.expect()` because `executor::execute`'s own up-front validation pass
/// ([`validate_execution_error_seed`]) already refused any DRM whose declared seed does not name
/// a real `seeds` key, before any instance ran, so this call can only be reached for a maneuver
/// that passed that check. A `Sampled` maneuver with no declared `execution_error` also returns
/// `(m.dv, None)` -- nothing to sample.
pub fn dv_to_apply(mode: ExecutionErrorMode, m: &ParsedManeuver, seeds: &BTreeMap<String, u64>) -> ([f64; 3], Option<SampledDv>) {
    match mode {
        ExecutionErrorMode::Nominal => (m.dv, None),
        ExecutionErrorMode::Sampled => match &m.execution_error {
            None => (m.dv, None),
            Some(ee) => {
                let base_seed = crate::rng::seed_for(seeds, &ee.seed)
                    .unwrap_or_else(|| panic!("maneuver {:?}: execute() must validate execution_error.seed {:?} against Scenario.seeds before any instance runs", m.id, ee.seed));
                let sampled = sample_execution_error(m, ee, base_seed);
                (sampled.applied_dv, Some(sampled))
            }
        },
    }
}

/// Add the Gates model's analytic covariance injection `G Q G^T` to `p` (row-major `n x n`, SI,
/// `n >= 6`) in place -- module doc comment's path (b). `triad` (already rotated into the
/// trajectory's own inertial frame -- see [`inertial_triad`]) maps into the velocity block
/// (state indices `3..6`, this crate's fixed Cartesian convention -- the same indices
/// `executor::apply_dv_to_state` itself bumps) with `Q = diag(sigma_m^2, sigma_p^2,
/// sigma_p^2)`; since `triad` is orthonormal, `G Q G^T`'s only nonzero block is `sigma_m^2 (u
/// u^T) + sigma_p^2 (p1 p1^T + p2 p2^T)`, added into `p`'s velocity-velocity 3x3 block. Every
/// other entry of `p` (position-position, position-velocity, and anything beyond index 6, e.g.
/// an attitude block) is left untouched. **`Phi` is not modified by this function or by any
/// caller of it** -- only `P` changes at a burn boundary with execution error, exactly as the
/// no-execution-error case already established.
pub fn inject_gates_covariance(p: &mut [f64], n: usize, sigma_m: f64, sigma_p: f64, triad: ([f64; 3], [f64; 3], [f64; 3])) {
    let (u, p1, p2) = triad;
    let sm2 = sigma_m * sigma_m;
    let sp2 = sigma_p * sigma_p;
    for a in 0..3 {
        for b in 0..3 {
            let delta = sm2 * u[a] * u[b] + sp2 * (p1[a] * p1[b] + p2[a] * p2[b]);
            p[(3 + a) * n + (3 + b)] += delta;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, instance: &str, values: &[(&str, f64)], attrs: &[(&str, &str)]) -> ScenarioEvent {
        ScenarioEvent {
            id: "m1".to_string(),
            tai_ns: 1000,
            kind: kind.to_string(),
            instance: instance.to_string(),
            values: values.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            attributes: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            execution_error: None,
        }
    }

    #[test]
    fn parses_a_well_formed_vnb_maneuver() {
        let e = event("maneuver", "leo", &[("dv_x", 20.0), ("dv_y", 0.0), ("dv_z", 0.0)], &[("frame_id", "AXES_KIND_VNB")]);
        let m = parse(&e).expect("parses");
        assert_eq!(m.instance, "leo");
        assert_eq!(m.dv, [20.0, 0.0, 0.0]);
        assert_eq!(m.axes, AxesKind::Vnb);
    }

    #[test]
    fn a_non_maneuver_kind_is_a_typed_error() {
        let e = event("mode", "leo", &[], &[]);
        let err = parse(&e).unwrap_err();
        assert!(matches!(err, DrmError::UnsupportedScenarioEventKind { .. }), "{err:?}");
    }

    #[test]
    fn a_missing_dv_component_is_a_typed_error() {
        let e = event("maneuver", "leo", &[("dv_x", 1.0), ("dv_y", 0.0)], &[("frame_id", "AXES_KIND_VNB")]);
        let err = parse(&e).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { .. }), "{err:?}");
    }

    #[test]
    fn an_unknown_values_key_is_a_typed_error_not_silently_ignored() {
        let e = event("maneuver", "leo", &[("dv_x", 1.0), ("dv_y", 0.0), ("dv_z", 0.0), ("bogus", 1.0)], &[("frame_id", "AXES_KIND_VNB")]);
        let err = parse(&e).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { .. }), "{err:?}");
    }

    #[test]
    fn a_missing_frame_id_is_a_typed_error() {
        let e = event("maneuver", "leo", &[("dv_x", 1.0), ("dv_y", 0.0), ("dv_z", 0.0)], &[]);
        let err = parse(&e).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { .. }), "{err:?}");
    }

    #[test]
    fn an_extra_attribute_key_is_a_typed_error() {
        let e = event("maneuver", "leo", &[("dv_x", 1.0), ("dv_y", 0.0), ("dv_z", 0.0)], &[("frame_id", "AXES_KIND_VNB"), ("bogus", "x")]);
        let err = parse(&e).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { .. }), "{err:?}");
    }

    #[test]
    fn an_unrecognized_frame_name_is_a_typed_error() {
        let e = event("maneuver", "leo", &[("dv_x", 1.0), ("dv_y", 0.0), ("dv_z", 0.0)], &[("frame_id", "NOT_A_REAL_AXES_KIND")]);
        let err = parse(&e).unwrap_err();
        assert!(matches!(err, DrmError::InvalidEnumValue { .. }), "{err:?}");
    }

    #[test]
    fn a_recognized_but_unsupported_frame_is_a_typed_error() {
        let e = event("maneuver", "leo", &[("dv_x", 1.0), ("dv_y", 0.0), ("dv_z", 0.0)], &[("frame_id", "AXES_KIND_BODY_FIXED")]);
        let err = parse(&e).unwrap_err();
        assert!(matches!(err, DrmError::ManeuverFrameNotSupported { .. }), "{err:?}");
    }

    #[test]
    fn missing_instance_is_a_typed_error() {
        let e = event("maneuver", "", &[("dv_x", 1.0), ("dv_y", 0.0), ("dv_z", 0.0)], &[("frame_id", "AXES_KIND_VNB")]);
        let err = parse(&e).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { .. }), "{err:?}");
    }

    // A simple orbit state to hand-check the VNB/RIC/VVLH bases: r along +X, v along +Y (so
    // h = r x v is along +Z) -- every basis vector below is then exactly one of +-X/+-Y/+-Z,
    // checkable by inspection (X x Y = Z, Y x Z = X, Z x X = Y) rather than trusting the same
    // cross products the function itself uses.
    const R: [f64; 3] = [7_000_000.0, 0.0, 0.0];
    const V: [f64; 3] = [0.0, 7_500.0, 0.0];

    #[test]
    fn vnb_prograde_burn_adds_along_velocity() {
        let dv = dv_to_inertial(AxesKind::Vnb, [10.0, 0.0, 0.0], R, V);
        assert!(dv[0].abs() < 1e-9 && (dv[1] - 10.0).abs() < 1e-9 && dv[2].abs() < 1e-9, "{dv:?}");
    }

    #[test]
    fn vnb_normal_burn_adds_along_orbit_normal_plus_z() {
        // h = r x v = X x Y = +Z, so N = +Z.
        let dv = dv_to_inertial(AxesKind::Vnb, [0.0, 10.0, 0.0], R, V);
        assert!(dv[0].abs() < 1e-9 && dv[1].abs() < 1e-9 && (dv[2] - 10.0).abs() < 1e-9, "{dv:?}");
    }

    #[test]
    fn vnb_binormal_burn_is_v_cross_n_equals_plus_x() {
        // B = V x N = Y x Z = +X.
        let dv = dv_to_inertial(AxesKind::Vnb, [0.0, 0.0, 10.0], R, V);
        assert!((dv[0] - 10.0).abs() < 1e-9 && dv[1].abs() < 1e-9 && dv[2].abs() < 1e-9, "{dv:?}");
    }

    #[test]
    fn ric_radial_burn_adds_along_x() {
        let dv = dv_to_inertial(AxesKind::Ric, [10.0, 0.0, 0.0], R, V);
        assert!((dv[0] - 10.0).abs() < 1e-9 && dv[1].abs() < 1e-9 && dv[2].abs() < 1e-9, "{dv:?}");
    }

    #[test]
    fn ric_in_track_burn_is_n_cross_r_equals_plus_y() {
        // I = N x R = Z x X = Y.
        let dv = dv_to_inertial(AxesKind::Ric, [0.0, 10.0, 0.0], R, V);
        assert!(dv[0].abs() < 1e-9 && (dv[1] - 10.0).abs() < 1e-9 && dv[2].abs() < 1e-9, "{dv:?}");
    }

    #[test]
    fn ric_cross_track_burn_equals_n_plus_z() {
        let dv = dv_to_inertial(AxesKind::Ric, [0.0, 0.0, 10.0], R, V);
        assert!(dv[0].abs() < 1e-9 && dv[1].abs() < 1e-9 && (dv[2] - 10.0).abs() < 1e-9, "{dv:?}");
    }

    #[test]
    fn vvlh_matches_its_documented_axes() {
        // R = +X, N = +Z, in_track = N x R = +Y. VVLH: X_vvlh = in_track = +Y, Y_vvlh = -N =
        // -Z, Z_vvlh = -R = -X. dv = [1,1,1] -> 1*(0,1,0) + 1*(0,0,-1) + 1*(-1,0,0) = (-1,1,-1).
        let dv = dv_to_inertial(AxesKind::Vvlh, [1.0, 1.0, 1.0], R, V);
        assert!((dv[0] + 1.0).abs() < 1e-9 && (dv[1] - 1.0).abs() < 1e-9 && (dv[2] + 1.0).abs() < 1e-9, "{dv:?}");
    }

    #[test]
    fn inertial_frame_is_identity() {
        assert_eq!(dv_to_inertial(AxesKind::Icrf, [1.0, 2.0, 3.0], R, V), [1.0, 2.0, 3.0]);
        assert_eq!(dv_to_inertial(AxesKind::Mj2000Eq, [1.0, 2.0, 3.0], R, V), [1.0, 2.0, 3.0]);
    }

    #[test]
    #[should_panic(expected = "unsupported AxesKind")]
    fn an_unrealized_frame_panics_rather_than_silently_transforming() {
        let _ = dv_to_inertial(AxesKind::Enu, [1.0, 0.0, 0.0], R, V);
    }

    // -----------------------------------------------------------------------------------
    // Gates model (question 100).
    // -----------------------------------------------------------------------------------

    fn ee(fixed_m: f64, prop_m: f64, fixed_p: f64, prop_p: f64, seed: &str) -> ParsedExecutionError {
        ParsedExecutionError { sigma_magnitude_fixed_mps: fixed_m, sigma_magnitude_proportional: prop_m, sigma_pointing_fixed_mps: fixed_p, sigma_pointing_proportional_rad: prop_p, seed: seed.to_string() }
    }

    #[test]
    fn gates_sigmas_combine_fixed_and_proportional_in_quadrature() {
        let e = ee(0.01, 0.001, 0.02, 0.0005, "s");
        let (sigma_m, sigma_p) = gates_sigmas(&e, 20.0);
        assert!((sigma_m - (0.01f64.powi(2) + (0.001 * 20.0f64).powi(2)).sqrt()).abs() < 1e-15);
        assert!((sigma_p - (0.02f64.powi(2) + (0.0005 * 20.0f64).powi(2)).sqrt()).abs() < 1e-15);
    }

    #[test]
    fn gates_sigmas_are_exactly_zero_for_a_zero_sigma_block_at_any_dv() {
        let e = ee(0.0, 0.0, 0.0, 0.0, "s");
        assert_eq!(gates_sigmas(&e, 0.0), (0.0, 0.0));
        assert_eq!(gates_sigmas(&e, 1234.5), (0.0, 0.0));
    }

    #[test]
    fn gates_sigmas_scale_with_dv_magnitude_for_a_proportional_only_block() {
        // A proportional-only block (both fixed terms zero) must scale linearly with |dv|.
        let e = ee(0.0, 0.002, 0.0, 0.0003, "s");
        let (sm_10, sp_10) = gates_sigmas(&e, 10.0);
        let (sm_40, sp_40) = gates_sigmas(&e, 40.0);
        assert!((sm_40 / sm_10 - 4.0).abs() < 1e-12, "sigma_m should scale linearly with |dv|: {sm_10} -> {sm_40}");
        assert!((sp_40 / sp_10 - 4.0).abs() < 1e-12, "sigma_p should scale linearly with |dv|: {sp_10} -> {sp_40}");
        assert!((sm_10 - 0.002 * 10.0).abs() < 1e-12);
        assert!((sp_10 - 0.0003 * 10.0).abs() < 1e-12);
    }

    fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    #[test]
    fn burn_triad_is_orthonormal_and_right_handed_with_u_along_dv() {
        let (u, p1, p2) = burn_triad([3.0, -4.0, 12.0]);
        for v in [u, p1, p2] {
            assert!((norm(v) - 1.0).abs() < 1e-12, "{v:?} is not unit");
        }
        assert!(dot(u, p1).abs() < 1e-12);
        assert!(dot(u, p2).abs() < 1e-12);
        assert!(dot(p1, p2).abs() < 1e-12);
        let expected_u = unit([3.0, -4.0, 12.0]);
        for i in 0..3 {
            assert!((u[i] - expected_u[i]).abs() < 1e-12);
        }
        // Right-handed: u x p1 == p2.
        let cross_up1 = cross(u, p1);
        for i in 0..3 {
            assert!((cross_up1[i] - p2[i]).abs() < 1e-9, "triad is not right-handed: {cross_up1:?} vs {p2:?}");
        }
    }

    #[test]
    fn burn_triad_falls_back_to_the_canonical_basis_for_a_zero_dv() {
        assert_eq!(burn_triad([0.0, 0.0, 0.0]), ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]));
    }

    #[test]
    fn inertial_triad_stays_orthonormal_through_a_vnb_rotation() {
        let (u, p1, p2) = inertial_triad(AxesKind::Vnb, [20.0, 0.0, 0.0], R, V);
        for v in [u, p1, p2] {
            assert!((norm(v) - 1.0).abs() < 1e-9, "{v:?} is not unit after rotation");
        }
        assert!(dot(u, p1).abs() < 1e-9);
        assert!(dot(u, p2).abs() < 1e-9);
        assert!(dot(p1, p2).abs() < 1e-9);
        // VNB's u = vhat = [0,1,0] in this test's R/V basis (see the VNB tests above).
        assert!((u[0]).abs() < 1e-9 && (u[1] - 1.0).abs() < 1e-9 && u[2].abs() < 1e-9, "{u:?}");
    }

    #[test]
    fn sample_execution_error_is_deterministic_for_the_same_seed_and_id() {
        let m = ParsedManeuver { id: "burn1".to_string(), tai_ns: 0, instance: "leo".to_string(), dv: [20.0, 0.0, 0.0], axes: AxesKind::Vnb, execution_error: None };
        let e = ee(0.01, 0.001, 0.02, 0.0005, "s");
        let a = sample_execution_error(&m, &e, 42);
        let b = sample_execution_error(&m, &e, 42);
        assert_eq!(a.applied_dv, b.applied_dv);
        assert_eq!(a.draws, b.draws);
    }

    #[test]
    fn sample_execution_error_with_zero_sigma_reproduces_the_commanded_dv_exactly() {
        let m = ParsedManeuver { id: "burn1".to_string(), tai_ns: 0, instance: "leo".to_string(), dv: [20.0, 0.0, 0.0], axes: AxesKind::Vnb, execution_error: None };
        let e = ee(0.0, 0.0, 0.0, 0.0, "s");
        let sampled = sample_execution_error(&m, &e, 42);
        assert_eq!(sampled.applied_dv, m.dv, "a zero-sigma block must not perturb the commanded dv at all");
    }

    #[test]
    fn different_event_ids_perturb_the_same_dv_differently() {
        let e = ee(0.5, 0.0, 0.5, 0.0, "s");
        let m_a = ParsedManeuver { id: "burn_a".to_string(), tai_ns: 0, instance: "leo".to_string(), dv: [20.0, 0.0, 0.0], axes: AxesKind::Vnb, execution_error: None };
        let m_b = ParsedManeuver { id: "burn_b".to_string(), tai_ns: 0, instance: "leo".to_string(), dv: [20.0, 0.0, 0.0], axes: AxesKind::Vnb, execution_error: None };
        let a = sample_execution_error(&m_a, &e, 7);
        let b = sample_execution_error(&m_b, &e, 7);
        assert_ne!(a.applied_dv, b.applied_dv, "different event ids sharing the same base_seed must draw different substreams");
    }

    #[test]
    fn inject_gates_covariance_matches_the_hand_derived_isotropic_case() {
        // u = p1 = p2 orthonormal standard basis; sigma_m == sigma_p == 1.0, so
        // G Q G^T's velocity block must be exactly the 3x3 identity (u u^T + p1 p1^T + p2 p2^T
        // == I for an orthonormal completion).
        let mut p = vec![0.0; 36];
        let triad = ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        inject_gates_covariance(&mut p, 6, 1.0, 1.0, triad);
        for a in 0..3 {
            for b in 0..3 {
                let expected = if a == b { 1.0 } else { 0.0 };
                assert!((p[(3 + a) * 6 + (3 + b)] - expected).abs() < 1e-15, "[{a},{b}] = {}", p[(3 + a) * 6 + (3 + b)]);
            }
        }
        // Nothing outside the velocity block was touched.
        for row in 0..3 {
            for col in 0..6 {
                assert_eq!(p[row * 6 + col], 0.0, "position rows must be untouched");
            }
        }
    }

    #[test]
    fn inject_gates_covariance_adds_rather_than_overwrites() {
        let mut p = vec![0.0; 36];
        p[3 * 6 + 3] = 5.0; // pre-existing velocity-x variance.
        let triad = ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        inject_gates_covariance(&mut p, 6, 2.0, 0.0, triad);
        assert!((p[3 * 6 + 3] - (5.0 + 4.0)).abs() < 1e-12, "must add to, not overwrite, the existing entry");
    }

    #[test]
    fn inject_gates_covariance_is_a_zero_matrix_for_zero_sigmas() {
        let mut p = vec![0.0; 36];
        let before = p.clone();
        let triad = burn_triad([20.0, 0.0, 0.0]);
        inject_gates_covariance(&mut p, 6, 0.0, 0.0, triad);
        assert_eq!(p, before, "zero sigmas must inject exactly nothing");
    }

    #[test]
    fn parse_accepts_a_present_zero_sigma_execution_error_block() {
        let e = event("maneuver", "leo", &[("dv_x", 20.0), ("dv_y", 0.0), ("dv_z", 0.0)], &[("frame_id", "AXES_KIND_VNB")]);
        let mut e = e;
        e.execution_error = Some(av_cdm::pb::ManeuverExecutionError {
            sigma_magnitude_fixed_mps: 0.0,
            sigma_magnitude_proportional: 0.0,
            sigma_pointing_fixed_mps: 0.0,
            sigma_pointing_proportional_rad: 0.0,
            seed: "burn_seed".to_string(),
        });
        let m = parse(&e).expect("a present, all-zero execution_error block parses");
        let ee = m.execution_error.expect("execution_error is Some, not defaulted away");
        assert_eq!(ee.sigma_magnitude_fixed_mps, 0.0);
        assert_eq!(ee.seed, "burn_seed");
    }

    #[test]
    fn parse_refuses_a_non_finite_sigma() {
        let mut e = event("maneuver", "leo", &[("dv_x", 20.0), ("dv_y", 0.0), ("dv_z", 0.0)], &[("frame_id", "AXES_KIND_VNB")]);
        e.execution_error = Some(av_cdm::pb::ManeuverExecutionError { sigma_magnitude_fixed_mps: f64::NAN, ..Default::default() });
        let err = parse(&e).unwrap_err();
        assert!(matches!(err, DrmError::InvalidManeuverExecutionError { .. }), "{err:?}");
    }

    #[test]
    fn parse_leaves_execution_error_none_when_absent_never_defaulting_it() {
        let e = event("maneuver", "leo", &[("dv_x", 20.0), ("dv_y", 0.0), ("dv_z", 0.0)], &[("frame_id", "AXES_KIND_VNB")]);
        let m = parse(&e).expect("parses");
        assert!(m.execution_error.is_none(), "an absent block must stay None, never defaulted to zero-sigma");
    }

    #[test]
    fn validate_execution_error_seed_accepts_a_known_key() {
        let m = ParsedManeuver {
            id: "burn1".to_string(),
            tai_ns: 0,
            instance: "leo".to_string(),
            dv: [1.0, 0.0, 0.0],
            axes: AxesKind::Vnb,
            execution_error: Some(ee(0.0, 0.0, 0.0, 0.0, "burn_seed")),
        };
        let seeds = BTreeMap::from([("burn_seed".to_string(), 42u64)]);
        assert!(validate_execution_error_seed(&m, &seeds).is_ok());
    }

    #[test]
    fn validate_execution_error_seed_refuses_an_unknown_key() {
        let m = ParsedManeuver {
            id: "burn1".to_string(),
            tai_ns: 0,
            instance: "leo".to_string(),
            dv: [1.0, 0.0, 0.0],
            axes: AxesKind::Vnb,
            execution_error: Some(ee(0.0, 0.0, 0.0, 0.0, "no_such_seed")),
        };
        let err = validate_execution_error_seed(&m, &BTreeMap::new()).unwrap_err();
        assert!(matches!(err, DrmError::UnknownManeuverSeed { ref id, ref seed } if id == "burn1" && seed == "no_such_seed"), "{err:?}");
    }

    #[test]
    fn validate_execution_error_seed_is_a_no_op_for_a_perfect_burn() {
        let m = ParsedManeuver { id: "burn1".to_string(), tai_ns: 0, instance: "leo".to_string(), dv: [1.0, 0.0, 0.0], axes: AxesKind::Vnb, execution_error: None };
        assert!(validate_execution_error_seed(&m, &BTreeMap::new()).is_ok());
    }

    // -----------------------------------------------------------------------------------
    // ExecutionErrorMode / dv_to_apply (question 103).
    // -----------------------------------------------------------------------------------

    #[test]
    fn execution_error_mode_defaults_to_nominal() {
        assert_eq!(ExecutionErrorMode::default(), ExecutionErrorMode::Nominal);
    }

    #[test]
    fn dv_to_apply_under_nominal_never_perturbs_even_with_a_non_zero_execution_error_block() {
        let m = ParsedManeuver {
            id: "burn1".to_string(),
            tai_ns: 0,
            instance: "leo".to_string(),
            dv: [20.0, 0.0, 0.0],
            axes: AxesKind::Vnb,
            execution_error: Some(ee(0.5, 0.01, 0.3, 0.002, "s")),
        };
        let seeds = BTreeMap::from([("s".to_string(), 42u64)]);
        let (applied, sampled) = dv_to_apply(ExecutionErrorMode::Nominal, &m, &seeds);
        assert_eq!(applied, m.dv, "Nominal must apply the commanded dv exactly, regardless of a declared execution_error block");
        assert!(sampled.is_none(), "Nominal never samples, so there is no realization to record");
    }

    #[test]
    fn dv_to_apply_under_sampled_draws_a_realization_when_execution_error_is_declared() {
        let m = ParsedManeuver {
            id: "burn1".to_string(),
            tai_ns: 0,
            instance: "leo".to_string(),
            dv: [20.0, 0.0, 0.0],
            axes: AxesKind::Vnb,
            execution_error: Some(ee(0.5, 0.01, 0.3, 0.002, "s")),
        };
        let seeds = BTreeMap::from([("s".to_string(), 42u64)]);
        let (applied, sampled) = dv_to_apply(ExecutionErrorMode::Sampled, &m, &seeds);
        assert_ne!(applied, m.dv, "Sampled with a non-zero execution_error block must perturb the applied dv");
        assert!(sampled.is_some(), "Sampled must record the realization for the MANEUVER event");
    }

    #[test]
    fn dv_to_apply_under_either_mode_is_a_no_op_for_a_perfect_burn() {
        let m = ParsedManeuver { id: "burn1".to_string(), tai_ns: 0, instance: "leo".to_string(), dv: [20.0, 0.0, 0.0], axes: AxesKind::Vnb, execution_error: None };
        let seeds = BTreeMap::new();
        let (nominal_dv, nominal_sampled) = dv_to_apply(ExecutionErrorMode::Nominal, &m, &seeds);
        let (sampled_dv, sampled_sampled) = dv_to_apply(ExecutionErrorMode::Sampled, &m, &seeds);
        assert_eq!(nominal_dv, m.dv);
        assert_eq!(sampled_dv, m.dv, "an absent execution_error block has nothing to sample, in either mode");
        assert!(nominal_sampled.is_none() && sampled_sampled.is_none());
    }
}
