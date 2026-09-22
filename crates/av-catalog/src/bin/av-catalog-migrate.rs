//! `av-catalog-migrate`: the small `cargo run`/binary harness `services/catalog/run-dev-
//! catalog.sh`'s own header comment has always pointed a developer at
//! ("run `Migrator::apply_pending` themselves, e.g. from a small cargo run/test harness") but
//! that, until this task, did not exist -- `crate::migrate::Migrator::apply_pending`'s only
//! two callers in the whole workspace were `tests/catalog_postgis.rs` and `crates/av-gateway/
//! tests/catalog_selector.rs`, both of which start and own a throwaway container of their own
//! (`docs/heavy-plan.md`'s "A gap in the drive path that nobody had noticed" section records
//! the gap this binary closes). `scripts/heavy/README.md` step 0.5b names this exact binary.
//!
//! # Why a `[[bin]]` in THIS crate, not a `cargo run --example`, and not a second crate
//!
//! `av-catalog` has no existing `[[bin]]` target and no reason to stay binary-free: unlike
//! `crates/av-jobs/Cargo.toml`'s own explicit "the runner must not depend on the catalog"
//! constraint (that crate's `av-catalog` dependency is `optional = true`, gated behind the
//! `store-fixture` feature, precisely so its own library code and every OTHER binary in that
//! crate stay free of `av-catalog`), nothing in THIS workspace says `av-catalog` itself must
//! stay bin-free. Grepping every `Cargo.toml` for `av-catalog =` turns up exactly two
//! unconditional dependents: `crates/av-gateway/Cargo.toml` (a real, required dependency --
//! `crate::catalog_selector` calls straight into this crate's `PgClient`/`Migrator`/`query`)
//! and `av-catalog` itself. Neither carries a "must not grow a binary" constraint, and a
//! `[[bin]]` target is a build artifact of the crate that DECLARES it -- adding one to
//! `av-catalog` does not add a binary to `av-gateway`'s own build, any more than `crates/
//! av-jobs`'s pre-existing `av-tile-fixture` binary forces `av-gateway` (which does not depend
//! on `av-jobs` at all) to build one. So this is the crate that should own the binary, matching
//! the established convention every other operational binary in this workspace already
//! follows -- `av-tile-fixture` lives in `av-jobs` (the crate whose data it produces),
//! `av-gateway`/`av-tiles`/`av-command` each live in their own crate -- rather than a new,
//! fifth workspace member whose only reason to exist would be "a home for one binary" (this
//! task's own rule: do not add a workspace member unless genuinely necessary, and loudly say
//! so if it happens; it does not happen here).
//!
//! A `cargo run --example` was the brief's other offered option, and was rejected: an
//! `examples/` target is Cargo's convention for "how to call this crate's own API", normally
//! excluded from a release build and not meant to be relied on as a stable, documented command
//! a developer runs as part of an operational recipe (`scripts/heavy/README.md` step 0.5b is
//! exactly that). Every other real, run-this-for-real tool in this workspace
//! (`av-tile-fixture`, `av-gateway`, `av-tiles`, `av-command`) is a `[[bin]]`, never an
//! `examples/` entry -- matching that convention, rather than being the one exception, is the
//! simpler choice and the one a developer reading this workspace's other binaries already
//! knows how to run (`cargo run -p av-catalog --bin av-catalog-migrate -- --help`-shaped usage,
//! identical to how they already invoke `av-tile-fixture`/`av-gateway`).
//!
//! # Flags -- the same `--catalog-*` shape `av-tile-fixture`/`av-gateway` already use
//!
//! `--catalog-host`/`--catalog-port`/`--catalog-user`/`--catalog-password`/`--catalog-database`
//! /`--catalog-tls-ca-file` are read verbatim off `std::env::args()` -- this task's own repo-
//! wide rule (`README.md`'s "Edge track" paragraph: "configured entirely on the command line,
//! never by environment variable", stated there for `av-ingest-server`, followed identically by
//! every `--catalog-*` flag family in this workspace already): NEITHER this binary NOR any
//! function it calls reads `std::env::var` for any of these. `--catalog-password` is a plain
//! command-line value, exactly matching `crates/av-jobs/src/bin/av-tile-fixture.rs::CliArgs::
//! catalog_password` and `crates/av-gateway/src/bin/av-gateway.rs::CliArgs::catalog_password` --
//! this workspace has no `--catalog-password-file` convention anywhere (grepped: zero hits for
//! `password.file`/`password_file` in `crates/`, `services/`, `scripts/`, `docs/`,
//! `README.md`), so inventing one here, for the one binary that never even opens a network
//! socket other than the catalog's own, would be a second, drifting convention rather than the
//! one this workspace already has. A command-line password is visible to anyone who can read
//! this process's argv (`ps`, `/proc/<pid>/cmdline`) for as long as it runs -- a real, known
//! cost of the convention this binary follows rather than invents, accepted twice already by
//! this same workspace's two existing `--catalog-password` binaries.
//!
//! # `--applied-tai-ns` -- injected, with a documented, non-silent default
//!
//! `crate::migrate`'s own module doc ("Timestamps are injected, never read") is about THAT
//! crate's internals: `Migrator::apply_pending` takes `applied_tai_ns: i64` as a parameter and
//! never calls a clock itself, leaving "where does the real reading come from" to whichever
//! caller wants the schema actually applied for real -- this binary is that caller. This flag
//! is OPTIONAL; when omitted, the default is the real OS wall clock, converted UTC -> TAI the
//! identical way `crates/av-jobs/src/clock.rs::SystemClock`/`crates/av-command/src/clock.rs::
//! SystemClock` both do (`av_cdm::time::Tai::from_utc_nanos`, the one shared TAI/UTC boundary
//! this workspace already has) -- never a fixed placeholder epoch, since a developer running
//! this binary for real (`scripts/heavy/README.md` step 0.5b) wants "now" recorded against the
//! migration they just applied, not an arbitrary constant. This is NOT "reading a wall clock
//! behind the caller's back": the read happens only through this one, named, documented flag
//! path, and the value actually used -- whichever source it came from -- is always printed
//! (`main`'s own `applied_tai_ns = {ns} ({source})` line below), so a caller who did not pass
//! `--applied-tai-ns` still sees exactly what got recorded, never a silent number. `av-catalog`
//! does not depend on `av-command` for this (mirrors `crates/av-jobs/src/clock.rs`'s own doc:
//! "`av-command` is a hot-path crate ... has no business [in an unrelated crate's] dependency
//! tree" -- restated here for `av-catalog`, which already depends on `av-cdm` directly for
//! other reasons, so `av_cdm::time::Tai` costs this crate nothing new).
//!
//! # What this binary prints, and why
//!
//! Every run prints: the connection target (never the password), what `schema_migrations`
//! already recorded BEFORE this run touched anything, the `applied_tai_ns` value used and
//! where it came from, exactly which migrations THIS run applied (empty on a database already
//! at the latest schema -- the idempotency this binary's own brief requires: a second run
//! against the same database applies nothing and says so, in that many words), and the
//! resulting schema version (the highest-ordered `schema_migrations.version` after this run).
//! A caller of `scripts/heavy/README.md` step 0.5b is meant to see evidence on their own
//! terminal, not silence -- exactly the gap `docs/heavy-plan.md`'s own "no way to apply it
//! outside a test" finding named.

use std::path::PathBuf;

use av_catalog::{CatalogError, MigrationRecord, Migrator, PgClient, PgConfig, PgTls};
use av_cdm::time::Tai;

const DEFAULT_CATALOG_PORT: u16 = 5432;

const USAGE: &str = "usage: av-catalog-migrate --catalog-host HOST --catalog-user USER --catalog-password PASSWORD \
                      --catalog-database DB [--catalog-port PORT] [--catalog-tls-ca-file PATH] [--applied-tai-ns NS]";

#[derive(Debug, PartialEq, Eq)]
struct CliArgs {
    catalog_host: String,
    catalog_port: u16,
    catalog_user: String,
    catalog_password: String,
    catalog_database: String,
    catalog_tls_ca_file: Option<PathBuf>,
    /// `None` means "use the default" (the real wall clock, converted UTC -> TAI -- see this
    /// module's own doc) -- `Some` means `--applied-tai-ns` was given explicitly.
    applied_tai_ns: Option<i64>,
}

fn parse_cli_args(args: impl Iterator<Item = String>) -> Result<CliArgs, String> {
    let mut catalog_host: Option<String> = None;
    let mut catalog_port: u16 = DEFAULT_CATALOG_PORT;
    let mut catalog_user: Option<String> = None;
    let mut catalog_password: Option<String> = None;
    let mut catalog_database: Option<String> = None;
    let mut catalog_tls_ca_file: Option<PathBuf> = None;
    let mut applied_tai_ns: Option<i64> = None;

    let mut it = args.skip(1).peekable();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg}: missing value. {USAGE}"));
        match arg.as_str() {
            "--catalog-host" => catalog_host = Some(value()?),
            "--catalog-port" => catalog_port = value()?.parse::<u16>().map_err(|e| format!("--catalog-port: {e}"))?,
            "--catalog-user" => catalog_user = Some(value()?),
            "--catalog-password" => catalog_password = Some(value()?),
            "--catalog-database" => catalog_database = Some(value()?),
            "--catalog-tls-ca-file" => catalog_tls_ca_file = Some(PathBuf::from(value()?)),
            "--applied-tai-ns" => applied_tai_ns = Some(value()?.parse::<i64>().map_err(|e| format!("--applied-tai-ns: {e}"))?),
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unrecognized argument {other:?}. {USAGE}")),
        }
    }

    let catalog_host = catalog_host.ok_or_else(|| format!("--catalog-host is required. {USAGE}"))?;
    let catalog_user = catalog_user.ok_or_else(|| format!("--catalog-user is required. {USAGE}"))?;
    let catalog_password = catalog_password.ok_or_else(|| format!("--catalog-password is required. {USAGE}"))?;
    let catalog_database = catalog_database.ok_or_else(|| format!("--catalog-database is required. {USAGE}"))?;

    Ok(CliArgs { catalog_host, catalog_port, catalog_user, catalog_password, catalog_database, catalog_tls_ca_file, applied_tai_ns })
}

/// This binary's own copy of the real-wall-clock -> TAI-nanoseconds conversion -- see this
/// module's own doc, "`--applied-tai-ns` -- injected, with a documented, non-silent default",
/// for why this is a local copy (mirroring `crates/av-jobs/src/clock.rs::SystemClock`) rather
/// than a new dependency on `av-command`.
fn wall_clock_tai_ns() -> i64 {
    let utc_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock reports a time before the Unix epoch")
        .as_nanos() as i64;
    Tai::from_utc_nanos(utc_ns).as_nanos()
}

fn format_record(r: &MigrationRecord) -> String {
    format!("{} (hash={}, applied_tai_ns={})", r.version, r.hash, r.applied_tai_ns)
}

async fn run(cli: CliArgs) -> Result<(), CatalogError> {
    let tls = match &cli.catalog_tls_ca_file {
        Some(ca_file) => PgTls::Required { ca_file: Some(ca_file.clone()) },
        None => PgTls::Disabled,
    };
    println!(
        "av-catalog-migrate: connecting to {}:{}/{} as {:?} (tls={})",
        cli.catalog_host,
        cli.catalog_port,
        cli.catalog_database,
        cli.catalog_user,
        if matches!(tls, PgTls::Disabled) { "disabled" } else { "required" }
    );
    let pg_config = PgConfig {
        host: cli.catalog_host.clone(),
        port: cli.catalog_port,
        user: cli.catalog_user.clone(),
        password: cli.catalog_password,
        database: cli.catalog_database.clone(),
        application_name: "av-catalog-migrate".to_string(),
        connect_timeout: std::time::Duration::from_secs(5),
        tls,
    };

    let mut client = PgClient::connect(&pg_config).await?;
    println!("av-catalog-migrate: connected");

    let already_before = Migrator::applied(&mut client).await?;
    if already_before.is_empty() {
        println!("av-catalog-migrate: schema_migrations recorded 0 migration(s) before this run (a fresh/empty database)");
    } else {
        println!("av-catalog-migrate: schema_migrations recorded {} migration(s) before this run:", already_before.len());
        for r in &already_before {
            println!("  - {}", format_record(r));
        }
    }

    let (applied_tai_ns, source) = match cli.applied_tai_ns {
        Some(ns) => (ns, "from --applied-tai-ns"),
        None => (wall_clock_tai_ns(), "default: the real OS wall clock, converted UTC -> TAI via av_cdm::time::Tai::from_utc_nanos"),
    };
    println!("av-catalog-migrate: applied_tai_ns = {applied_tai_ns} ({source})");

    let newly_applied = Migrator::apply_pending(&mut client, applied_tai_ns).await?;
    if newly_applied.is_empty() {
        println!("av-catalog-migrate: no pending migrations -- database already at the latest schema version (idempotent: this run applied nothing)");
    } else {
        println!("av-catalog-migrate: applied {} migration(s) this run: {:?}", newly_applied.len(), newly_applied);
    }

    let after = Migrator::applied(&mut client).await?;
    match after.last() {
        Some(latest) => println!("av-catalog-migrate: resulting schema version: {} ({} migration(s) recorded total)", latest.version, after.len()),
        None => println!("av-catalog-migrate: resulting schema version: none (0 migrations recorded -- crate::migrate::MIGRATIONS is unexpectedly empty)"),
    }

    client.close().await?;
    println!("av-catalog-migrate: done");
    Ok(())
}

fn main() {
    let cli = match parse_cli_args(std::env::args()) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("av-catalog-migrate: {e}");
            std::process::exit(if e == USAGE { 0 } else { 1 });
        }
    };

    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("av-catalog-migrate: building the tokio Runtime: {e}");
        std::process::exit(1);
    });
    if let Err(e) = runtime.block_on(run(cli)) {
        eprintln!("av-catalog-migrate: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(extra: &[&str]) -> impl Iterator<Item = String> {
        std::iter::once("av-catalog-migrate".to_string()).chain(extra.iter().map(|s| s.to_string())).collect::<Vec<_>>().into_iter()
    }

    const REQUIRED: &[&str] = &["--catalog-host", "127.0.0.1", "--catalog-user", "u", "--catalog-password", "p", "--catalog-database", "d"];

    #[test]
    fn parse_cli_args_reads_every_required_catalog_flag_with_the_default_port_and_no_applied_tai_ns() {
        let cli = parse_cli_args(args(REQUIRED)).unwrap();
        assert_eq!(cli.catalog_host, "127.0.0.1");
        assert_eq!(cli.catalog_port, DEFAULT_CATALOG_PORT);
        assert_eq!(cli.catalog_user, "u");
        assert_eq!(cli.catalog_password, "p");
        assert_eq!(cli.catalog_database, "d");
        assert_eq!(cli.catalog_tls_ca_file, None);
        assert_eq!(cli.applied_tai_ns, None, "omitted --applied-tai-ns must be None (main() then computes the documented wall-clock default), never silently 0 or some other placeholder");
    }

    #[test]
    fn parse_cli_args_reads_an_explicit_applied_tai_ns_and_port_and_tls_ca_file() {
        let cli = parse_cli_args(args(&[
            "--catalog-host",
            "127.0.0.1",
            "--catalog-port",
            "55432",
            "--catalog-user",
            "u",
            "--catalog-password",
            "p",
            "--catalog-database",
            "d",
            "--catalog-tls-ca-file",
            "/tmp/ca.pem",
            "--applied-tai-ns",
            "1700000000000000000",
        ]))
        .unwrap();
        assert_eq!(cli.catalog_port, 55432);
        assert_eq!(cli.catalog_tls_ca_file, Some(PathBuf::from("/tmp/ca.pem")));
        assert_eq!(cli.applied_tai_ns, Some(1_700_000_000_000_000_000));
    }

    #[test]
    fn parse_cli_args_refuses_a_missing_required_flag_naming_it() {
        let err = parse_cli_args(args(&["--catalog-host", "127.0.0.1"])).unwrap_err();
        assert!(err.contains("--catalog-user"), "{err}");
    }

    #[test]
    fn parse_cli_args_refuses_an_unparseable_port() {
        let err = parse_cli_args(args(&["--catalog-port", "not-a-number"])).unwrap_err();
        assert!(err.contains("--catalog-port"), "{err}");
    }

    #[test]
    fn parse_cli_args_refuses_an_unparseable_applied_tai_ns() {
        let err = parse_cli_args(args(&["--applied-tai-ns", "not-a-number"])).unwrap_err();
        assert!(err.contains("--applied-tai-ns"), "{err}");
    }

    #[test]
    fn parse_cli_args_refuses_an_unrecognized_flag() {
        let err = parse_cli_args(args(&["--bogus"])).unwrap_err();
        assert!(err.contains("--bogus"), "{err}");
    }

    /// `wall_clock_tai_ns` returns something plausible (comfortably past the year-2020 mark
    /// on the TAI scale, and comfortably before an obviously-wrong far future) -- a coarse
    /// sanity bound, mirroring `crates/av-command/src/clock.rs::tests::
    /// system_clock_reports_a_plausible_epoch`'s own identical style, without depending on
    /// wall-clock TIME for the test's own pass/fail (only that the conversion landed in a sane
    /// range, whatever "now" happens to be when this test runs).
    #[test]
    fn wall_clock_tai_ns_reports_a_plausible_epoch() {
        let ns = wall_clock_tai_ns();
        // 2020-01-01 UTC in Unix nanoseconds, well below any real TAI-UTC offset correction.
        assert!(ns > 1_577_836_800_000_000_000, "expected a post-2020 epoch, got {ns}");
        // 2100-01-01 UTC in Unix nanoseconds -- an upper sanity bound, not a real deadline.
        assert!(ns < 4_102_444_800_000_000_000, "expected a pre-2100 epoch, got {ns}");
    }
}
