//! `av-sweep`: F1a (`docs/feasibility-plan.md`'s F1 milestone, first half) -- a pure,
//! GMAT-free library for the feasibility-mode parameter sweep executor. Loads a
//! `ParameterSweep` (YAML, typed, hashed like every other artifact this platform authors),
//! expands its axes into the study's grid, derives each sample's seed, and produces each
//! sample's per-sample DRM/SOS configuration and its config hash. Consumes
//! `av_kernel::drm::{schema, hash}` as a library; never edits, and this crate's own tests never
//! run, `crates/av-kernel/src/drm/{executor,router,fault,sensors}.rs`.
//!
//! F1b (the process-parallel executor and the `av-sweep` CLI binary that actually calls
//! `av_kernel::drm::execute`) is now built on top of this library, at
//! `src/bin/av-sweep/{main,cli,study,sample_mode,json}.rs` -- see that binary's own module doc
//! comments (`main.rs` in particular) for the two-mode (study/sample) design and why GMAT is
//! only ever touched in sample mode. F2 (per-point aggregates across draws, and a study-store
//! trait) is not built here either.
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

pub mod error;
pub mod grid;
pub mod hash;
pub mod sample;
pub mod schema;
pub mod seed;

pub use error::SweepError;
pub use grid::{expand_grid, AxisValue, GridPoint};
pub use hash::{canonical_sweep_hash, sample_config_hash, sha256_hex, verify_sweep_hash};
pub use sample::{sample_config, SampleConfig};
pub use schema::parse_sweep_yaml;
pub use seed::derive_seed;
