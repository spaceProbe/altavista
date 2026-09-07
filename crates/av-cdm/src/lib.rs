//! AltaVista Common Data Model v1 (`altavista.v1`, ADR-001).
//!
//! This crate is the Rust binding for `proto/altavista/v1/*.proto`: the generated wire
//! types ([`pb`]), TAI epoch handling with the platform's versioned leap-second table
//! ([`time`]), the SI/GMAT unit boundary ([`units`]), and the `spoore.v0` compatibility
//! adapter ([`spoore_v0`]), per `proto/README.md`'s "Compatibility" section.
//!
//! ## Module layout
//!
//! - [`pb`] -- generated `altavista.v1` message and enum types, compiled from
//!   `proto/altavista/v1/*.proto` by `build.rs` via `prost-build`. Treated strictly as a
//!   transport encoding (every proto field is optional and every `repeated double` is an
//!   unconstrained `Vec`), exactly like spoore's own `proto` module.
//! - [`time`] -- `Tai`, a newtype over `i64` nanoseconds on the TAI scale, and the
//!   boundary conversions to/from UTC (table-driven), TT, GPS and GMAT's A.1 (as an MJD).
//! - [`units`] -- SI (metres, metres/second) <-> kilometres, the one conversion site for
//!   the GMAT boundary (ADR-001 "Units").
//! - [`spoore_v0`] -- conversions between `spoore-cdm`'s generated `spoore.v0` wire types
//!   (`spoore_cdm::proto::{GaussianState, Belief, MixtureComponent, Measurement,
//!   Innovation}`) and this crate's [`pb`] types, including the TAI/UTC epoch shift and the
//!   `Frame` <-> `frame_id` mapping (ADR-001 "Compatibility"). See that module's own doc
//!   comment for why the wire types, not `spoore-cdm`'s hand-validated native types, are the
//!   adapter's source and target.
//! - [`covariance`] -- a Cholesky-based SPD hygiene check, mirroring the exact bar
//!   `spoore_cdm::GaussianState`'s constructor enforces, applied to a propagated covariance
//!   *before* it crosses into a spoore type (`docs/open-questions.md` question 80): a typed,
//!   counted error on failure, and an opt-in nearest-SPD projection.

pub mod pb {
    //! Generated `altavista.v1` types.
    //!
    //! Exposed as `av_cdm::pb` (rather than re-exported at the crate root) so call sites
    //! read `av_cdm::pb::GaussianState`, keeping the generated wire types visually distinct
    //! from this crate's hand-written adapters -- the same convention spoore-cdm uses for
    //! its own `proto` module.
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/altavista.v1.rs"));
}

pub mod covariance;
pub mod spoore_v0;
pub mod time;
pub mod units;
