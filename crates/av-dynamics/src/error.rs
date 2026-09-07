//! `ModelError`: the one concrete error type every trait-object [`crate::DynamicsModel`]
//! speaks (ADR-005 sec 1, "Models are trait objects behind one error type").
//!
//! `DynamicsModel::Error` stays an associated type on the trait itself (unconstrained, exactly
//! as it always was: an FFI error, `std::convert::Infallible`, a native model's own domain
//! error, ...) -- **nothing about the trait definition changed**, so every existing
//! `impl DynamicsModel for X { type Error = ... }` in this workspace (`gmat_sys::model::
//! GmatModel`'s `GmatError`, this crate's own `ConstantAccel`/`Rotator` test models'
//! `Infallible`, `crate::drm::binding::AnyModel`'s `AnyModelError`, ...) keeps compiling and
//! behaving exactly as before. What makes a model usable as a trait object is fixing that
//! associated type at the point of erasure: `Box<dyn DynamicsModel<Error = ModelError>>` is
//! already valid, dyn-compatible Rust today (the trait has no `Self: Sized` bound on any
//! method, no generic methods, and every default method only ever calls back into `&self`) --
//! see [`crate::erase::ErasedModel`] for the adapter that gets a model with some other `Error`
//! type into that shape.
//!
//! Every variant carries the offending model's id (`av_cdm::pb::ModelInfo.id`, ADR-002), so a
//! caller holding only a `ModelError` -- several models deep behind a registry, a scheduler, a
//! DRM executor -- can still name which model failed.
use std::fmt;

/// One error type every trait-object [`crate::DynamicsModel`] speaks (`Box<dyn
/// DynamicsModel<Error = ModelError>>`, ADR-005 sec 1). See the module doc comment for why the
/// trait's own associated type did not need to change to get here.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelError {
    /// The model's own derivative/integration evaluation failed numerically (a native model's
    /// domain error, an integrator step that could not be accepted, ...).
    Numerical { model_id: String, detail: String },
    /// A capability was invoked that the model's own `describe()`/`stm_capable()` did not
    /// declare (e.g. `stm_derivatives` called on a model that never opted in) -- a caller bug
    /// surfaced as data rather than a panic, once the call crosses a trait-object/registry
    /// boundary where "the caller already checked `stm_capable()` first" cannot be enforced at
    /// compile time.
    CapabilityMissing { model_id: String, capability: String },
    /// A binding transport (gRPC to a remote `DynamicsService`, a container's lockstep
    /// protocol, ...) failed to deliver a step -- ADR-005 sec 1's "binding transport failure".
    BindingTransport { model_id: String, detail: String },
    /// A GMAT FFI call failed (`gmat_sys::GmatError`, erased at the boundary where the concrete
    /// GMAT-touching model is constructed -- see `crate::erase` -- since this crate itself
    /// never depends on GMAT, per this crate's own module doc comment).
    Gmat { model_id: String, detail: String },
    /// M22.1b (`docs/open-questions.md` questions 151/152): a native model's own construction
    /// from a declared spec failed one of that model's typed, load-time checks (a state-space
    /// dimension mismatch, an out-of-tolerance physical parameter, an unrecognized unit, ...) --
    /// the general-purpose counterpart to [`ModelError::Gmat`] for a native (non-GMAT, non-FFI)
    /// constructor that can fail: `crate::drm::attitude::AttitudeWheelsModel::new` (via
    /// `crate::registry::ModelRegistry::construct_attitude`) is the first caller, but this
    /// variant names no attitude-specific concept -- any future native constructor with its own
    /// load-time validation belongs here too, rather than each inventing a parallel variant.
    InvalidSpec { model_id: String, detail: String },
}

impl ModelError {
    /// The id of the model this error came from, common to every variant.
    pub fn model_id(&self) -> &str {
        match self {
            ModelError::Numerical { model_id, .. }
            | ModelError::CapabilityMissing { model_id, .. }
            | ModelError::BindingTransport { model_id, .. }
            | ModelError::Gmat { model_id, .. }
            | ModelError::InvalidSpec { model_id, .. } => model_id,
        }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::Numerical { model_id, detail } => write!(f, "model {model_id:?}: numerical failure: {detail}"),
            ModelError::CapabilityMissing { model_id, capability } => write!(f, "model {model_id:?}: capability {capability:?} was invoked but is not declared"),
            ModelError::BindingTransport { model_id, detail } => write!(f, "model {model_id:?}: binding transport failure: {detail}"),
            ModelError::Gmat { model_id, detail } => write!(f, "model {model_id:?}: GMAT error: {detail}"),
            ModelError::InvalidSpec { model_id, detail } => write!(f, "model {model_id:?}: invalid spec: {detail}"),
        }
    }
}

impl std::error::Error for ModelError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_carries_and_exposes_the_model_id() {
        let cases = [
            ModelError::Numerical { model_id: "m1".to_string(), detail: "d".to_string() },
            ModelError::CapabilityMissing { model_id: "m2".to_string(), capability: "stm".to_string() },
            ModelError::BindingTransport { model_id: "m3".to_string(), detail: "d".to_string() },
            ModelError::Gmat { model_id: "m4".to_string(), detail: "d".to_string() },
            ModelError::InvalidSpec { model_id: "m5".to_string(), detail: "d".to_string() },
        ];
        let want = ["m1", "m2", "m3", "m4", "m5"];
        for (e, id) in cases.iter().zip(want.iter()) {
            assert_eq!(e.model_id(), *id);
            assert!(format!("{e}").contains(id), "Display for {e:?} must mention the model id");
        }
    }
}
