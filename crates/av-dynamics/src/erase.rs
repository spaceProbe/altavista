//! [`ErasedModel`]: the adapter that gets any [`DynamicsModel`] into the one shape a trait
//! object can hold -- `Box<dyn DynamicsModel<Error = ModelError>>` (ADR-005 sec 1).
//!
//! The trait itself did not change (see `crate::error`'s module doc comment for why not);
//! `ErasedModel<M>` just wraps an `M: DynamicsModel` alongside a conversion from `M::Error` to
//! [`ModelError`] and re-exposes every `DynamicsModel` method by delegating to the wrapped
//! model, mapping the error at the four methods that can actually produce one
//! (`derivatives`/`stm_derivatives`/`step`/`step_with_stm`).
//!
//! **`step`/`step_with_stm` delegate too, as of M10.3.** Through M10.2 these were *not*
//! overridden: both are default-implemented on the trait purely in terms of
//! `derivatives`/`stm_derivatives` and `self.integrator()`, so `ErasedModel` inherited the
//! identical default -- reasoned at the time as "erasure changes nothing about the physics,
//! only the error type a caller sees." That reasoning quietly assumed no wrapped model ever
//! overrides `step`/`step_with_stm` itself, which stopped being true the moment
//! `gmat_sys::model::GmatModel::step` was overridden (task M10.2, `docs/open-questions.md`
//! question 95's second half) to populate `StepResult.outputs`: boxing a `GmatModel` through
//! [`erase_with_id`] silently and *permanently* discarded that override -- the inherited
//! default recomputes the identical physical state via `derivatives` (so nothing looked
//! broken), but `outputs` was always empty, with no error to notice it by.
//! `av-kernel::drm::executor::StepDelegating` was `av-kernel`'s in-ownership workaround for
//! exactly this gap (a local `BoxedModel` wrapper that *did* delegate `step`, used only for the
//! plain, non-covariance path); this module is the actual owner of the gap it worked around,
//! so the fix belongs here: `ErasedModel::step`/`step_with_stm` now delegate to
//! `self.inner.step`/`step_with_stm` directly (mapping the error the same way
//! `derivatives`/`stm_derivatives` already did), so any wrapped model's own override -- whether
//! it exists yet or not -- survives erasure. For a model that does not override `step`/
//! `step_with_stm` (every model in this workspace except `GmatModel`), this is behaviourally
//! identical to the old inherited-default path: both ultimately integrate `derivatives`/
//! `stm_derivatives` through `Dopri5`, just reached one call frame differently -- see
//! `erased_models_default_step_still_drives_dopri5_over_derivatives` below, unchanged by this
//! fix, and `erased_model_delegates_a_wrapped_models_own_step_override` (new), which proves the
//! actual gap this closes.
//!
//! `ErasedModel<M>` is `!Send`/`!Sync` whenever `M` is (Rust's auto traits propagate through
//! the wrapper unchanged) -- in particular, wrapping a GMAT-backed model here does **not**
//! paper over `gmat_sys::model::GmatModel`'s `!Send` (that model holds a raw pointer into
//! GMAT's process-global, not-thread-safe configuration; see that module's own doc comment).
//! Nothing in this file asserts `Send` for anything.

use crate::error::ModelError;
use crate::integrate::Dopri5;
use crate::{AppliedCommand, DynamicsModel, Inbox, Outbox, StepResult, StmStepResult};

/// The per-model error conversion `erase_with_id` stores. Factored into an alias because the
/// inline `Box<dyn Fn(&M, M::Error) -> ModelError>` trips `clippy::type_complexity`.
type ConvertFn<M> = Box<dyn Fn(&M, <M as DynamicsModel>::Error) -> ModelError>;

/// A boxed, object-safe [`DynamicsModel`] speaking the one shared [`ModelError`] (ADR-005 sec
/// 1's `Box<dyn DynamicsModel<Error = ModelError>>`). Deliberately not `Send`/`Sync`-bounded:
/// a GMAT-backed model erased into this shape stays exactly as `!Send` as
/// `gmat_sys::model::GmatModel` itself is.
pub type BoxedModel = Box<dyn DynamicsModel<Error = ModelError>>;

/// Wraps an `M: DynamicsModel` so it can be boxed as a [`BoxedModel`]: `M::Error` is converted
/// to [`ModelError`] by a caller-supplied closure at the four methods that can produce one. See
/// the module doc comment for why `step`/`step_with_stm` delegate too, as of M10.3.
pub struct ErasedModel<M: DynamicsModel> {
    inner: M,
    convert: ConvertFn<M>,
}

impl<M: DynamicsModel> ErasedModel<M> {
    /// `convert` gets `&M` (so it can read `inner.describe().id` or any other field of the
    /// model itself) and the model's own error, and must produce the [`ModelError`] that error
    /// corresponds to -- e.g. `|m, e| ModelError::Gmat { model_id: m.describe().id, detail:
    /// e.to_string() }`.
    pub fn new(inner: M, convert: impl Fn(&M, M::Error) -> ModelError + 'static) -> Self {
        Self { inner, convert: Box::new(convert) }
    }

    /// The wrapped model.
    pub fn inner(&self) -> &M {
        &self.inner
    }
}

impl<M: DynamicsModel> DynamicsModel for ErasedModel<M> {
    type Error = ModelError;

    fn state_dim(&self) -> usize {
        self.inner.state_dim()
    }

    fn derivatives(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], state_dot: &mut [f64]) -> Result<(), ModelError> {
        self.inner.derivatives(state, t_tai_ns, controls, state_dot).map_err(|e| (self.convert)(&self.inner, e))
    }

    fn describe(&self) -> av_cdm::pb::ModelInfo {
        self.inner.describe()
    }

    fn integrator(&self) -> Dopri5 {
        self.inner.integrator()
    }

    fn stm_capable(&self) -> bool {
        self.inner.stm_capable()
    }

    /// **Question 112 addendum.** Guards on `self.inner.stm_capable()` before ever calling
    /// `self.inner.stm_derivatives` -- the trait's own default `stm_derivatives`
    /// (`DynamicsModel::stm_derivatives`'s own doc comment) `unimplemented!()`s on exactly this
    /// precondition violation, which is the right answer for a caller driving `M` directly
    /// (where "check `stm_capable()` first" is an enforceable-by-review precondition on code the
    /// same author controls). `ErasedModel` sits at ADR-005 sec 1's trait-object boundary,
    /// exactly where `ModelError::CapabilityMissing`'s own doc comment says that precondition
    /// "cannot be enforced at compile time" any more -- a caller several layers behind a
    /// registry/scheduler has no way to know this instance is not really STM-capable except by
    /// calling `stm_capable()` itself, which nothing forces it to do. Converting the panic into
    /// this typed, catchable `Result::Err` here -- rather than leaving the trait's own default in
    /// place -- is possible only because `ErasedModel::Error` is concretely `ModelError` (not the
    /// generic `Self::Error` the trait-level default is stuck with), so this is not a change to
    /// the trait itself, just the one place erasure can do better than the generic default can.
    fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], augmented_state_dot: &mut [f64]) -> Result<(), ModelError> {
        if !self.inner.stm_capable() {
            return Err(ModelError::CapabilityMissing { model_id: self.inner.describe().id, capability: "stm_derivatives".to_string() });
        }
        self.inner.stm_derivatives(augmented_state, t_tai_ns, controls, augmented_state_dot).map_err(|e| (self.convert)(&self.inner, e))
    }

    /// Delegates to `self.inner.step` (M10.3 -- see the module doc comment). Whatever the
    /// wrapped model's own `step` does -- the trait's default, or an override like
    /// `gmat_sys::model::GmatModel::step`'s `outputs` population -- survives erasure now.
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, ModelError> {
        self.inner.step(state, t_tai_ns, controls, dt_ns).map_err(|e| (self.convert)(&self.inner, e))
    }

    /// Delegates to `self.inner.step_with_stm`, for the same reason [`ErasedModel::step`] does.
    fn step_with_stm(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StmStepResult, ModelError> {
        self.inner.step_with_stm(state, t_tai_ns, controls, dt_ns).map_err(|e| (self.convert)(&self.inner, e))
    }

    /// Delegates to `self.inner.step_with_ports`, for the same reason [`ErasedModel::step`]
    /// does. Without this, erasing a port-aware model silently drops its override and falls
    /// back to the trait default (plain `step`, empty `Outbox`) -- the same bug class `step`
    /// and `step_with_stm` each had before M10.3 and M11.2 fixed them. Inert until a model
    /// overrides `step_with_ports`, and live the moment a bound container model is erased,
    /// which is why it is closed here rather than left as M13.1's disclosed escalation. The
    /// third tuple element (`Vec<AppliedCommand>`, question 130, M19.3) delegates exactly the
    /// same way -- an erased model that applied a command must not have that silently dropped
    /// on the way through this wrapper either.
    fn step_with_ports(
        &self,
        state: &[f64],
        t_tai_ns: i64,
        controls: &[f64],
        dt_ns: i64,
        inbox: &Inbox,
    ) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), ModelError> {
        self.inner
            .step_with_ports(state, t_tai_ns, controls, dt_ns, inbox)
            .map_err(|e| (self.convert)(&self.inner, e))
    }

    /// Delegates to `self.inner.last_measurements` (question 173, M25.3) -- for the same reason
    /// [`ErasedModel::step_with_ports`] does: without this, erasing a measurement-producing
    /// model would silently fall back to the trait's own empty default regardless of what the
    /// wrapped model actually computed, the same bug class `step`/`step_with_stm`/
    /// `step_with_ports` each had before M10.3/M11.2 fixed them.
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        self.inner.last_measurements()
    }
}

/// `ErasedModel::new` plus `Box::new`, for the common case of building a [`BoxedModel`]
/// directly. `model_id` is captured by the closure (not read from `describe()`) so a caller
/// that has not yet built a full `ModelInfo` can still erase -- matching how
/// `crate::error::ModelError`'s variants are always constructed with an explicit id string
/// elsewhere in this workspace (`crate::registry`-style callers, `av-kernel`'s DRM binding).
pub fn erase_with_id<M: DynamicsModel + 'static>(model_id: impl Into<String>, inner: M, wrap: impl Fn(String, M::Error) -> ModelError + 'static) -> BoxedModel {
    let id = model_id.into();
    Box::new(ErasedModel::new(inner, move |_m, e| wrap(id.clone(), e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A model whose `Error` is a plain `String` (not `ModelError`, not `Infallible`,
    /// unrelated to any real model in this workspace) -- proves erasure works for an
    /// arbitrary, unrelated error type, not just the two concrete ones this workspace happens
    /// to have today.
    struct Flaky {
        fail_after: Cell<usize>,
    }
    impl DynamicsModel for Flaky {
        type Error = String;
        fn state_dim(&self) -> usize {
            6
        }
        fn derivatives(&self, state: &[f64], _t: i64, _c: &[f64], out: &mut [f64]) -> Result<(), String> {
            let n = self.fail_after.get();
            if n == 0 {
                return Err("synthetic numerical failure".to_string());
            }
            self.fail_after.set(n - 1);
            out[0..3].copy_from_slice(&state[3..6]);
            out[3..6].copy_from_slice(&[0.0, 0.0, 0.0]);
            Ok(())
        }
        fn describe(&self) -> av_cdm::pb::ModelInfo {
            av_cdm::pb::ModelInfo { id: "test.flaky".to_string(), ..Default::default() }
        }
        // A synthetic error-path model; never emits telemetry mapped to a CDM measurement.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
    }

    #[test]
    fn erased_model_delegates_state_dim_and_describe_unchanged() {
        let boxed: BoxedModel = erase_with_id("test.flaky", Flaky { fail_after: Cell::new(100) }, |model_id, detail| ModelError::Numerical { model_id, detail });
        assert_eq!(boxed.state_dim(), 6);
        assert_eq!(boxed.describe().id, "test.flaky");
        assert!(!boxed.stm_capable(), "Flaky never declares stm_capable");
    }

    #[test]
    fn erased_model_maps_the_inner_error_type_into_model_error_carrying_the_model_id() {
        let boxed: BoxedModel = erase_with_id("test.flaky", Flaky { fail_after: Cell::new(0) }, |model_id, detail| ModelError::Numerical { model_id, detail });
        let mut out = [0.0; 6];
        let err = boxed.derivatives(&[0.0; 6], 0, &[], &mut out).unwrap_err();
        match err {
            ModelError::Numerical { model_id, detail } => {
                assert_eq!(model_id, "test.flaky");
                assert_eq!(detail, "synthetic numerical failure");
            }
            other => panic!("expected ModelError::Numerical, got {other:?}"),
        }
    }

    #[test]
    fn erased_models_default_step_still_drives_dopri5_over_derivatives() {
        // Same closed-form check `av_dynamics`'s own default-step test uses, run through the
        // erased boxed model instead of the concrete type -- `Flaky` overrides no step of its
        // own, so `ErasedModel::step`'s delegation (M10.3) reaches the identical inherited
        // default `Flaky::step` would have used un-erased, proving the delegation fix is a
        // no-op for a model that never overrides `step` in the first place.
        let boxed: BoxedModel = erase_with_id("test.flaky", Flaky { fail_after: Cell::new(1_000_000) }, |model_id, detail| ModelError::Numerical { model_id, detail });
        let x0 = [0.0, 0.0, 0.0, 1.0, 2.0, 0.0];
        let result = boxed.step(&x0, 0, &[], 10_000_000_000).unwrap(); // 10 s, constant velocity (accel = 0)
        assert!((result.state[0] - 10.0).abs() < 1e-6);
        assert!((result.state[1] - 20.0).abs() < 1e-6);
        assert_eq!(result.t_tai_ns, 10_000_000_000);
    }

    /// A model whose own `step` override does something `derivatives` alone never could --
    /// populate `StepResult.outputs` -- mirroring exactly why `gmat_sys::model::GmatModel::
    /// step` is overridden (M10.2, `docs/open-questions.md` question 95's second half).
    struct OutputStepping;
    impl DynamicsModel for OutputStepping {
        type Error = std::convert::Infallible;
        fn state_dim(&self) -> usize {
            1
        }
        fn derivatives(&self, _state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            out[0] = 0.0;
            Ok(())
        }
        fn describe(&self) -> av_cdm::pb::ModelInfo {
            av_cdm::pb::ModelInfo { id: "test.output_stepping".to_string(), ..Default::default() }
        }
        fn step(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
            let mut outputs = std::collections::BTreeMap::new();
            outputs.insert("marker".to_string(), 42.0);
            Ok(StepResult { state: state.to_vec(), t_tai_ns: t_tai_ns + dt_ns, outputs })
        }
        // This model's own override is about StepResult.outputs, not telemetry -- no CDM
        // measurement here.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
    }

    #[test]
    fn erased_model_delegates_a_wrapped_models_own_step_override() {
        // The actual gap M10.3 closes: before this fix, `ErasedModel::step` inherited the
        // trait's blanket default (state recomputed via `derivatives`, `outputs` always empty)
        // regardless of what the wrapped model's own `step` did -- this would have silently
        // dropped `OutputStepping::step`'s `outputs` entry. Erased through `erase_with_id`
        // exactly like any real model, and the marker survives.
        let boxed: BoxedModel = erase_with_id("test.output_stepping", OutputStepping, |_model_id, never: std::convert::Infallible| match never {});
        let result = boxed.step(&[0.0], 0, &[], 1_000_000_000).unwrap();
        assert_eq!(result.outputs.get("marker"), Some(&42.0), "ErasedModel::step must delegate to the wrapped model's own step override, not the trait default");
    }

    /// A model that overrides `step_with_ports` to emit one message, so erasure can be shown
    /// to preserve the override rather than falling back to the trait default's empty `Outbox`.
    struct PortStepping;
    impl DynamicsModel for PortStepping {
        type Error = std::convert::Infallible;
        fn state_dim(&self) -> usize {
            1
        }
        fn derivatives(&self, _state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            out[0] = 0.0;
            Ok(())
        }
        fn describe(&self) -> av_cdm::pb::ModelInfo {
            av_cdm::pb::ModelInfo { id: "test.port_stepping".to_string(), ..Default::default() }
        }
        fn step_with_ports(
            &self,
            state: &[f64],
            t_tai_ns: i64,
            controls: &[f64],
            dt_ns: i64,
            _inbox: &Inbox,
        ) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
            let stepped = self.step(state, t_tai_ns, controls, dt_ns)?;
            let mut outbox = Outbox::new();
            outbox.push_signal("out", stepped.t_tai_ns, 7.0);
            let applied = vec![AppliedCommand { port: "in".to_string(), field: "marker_field".to_string(), value: 3.0, applied_tai_ns: t_tai_ns }];
            Ok((stepped, outbox, applied))
        }
        // This model's own override is about port traffic, not telemetry -- no CDM measurement.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
    }

    #[test]
    fn erased_model_delegates_a_wrapped_models_own_step_with_ports_override() {
        // The same gap `step` (M10.3) and `step_with_stm` (M11.2) each had, for the port
        // method M13.1 added: without an `ErasedModel::step_with_ports` override, erasing a
        // port-aware model silently falls back to the trait default -- plain `step` and an
        // *empty* `Outbox` -- with no error to notice it by. Inert while nothing overrides
        // `step_with_ports`, and live the moment a bound container model is erased. Also checks
        // the third tuple element (question 130): a regression that mapped only `(result,
        // outbox)` through and dropped `applied` (e.g. `.map(|(r, o)| (r, o, Vec::new()))`)
        // would pass every other assertion here but fail the one on `applied`.
        let boxed: BoxedModel = erase_with_id("test.port_stepping", PortStepping, |_model_id, never: std::convert::Infallible| match never {});
        let (_stepped, outbox, applied) = boxed.step_with_ports(&[0.0], 0, &[], 1_000_000_000, &Inbox::empty()).unwrap();
        let sent = outbox.messages();
        assert_eq!(sent.len(), 1, "ErasedModel::step_with_ports must delegate to the wrapped model's own override, not the trait default's empty Outbox");
        assert_eq!(sent[0].port, "out");
        assert_eq!(crate::decode_signal(&sent[0].payload).unwrap(), 7.0);
        assert_eq!(applied.len(), 1, "ErasedModel::step_with_ports must delegate the wrapped model's own applied commands too, not silently drop them");
        assert_eq!(applied[0].field, "marker_field");
    }

    // -- Full per-method delegation coverage (question 112) ---------------------------------
    //
    // "ErasedModel delegation is a recurring defect class" -- three occurrences so far (`step`
    // in M10.3, `step_with_stm` in M11.2, `step_with_ports` in M13.1/M13.2), each a
    // `DynamicsModel` method whose *trait* default silently no-opped through the erased path
    // because `ErasedModel` had not been taught to override it yet. `ErasedModel` above now
    // overrides every method the trait declares, but "currently does" is not the same guarantee
    // as "a future ninth method added to the trait will be caught if this crate's author forgets
    // to add the tenth override" -- the tests below are the one-per-method proof this task's
    // brief asks for: each one drives the *erased* path and checks for a marker only the
    // *wrapped* model's own override could have produced, so a regression that silently deletes
    // one of `ErasedModel`'s overrides (falling back to the trait's own default) fails exactly
    // one of these, by name.

    /// Every `DynamicsModel` method overridden with its own distinguishable, non-default
    /// value or behavior -- deliberately never a value the trait's own default could produce by
    /// coincidence -- so each test below fails loudly if `ErasedModel` silently stopped
    /// delegating that one method.
    struct AllOverridden;
    impl DynamicsModel for AllOverridden {
        type Error = std::convert::Infallible;

        fn state_dim(&self) -> usize {
            3
        }
        fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            // A transform no default step/integration path would coincidentally reproduce.
            for (o, s) in out.iter_mut().zip(state) {
                *o = s * 10.0 + 1.0;
            }
            Ok(())
        }
        fn describe(&self) -> av_cdm::pb::ModelInfo {
            av_cdm::pb::ModelInfo { id: "test.all_overridden".to_string(), ..Default::default() }
        }
        fn integrator(&self) -> Dopri5 {
            // Every field moved away from `Dopri5::default()` (rtol=atol=1e-12,
            // initial_step=30, max_step=600), so a test that saw the default by coincidence
            // (delegation silently missing) cannot pass by accident.
            Dopri5 { rtol: 1e-3, atol: 1e-4, initial_step: 1.0, max_step: 2.0 }
        }
        fn stm_capable(&self) -> bool {
            true
        }
        fn stm_derivatives(&self, _augmented_state: &[f64], _t_tai_ns: i64, _controls: &[f64], augmented_state_dot: &mut [f64]) -> Result<(), Self::Error> {
            // A constant marker, distinguishable from both zero and from `derivatives`'s own
            // transform above.
            for o in augmented_state_dot.iter_mut() {
                *o = 7.0;
            }
            Ok(())
        }
        fn step(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
            let mut outputs = std::collections::BTreeMap::new();
            outputs.insert("step_marker".to_string(), 99.0);
            Ok(StepResult { state: state.to_vec(), t_tai_ns: t_tai_ns + dt_ns, outputs })
        }
        fn step_with_stm(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64) -> Result<StmStepResult, Self::Error> {
            let mut outputs = std::collections::BTreeMap::new();
            outputs.insert("stm_step_marker".to_string(), 88.0);
            Ok(StmStepResult { state: state.to_vec(), phi: vec![5.0; state.len() * state.len()], t_tai_ns: t_tai_ns + dt_ns, outputs })
        }
        fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64, _inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
            let mut outbox = Outbox::new();
            outbox.push_signal("all_overridden_port", t_tai_ns + dt_ns, 55.0);
            let applied = vec![AppliedCommand { port: "all_overridden_in".to_string(), field: "all_overridden_field".to_string(), value: 66.0, applied_tai_ns: t_tai_ns }];
            Ok((StepResult { state: state.to_vec(), t_tai_ns: t_tai_ns + dt_ns, outputs: std::collections::BTreeMap::new() }, outbox, applied))
        }
        // A marker measurement no default/inherited path could produce by coincidence -- the
        // `last_measurements` counterpart of every other `..._marker`/distinguishable-value
        // override above (question 173, M25.3c: this method is now required, not defaulted).
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            vec![av_cdm::pb::Measurement { measurement_id: "all_overridden_measurement".to_string(), z: vec![123.0], epoch_ns: 0, sensor_id: String::new(), frame_id: String::new(), ..Default::default() }]
        }
    }

    fn all_overridden_boxed() -> BoxedModel {
        erase_with_id("test.all_overridden", AllOverridden, |_model_id, never: std::convert::Infallible| match never {})
    }

    #[test]
    fn erased_model_reaches_inner_state_dim() {
        assert_eq!(all_overridden_boxed().state_dim(), 3);
    }

    #[test]
    fn erased_model_reaches_inner_derivatives() {
        let boxed = all_overridden_boxed();
        let mut out = [0.0; 3];
        boxed.derivatives(&[1.0, 2.0, 3.0], 0, &[], &mut out).unwrap();
        assert_eq!(out, [11.0, 21.0, 31.0], "must reach AllOverridden::derivatives's own transform, not some other computation");
    }

    #[test]
    fn erased_model_reaches_inner_describe() {
        assert_eq!(all_overridden_boxed().describe().id, "test.all_overridden");
    }

    #[test]
    fn erased_model_reaches_inner_integrator() {
        let got = all_overridden_boxed().integrator();
        assert_eq!(got.rtol, 1e-3);
        assert_eq!(got.atol, 1e-4);
        assert_eq!(got.initial_step, 1.0);
        assert_eq!(got.max_step, 2.0);
    }

    #[test]
    fn erased_model_reaches_inner_stm_capable() {
        assert!(all_overridden_boxed().stm_capable(), "AllOverridden declares stm_capable() true");
    }

    #[test]
    fn erased_model_reaches_inner_stm_derivatives() {
        let boxed = all_overridden_boxed();
        let mut out = [0.0; 12]; // n=3, n + n^2 = 12
        boxed.stm_derivatives(&[0.0; 12], 0, &[], &mut out).unwrap();
        assert!(out.iter().all(|&v| v == 7.0), "must reach AllOverridden::stm_derivatives's own marker");
    }

    #[test]
    fn erased_model_reaches_inner_step() {
        let boxed = all_overridden_boxed();
        let result = boxed.step(&[1.0, 2.0, 3.0], 0, &[], 1_000_000_000).unwrap();
        assert_eq!(result.outputs.get("step_marker"), Some(&99.0), "must reach AllOverridden::step's own override, not the trait's default integration");
    }

    #[test]
    fn erased_model_reaches_inner_step_with_stm() {
        let boxed = all_overridden_boxed();
        let result = boxed.step_with_stm(&[1.0, 2.0, 3.0], 0, &[], 1_000_000_000).unwrap();
        assert_eq!(result.outputs.get("stm_step_marker"), Some(&88.0), "must reach AllOverridden::step_with_stm's own override, not the trait's default integration");
        assert!(result.phi.iter().all(|&v| v == 5.0));
    }

    #[test]
    fn erased_model_reaches_inner_step_with_ports() {
        let boxed = all_overridden_boxed();
        let (_, outbox, applied) = boxed.step_with_ports(&[1.0, 2.0, 3.0], 0, &[], 1_000_000_000, &Inbox::empty()).unwrap();
        let sent = outbox.messages();
        assert_eq!(sent.len(), 1, "must reach AllOverridden::step_with_ports's own override, not the trait default's empty Outbox");
        assert_eq!(sent[0].port, "all_overridden_port");
        assert_eq!(applied.len(), 1, "must reach AllOverridden::step_with_ports's own applied-commands override, not the trait default's empty Vec");
        assert_eq!(applied[0].field, "all_overridden_field");
    }

    /// M25.3c: `ErasedModel::last_measurements` must reach the wrapped model's own override, not
    /// silently fall back to some other empty result -- the identical delegation-recurring-defect
    /// concern this file's own "Full per-method delegation coverage" section doc comment
    /// describes for `step`/`step_with_stm`/`step_with_ports`, now proven for the newest trait
    /// method too. Fails against an `ErasedModel::last_measurements` that returns `Vec::new()`
    /// unconditionally instead of delegating to `self.inner.last_measurements()`.
    #[test]
    fn erased_model_reaches_inner_last_measurements() {
        let boxed = all_overridden_boxed();
        let got = boxed.last_measurements();
        assert_eq!(got.len(), 1, "must reach AllOverridden::last_measurements's own override, not the wrong empty result");
        assert_eq!(got[0].measurement_id, "all_overridden_measurement");
        assert_eq!(got[0].z, vec![123.0]);
    }

    #[test]
    fn erased_model_stm_derivatives_returns_a_typed_capability_missing_error_instead_of_panicking_when_stm_capable_is_false() {
        // `Flaky` never overrides `stm_capable` (default `false`); before this task's addendum,
        // `ErasedModel::stm_derivatives` delegated unconditionally and this call would have
        // reached the trait's own `unimplemented!()` default and panicked the whole process.
        let boxed: BoxedModel = erase_with_id("test.flaky", Flaky { fail_after: Cell::new(100) }, |model_id, detail| ModelError::Numerical { model_id, detail });
        let mut out = [0.0; 42]; // size is irrelevant: the capability check short-circuits first
        let err = boxed.stm_derivatives(&[0.0; 42], 0, &[], &mut out).unwrap_err();
        assert!(matches!(err, ModelError::CapabilityMissing { ref capability, .. } if capability == "stm_derivatives"), "{err:?}");
        assert_eq!(err.model_id(), "test.flaky");
    }
}
