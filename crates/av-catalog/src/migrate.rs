//! The catalog's schema migrator: an ordered, hashed, `include_str!`-embedded set of `.sql`
//! files, applied exactly once each, inside a transaction, in filename order -- and, on every
//! run, a refusal if a committed migration's file no longer hashes to what `schema_migrations`
//! recorded when it was applied.
//!
//! # Why the migration set is a `const` array, never a directory walk
//!
//! [`MIGRATIONS`] lists every migration file explicitly, each one `include_str!`'d from
//! `migrations/` at COMPILE time -- so a plain clone of this repository (this task's rule 9)
//! carries every migration inside the compiled binary itself, with no runtime dependency on
//! `migrations/` existing on disk at all. The alternative -- `std::fs::read_dir("migrations")`
//! at startup -- would make the actually-applied migration SET depend on whatever happens to
//! be on the filesystem a given process runs against, which is exactly the kind of
//! environment-dependent behaviour this workspace's own rule 9 (and ADR-004's determinism
//! preference generally) rules out: two processes built from the identical source could
//! otherwise observe two different migration sets just because one's working directory had an
//! extra or missing `.sql` file. [`tests::const_array_and_migrations_directory_agree`] is the
//! other half of this guarantee: it asserts [`MIGRATIONS`] and the real `migrations/`
//! directory's own file listing name the exact same set, in the exact same order, so a new
//! `.sql` file nobody added to [`MIGRATIONS`] fails THIS crate's own test suite (a compile-time
//! constant a human forgot to update) rather than being silently skipped by every process that
//! ever runs this crate's migrator against a real database.
//!
//! # `schema_migrations` is bootstrap, not migration `0000`
//!
//! The bookkeeping table itself ([`BOOTSTRAP_SQL`]) is created by [`Migrator`] directly
//! (`CREATE TABLE IF NOT EXISTS`, idempotent), NOT as `migrations/0000_bootstrap.sql` or
//! similar. This avoids the chicken-and-egg problem a numbered migration for it would create --
//! [`Migrator::applied`] needs to SELECT from `schema_migrations` before it can know which
//! numbered migrations (including a hypothetical "0000") have already run, so the table that
//! answers that question cannot itself be one of the things the answer is about. It also keeps
//! `schema_migrations`'s own DDL out of [`MIGRATIONS`]' hash set entirely: this table is this
//! crate's own bookkeeping, not part of the catalog's committed, reviewed business schema
//! (`0001_init.sql`'s own module-doc-adjacent comment covers what IS part of that schema).
//!
//! # Timestamps are injected, never read
//!
//! [`Migrator::apply_pending`] takes `applied_tai_ns: i64` as a parameter -- this crate never
//! calls `SystemTime::now()` or any other clock itself (this task's rule 7, and the identical
//! convention `crates/av-command/src/ledger.rs`'s own module doc states for `Ledger::append`'s
//! `clock` parameter). A caller (a later ingest/job task, or this crate's own integration test)
//! supplies the real reading.

use openssl::sha::sha256;

use crate::client::PgClient;
use crate::error::CatalogError;

/// One embedded migration: its filename (the `schema_migrations.version` value, and the
/// ordering key -- see [`MIGRATIONS`]'s own doc) and its full SQL text, `include_str!`'d at
/// compile time.
pub struct MigrationFile {
    pub filename: &'static str,
    pub sql: &'static str,
}

/// Every migration this crate ships, in the exact order [`Migrator::apply_pending`] applies
/// them -- filename order, which (zero-padded `NNNN_` prefixes) is also numeric order. See this
/// module's own doc, "Why the migration set is a `const` array", for why this is a hand-written
/// list rather than a runtime directory walk, and [`tests::const_array_and_migrations_directory_agree`]
/// for the test that keeps it truthful.
pub const MIGRATIONS: &[MigrationFile] = &[MigrationFile { filename: "0001_init.sql", sql: include_str!("../migrations/0001_init.sql") }];

/// The migrator's own bookkeeping table -- see this module's own doc, "`schema_migrations` is
/// bootstrap, not migration `0000`", for why this is not one of [`MIGRATIONS`]. `IF NOT EXISTS`
/// makes this idempotent to run on every [`Migrator::applied`]/[`Migrator::apply_pending`] call,
/// the same way `0001_init.sql`'s own `CREATE EXTENSION IF NOT EXISTS postgis` is idempotent
/// against an already-provisioned database (`services/catalog/IMAGE_DIGEST.md`'s measured
/// fact).
const BOOTSTRAP_SQL: &str = "
CREATE TABLE IF NOT EXISTS schema_migrations (
    version text PRIMARY KEY,
    hash text NOT NULL,
    applied_tai_ns bigint NOT NULL
);
COMMENT ON TABLE schema_migrations IS 'crate::migrate::Migrator''s own bookkeeping: which of crate::migrate::MIGRATIONS have been applied to this database, the SHA-256 (hex) each one had at the time, and when (TAI nanoseconds, caller-supplied -- this crate never reads a clock itself).';
COMMENT ON COLUMN schema_migrations.version IS 'The migration file''s own filename (crate::migrate::MigrationFile.filename), e.g. 0001_init.sql.';
COMMENT ON COLUMN schema_migrations.hash IS 'openssl::sha::sha256 of the migration file''s exact text, hex, lower case -- compared against the COMMITTED file''s own hash on every Migrator::apply_pending/verify call; a mismatch is CatalogError::MigrationDrift.';
COMMENT ON COLUMN schema_migrations.applied_tai_ns IS 'When this migration was applied, TAI NANOSECONDS, supplied by the caller of Migrator::apply_pending.';
";

/// One row of `schema_migrations`, as reported by [`Migrator::applied`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRecord {
    pub version: String,
    pub hash: String,
    pub applied_tai_ns: i64,
}

/// `openssl::sha::sha256`, hex, lower case -- this crate's own copy of the convention
/// `crates/av-command/src/ledger.rs`'s module doc states ("computed with the `openssl` crate...
/// ADR-004's crypto rule") and that file's own local `hex_encode` mirrors byte for byte; every
/// file in this workspace that needs a hex-encoded SHA-256 writes this same small helper
/// locally rather than sharing one across crates (the established convention, not an oversight
/// -- `crates/av-store/tests/minio_store.rs::hex` is the identical pattern one crate over).
fn hex_sha256(bytes: &[u8]) -> String {
    let digest = sha256(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Wraps `s` as a single-quoted SQL string literal, doubling every embedded `'` (the standard
/// SQL escape) -- used only for [`Migrator::apply_pending`]'s own bookkeeping `INSERT`, whose
/// values (a migration filename, a hex digest) are never caller-supplied data (this crate's own
/// `src/client.rs` module doc: "SQL injection is structurally impossible through THIS API" is
/// about parameterised queries; [`PgClient::simple_batch`] is the one function that sends a
/// whole SQL string verbatim, and its own contract -- restated there -- is that it is used only
/// for a committed migration script, never runtime data. This helper's escaping exists as
/// defence in depth for that verbatim text, not because a filename or a hex digest could
/// plausibly contain a quote).
fn sql_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The catalog's schema migrator. See this module's own doc for the full design; every method
/// here is an associated function (`Migrator` itself carries no state -- every fact it needs
/// lives in [`MIGRATIONS`] or is read fresh from `schema_migrations` on each call).
pub struct Migrator;

impl Migrator {
    /// Ensures `schema_migrations` exists (see [`BOOTSTRAP_SQL`]'s own doc) -- idempotent,
    /// called internally by both [`Self::applied`] and [`Self::apply_pending`] so neither
    /// requires the caller to have bootstrapped first.
    async fn ensure_bootstrap(client: &mut PgClient) -> Result<(), CatalogError> {
        client.simple_batch(BOOTSTRAP_SQL).await
    }

    /// Every migration `schema_migrations` currently records as applied, in `version` order.
    /// Bootstraps `schema_migrations` first (see [`Self::ensure_bootstrap`]), so this is safe
    /// to call against a brand-new database (returns an empty `Vec`, not an error, for one with
    /// no migrations applied yet).
    pub async fn applied(client: &mut PgClient) -> Result<Vec<MigrationRecord>, CatalogError> {
        Self::ensure_bootstrap(client).await?;
        let rows = client.query("SELECT version, hash, applied_tai_ns FROM schema_migrations ORDER BY version", &[]).await?;
        rows.iter()
            .map(|row| {
                Ok(MigrationRecord {
                    version: row.get_str("version")?.to_string(),
                    hash: row.get_str("hash")?.to_string(),
                    applied_tai_ns: row.get_i64("applied_tai_ns")?,
                })
            })
            .collect()
    }

    /// For every `already`-applied migration whose `version` still names one of [`MIGRATIONS`],
    /// asserts its recorded hash equals the committed file's own hash -- [`CatalogError::
    /// MigrationDrift`] on the first mismatch found (`version` order, so the error names the
    /// EARLIEST drifted migration when more than one has drifted). A recorded version with no
    /// matching entry in [`MIGRATIONS`] at all (a migration file deleted after being applied) is
    /// a different, narrower failure mode this function does not detect -- named here rather
    /// than silently treated as "no drift": a future caller that wants that case caught too has
    /// something concrete to extend, not a gap this doc comment pretends does not exist.
    fn check_drift(already: &[MigrationRecord]) -> Result<(), CatalogError> {
        for record in already {
            if let Some(m) = MIGRATIONS.iter().find(|m| m.filename == record.version) {
                let file_hash = hex_sha256(m.sql.as_bytes());
                if file_hash != record.hash {
                    return Err(CatalogError::MigrationDrift { version: record.version.clone(), recorded_hash: record.hash.clone(), file_hash });
                }
            }
        }
        Ok(())
    }

    /// [`Self::applied`] plus [`Self::check_drift`] -- a read-only drift check with no side
    /// effect, for a caller (or a test) that wants to know "has anything drifted?" without also
    /// applying whatever migrations are still pending.
    pub async fn verify(client: &mut PgClient) -> Result<(), CatalogError> {
        let already = Self::applied(client).await?;
        Self::check_drift(&already)
    }

    /// Applies every migration in [`MIGRATIONS`] not yet recorded in `schema_migrations`, in
    /// filename order, each inside its own `BEGIN`/`COMMIT` -- via [`PgClient::simple_batch`]
    /// (the one function in this crate that runs a whole SQL string verbatim, per that method's
    /// own doc; this is exactly the "a committed migration script" case it names). Checks
    /// [`Self::check_drift`] FIRST, before applying anything new: a tampered already-applied
    /// migration is refused even on a call that would otherwise have nothing new to do.
    /// `applied_tai_ns` is the caller-injected clock reading (this module's own doc, "Timestamps
    /// are injected, never read") recorded against every migration this call actually applies.
    /// Returns the filenames actually applied THIS call, in the order they ran -- empty on a
    /// database already fully up to date (this function's own "a second call is a no-op"
    /// contract, proven for real in `tests/catalog_postgis.rs`).
    pub async fn apply_pending(client: &mut PgClient, applied_tai_ns: i64) -> Result<Vec<&'static str>, CatalogError> {
        let already = Self::applied(client).await?;
        Self::check_drift(&already)?;
        let already_versions: std::collections::BTreeSet<&str> = already.iter().map(|r| r.version.as_str()).collect();

        let mut newly_applied = Vec::new();
        for m in MIGRATIONS {
            if already_versions.contains(m.filename) {
                continue;
            }
            let hash = hex_sha256(m.sql.as_bytes());
            let statement = format!(
                "BEGIN;\n{sql}\nINSERT INTO schema_migrations (version, hash, applied_tai_ns) VALUES ({version}, {hash}, {applied_tai_ns});\nCOMMIT;",
                sql = m.sql,
                version = sql_literal(m.filename),
                hash = sql_literal(&hash),
            );
            client.simple_batch(&statement).await?;
            newly_applied.push(m.filename);
        }
        Ok(newly_applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// [`hex_sha256`] against a published SHA-256 known-answer vector -- the empty string --
    /// rather than only round-tripping against itself, so this test would fail against an
    /// implementation that computed a self-consistent but WRONG hash function.
    #[test]
    fn hex_sha256_matches_the_published_empty_string_vector() {
        assert_eq!(hex_sha256(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn hex_sha256_matches_the_published_abc_vector() {
        assert_eq!(hex_sha256(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    /// [`MIGRATIONS`]' own filenames are in strictly increasing order -- the order
    /// [`Migrator::apply_pending`] applies them in, and (zero-padded `NNNN_` prefixes) also
    /// numeric order. A future migration added out of order (or with a colliding number) fails
    /// this test rather than silently applying in the wrong sequence.
    #[test]
    fn migrations_are_listed_in_strictly_increasing_filename_order() {
        let filenames: Vec<&str> = MIGRATIONS.iter().map(|m| m.filename).collect();
        let mut sorted = filenames.clone();
        sorted.sort_unstable();
        assert_eq!(filenames, sorted, "MIGRATIONS must already be in sorted filename order");
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(sorted.len(), deduped.len(), "MIGRATIONS must not list the same filename twice");
    }

    /// This module's own doc, "Why the migration set is a `const` array": [`MIGRATIONS`] and
    /// the real `migrations/` directory's own `.sql` file listing must name the exact same set,
    /// in the exact same order -- a new `.sql` file nobody added to [`MIGRATIONS`] fails HERE,
    /// at build/test time, rather than being silently skipped by a real migrator run.
    /// `CARGO_MANIFEST_DIR` (this task's rule 9), never an absolute path baked into this file.
    #[test]
    fn const_array_and_migrations_directory_agree() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("could not read {dir:?}: {e}"))
            .map(|entry| entry.unwrap_or_else(|e| panic!("reading a directory entry of {dir:?}: {e}")).file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".sql"))
            .collect();
        on_disk.sort_unstable();
        let registered: Vec<String> = MIGRATIONS.iter().map(|m| m.filename.to_string()).collect();
        assert_eq!(registered, on_disk, "crate::migrate::MIGRATIONS must list exactly the .sql files present in migrations/, in the same (sorted) order -- a file on disk with no entry here would be silently skipped by every real migrator run");
    }

    /// Every `include_str!`'d migration's own text hashes to something [`hex_sha256`] can
    /// compute without panicking, and hashing the same text twice gives the same answer
    /// (determinism -- ADR-004) -- a basic sanity check on the embedded content itself, distinct
    /// from the published-vector test above (which proves the FUNCTION is correct; this proves
    /// it is actually being called against real embedded file content, not an empty/placeholder
    /// string).
    #[test]
    fn every_migrations_own_hash_is_deterministic_and_non_trivial() {
        for m in MIGRATIONS {
            assert!(!m.sql.trim().is_empty(), "{}: embedded SQL text must not be empty", m.filename);
            let h1 = hex_sha256(m.sql.as_bytes());
            let h2 = hex_sha256(m.sql.as_bytes());
            assert_eq!(h1, h2);
            assert_eq!(h1.len(), 64);
            assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    /// [`Migrator::check_drift`]'s own pure logic, against a synthetic fixture -- no database
    /// needed: a recorded hash that disagrees with the REAL first migration file's own hash is
    /// refused as [`CatalogError::MigrationDrift`], naming the version and both hashes.
    #[test]
    fn check_drift_refuses_a_tampered_recorded_hash() {
        let real_version = MIGRATIONS[0].filename;
        let real_hash = hex_sha256(MIGRATIONS[0].sql.as_bytes());
        let tampered = MigrationRecord { version: real_version.to_string(), hash: "0".repeat(64), applied_tai_ns: 1 };
        let err = Migrator::check_drift(&[tampered]).unwrap_err();
        match err {
            CatalogError::MigrationDrift { version, recorded_hash, file_hash } => {
                assert_eq!(version, real_version);
                assert_eq!(recorded_hash, "0".repeat(64));
                assert_eq!(file_hash, real_hash);
            }
            other => panic!("{other:?}"),
        }
    }

    /// The non-drift case: a recorded hash that DOES match the real file's own hash passes.
    #[test]
    fn check_drift_accepts_a_matching_recorded_hash() {
        let real_version = MIGRATIONS[0].filename;
        let real_hash = hex_sha256(MIGRATIONS[0].sql.as_bytes());
        let matching = MigrationRecord { version: real_version.to_string(), hash: real_hash, applied_tai_ns: 1 };
        Migrator::check_drift(&[matching]).unwrap();
    }

    /// A recorded version with no entry in [`MIGRATIONS]` at all is not treated as drift (this
    /// module's own doc names this as a distinct, narrower gap, not something `check_drift`
    /// silently mis-detects as tampering).
    #[test]
    fn check_drift_ignores_a_recorded_version_with_no_matching_migration_file() {
        let unknown = MigrationRecord { version: "9999_does_not_exist.sql".to_string(), hash: "irrelevant".to_string(), applied_tai_ns: 1 };
        Migrator::check_drift(&[unknown]).unwrap();
    }

    /// [`sql_literal`]'s own escaping: a single quote is doubled, and the whole value stays
    /// wrapped in single quotes -- pure, docker-free.
    #[test]
    fn sql_literal_doubles_embedded_single_quotes() {
        assert_eq!(sql_literal("0001_init.sql"), "'0001_init.sql'");
        assert_eq!(sql_literal("a'b"), "'a''b'");
    }
}
