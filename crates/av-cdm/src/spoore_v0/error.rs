//! Errors raised converting between `spoore.v0` (via `spoore-cdm`'s hand-written native
//! types) and `altavista.v1` (this crate's generated [`crate::pb`] types).

use thiserror::Error;

/// A `spoore.v0` <-> `altavista.v1` conversion failed.
///
/// Never `unwrap`ped away: `pb::*` values arrive from outside the boundary (decode), so
/// every fallible step is represented here. Encoding (native `spoore_cdm` -> `pb`) is
/// infallible instead -- see the module docs of [`crate::spoore_v0`].
#[derive(Debug, Error)]
pub enum Error {
    /// `spoore-cdm`'s native constructor rejected the decoded values: wrong shape, a
    /// non-finite element, a covariance that is not symmetric positive definite, mixture
    /// weights that do not sum to 1, an unknown state space, and so on. Validation happens
    /// exactly once, here, at the `pb -> spoore_cdm` boundary -- the same rule spoore-cdm's
    /// own `proto` module follows for its `spoore.v0 -> spoore_cdm` boundary.
    #[error(transparent)]
    Spoore(#[from] spoore_cdm::CdmError),

    /// A v1 `frame_id` string did not match any entry of this adapter's frame table
    /// ([`crate::spoore_v0::frame`]). Never defaulted to a frame silently.
    #[error("unknown altavista.v1 frame_id {frame_id:?}; expected one of {known:?}")]
    UnknownFrameId {
        frame_id: String,
        known: &'static [&'static str],
    },

    /// A `pb` message field that spoore.v0's shape requires (an embedded message spoore
    /// always sets) was absent from the decoded value.
    #[error("v1 {message}.{field} is required but was not set")]
    MissingField {
        message: &'static str,
        field: &'static str,
    },

    /// A `pb::Innovation`'s `s` (row-major, square) does not have `nu.len()^2` elements, so
    /// it cannot be reshaped into a matrix. `spoore_cdm::Innovation::new` itself performs no
    /// validation (its caller owns the arithmetic, per its doc comment); this is the one
    /// check the adapter still owes the input, since building the matrix would otherwise
    /// need to panic on malformed data.
    #[error("innovation: nu has dimension {nu_dim} but s has {s_len} elements, not {nu_dim}^2")]
    InnovationDimensionMismatch { nu_dim: usize, s_len: usize },
}

pub type Result<T> = std::result::Result<T, Error>;
