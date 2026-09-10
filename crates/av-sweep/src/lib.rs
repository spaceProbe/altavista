//! `av-sweep`: F1a (`docs/feasibility-plan.md`'s F1 milestone, first half) -- a pure,
//! GMAT-free library for the feasibility-mode parameter sweep executor. Loads a
//! `ParameterSweep` (YAML, typed, hashed like every other artifact this platform authors),
//! expands its axes into the study's grid, derives each sample's seed, and produces each
//! sample's per-sample DRM/SOS configuration and its config hash. Consumes
//! `av_kernel::drm::{schema, hash}` as a library; never edits, and this crate's own tests never
//! run, `crates/av-kernel/src/drm/{executor,router,fault,sensors}.rs`.
//!
//! F1b (the process-parallel executor and the `av-sweep` CLI binary that actually calls
//! `av_kernel::drm::execute`) is built on top of this library, at
//! `src/bin/av-sweep/{main,cli,study,sample_mode}.rs` -- see that binary's own module doc
//! comments (`main.rs` in particular) for the two-mode (study/sample) design and why GMAT is
//! only ever touched in sample mode.
//!
//! F2 (`docs/feasibility-plan.md`'s F2 milestone: per-point aggregates across draws, and a
//! study-store trait) IS built here, in [`aggregate`] and [`store`] -- both pure Rust, no GMAT,
//! consumed by the binary's own `study.rs`. [`json`] also moved here as part of F2 (it lived at
//! `src/bin/av-sweep/json.rs` under F1b) so [`store::FileStudyStore`] can reuse the same
//! hand-written proto3 canonical JSON encoder rather than this crate writing a second one; see
//! `json`'s own module doc comment for that move's disclosure.
//!
//! ## Module layout
//!
//! - [`error`] -- [`error::SweepError`], every way a sweep operation can be refused.
//! - [`schema`] -- the `ParameterSweep`/`SweepAxis` YAML authoring format.
//! - [`hash`] -- the canonical `ParameterSweep` hash, and [`hash::sample_config_hash`] (F1a's
//!   own addition, not a mirror of `av_kernel::drm::hash`).
//! - [`grid`] -- [`grid::expand_grid`], turning declared axes into the study's ordered grid.
//! - [`seed`] -- [`seed::derive_seed`], per-sample seed derivation.
//! - [`sample`] -- [`sample::sample_config`], the per-sample DRM/SOS configuration.
//! - [`aggregate`] -- [`aggregate::aggregate`] (F2), per-`(point, score)` mean/std_dev/min/max/
//!   pass_fraction across a study's draws.
//! - [`json`] -- the proto3 canonical JSON encoder for `SweepResults` and everything it reaches.
//! - [`store`] -- [`store::StudyStore`] (F2), the study-store trait, [`store::FileStudyStore`],
//!   and the (deferred, typed-refusal) ClickHouse backend seam.

pub mod aggregate;
pub mod error;
pub mod grid;
pub mod hash;
pub mod json;
pub mod sample;
pub mod schema;
pub mod seed;
pub mod store;

pub use aggregate::aggregate;
pub use error::SweepError;
pub use grid::{expand_grid, AxisValue, GridPoint};
pub use hash::{canonical_sweep_hash, sample_config_hash, sha256_hex, verify_sweep_hash};
pub use sample::{sample_config, SampleConfig};
pub use schema::parse_sweep_yaml;
pub use seed::derive_seed;
