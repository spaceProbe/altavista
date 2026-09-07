//! A DRM author's workflow for question 87's "compute and verify the canonical hashes":
//! write the YAML with `hash: ""` (or any placeholder), run this tool, paste the printed
//! digest back into the file's own `hash` field. `crates/av-kernel/src/drm/executor.rs`
//! recomputes and checks the same digest at run time (`hash::canonical_*_hash`) and refuses a
//! DRM/SosConfiguration/SystemDefinition whose declared `hash` disagrees -- this is simply
//! that same computation exposed as a standalone tool, so an author is not left guessing.
//!
//! ```text
//! cargo run -p av-kernel --example drm_hash -- drm    drms/leo_1day_golden.drm.yaml
//! cargo run -p av-kernel --example drm_hash -- sos    drms/leo_1day_golden.sos.yaml
//! cargo run -p av-kernel --example drm_hash -- system drms/leo_1day_golden.system.yaml
//! ```

use std::env;
use std::fs;
use std::process::ExitCode;

use av_kernel::drm::{hash, schema};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let [_, kind, path] = args.as_slice() else {
        eprintln!("usage: drm_hash <drm|sos|system> <path.yaml>");
        return ExitCode::FAILURE;
    };
    let yaml = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("reading {path:?}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let digest = match kind.as_str() {
        "drm" => schema::parse_drm_yaml(&yaml).map(|m| hash::canonical_drm_hash(&m)),
        "sos" => schema::parse_sos_yaml(&yaml).map(|m| hash::canonical_sos_hash(&m)),
        "system" => schema::parse_system_definition_yaml(&yaml).map(|m| hash::canonical_system_hash(&m)),
        other => {
            eprintln!("unknown kind {other:?}; expected drm, sos, or system");
            return ExitCode::FAILURE;
        }
    };
    match digest {
        Ok(d) => {
            println!("{d}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{path}: {e}");
            ExitCode::FAILURE
        }
    }
}
