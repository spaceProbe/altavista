//! [`GmatBodyFixedRotation`]: the GMAT-backed [`crate::frame::BodyFixedRotation`] (charter
//! decision 222(c), `docs/native-dynamics-plan.md` milestone N1) -- feature-gated behind
//! `gmat-frames` (default-on) so `crates/av-orbital` still builds, and its non-GMAT tests
//! still pass, with `--no-default-features` (verified in this task's own report).
//!
//! **The exact GMAT call.** [`GmatBodyFixedRotation::inertial_to_fixed`] calls
//! `Gmat::convert_with_rotation(epoch_a1mjd, &[0.0; 6], inertial_cs, fixed_cs)` -- the same
//! shim call `av_kernel::drm::executor::convert_gmat_trajectory_to_declared_frame` uses for a
//! covariance-bearing trajectory sample, reused here for the same reason: it is the one call
//! that returns the rotation matrix and its time derivative alongside the (here, unused --
//! see below) converted state, from a single `CoordinateConverter::Convert` invocation.
//!
//! - `inertial_cs`/`fixed_cs` are two `CoordinateSystem` objects THIS type constructs itself,
//!   under names namespaced by the caller-supplied `namespace` (see [`GmatBodyFixedRotation
//!   ::new`]) -- never GMAT's own pre-existing named defaults (e.g. `"EarthMJ2000Eq"`), for
//!   the identical reason `av_kernel`'s own `materialize_gmat`/`convert_gmat_trajectory_to_
//!   declared_frame`/`fill_fixed_rotations` never touch those defaults either (their own doc
//!   comments: "never touching any of GMAT's own pre-existing named defaults ... so this can
//!   never reconfigure an object some other instance, or GMAT itself, is already relying on").
//!   Built with the exact surface this task's brief names -- `Gmat::coordinate_system(name,
//!   body, axes)` for each (`axes` = `"MJ2000Eq"` for `inertial_cs`, `"BodyFixed"` for
//!   `fixed_cs`), then one `Gmat::initialize()` call after both are constructed, matching
//!   `materialize_gmat`'s own convention (construct everything this instance needs, then
//!   initialize once) -- never `crates/gmat-sys` itself, which this task does not edit.
//! - `epoch_a1mjd` is `Tai::from_nanos(t_tai_ns).to_a1_mjd()` -- byte-for-byte the same
//!   expression `av_kernel::drm::executor::fill_fixed_rotations`/`convert_gmat_trajectory_to_
//!   declared_frame` use at their own `Gmat::convert`/`Gmat::convert_with_rotation` call
//!   sites (`let epoch_a1mjd = Tai::from_nanos(sample.tai_ns).to_a1_mjd();`).
//! - The state argument is `[0.0; 6]` -- the ROTATION this call returns (`rotation`/
//!   `rotation_dot`) is a property of the two `CoordinateSystem`s and the epoch alone, not of
//!   the state being converted (see `Gmat::convert_with_rotation`'s own doc comment: it is
//!   "the 3x3 rotation matrix ... that the SAME `CoordinateConverter::Convert` call computed
//!   WHILE PERFORMING this conversion" -- the Jacobian of the transformation, not a function
//!   of its input), so the converted `state_km` this call also returns is deliberately
//!   discarded (`converted.state_km` is never read) and any zero vector is exactly as valid an
//!   argument as a real state would be, cheaper to construct.
//!
//! **Why one call suffices for both directions.** A naive implementation would call this
//! twice per [`crate::model::EarthGravityModel::derivatives`] evaluation -- once to rotate the
//! inertial position into the body-fixed frame, once more (with `from_cs`/`to_cs` swapped) to
//! rotate the computed acceleration back. This type calls it once: `rotation`/`rotation_dot`
//! from `inertial_cs -> fixed_cs` already give the inverse transform for free, because a
//! direction-cosine matrix between two orthonormal bases is always a proper rotation
//! (`r^-1 == r^T` exactly) -- [`crate::frame::Rotation::apply_transpose`] is that inverse, not
//! a second GMAT call. `derivatives` uses exactly this.
//!
//! **Caching.** `inertial_to_fixed` is called on every derivative evaluation -- tens of
//! thousands of times over a day-long arc (`docs/adr/002-dynamics-contract.md`'s P0-spike
//! amendment measured 63,602 `GetDerivatives` calls for one such arc; this model calls this
//! function once per `derivatives`, so a comparable count). This type keeps a single-entry,
//! EXACT cache keyed on the identical `i64` TAI-nanosecond epoch (`cache`, below) -- never an
//! interpolation or a nearest-epoch match, so a cache hit returns bit-identical output to a
//! fresh call. This is not a hypothetical optimisation: `av_dynamics::integrate::Dopri5`'s own
//! loop evaluates `f(t, x, k[0])` again at the TOP of every step using the just-accepted
//! `(t, x)` from the PREVIOUS step's own last stage (`B5 == A[6]`, the last row of the
//! Butcher tableau, so stage 7 of an accepted step and stage 1 of the next step are evaluated
//! at the identical `(t, x)` pair -- a First-Same-As-Last coincidence this integrator does not
//! itself exploit), so a genuine repeat epoch occurs at every accepted step boundary, not only
//! in pathological input. See this crate's own N1 report for the measured per-call cost and
//! cache hit rate.
use std::cell::RefCell;

use av_cdm::time::Tai;
use gmat_sys::{Gmat, GmatError};

use crate::frame::{BodyFixedRotation, Rotation};

fn unflatten(m: &[f64; 9]) -> [[f64; 3]; 3] {
    [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]]
}

/// The GMAT-backed [`BodyFixedRotation`] (see this module's own doc comment for the exact
/// call, the caching contract, and why one call suffices for both rotation directions).
pub struct GmatBodyFixedRotation {
    gmat: Gmat,
    inertial_cs: String,
    fixed_cs: String,
    /// Single-entry, exact cache (see this module's doc comment) -- `RefCell` because
    /// [`BodyFixedRotation::inertial_to_fixed`] takes `&self` (mirroring `gmat_sys::model::
    /// GmatModel`'s own interior-mutable `last_applied` cache, not a new pattern this crate
    /// introduces), and this type is `!Send` regardless (it owns a `gmat_sys::Gmat` handle
    /// into GMAT's process-wide, non-thread-safe configuration -- see `gmat_sys`'s own module
    /// doc on `engine_lock`), so the interior mutability adds no new cross-thread hazard.
    cache: RefCell<Option<(i64, Rotation)>>,
}

impl GmatBodyFixedRotation {
    /// Builds `inertial_cs`/`fixed_cs` under names namespaced by `namespace` (unique per
    /// caller/run -- see this module's own doc comment for why fresh, namespaced objects are
    /// built rather than GMAT's own pre-existing defaults) and calls [`Gmat::initialize`] once,
    /// exactly `av_kernel::drm::binding::materialize_gmat`'s own convention: construct
    /// everything this instance needs, then initialize once, never once per object.
    pub fn new(gmat: Gmat, central_body: &str, namespace: &str) -> Result<Self, GmatError> {
        let inertial_cs = format!("NatRot{namespace}_{central_body}_Inertial");
        let fixed_cs = format!("NatRot{namespace}_{central_body}_Fixed");
        gmat.coordinate_system(&inertial_cs, central_body, "MJ2000Eq")?;
        gmat.coordinate_system(&fixed_cs, central_body, "BodyFixed")?;
        gmat.initialize()?;
        Ok(Self { gmat, inertial_cs, fixed_cs, cache: RefCell::new(None) })
    }

    /// The `CoordinateSystem` name this type built for the inertial frame (exposed so a test
    /// or caller can drive `Gmat::convert`/`Gmat::convert_with_rotation` independently against
    /// the identical objects this type itself uses -- see `tests/frame_gmat.rs`).
    pub fn inertial_cs_name(&self) -> &str {
        &self.inertial_cs
    }

    /// The `CoordinateSystem` name this type built for the body-fixed frame (see
    /// [`GmatBodyFixedRotation::inertial_cs_name`]'s own doc comment).
    pub fn fixed_cs_name(&self) -> &str {
        &self.fixed_cs
    }
}

impl BodyFixedRotation for GmatBodyFixedRotation {
    type Error = GmatError;

    fn inertial_to_fixed(&self, t_tai_ns: i64) -> Result<Rotation, GmatError> {
        if let Some((cached_t, rotation)) = *self.cache.borrow() {
            if cached_t == t_tai_ns {
                return Ok(rotation);
            }
        }
        let epoch_a1mjd = Tai::from_nanos(t_tai_ns).to_a1_mjd();
        let converted = self.gmat.convert_with_rotation(epoch_a1mjd, &[0.0; 6], &self.inertial_cs, &self.fixed_cs)?;
        let rotation = Rotation { r: unflatten(&converted.rotation), r_dot: unflatten(&converted.rotation_dot) };
        *self.cache.borrow_mut() = Some((t_tai_ns, rotation));
        Ok(rotation)
    }
}
