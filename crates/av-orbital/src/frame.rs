//! The inertial <-> body-fixed rotation, as a trait `av_orbital::model::EarthGravityModel` is
//! generic over (`docs/native-dynamics-plan.md` milestone N1, charter decision 222(c)).
//!
//! Decision 222(c): frames use the GMAT-validated `convert` shim (`crate::frame_gmat`, behind
//! the `gmat-frames` cargo feature) until N5 replaces the rotation with a native
//! IAU-76/FK5 + EOP implementation. [`BodyFixedRotation`] is the seam that makes N5 a drop-in:
//! [`crate::model::EarthGravityModel`] is generic over any `R: BodyFixedRotation`, never over
//! the GMAT-backed type by name, so an N5 native rotation type implementing this same trait
//! needs no change to the model, only a new impl of this module's trait.
//!
//! This module itself has no GMAT dependency and is always compiled -- only
//! [`crate::frame_gmat::GmatBodyFixedRotation`] (a separate module, behind `gmat-frames`) does.

/// A proper 3x3 rotation matrix and its time derivative, at one instant.
///
/// **Direction convention (read before using `r` or `r_dot`):** `r` maps a vector's
/// components from the INERTIAL frame to the BODY-FIXED frame -- for any physical vector
/// (not a full 6-state; this crate never rotates velocity through this matrix, see
/// [`crate::model`]'s module doc for why), `v_fixed = r * v_inertial`. Equivalently, `r`'s
/// row `i` is the body-fixed frame's `i`-th basis vector expressed in inertial coordinates
/// (`r[i][j] = dot(fixed_axis_i, inertial_axis_j)`), matching
/// `av_kernel::drm::executor::rotation_matrix`'s own documented convention (`v_to = C *
/// v_from`, built the identical way here: from a GMAT `CoordinateConverter::Convert` call
/// whose `from_cs` is the inertial frame and whose `to_cs` is the body-fixed frame -- see
/// [`crate::frame_gmat`]'s module doc for the exact call). `r` is a proper rotation (`r^T =
/// r^-1`, `det(r) = 1`) for any physically valid inertial/body-fixed frame pair, so the
/// INVERSE (body-fixed to inertial) is always `r`'s transpose -- [`Rotation::apply_transpose`]
/// -- never a second, independently-computed matrix; [`crate::model::EarthGravityModel::
/// derivatives`] relies on exactly this fact to need only one rotation call per derivative
/// evaluation (see that module's doc for the measured cost this saves).
///
/// `r_dot` is `dR/dt`, units s^-1 (the same `r` above, differentiated with respect to time --
/// NOT scaled by any state unit, since `r` itself is unitless). Consistency between `r` and
/// `r_dot` (that `r_dot` really is `r`'s own time derivative, not merely self-consistent
/// output from whatever call produced both) is proved by a central finite difference in
/// `tests/frame_gmat.rs`'s `rotation_dot_matches_a_finite_difference_of_the_rotation_matrix`
/// test, mirroring `crates/gmat-sys/tests/convert_rotation.rs`'s identical check on the
/// underlying GMAT call this module's GMAT-backed impl wraps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rotation {
    pub r: [[f64; 3]; 3],
    pub r_dot: [[f64; 3]; 3],
}

impl Rotation {
    /// `r * v` -- rotate a vector's components from the inertial frame to the body-fixed frame
    /// (see this struct's own doc comment for the exact direction convention).
    pub fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        let mut out = [0.0; 3];
        for (i, row) in self.r.iter().enumerate() {
            out[i] = row[0] * v[0] + row[1] * v[1] + row[2] * v[2];
        }
        out
    }

    /// `r^T * v` -- rotate a vector's components from the body-fixed frame back to the
    /// inertial frame. Exact (never an approximation of an inverse): `r` is a proper rotation,
    /// so `r^-1 == r^T` exactly, for any `r` this crate ever constructs (see this struct's own
    /// doc comment).
    pub fn apply_transpose(&self, v: [f64; 3]) -> [f64; 3] {
        // Written out directly (not a loop over an index) because this is a genuine
        // matrix-transpose access (column `j` of `self.r`, for each output component `j`) --
        // there is no single slice `apply`'s own `self.r.iter().enumerate()` form would walk.
        [
            self.r[0][0] * v[0] + self.r[1][0] * v[1] + self.r[2][0] * v[2],
            self.r[0][1] * v[0] + self.r[1][1] * v[1] + self.r[2][1] * v[2],
            self.r[0][2] * v[0] + self.r[1][2] * v[1] + self.r[2][2] * v[2],
        ]
    }
}

/// One inertial <-> body-fixed rotation source, at any absolute TAI instant.
///
/// `Error` mirrors `av_dynamics::DynamicsModel::Error`'s own bound (`Debug + Display`, never
/// constrained to a single concrete error type -- the same reasoning `av_dynamics`'s own
/// module doc gives for why `DynamicsModel::Error` is unconstrained applies here: a native N5
/// implementation's own domain error and `gmat_sys::GmatError` must both be able to implement
/// this trait without either needing a conversion to the other).
pub trait BodyFixedRotation {
    type Error: std::fmt::Debug + std::fmt::Display;

    /// The rotation from the inertial frame to the body-fixed frame, valid at `t_tai_ns`
    /// (absolute TAI nanoseconds -- never a step-relative offset, matching
    /// `DynamicsModel::derivatives`'s own `t_tai_ns` convention).
    fn inertial_to_fixed(&self, t_tai_ns: i64) -> Result<Rotation, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rotation by 90 degrees about the z-axis: inertial x -> body-fixed -y, inertial y ->
    /// body-fixed x (i.e. `r` is the standard `Rz(90deg)` direction-cosine matrix). Picked
    /// because it is easy to check by inspection and is NOT its own inverse (unlike the
    /// identity or a 180-degree rotation), so a transposed `apply`/`apply_transpose` swap would
    /// be caught here.
    fn rz90() -> Rotation {
        Rotation {
            r: [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            r_dot: [[0.0; 3]; 3],
        }
    }

    #[test]
    fn apply_rotates_inertial_x_to_body_fixed_negative_y() {
        let rot = rz90();
        let out = rot.apply([1.0, 0.0, 0.0]);
        assert!((out[0] - 0.0).abs() < 1e-15);
        assert!((out[1] - (-1.0)).abs() < 1e-15);
        assert!((out[2] - 0.0).abs() < 1e-15);
    }

    #[test]
    fn apply_transpose_is_the_genuine_matrix_inverse_not_a_relabelled_apply() {
        let rot = rz90();
        let v = [3.0, -2.0, 5.0];
        let round_trip = rot.apply_transpose(rot.apply(v));
        for i in 0..3 {
            assert!((round_trip[i] - v[i]).abs() < 1e-12, "component {i}: {round_trip:?} vs {v:?}");
        }
        // And the two operations are NOT interchangeable for this (non-self-inverse) rotation
        // -- guards against `apply`/`apply_transpose` being accidentally implemented identically.
        let applied_twice = rot.apply(rot.apply(v));
        let mut differs = false;
        for i in 0..3 {
            if (applied_twice[i] - v[i]).abs() > 1e-9 {
                differs = true;
            }
        }
        assert!(differs, "Rz(90deg) applied twice must not return the original vector (it is a 180deg rotation, not identity)");
    }
}
