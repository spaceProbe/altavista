//! Command-line parsing for the two modes this binary has -- see `main.rs`'s own module doc
//! comment for why one binary has two modes at all (GMAT is a process-global singleton, so
//! exactly one DRM run happens per process). Mirrors `crates/av-run/src/main.rs`'s own CLI
//! parsing style (`Result<_, String>`, a `usage()` string appended to every parse error) rather
//! than inventing a new convention for this binary.

use std::path::PathBuf;

use av_kernel::drm::ExecutionErrorMode;

#[derive(Debug, PartialEq)]
pub enum Cli {
    Study(StudyArgs),
    Sample(SampleArgs),
}

/// Study mode: the parent. Never initializes GMAT (see `study.rs`'s own module doc comment).
#[derive(Debug, PartialEq)]
pub struct StudyArgs {
    pub sweep: PathBuf,
    pub drm: PathBuf,
    pub sos: PathBuf,
    pub systems: Vec<PathBuf>,
    pub out_dir: PathBuf,
    /// Required -- no silent default (this task's own brief: "`--workers` is required"). `0` is
    /// a typed refusal ([`parse_study`]), not treated as "run nothing" or "unbounded".
    pub workers: u32,
    pub gmat_startup: Option<String>,
}

/// Sample mode: one child, one sample, the only place GMAT is touched.
#[derive(Debug, PartialEq)]
pub struct SampleArgs {
    pub drm_pb: PathBuf,
    pub sos_pb: PathBuf,
    pub system_pbs: Vec<PathBuf>,
    pub run_id: String,
    pub error_mode: ExecutionErrorMode,
    pub out: PathBuf,
    pub gmat_startup: Option<String>,
}

fn usage_study(prog: &str) -> String {
    format!(
        "usage: {prog} --sweep <sweep.yaml> --drm <drm.yaml> --sos <sos.yaml> \
         --system <path> [--system <path> ...] --out-dir <dir> --workers <N> \
         [--gmat-startup <path>]\n\n\
         Study mode (the parent): loads and hash-verifies the sweep, expands its grid, builds \
         every sample's per-sample DRM/SOS configuration, writes each sample's inputs, spawns \
         up to --workers children (one per sample, each this same binary invoked with \
         --run-sample) at a time, collects their results, and writes sweep_results.pb/.json \
         under --out-dir. --workers is required; 0 is refused."
    )
}

fn usage_sample(prog: &str) -> String {
    format!(
        "usage: {prog} --run-sample --drm-pb <p> --sos-pb <p> --system-pb <p> \
         [--system-pb <p> ...] --run-id <id> --error-mode nominal|sampled --out <run_products.pb> \
         [--gmat-startup <path>]\n\n\
         Sample mode (a child, one per sample): decodes the three message kinds \
         (prost::Message::decode, no bespoke envelope) and validates them (canonical hash \
         checks) BEFORE touching GMAT, then runs av_kernel::drm::execute exactly as \
         crates/av-run/src/main.rs does and writes the resulting RunProducts protobuf to --out."
    )
}

/// Dispatches on `--run-sample`'s presence anywhere in `args` (after the program name) --
/// study mode never has a reason to pass that flag, and sample mode always does.
pub fn parse_cli(args: &[String]) -> Result<Cli, String> {
    let prog = args.first().map(String::as_str).unwrap_or("av-sweep");
    if args.iter().skip(1).any(|a| a == "--run-sample") {
        parse_sample(args, prog).map(Cli::Sample)
    } else {
        parse_study(args, prog).map(Cli::Study)
    }
}

fn take_next(args: &[String], i: &mut usize, flag: &str, usage: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i).cloned().ok_or_else(|| format!("{flag} needs a value\n\n{usage}"))
}

fn parse_study(args: &[String], prog: &str) -> Result<StudyArgs, String> {
    let usage = usage_study(prog);
    let mut sweep = None;
    let mut drm = None;
    let mut sos = None;
    let mut systems = Vec::new();
    let mut out_dir = None;
    let mut workers = None;
    let mut gmat_startup = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--sweep" => sweep = Some(PathBuf::from(take_next(args, &mut i, "--sweep", &usage)?)),
            "--drm" => drm = Some(PathBuf::from(take_next(args, &mut i, "--drm", &usage)?)),
            "--sos" => sos = Some(PathBuf::from(take_next(args, &mut i, "--sos", &usage)?)),
            "--system" => systems.push(PathBuf::from(take_next(args, &mut i, "--system", &usage)?)),
            "--out-dir" => out_dir = Some(PathBuf::from(take_next(args, &mut i, "--out-dir", &usage)?)),
            "--workers" => {
                let v = take_next(args, &mut i, "--workers", &usage)?;
                let n: u32 = v.parse().map_err(|_| format!("--workers must be a non-negative integer, got {v:?}\n\n{usage}"))?;
                workers = Some(n);
            }
            "--gmat-startup" => gmat_startup = Some(take_next(args, &mut i, "--gmat-startup", &usage)?),
            "-h" | "--help" => return Err(usage),
            other => return Err(format!("unrecognized argument {other:?}\n\n{usage}")),
        }
        i += 1;
    }

    let sweep = sweep.ok_or_else(|| format!("--sweep is required\n\n{usage}"))?;
    let drm = drm.ok_or_else(|| format!("--drm is required\n\n{usage}"))?;
    let sos = sos.ok_or_else(|| format!("--sos is required\n\n{usage}"))?;
    if systems.is_empty() {
        return Err(format!("at least one --system is required\n\n{usage}"));
    }
    let out_dir = out_dir.ok_or_else(|| format!("--out-dir is required\n\n{usage}"))?;
    let workers = workers.ok_or_else(|| format!("--workers is required\n\n{usage}"))?;
    if workers == 0 {
        return Err(format!("--workers must be at least 1, got 0\n\n{usage}"));
    }
    Ok(StudyArgs { sweep, drm, sos, systems, out_dir, workers, gmat_startup })
}

fn parse_sample(args: &[String], prog: &str) -> Result<SampleArgs, String> {
    let usage = usage_sample(prog);
    let mut drm_pb = None;
    let mut sos_pb = None;
    let mut system_pbs = Vec::new();
    let mut run_id = None;
    let mut error_mode = None;
    let mut out = None;
    let mut gmat_startup = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--run-sample" => {} // routing flag only, consumed here as a no-op
            "--drm-pb" => drm_pb = Some(PathBuf::from(take_next(args, &mut i, "--drm-pb", &usage)?)),
            "--sos-pb" => sos_pb = Some(PathBuf::from(take_next(args, &mut i, "--sos-pb", &usage)?)),
            "--system-pb" => system_pbs.push(PathBuf::from(take_next(args, &mut i, "--system-pb", &usage)?)),
            "--run-id" => run_id = Some(take_next(args, &mut i, "--run-id", &usage)?),
            "--error-mode" => {
                let v = take_next(args, &mut i, "--error-mode", &usage)?;
                error_mode = Some(match v.as_str() {
                    "nominal" => ExecutionErrorMode::Nominal,
                    "sampled" => ExecutionErrorMode::Sampled,
                    other => return Err(format!("--error-mode must be 'nominal' or 'sampled', got {other:?}\n\n{usage}")),
                });
            }
            "--out" => out = Some(PathBuf::from(take_next(args, &mut i, "--out", &usage)?)),
            "--gmat-startup" => gmat_startup = Some(take_next(args, &mut i, "--gmat-startup", &usage)?),
            "-h" | "--help" => return Err(usage),
            other => return Err(format!("unrecognized argument {other:?}\n\n{usage}")),
        }
        i += 1;
    }

    let drm_pb = drm_pb.ok_or_else(|| format!("--drm-pb is required\n\n{usage}"))?;
    let sos_pb = sos_pb.ok_or_else(|| format!("--sos-pb is required\n\n{usage}"))?;
    if system_pbs.is_empty() {
        return Err(format!("at least one --system-pb is required\n\n{usage}"));
    }
    let run_id = run_id.ok_or_else(|| format!("--run-id is required\n\n{usage}"))?;
    let error_mode = error_mode.ok_or_else(|| format!("--error-mode is required\n\n{usage}"))?;
    let out = out.ok_or_else(|| format!("--out is required\n\n{usage}"))?;
    Ok(SampleArgs { drm_pb, sos_pb, system_pbs, run_id, error_mode, out, gmat_startup })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_a_minimal_study_command_line() {
        let args = v(&["av-sweep", "--sweep", "s.yaml", "--drm", "d.yaml", "--sos", "o.yaml", "--system", "sys.yaml", "--out-dir", "out", "--workers", "4"]);
        let cli = parse_cli(&args).unwrap();
        assert_eq!(
            cli,
            Cli::Study(StudyArgs {
                sweep: PathBuf::from("s.yaml"),
                drm: PathBuf::from("d.yaml"),
                sos: PathBuf::from("o.yaml"),
                systems: vec![PathBuf::from("sys.yaml")],
                out_dir: PathBuf::from("out"),
                workers: 4,
                gmat_startup: None,
            })
        );
    }

    /// Fails against a wrong implementation that defaults `--workers` to some value (e.g. 1)
    /// when the flag is simply absent -- this task's own brief: "`--workers` is required (no
    /// silent default)".
    #[test]
    fn refuses_a_missing_workers_flag() {
        let args = v(&["av-sweep", "--sweep", "s.yaml", "--drm", "d.yaml", "--sos", "o.yaml", "--system", "sys.yaml", "--out-dir", "out"]);
        let err = parse_cli(&args).unwrap_err();
        assert!(err.contains("--workers is required"), "{err}");
    }

    /// Fails against a wrong implementation that treats `--workers 0` as "unbounded" or "run
    /// nothing" rather than a typed refusal.
    #[test]
    fn refuses_zero_workers() {
        let args = v(&["av-sweep", "--sweep", "s.yaml", "--drm", "d.yaml", "--sos", "o.yaml", "--system", "sys.yaml", "--out-dir", "out", "--workers", "0"]);
        let err = parse_cli(&args).unwrap_err();
        assert!(err.contains("--workers must be at least 1, got 0"), "{err}");
    }

    #[test]
    fn refuses_a_non_numeric_workers_value() {
        let args = v(&["av-sweep", "--sweep", "s.yaml", "--drm", "d.yaml", "--sos", "o.yaml", "--system", "sys.yaml", "--out-dir", "out", "--workers", "banana"]);
        let err = parse_cli(&args).unwrap_err();
        assert!(err.contains("--workers must be a non-negative integer"), "{err}");
    }

    #[test]
    fn parses_a_minimal_sample_command_line_and_routes_by_run_sample_flag() {
        let args = v(&[
            "av-sweep",
            "--run-sample",
            "--drm-pb",
            "d.pb",
            "--sos-pb",
            "s.pb",
            "--system-pb",
            "sys.pb",
            "--run-id",
            "r1",
            "--error-mode",
            "sampled",
            "--out",
            "out.pb",
        ]);
        let cli = parse_cli(&args).unwrap();
        assert_eq!(
            cli,
            Cli::Sample(SampleArgs {
                drm_pb: PathBuf::from("d.pb"),
                sos_pb: PathBuf::from("s.pb"),
                system_pbs: vec![PathBuf::from("sys.pb")],
                run_id: "r1".to_string(),
                error_mode: ExecutionErrorMode::Sampled,
                out: PathBuf::from("out.pb"),
                gmat_startup: None,
            })
        );
    }

    #[test]
    fn refuses_a_missing_error_mode_in_sample_mode() {
        let args = v(&["av-sweep", "--run-sample", "--drm-pb", "d.pb", "--sos-pb", "s.pb", "--system-pb", "sys.pb", "--run-id", "r1", "--out", "out.pb"]);
        let err = parse_cli(&args).unwrap_err();
        assert!(err.contains("--error-mode is required"), "{err}");
    }

    #[test]
    fn refuses_an_invalid_error_mode_value() {
        let args =
            v(&["av-sweep", "--run-sample", "--drm-pb", "d.pb", "--sos-pb", "s.pb", "--system-pb", "sys.pb", "--run-id", "r1", "--error-mode", "banana", "--out", "out.pb"]);
        let err = parse_cli(&args).unwrap_err();
        assert!(err.contains("--error-mode must be 'nominal' or 'sampled'"), "{err}");
    }
}
