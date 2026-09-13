//! `av-run`'s library half: `src/main.rs`'s CLI binary is the M16.3 half of this crate; this
//! `lib.rs` exists so `crates/av-run/tests/*.rs` (a separate compilation unit, integration
//! tests, per Cargo convention) can `use av_run::command_adapter::...` -- a binary-only crate
//! has no public API a `tests/` directory could otherwise import at all. `src/main.rs` is
//! unchanged by this: it is still the crate's `[[bin]]` target, compiled and run exactly as
//! before.
//!
//! See [`command_adapter`]'s own module doc comment for what actually lives here (A3.2,
//! `docs/aiplane-plan.md` milestone A3's D1: the seam between `av_command::service::
//! DispatchSink` and `av_kernel::drm::command_source::ExternalCommandSource`).
pub mod command_adapter;
