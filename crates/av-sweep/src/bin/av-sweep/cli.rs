//! Command-line parsing for the three modes this binary has -- see `main.rs`'s own module doc
//! comment for why one binary has a study mode and a sample mode (GMAT is a process-global
//! singleton, so exactly one DRM run happens per process). Mirrors `crates/av-run/src/main.rs`'s
//! own CLI parsing style (`Result<_, String>`, a `usage()` string appended to every parse error)
//! rather than inventing a new convention for this binary.
//!
//! The third mode (question 245) is **store mode**: it never touches GMAT or a sweep. It reads
//! one study out of a store into a file (`--read-study`), or writes a finished `SweepResults`
//! protobuf into one or both stores (`--import-study`). The feasibility panel's data path takes a
//! `SweepResults` posted to `/api/cdm/sweep`; `--read-study` is how a study that lives in
//! ClickHouse becomes that file.

use std::path::PathBuf;

use av_kernel::drm::ExecutionErrorMode;
use av_sweep::clickhouse::ClickHouseConfig;

#[derive(Debug, PartialEq)]
pub enum Cli {
    Study(StudyArgs),
    Sample(SampleArgs),
    Store(StoreArgs),
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
    /// F2 (`docs/feasibility-plan.md`'s F2 milestone): an ADDITIONAL, OPTIONAL destination --
    /// when set, the finished `SweepResults` is also written through
    /// `av_sweep::store::FileStudyStore` rooted here. Never changes `out_dir`'s own layout;
    /// `None` (the default, `--store-dir` simply omitted) writes no store directory at all.
    pub store_dir: Option<PathBuf>,
    /// Question 245: a second additional, optional destination -- the finished `SweepResults` is
    /// also written through `av_sweep::clickhouse::ClickHouseStudyStore`. May be combined with
    /// `store_dir`; each store gets the same message.
    pub clickhouse: Option<ClickHouseConfig>,
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

/// Store mode (question 245): move one study between a store and a file.
#[derive(Debug, PartialEq)]
pub struct StoreArgs {
    pub op: StoreOp,
    pub store_dir: Option<PathBuf>,
    pub clickhouse: Option<ClickHouseConfig>,
}

#[derive(Debug, PartialEq)]
pub enum StoreOp {
    /// Load `sweep_id` from the (single) selected store and write its `SweepResults` protobuf to
    /// `out_pb`, and optionally its canonical JSON to `out_json`.
    ReadStudy { sweep_id: String, out_pb: PathBuf, out_json: Option<PathBuf> },
    /// Decode a `SweepResults` protobuf from `pb` and write it into every selected store.
    ImportStudy { pb: PathBuf },
}

const STORE_FLAGS_USAGE: &str = "[--store-dir <dir>] [--store-clickhouse <url> --clickhouse-database <db> --clickhouse-user <user> [--clickhouse-password-env <VAR>] [--clickhouse-ca-file <pem>]]";

const STORE_FLAGS_HELP: &str = "--store-dir selects the file study store (FileStudyStore) rooted there. \
--store-clickhouse selects the ClickHouse study store at <url> (http://127.0.0.1:<port> on loopback, \
or https://host:port); it needs --clickhouse-database (which must already exist) and \
--clickhouse-user. The password is never a flag and never in the URL: --clickhouse-password-env \
names an environment variable that holds it. --clickhouse-ca-file is a PEM file of the CA(s) to \
trust for an https:// URL (it replaces the system store; certificate and hostname verification \
are always on). Plaintext http:// is refused for any non-loopback host.";

fn usage_study(prog: &str) -> String {
    format!(
        "usage: {prog} --sweep <sweep.yaml> --drm <drm.yaml> --sos <sos.yaml> \
         --system <path> [--system <path> ...] --out-dir <dir> --workers <N> \
         [--gmat-startup <path>] {STORE_FLAGS_USAGE}\n\n\
         Study mode (the parent): loads and hash-verifies the sweep, expands its grid, builds \
         every sample's per-sample DRM/SOS configuration, writes each sample's inputs, spawns \
         up to --workers children (one per sample, each this same binary invoked with \
         --run-sample) at a time, collects their results, computes per-point aggregates, and \
         writes sweep_results.pb/.json under --out-dir. --workers is required; 0 is refused. \
         The store flags are additional, optional destinations: when given, the finished \
         SweepResults is also written through each selected study store, without changing \
         --out-dir's own layout (both may be given; each gets the same SweepResults). {STORE_FLAGS_HELP}"
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

fn usage_store(prog: &str) -> String {
    format!(
        "usage: {prog} --read-study <sweep_id> --out-pb <file> [--out-json <file>] <one store>\n       \
         {prog} --import-study <sweep_results.pb> <one or two stores>\n       \
         where a store is {STORE_FLAGS_USAGE}\n\n\
         Store mode (never touches GMAT): --read-study loads one study from the single selected \
         store and writes its altavista.v1.SweepResults protobuf to --out-pb (and its canonical \
         JSON to --out-json), the body the feasibility panel posts to /api/cdm/sweep; it exits \
         non-zero if the store holds no such study. --import-study decodes a SweepResults \
         protobuf and writes it into every selected store. {STORE_FLAGS_HELP}"
    )
}

/// Dispatches on `--run-sample`'s presence anywhere in `args` (after the program name) -- study
/// mode never has a reason to pass that flag, and sample mode always does -- then on
/// `--read-study` / `--import-study` (store mode); anything else is study mode.
pub fn parse_cli(args: &[String]) -> Result<Cli, String> {
    let prog = args.first().map(String::as_str).unwrap_or("av-sweep");
    let has = |flag: &str| args.iter().skip(1).any(|a| a == flag);
    if has("--run-sample") {
        parse_sample(args, prog).map(Cli::Sample)
    } else if has("--read-study") || has("--import-study") {
        parse_store(args, prog).map(Cli::Store)
    } else {
        parse_study(args, prog).map(Cli::Study)
    }
}

fn take_next(args: &[String], i: &mut usize, flag: &str, usage: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i).cloned().ok_or_else(|| format!("{flag} needs a value\n\n{usage}"))
}

/// The store-selecting flags shared by study mode and store mode.
#[derive(Default)]
struct StoreFlags {
    store_dir: Option<PathBuf>,
    clickhouse_url: Option<String>,
    clickhouse_database: Option<String>,
    clickhouse_user: Option<String>,
    clickhouse_password_env: Option<String>,
    clickhouse_ca_file: Option<PathBuf>,
}

impl StoreFlags {
    /// Consumes `args[*i]` if it is one of the store flags (advancing `i` past its value).
    fn take(&mut self, args: &[String], i: &mut usize, usage: &str) -> Result<bool, String> {
        let flag = args[*i].as_str();
        match flag {
            "--store-dir" => self.store_dir = Some(PathBuf::from(take_next(args, i, flag, usage)?)),
            "--store-clickhouse" => self.clickhouse_url = Some(take_next(args, i, flag, usage)?),
            "--clickhouse-database" => self.clickhouse_database = Some(take_next(args, i, flag, usage)?),
            "--clickhouse-user" => self.clickhouse_user = Some(take_next(args, i, flag, usage)?),
            "--clickhouse-password-env" => self.clickhouse_password_env = Some(take_next(args, i, flag, usage)?),
            "--clickhouse-ca-file" => self.clickhouse_ca_file = Some(PathBuf::from(take_next(args, i, flag, usage)?)),
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The ClickHouse config, if `--store-clickhouse` was given; a `--clickhouse-*` flag without
    /// it, or the URL without a database or user, is a refusal naming the missing flag.
    fn clickhouse(&self, usage: &str) -> Result<Option<ClickHouseConfig>, String> {
        let Some(url) = &self.clickhouse_url else {
            let stray = [
                (self.clickhouse_database.is_some(), "--clickhouse-database"),
                (self.clickhouse_user.is_some(), "--clickhouse-user"),
                (self.clickhouse_password_env.is_some(), "--clickhouse-password-env"),
                (self.clickhouse_ca_file.is_some(), "--clickhouse-ca-file"),
            ];
            if let Some((_, flag)) = stray.iter().find(|(given, _)| *given) {
                return Err(format!("{flag} needs --store-clickhouse <url>\n\n{usage}"));
            }
            return Ok(None);
        };
        let database = self.clickhouse_database.clone().ok_or_else(|| format!("--store-clickhouse needs --clickhouse-database\n\n{usage}"))?;
        let user = self.clickhouse_user.clone().ok_or_else(|| format!("--store-clickhouse needs --clickhouse-user\n\n{usage}"))?;
        let mut config = ClickHouseConfig::new(url.clone(), database, user);
        config.password_env = self.clickhouse_password_env.clone();
        config.ca_file = self.clickhouse_ca_file.clone();
        Ok(Some(config))
    }
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
    let mut stores = StoreFlags::default();

    let mut i = 1;
    while i < args.len() {
        if stores.take(args, &mut i, &usage)? {
            i += 1;
            continue;
        }
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
    let clickhouse = stores.clickhouse(&usage)?;
    Ok(StudyArgs { sweep, drm, sos, systems, out_dir, workers, gmat_startup, store_dir: stores.store_dir, clickhouse })
}

fn parse_store(args: &[String], prog: &str) -> Result<StoreArgs, String> {
    let usage = usage_store(prog);
    let mut read_study = None;
    let mut import_study = None;
    let mut out_pb = None;
    let mut out_json = None;
    let mut stores = StoreFlags::default();

    let mut i = 1;
    while i < args.len() {
        if stores.take(args, &mut i, &usage)? {
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--read-study" => read_study = Some(take_next(args, &mut i, "--read-study", &usage)?),
            "--import-study" => import_study = Some(PathBuf::from(take_next(args, &mut i, "--import-study", &usage)?)),
            "--out-pb" => out_pb = Some(PathBuf::from(take_next(args, &mut i, "--out-pb", &usage)?)),
            "--out-json" => out_json = Some(PathBuf::from(take_next(args, &mut i, "--out-json", &usage)?)),
            "-h" | "--help" => return Err(usage),
            other => return Err(format!("unrecognized argument {other:?}\n\n{usage}")),
        }
        i += 1;
    }

    let clickhouse = stores.clickhouse(&usage)?;
    let store_dir = stores.store_dir;
    let selected = usize::from(store_dir.is_some()) + usize::from(clickhouse.is_some());
    let op = match (read_study, import_study) {
        (Some(_), Some(_)) => return Err(format!("--read-study and --import-study are different operations; give one\n\n{usage}")),
        (Some(sweep_id), None) => {
            let out_pb = out_pb.ok_or_else(|| format!("--read-study needs --out-pb <file>\n\n{usage}"))?;
            if selected != 1 {
                return Err(format!("--read-study reads from exactly one store; got {selected} (give --store-dir or --store-clickhouse, not both and not neither)\n\n{usage}"));
            }
            StoreOp::ReadStudy { sweep_id, out_pb, out_json }
        }
        (None, Some(pb)) => {
            if out_pb.is_some() || out_json.is_some() {
                return Err(format!("--out-pb and --out-json belong to --read-study\n\n{usage}"));
            }
            if selected == 0 {
                return Err(format!("--import-study needs at least one store (--store-dir and/or --store-clickhouse)\n\n{usage}"));
            }
            StoreOp::ImportStudy { pb }
        }
        (None, None) => return Err(format!("store mode needs --read-study or --import-study\n\n{usage}")),
    };
    Ok(StoreArgs { op, store_dir, clickhouse })
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
                store_dir: None,
                clickhouse: None,
            })
        );
    }

    /// `--store-dir` is optional and additional -- given, it is parsed into `StudyArgs.store_dir`
    /// without disturbing anything else (this task's own brief: an ADDITIONAL, OPTIONAL
    /// destination, never a replacement for `--out-dir`).
    #[test]
    fn parses_an_optional_store_dir_flag() {
        let args = v(&["av-sweep", "--sweep", "s.yaml", "--drm", "d.yaml", "--sos", "o.yaml", "--system", "sys.yaml", "--out-dir", "out", "--workers", "4", "--store-dir", "store"]);
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
                store_dir: Some(PathBuf::from("store")),
                clickhouse: None,
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

    fn study_base() -> Vec<String> {
        v(&["av-sweep", "--sweep", "s.yaml", "--drm", "d.yaml", "--sos", "o.yaml", "--system", "sys.yaml", "--out-dir", "out", "--workers", "4"])
    }

    /// Question 245: the ClickHouse flags build a config that holds the password variable's NAME
    /// only, and may sit beside `--store-dir` (each store gets the same SweepResults).
    #[test]
    fn parses_the_clickhouse_store_flags_beside_store_dir() {
        let mut args = study_base();
        args.extend(v(&[
            "--store-dir", "store", "--store-clickhouse", "https://ch.example:8443", "--clickhouse-database", "studies", "--clickhouse-user", "avsweep", "--clickhouse-password-env",
            "AV_CH_PW", "--clickhouse-ca-file", "ca.pem",
        ]));
        let Cli::Study(study) = parse_cli(&args).unwrap() else { panic!("study mode expected") };
        assert_eq!(study.store_dir, Some(PathBuf::from("store")));
        let mut expected = ClickHouseConfig::new("https://ch.example:8443", "studies", "avsweep");
        expected.password_env = Some("AV_CH_PW".to_string());
        expected.ca_file = Some(PathBuf::from("ca.pem"));
        assert_eq!(study.clickhouse, Some(expected));
    }

    #[test]
    fn a_clickhouse_url_needs_its_database_and_user_and_a_stray_flag_needs_the_url() {
        let mut args = study_base();
        args.extend(v(&["--store-clickhouse", "http://127.0.0.1:8123", "--clickhouse-user", "u"]));
        let err = parse_cli(&args).unwrap_err();
        assert!(err.contains("--store-clickhouse needs --clickhouse-database"), "{err}");

        let mut args = study_base();
        args.extend(v(&["--store-clickhouse", "http://127.0.0.1:8123", "--clickhouse-database", "d"]));
        let err = parse_cli(&args).unwrap_err();
        assert!(err.contains("--store-clickhouse needs --clickhouse-user"), "{err}");

        for stray in ["--clickhouse-database", "--clickhouse-user", "--clickhouse-password-env", "--clickhouse-ca-file"] {
            let mut args = study_base();
            args.extend(v(&[stray, "x"]));
            let err = parse_cli(&args).unwrap_err();
            assert!(err.contains(&format!("{stray} needs --store-clickhouse <url>")), "{stray}: {err}");
        }
    }

    #[test]
    fn the_study_usage_documents_the_store_flags() {
        let err = parse_cli(&v(&["av-sweep", "--help"])).unwrap_err();
        for flag in ["--store-dir", "--store-clickhouse", "--clickhouse-database", "--clickhouse-user", "--clickhouse-password-env", "--clickhouse-ca-file"] {
            assert!(err.contains(flag), "usage must document {flag}: {err}");
        }
        assert!(err.contains("loopback"), "usage must say plaintext is loopback-only: {err}");
    }

    #[test]
    fn parses_read_study_from_one_store_and_import_study_into_either_or_both() {
        let read = v(&["av-sweep", "--read-study", "sweep1", "--out-pb", "r.pb", "--out-json", "r.json", "--store-clickhouse", "http://127.0.0.1:8123", "--clickhouse-database", "d", "--clickhouse-user", "u"]);
        let Cli::Store(s) = parse_cli(&read).unwrap() else { panic!("store mode expected") };
        assert_eq!(s.op, StoreOp::ReadStudy { sweep_id: "sweep1".to_string(), out_pb: PathBuf::from("r.pb"), out_json: Some(PathBuf::from("r.json")) });
        assert!(s.clickhouse.is_some() && s.store_dir.is_none());

        let read_dir = v(&["av-sweep", "--read-study", "sweep1", "--out-pb", "r.pb", "--store-dir", "store"]);
        let Cli::Store(s) = parse_cli(&read_dir).unwrap() else { panic!("store mode expected") };
        assert_eq!(s.op, StoreOp::ReadStudy { sweep_id: "sweep1".to_string(), out_pb: PathBuf::from("r.pb"), out_json: None });
        assert_eq!(s.store_dir, Some(PathBuf::from("store")));

        let import_both = v(&["av-sweep", "--import-study", "in.pb", "--store-dir", "store", "--store-clickhouse", "http://127.0.0.1:8123", "--clickhouse-database", "d", "--clickhouse-user", "u"]);
        let Cli::Store(s) = parse_cli(&import_both).unwrap() else { panic!("store mode expected") };
        assert_eq!(s.op, StoreOp::ImportStudy { pb: PathBuf::from("in.pb") });
        assert!(s.clickhouse.is_some() && s.store_dir.is_some());
    }

    #[test]
    fn store_mode_refuses_the_wrong_number_of_stores_and_mixed_operations() {
        let both = v(&["av-sweep", "--read-study", "s", "--out-pb", "r.pb", "--store-dir", "x", "--store-clickhouse", "http://127.0.0.1:8123", "--clickhouse-database", "d", "--clickhouse-user", "u"]);
        assert!(parse_cli(&both).unwrap_err().contains("exactly one store"));
        let none = v(&["av-sweep", "--read-study", "s", "--out-pb", "r.pb"]);
        assert!(parse_cli(&none).unwrap_err().contains("exactly one store"));
        let no_out = v(&["av-sweep", "--read-study", "s", "--store-dir", "x"]);
        assert!(parse_cli(&no_out).unwrap_err().contains("--read-study needs --out-pb"));
        let import_none = v(&["av-sweep", "--import-study", "in.pb"]);
        assert!(parse_cli(&import_none).unwrap_err().contains("at least one store"));
        let import_out = v(&["av-sweep", "--import-study", "in.pb", "--store-dir", "x", "--out-pb", "o.pb"]);
        assert!(parse_cli(&import_out).unwrap_err().contains("belong to --read-study"));
        let both_ops = v(&["av-sweep", "--import-study", "in.pb", "--read-study", "s", "--store-dir", "x"]);
        assert!(parse_cli(&both_ops).unwrap_err().contains("different operations"));
    }
}
