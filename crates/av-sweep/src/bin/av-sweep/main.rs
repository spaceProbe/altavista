//! `av-sweep`: F1b (`docs/feasibility-plan.md`'s F1 milestone, second half; F1a is
//! `crates/av-sweep/src/lib.rs`, the pure, GMAT-free sweep library this binary drives) -- the
//! process-parallel executor for a feasibility-mode parameter sweep.
//!
//! ## Two modes, one executable, and why
//!
//! `gmat_sys`'s own module doc: GMAT is a process-global singleton, not thread-safe -- exactly
//! one `Gmat` handle, and one DRM run, per process (the same constraint
//! `crates/av-run/src/main.rs` documents and honours). A study fans a sweep out over many
//! samples, each of which needs its own independent GMAT run, so this binary is deliberately
//! two modes in one executable rather than a library function a caller loops over in-process:
//!
//! - **Study mode** (the default; [`study::run_study`]) -- the parent. Loads and hash-verifies
//!   the sweep and its base DRM/SOS/`SystemDefinition`s, expands the grid
//!   (`av_sweep::expand_grid`), builds every sample's own per-sample configuration
//!   (`av_sweep::sample_config`), writes each sample's inputs to disk, and spawns up to
//!   `--workers` children at a time -- each this same binary, re-invoked with `--run-sample` --
//!   polling with `Child::try_wait` (no new dependency, no reader thread; see `study.rs`'s own
//!   doc comment on `spawn_sample` for how the child's stderr is captured without one). Never
//!   constructs a `Gmat` handle itself.
//! - **Sample mode** ([`sample_mode::run_sample`]) -- a child, one per sample, the *only* place
//!   this binary touches GMAT. Decodes its three `prost::Message` inputs and validates them
//!   (canonical hash checks) before ever calling `gmat_sys::engine_lock()`, then runs
//!   `av_kernel::drm::execute` exactly as `av-run` does and writes the resulting `RunProducts`
//!   protobuf to `--out`.
//!
//! See `study.rs`, `sample_mode.rs`, `cli.rs` and `json.rs` for the rest -- each carries its own
//! module doc comment for the part of this task's brief it implements.

mod cli;
mod json;
mod sample_mode;
mod study;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("av-sweep: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match cli::parse_cli(args)? {
        cli::Cli::Study(a) => {
            let exe = std::env::current_exe().map_err(|e| format!("resolving this binary's own path (to re-invoke it as --run-sample children): {e}"))?;
            study::run_study(a, &exe)
        }
        cli::Cli::Sample(a) => sample_mode::run_sample(a),
    }
}
