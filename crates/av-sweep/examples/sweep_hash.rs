//! A `ParameterSweep` author's workflow, mirroring `crates/av-kernel/examples/drm_hash.rs`
//! exactly (same "write hash: \"\", run this, paste the digest back" convention, same
//! `-> ExitCode` shape) -- this task's own brief needs a hash tool for `ParameterSweep` the way
//! `drm_hash` already exists for `DesignReferenceMission`/`SosConfiguration`/`SystemDefinition`,
//! and there is no reason to invent a second convention for it.
//!
//! ```text
//! cargo run -p av-sweep --example sweep_hash -- drms/demo_two_instance_sweep.sweep.yaml
//! ```

use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let [_, path] = args.as_slice() else {
        eprintln!("usage: sweep_hash <sweep.yaml>");
        return ExitCode::FAILURE;
    };
    let yaml = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("reading {path:?}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match av_sweep::parse_sweep_yaml(&yaml).map(|sweep| av_sweep::canonical_sweep_hash(&sweep)) {
        Ok(digest) => {
            println!("{digest}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{path}: {e}");
            ExitCode::FAILURE
        }
    }
}
