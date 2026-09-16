//! H2 (`docs/heavy-plan.md`): the real PostGIS integration proof for `av-catalog`'s schema,
//! migrator and queries. Every test here runs a genuine `imresamu/postgis` container
//! (`services/catalog/IMAGE_DIGEST.md`'s own recorded digest) and drives
//! [`av_catalog::client::PgClient`], [`av_catalog::migrate::Migrator`] and
//! [`av_catalog::query::find_assets`] against it over the real wire -- nothing in this file
//! mocks or stubs PostgreSQL. `crates/av-catalog/src/**`'s own `#[cfg(test)]` unit tests
//! already cover every pure function (wire framing, SCRAM known-answer vectors, `pgtext`'s
//! array/`bytea` encode/decode, the migrator's drift logic against a synthetic fixture, the
//! `AssetRef`/`CatalogAsset` round trip); this file exists specifically to prove the parts a
//! unit test cannot: that a real server applies this crate's migrations, that PostGIS's own
//! `geography(POLYGON,4326)` type is really what `footprint` is, that the label filter really
//! runs in SQL (proven by the exact asset-id sets three different clearances see, not merely
//! read from source), and that a real constraint violation surfaces as a typed
//! `CatalogError::Server` with the SQLSTATE this crate's own doc promises.
//!
//! # Never pulls -- question 154, question 212(a)
//!
//! The manager pulled `imresamu/postgis@sha256:...` once, at setup, on this host, and recorded
//! its digest beside this file's own image reference in `services/catalog/IMAGE_DIGEST.md`. A
//! test in this workspace must never run `docker pull` -- [`gate`] below calls
//! [`av_lockstep::docker::recorded_digest_gate`], which only ever runs `docker image inspect`
//! (a local, read-only query), and every container this file starts goes through
//! [`av_lockstep::docker::ManagedContainer::run_local`] (question 154/212(a)'s additive,
//! no-pull sibling of `pull_and_run`), never `pull_and_run` itself. `parse_image_digest_md`
//! reads `services/catalog/IMAGE_DIGEST.md` at TEST TIME, deriving its path from
//! `env!("CARGO_MANIFEST_DIR")` (this task's rule 9) rather than hard-coding either the image
//! reference or its digest a second time in this file's own source -- the recorded digest has
//! exactly one home. This whole section, and [`gate`]/[`fenced_block_after`] themselves, mirror
//! `crates/av-store/tests/minio_store.rs`'s identically-named functions line for line -- the
//! established pattern this task's own brief says to mirror exactly.
//!
//! # Gate/skip/label/lock discipline -- mirrored from `crates/av-store/tests/minio_store.rs`
//!
//! Every `#[tokio::test]` below: calls [`gate`] first (a gate failure announces a visible,
//! named `SKIPPED` line via [`av_lockstep::docker::announce_gate_skip`] and returns -- never a
//! silent pass, question 194); takes [`av_lockstep::docker::lock_docker_tests`] INSIDE its own
//! body (this task's rule 5) so a skipping test serialises nothing; calls
//! [`av_lockstep::docker::prune_stale_test_resources`] before creating anything (question 156);
//! starts its own PostgreSQL container labelled with [`av_lockstep::docker::test_label_args`],
//! no bind mount, with a per-run `POSTGRES_PASSWORD`/`POSTGRES_DB`; and relies on
//! [`av_lockstep::docker::ManagedContainer`]'s own `Drop` impl for teardown, even on a panic.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use av_cdm::pb::{Label, Provenance};
use av_catalog::client::{Param, PgClient, PgConfig, PgTls};
use av_catalog::error::CatalogError;
use av_catalog::labels::ClearanceLadder;
use av_catalog::migrate::{Migrator, MIGRATIONS};
use av_catalog::model::CatalogAsset;
use av_catalog::query::{find_assets, insert_lineage, lineage_parents, AssetQuery, GeoBbox, TimeRange};
use av_lockstep::docker::{announce_gate_skip, lock_docker_tests, prune_stale_test_resources, recorded_digest_gate, test_label_args, test_run_id, DockerGateReason, ManagedContainer};
use openssl::sha::sha256;

const CONTAINER_PORT: u16 = 5432;
// 60s, not `crates/av-store/tests/minio_store.rs`'s own 30s: a PostgreSQL/PostGIS container's
// own startup (initdb, then this image's own postgis-extension init scripts --
// `services/catalog/IMAGE_DIGEST.md`'s own measured fact) is genuinely heavier than a
// stateless MinIO server's, and this host runs under real, documented memory pressure
// (`common.md`: "This machine is SHARED with other concurrent sessions and is under real
// memory pressure") -- measured directly: a 30s budget produced a real, reproducible
// `wait_for_ready` timeout under load in this task's own acceptance run (the container itself
// had already been torn down by the time the failure's own diagnostic `docker logs` call ran,
// consistent with Docker/Colima falling behind under contention, not a bug in this file's own
// lock/prune/label discipline -- a full `docker ps`-clean before/after and a repeated,
// consistently-passing serial run both held). 60s gives real headroom without weakening the
// gate itself: a genuinely absent/undreachable daemon still fails fast through [`gate`], never
// through this timeout.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// A fixed, deterministic TAI-nanosecond epoch this whole file injects wherever a caller-
/// supplied clock reading is needed (`Migrator::apply_pending`'s `applied_tai_ns`,
/// `CatalogAsset::created_tai_ns`) -- this crate never reads a clock itself (rule 7), and this
/// file's own tests need no relationship to the real wall clock at all, only internal
/// consistency, so a fixed constant is simpler and more deterministic (ADR-004) than reading
/// `SystemTime::now()` the way `crates/av-store/tests/minio_store.rs`'s own `now()` must
/// (that file's SigV4 signing genuinely needs a real-ish timestamp; nothing here does).
const TEST_EPOCH_TAI_NS: i64 = 1_700_000_000_000_000_000;

// -----------------------------------------------------------------------------------------
// services/catalog/IMAGE_DIGEST.md parsing -- the recorded digest's one home. Mirrors
// crates/av-store/tests/minio_store.rs's identically-named functions.
// -----------------------------------------------------------------------------------------

fn image_digest_md_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../services/catalog/IMAGE_DIGEST.md")
}

fn fenced_block_after(text: &str, marker: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let marker_idx = lines.iter().position(|l| l.contains(marker))?;
    let fence_start = marker_idx + lines[marker_idx..].iter().position(|l| l.trim() == "```")?;
    let fence_end = fence_start + 1 + lines[fence_start + 1..].iter().position(|l| l.trim() == "```")?;
    let content = lines[fence_start + 1..fence_end].join("\n").trim().to_string();
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

fn parse_image_digest_md() -> (String, String) {
    let path = image_digest_md_path();
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {path:?}: {e}"));
    let image_ref = fenced_block_after(&text, "Registry reference").unwrap_or_else(|| panic!("could not find the 'Registry reference' fenced code block in {path:?}"));
    let recorded_id = fenced_block_after(&text, "docker image inspect").unwrap_or_else(|| panic!("could not find the 'docker image inspect' fenced code block in {path:?}"));
    (image_ref, recorded_id)
}

fn gate() -> Result<(), DockerGateReason> {
    let (image_ref, recorded_id) = parse_image_digest_md();
    recorded_digest_gate(&image_ref, &recorded_id)
}

macro_rules! gate_or_skip {
    ($test_name:expr) => {
        if let Err(reason) = gate() {
            let line = announce_gate_skip($test_name, &reason);
            assert!(line.starts_with("SKIPPED "), "{line:?}");
            return;
        }
    };
}

// -----------------------------------------------------------------------------------------
// Fixture: one PostgreSQL/PostGIS container + a ready PgClient connected to it.
// -----------------------------------------------------------------------------------------

fn docker(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).output().unwrap_or_else(|e| panic!("could not launch `docker {args:?}`: {e}"));
    if !output.status.success() {
        panic!("`docker {args:?}` failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn labels_from_test_label_args(run_id: &str) -> BTreeMap<String, String> {
    let args = test_label_args(run_id);
    let mut map = BTreeMap::new();
    for kv in args.iter().skip(1).step_by(2) {
        let (key, value) = kv.split_once('=').unwrap_or_else(|| panic!("test_label_args produced a non-KEY=VALUE entry: {kv:?}"));
        map.insert(key.to_string(), value.to_string());
    }
    map
}

/// One retry loop, connecting for real: `PgClient::connect` fails until the server inside the
/// just-started container has finished its own startup (initdb, then -- for this image --
/// running its `postgis`-extension init scripts). Rule 7's own exception: a bounded retry loop
/// polling for readiness in a test helper. Returns the first successful connection so the
/// caller does not have to reconnect immediately; every LATER connection in the same test just
/// calls `PgClient::connect` directly (the server is already known ready by then).
async fn wait_for_ready(config: &PgConfig, container_id: &str, test_name: &str) -> PgClient {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match PgClient::connect(config).await {
            Ok(client) => return client,
            Err(e) => {
                if Instant::now() > deadline {
                    let logs = docker(&["logs", container_id]);
                    panic!(
                        "{test_name}: PostgreSQL at {}:{} (container {container_id}) did not accept a connection within {READY_TIMEOUT:?}; last connect error: {e}; docker logs:\n{logs}",
                        config.host, config.port
                    );
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Holds the [`ManagedContainer`] alive for the fixture's whole lifetime (its `Drop` impl
/// removes the container when the fixture -- and therefore the test -- ends, panic or not).
/// `container_id` is read straight off `container.container_id` (`ManagedContainer`'s own
/// public field) rather than duplicated onto a second field here -- there is exactly one home
/// for it.
struct PostgisFixture {
    container: ManagedContainer,
}

/// Starts a fresh, labelled, loopback-only PostGIS container from `image_ref` (via
/// [`ManagedContainer::run_local`] -- no `docker pull`), waits for it to accept a real
/// connection, and returns the fixture plus that first, already-connected [`PgClient`].
async fn start_fixture(image_ref: &str, test_name: &str) -> (PostgisFixture, PgClient) {
    let run_id = test_run_id();
    let labels = labels_from_test_label_args(&run_id);

    // Postgres database-name rules are stricter than MinIO's access-key rules (letters/digits/
    // underscore, must not start with a digit) -- `test_run_id()`'s own "<pid>-<nanos>" shape
    // has a leading digit and a hyphen, so this file (unlike minio_store.rs's own
    // `sanitized_run_id`, which just strips the hyphen) also prefixes with a letter, matching
    // `services/catalog/run-dev-catalog.sh`'s own identical `avtest<sanitized>` shape.
    let sanitized_run_id: String = run_id.chars().filter(char::is_ascii_alphanumeric).collect();
    let password = format!("avtestpw{sanitized_run_id}");
    let db = format!("avtestdb{sanitized_run_id}");

    let mut env = BTreeMap::new();
    env.insert("POSTGRES_PASSWORD".to_string(), password.clone());
    env.insert("POSTGRES_DB".to_string(), db.clone());

    let (container, host_port) = ManagedContainer::run_local(image_ref, &[], CONTAINER_PORT, &BTreeMap::new(), &env, &BTreeMap::new(), &labels)
        .unwrap_or_else(|e| panic!("{test_name}: ManagedContainer::run_local({image_ref:?}) failed: {e}"));
    let container_id = container.container_id.clone();

    let config = PgConfig {
        host: "127.0.0.1".to_string(),
        port: host_port,
        user: "postgres".to_string(),
        password,
        database: db,
        application_name: "av-catalog-postgis-it".to_string(),
        connect_timeout: Duration::from_secs(5),
        tls: PgTls::Disabled,
    };

    let client = wait_for_ready(&config, &container_id, test_name).await;
    (PostgisFixture { container }, client)
}

// -----------------------------------------------------------------------------------------
// Fixtures: CatalogAsset builders shared by several tests below.
// -----------------------------------------------------------------------------------------

/// A minimal, valid `CatalogAsset` -- no spatial/temporal extent, no job -- for tests whose
/// own point is something other than extent/time (the label filter, lineage, the constraint
/// violation). `sha256` is a fixed, valid-looking (but not content-real) hex string: this
/// column has no UNIQUE constraint (`migrations/0001_init.sql`'s own doc: `asset_id`, not
/// `sha256`, is the primary key precisely so more than one row CAN share a `sha256`), so
/// reusing the same value across fixture assets in one test is fine.
fn make_asset(asset_id: &str, marking: &str, caveats: Vec<String>) -> CatalogAsset {
    CatalogAsset {
        asset_id: asset_id.to_string(),
        sha256: "a".repeat(64),
        uri: format!("s3://av-catalog-it/{asset_id}"),
        size_bytes: 100,
        media_type: "application/octet-stream".to_string(),
        label: Label { marking: marking.to_string(), caveats },
        frame_id: String::new(),
        extent_min: Vec::new(),
        extent_max: Vec::new(),
        footprint_wkt: None,
        start_tai_ns: None,
        end_tai_ns: None,
        provenance: Provenance { principal: "av-catalog-postgis-it".to_string(), tool: "catalog_postgis.rs".to_string(), ..Default::default() },
        job_id: None,
        created_tai_ns: TEST_EPOCH_TAI_NS,
    }
}

fn make_asset_with_footprint(asset_id: &str, footprint_wkt: Option<&str>) -> CatalogAsset {
    let mut asset = make_asset(asset_id, "UNCLASSIFIED", vec![]);
    asset.footprint_wkt = footprint_wkt.map(str::to_string);
    asset
}

fn make_asset_with_time(asset_id: &str, start_tai_ns: Option<i64>, end_tai_ns: Option<i64>) -> CatalogAsset {
    let mut asset = make_asset(asset_id, "UNCLASSIFIED", vec![]);
    asset.start_tai_ns = start_tai_ns;
    asset.end_tai_ns = end_tai_ns;
    asset
}

fn asset_ids(assets: &[CatalogAsset]) -> BTreeSet<String> {
    assets.iter().map(|a| a.asset_id.clone()).collect()
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(64);
    for b in sha256(bytes) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// -----------------------------------------------------------------------------------------
// The tests.
// -----------------------------------------------------------------------------------------

/// 1. `apply_pending` on an empty database creates the schema and records every migration with
///    its hash; a second `apply_pending` is a no-op; the recorded hash equals the committed
///    file's hash, computed independently here (never by calling back into `crate::migrate`'s
///    own hashing -- this test's `hex_sha256` is a second, from-scratch computation).
#[tokio::test]
async fn apply_pending_creates_schema_and_second_apply_is_a_noop() {
    const TEST_NAME: &str = "apply_pending_creates_schema_and_second_apply_is_a_noop";
    gate_or_skip!(TEST_NAME);
    let (image_ref, _recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (_fixture, mut client) = start_fixture(&image_ref, TEST_NAME).await;

    let applied_first = Migrator::apply_pending(&mut client, TEST_EPOCH_TAI_NS).await.unwrap_or_else(|e| panic!("{TEST_NAME}: first apply_pending: {e}"));
    assert_eq!(applied_first, vec!["0001_init.sql"], "{TEST_NAME}: first apply_pending must apply exactly crate::migrate::MIGRATIONS, in order");

    let recorded = Migrator::applied(&mut client).await.unwrap_or_else(|e| panic!("{TEST_NAME}: applied: {e}"));
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].version, "0001_init.sql");
    let independently_computed_hash = hex_sha256(MIGRATIONS[0].sql.as_bytes());
    assert_eq!(recorded[0].hash, independently_computed_hash, "{TEST_NAME}: schema_migrations.hash must equal the committed file's own SHA-256, computed independently in THIS test, not by calling back into the migrator's own hashing");
    assert_eq!(recorded[0].applied_tai_ns, TEST_EPOCH_TAI_NS);

    let applied_second = Migrator::apply_pending(&mut client, TEST_EPOCH_TAI_NS + 1).await.unwrap_or_else(|e| panic!("{TEST_NAME}: second apply_pending: {e}"));
    assert!(applied_second.is_empty(), "{TEST_NAME}: a second apply_pending against an already-migrated database must be a no-op, got {applied_second:?}");

    println!("{TEST_NAME}: first apply_pending applied {applied_first:?}, recorded hash {} at applied_tai_ns={}; second apply_pending applied {applied_second:?}", recorded[0].hash, recorded[0].applied_tai_ns);
}

/// 2. Migration drift is refused: tamper a recorded hash in `schema_migrations` directly (real
///    SQL, not a mock), then assert both `Migrator::verify` and `Migrator::apply_pending`
///    refuse with `CatalogError::MigrationDrift`.
#[tokio::test]
async fn migration_drift_is_refused() {
    const TEST_NAME: &str = "migration_drift_is_refused";
    gate_or_skip!(TEST_NAME);
    let (image_ref, _recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (_fixture, mut client) = start_fixture(&image_ref, TEST_NAME).await;

    Migrator::apply_pending(&mut client, TEST_EPOCH_TAI_NS).await.unwrap_or_else(|e| panic!("{TEST_NAME}: apply_pending: {e}"));

    let tampered_hash = "0".repeat(64);
    client
        .execute("UPDATE schema_migrations SET hash = $1 WHERE version = $2", &[Param::Text(tampered_hash.clone()), Param::Text("0001_init.sql".to_string())])
        .await
        .unwrap_or_else(|e| panic!("{TEST_NAME}: tampering schema_migrations.hash: {e}"));

    let verify_err = Migrator::verify(&mut client).await.unwrap_err();
    println!("{TEST_NAME}: real CatalogError::MigrationDrift Display (from verify): {verify_err}");
    match verify_err {
        CatalogError::MigrationDrift { version, recorded_hash, file_hash } => {
            assert_eq!(version, "0001_init.sql");
            assert_eq!(recorded_hash, tampered_hash);
            assert_eq!(file_hash, hex_sha256(MIGRATIONS[0].sql.as_bytes()));
        }
        other => panic!("{TEST_NAME}: expected CatalogError::MigrationDrift from verify, got {other:?}"),
    }

    let apply_err = Migrator::apply_pending(&mut client, TEST_EPOCH_TAI_NS + 2).await.unwrap_err();
    println!("{TEST_NAME}: real CatalogError::MigrationDrift Display (from apply_pending): {apply_err}");
    assert!(matches!(apply_err, CatalogError::MigrationDrift { .. }), "{TEST_NAME}: apply_pending must also refuse on drift before applying anything new, got {apply_err:?}");
}

/// 3. PostGIS is really there: `SELECT postgis_version()` returns 3.5.x (quoted in this test's
///    own stdout), and `footprint` really is `geography(POLYGON,4326)` -- asserted from
///    PostGIS's own `geography_columns` catalog view, never from the DDL this crate wrote.
#[tokio::test]
async fn postgis_is_really_there_and_footprint_is_really_geography_polygon_4326() {
    const TEST_NAME: &str = "postgis_is_really_there_and_footprint_is_really_geography_polygon_4326";
    gate_or_skip!(TEST_NAME);
    let (image_ref, _recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (_fixture, mut client) = start_fixture(&image_ref, TEST_NAME).await;
    Migrator::apply_pending(&mut client, TEST_EPOCH_TAI_NS).await.unwrap_or_else(|e| panic!("{TEST_NAME}: apply_pending: {e}"));

    let version_rows = client.query("SELECT postgis_version() AS v", &[]).await.unwrap_or_else(|e| panic!("{TEST_NAME}: SELECT postgis_version(): {e}"));
    let version = version_rows[0].get_str("v").unwrap_or_else(|e| panic!("{TEST_NAME}: reading postgis_version(): {e}")).to_string();
    println!("{TEST_NAME}: SELECT postgis_version() = {version:?}");
    assert!(version.starts_with("3.5"), "{TEST_NAME}: expected PostGIS 3.5.x (services/catalog/IMAGE_DIGEST.md's own measured version), got {version:?}");

    let geog_rows = client
        .query("SELECT type, srid FROM geography_columns WHERE f_table_name = 'assets' AND f_geography_column = 'footprint'", &[])
        .await
        .unwrap_or_else(|e| panic!("{TEST_NAME}: querying geography_columns: {e}"));
    assert_eq!(geog_rows.len(), 1, "{TEST_NAME}: geography_columns must report exactly one 'footprint' geography column on 'assets'");
    let geog_type = geog_rows[0].get_str("type").unwrap();
    let geog_srid = geog_rows[0].get_i64("srid").unwrap();
    println!("{TEST_NAME}: geography_columns reports assets.footprint as type={geog_type:?} srid={geog_srid} (from PostGIS's own catalog view, not this crate's DDL)");
    // Measured directly against the real server (services/catalog/IMAGE_DIGEST.md's own
    // PostGIS 3.5.4): geography_columns.type reports "Polygon" (PostGIS's own mixed-case
    // spelling for this catalog view), not the all-caps "POLYGON" the DDL itself writes --
    // asserted case-insensitively so this test tracks what the SERVER actually reports, not a
    // guess at its casing convention.
    assert!(geog_type.eq_ignore_ascii_case("POLYGON"), "expected a POLYGON geography column, got {geog_type:?}");
    assert_eq!(geog_srid, 4326);
}

/// 4. Label filter, exactly: three assets at three ladder markings, plus a fourth asset whose
///    marking (`TOP-SECRET`) is NOT on the ladder at all. Queried at every clearance, this test
///    asserts the EXACT returned asset-id set each time -- proving both that the higher-labelled
///    asset is invisible at a lower clearance AND that the lower-labelled ones ARE returned in
///    that same call (never merely "the list happened to be empty for some other reason"), and
///    that the off-ladder asset is invisible even at the top clearance.
#[tokio::test]
async fn label_filter_returns_exactly_the_assets_at_or_below_caller_clearance() {
    const TEST_NAME: &str = "label_filter_returns_exactly_the_assets_at_or_below_caller_clearance";
    gate_or_skip!(TEST_NAME);
    let (image_ref, _recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (_fixture, mut client) = start_fixture(&image_ref, TEST_NAME).await;
    Migrator::apply_pending(&mut client, TEST_EPOCH_TAI_NS).await.unwrap_or_else(|e| panic!("{TEST_NAME}: apply_pending: {e}"));

    let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]);

    for asset in [make_asset("label-u", "UNCLASSIFIED", vec![]), make_asset("label-c", "CUI", vec![]), make_asset("label-s", "SECRET", vec![]), make_asset("label-x", "TOP-SECRET", vec![])] {
        asset.insert(&mut client).await.unwrap_or_else(|e| panic!("{TEST_NAME}: inserting {:?}: {e}", asset.asset_id));
    }

    let query = AssetQuery { bbox: None, time: None, media_type: None, job_id: None, limit: 100 };

    let at_unclassified = find_assets(&mut client, &query, "UNCLASSIFIED", &ladder).await.unwrap_or_else(|e| panic!("{TEST_NAME}: find_assets at UNCLASSIFIED: {e}"));
    let at_cui = find_assets(&mut client, &query, "CUI", &ladder).await.unwrap_or_else(|e| panic!("{TEST_NAME}: find_assets at CUI: {e}"));
    let at_secret = find_assets(&mut client, &query, "SECRET", &ladder).await.unwrap_or_else(|e| panic!("{TEST_NAME}: find_assets at SECRET: {e}"));

    let ids_u = asset_ids(&at_unclassified);
    let ids_c = asset_ids(&at_cui);
    let ids_s = asset_ids(&at_secret);
    println!("{TEST_NAME}: UNCLASSIFIED saw {ids_u:?}, CUI saw {ids_c:?}, SECRET saw {ids_s:?}");

    assert_eq!(ids_u, BTreeSet::from(["label-u".to_string()]), "{TEST_NAME}: at UNCLASSIFIED, exactly the UNCLASSIFIED asset");
    assert_eq!(ids_c, BTreeSet::from(["label-u".to_string(), "label-c".to_string()]), "{TEST_NAME}: at CUI, exactly the UNCLASSIFIED and CUI assets (the lower-labelled one is still returned, proving this is a real filter, not an empty result)");
    assert_eq!(
        ids_s,
        BTreeSet::from(["label-u".to_string(), "label-c".to_string(), "label-s".to_string()]),
        "{TEST_NAME}: at SECRET (the top ladder rung), exactly the three on-ladder assets -- label-x (marking TOP-SECRET, not on the ladder at all) must NEVER be returned, at any clearance"
    );
}

/// 5. Extent: an asset whose footprint intersects the bbox is returned; one that touches only
///    at a shared edge IS returned too (this crate's documented rule: `ST_Intersects` treats a
///    touching boundary as intersecting, per OGC's "intersects = not disjoint"); one entirely
///    outside is not; one with a NULL footprint is excluded from a bbox-filtered query and
///    included when no bbox filter is given at all.
#[tokio::test]
async fn extent_query_bbox_semantics() {
    const TEST_NAME: &str = "extent_query_bbox_semantics";
    gate_or_skip!(TEST_NAME);
    let (image_ref, _recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (_fixture, mut client) = start_fixture(&image_ref, TEST_NAME).await;
    Migrator::apply_pending(&mut client, TEST_EPOCH_TAI_NS).await.unwrap_or_else(|e| panic!("{TEST_NAME}: apply_pending: {e}"));

    let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string()]);

    // Query bbox: (0,0) to (10,10).
    let inside = make_asset_with_footprint("ext-inside", Some("POLYGON((2 2,2 8,8 8,8 2,2 2))"));
    // Shares the line x=10, y in [2,8] with the bbox's own right edge -- touches, does not
    // overlap any area.
    let edge_touch = make_asset_with_footprint("ext-edge-touch", Some("POLYGON((10 2,10 8,15 8,15 2,10 2))"));
    let outside = make_asset_with_footprint("ext-outside", Some("POLYGON((20 20,20 25,25 25,25 20,20 20))"));
    let no_footprint = make_asset_with_footprint("ext-null", None);
    for asset in [&inside, &edge_touch, &outside, &no_footprint] {
        asset.insert(&mut client).await.unwrap_or_else(|e| panic!("{TEST_NAME}: inserting {:?}: {e}", asset.asset_id));
    }

    let bbox_query = AssetQuery { bbox: Some(GeoBbox { min_lon: 0.0, min_lat: 0.0, max_lon: 10.0, max_lat: 10.0 }), time: None, media_type: None, job_id: None, limit: 100 };
    let bbox_results = find_assets(&mut client, &bbox_query, "UNCLASSIFIED", &ladder).await.unwrap_or_else(|e| panic!("{TEST_NAME}: find_assets (bbox): {e}"));
    let bbox_ids = asset_ids(&bbox_results);
    println!("{TEST_NAME}: bbox-filtered ids = {bbox_ids:?}");
    assert!(bbox_ids.contains("ext-inside"), "{TEST_NAME}: a footprint fully inside the bbox must be returned: {bbox_ids:?}");
    assert!(bbox_ids.contains("ext-edge-touch"), "{TEST_NAME}: a footprint touching the bbox only at a shared edge must be returned (ST_Intersects treats touching as intersecting): {bbox_ids:?}");
    assert!(!bbox_ids.contains("ext-outside"), "{TEST_NAME}: a footprint entirely outside the bbox must not be returned: {bbox_ids:?}");
    assert!(!bbox_ids.contains("ext-null"), "{TEST_NAME}: a NULL footprint must be excluded from a bbox-filtered query: {bbox_ids:?}");

    let unfiltered_query = AssetQuery { bbox: None, time: None, media_type: None, job_id: None, limit: 100 };
    let unfiltered_results = find_assets(&mut client, &unfiltered_query, "UNCLASSIFIED", &ladder).await.unwrap_or_else(|e| panic!("{TEST_NAME}: find_assets (unfiltered): {e}"));
    let unfiltered_ids = asset_ids(&unfiltered_results);
    println!("{TEST_NAME}: unfiltered ids = {unfiltered_ids:?}");
    assert!(unfiltered_ids.contains("ext-null"), "{TEST_NAME}: a NULL footprint must be INCLUDED when no bbox filter is given: {unfiltered_ids:?}");
    assert!(unfiltered_ids.contains("ext-inside") && unfiltered_ids.contains("ext-edge-touch") && unfiltered_ids.contains("ext-outside"), "{TEST_NAME}: every asset must be present with no bbox filter: {unfiltered_ids:?}");
}

/// 6. Time boundaries: half-open `[start, end)` overlap against a query window `[100, 200)`.
///    An asset ending exactly at the query's own start does NOT match; one starting exactly at
///    the query's own end does NOT match either (the symmetric boundary); one starting exactly
///    at the query's own start DOES match; a NULL time range is excluded when a time filter is
///    given and included when it is not.
#[tokio::test]
async fn time_query_half_open_boundaries() {
    const TEST_NAME: &str = "time_query_half_open_boundaries";
    gate_or_skip!(TEST_NAME);
    let (image_ref, _recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (_fixture, mut client) = start_fixture(&image_ref, TEST_NAME).await;
    Migrator::apply_pending(&mut client, TEST_EPOCH_TAI_NS).await.unwrap_or_else(|e| panic!("{TEST_NAME}: apply_pending: {e}"));

    let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string()]);

    // Query time range: [100, 200).
    let full_overlap = make_asset_with_time("time-full-overlap", Some(120), Some(150));
    let ends_at_query_start = make_asset_with_time("time-ends-at-query-start", Some(50), Some(100));
    let starts_at_query_end = make_asset_with_time("time-starts-at-query-end", Some(200), Some(250));
    let starts_at_query_start = make_asset_with_time("time-starts-at-query-start", Some(100), Some(150));
    let no_time = make_asset_with_time("time-null", None, None);
    for asset in [&full_overlap, &ends_at_query_start, &starts_at_query_end, &starts_at_query_start, &no_time] {
        asset.insert(&mut client).await.unwrap_or_else(|e| panic!("{TEST_NAME}: inserting {:?}: {e}", asset.asset_id));
    }

    let time_query = AssetQuery { bbox: None, time: Some(TimeRange { start_tai_ns: 100, end_tai_ns: 200 }), media_type: None, job_id: None, limit: 100 };
    let time_results = find_assets(&mut client, &time_query, "UNCLASSIFIED", &ladder).await.unwrap_or_else(|e| panic!("{TEST_NAME}: find_assets (time): {e}"));
    let time_ids = asset_ids(&time_results);
    println!("{TEST_NAME}: time-filtered [100,200) ids = {time_ids:?}");
    assert!(time_ids.contains("time-full-overlap"), "{TEST_NAME}: an asset fully inside the query window must match: {time_ids:?}");
    assert!(time_ids.contains("time-starts-at-query-start"), "{TEST_NAME}: an asset starting exactly at the query's own start must match (half-open on both sides): {time_ids:?}");
    assert!(!time_ids.contains("time-ends-at-query-start"), "{TEST_NAME}: an asset ending exactly at the query's own start must NOT match: {time_ids:?}");
    assert!(!time_ids.contains("time-starts-at-query-end"), "{TEST_NAME}: an asset starting exactly at the query's own end must NOT match: {time_ids:?}");
    assert!(!time_ids.contains("time-null"), "{TEST_NAME}: a NULL time range must be excluded from a time-filtered query: {time_ids:?}");

    let unfiltered_query = AssetQuery { bbox: None, time: None, media_type: None, job_id: None, limit: 100 };
    let unfiltered_results = find_assets(&mut client, &unfiltered_query, "UNCLASSIFIED", &ladder).await.unwrap_or_else(|e| panic!("{TEST_NAME}: find_assets (unfiltered): {e}"));
    let unfiltered_ids = asset_ids(&unfiltered_results);
    println!("{TEST_NAME}: unfiltered ids = {unfiltered_ids:?}");
    assert!(unfiltered_ids.contains("time-null"), "{TEST_NAME}: a NULL time range must be INCLUDED when no time filter is given: {unfiltered_ids:?}");
}

/// 7. Lineage: an asset with two parents and a job id round-trips.
#[tokio::test]
async fn lineage_round_trips_two_parents_and_a_job_id() {
    const TEST_NAME: &str = "lineage_round_trips_two_parents_and_a_job_id";
    gate_or_skip!(TEST_NAME);
    let (image_ref, _recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (_fixture, mut client) = start_fixture(&image_ref, TEST_NAME).await;
    Migrator::apply_pending(&mut client, TEST_EPOCH_TAI_NS).await.unwrap_or_else(|e| panic!("{TEST_NAME}: apply_pending: {e}"));

    for asset in [make_asset("lineage-parent-1", "UNCLASSIFIED", vec![]), make_asset("lineage-parent-2", "UNCLASSIFIED", vec![]), make_asset("lineage-child", "UNCLASSIFIED", vec![])] {
        asset.insert(&mut client).await.unwrap_or_else(|e| panic!("{TEST_NAME}: inserting {:?}: {e}", asset.asset_id));
    }

    insert_lineage(&mut client, "lineage-child", "lineage-parent-1", "job-42").await.unwrap_or_else(|e| panic!("{TEST_NAME}: insert_lineage parent-1: {e}"));
    insert_lineage(&mut client, "lineage-child", "lineage-parent-2", "job-42").await.unwrap_or_else(|e| panic!("{TEST_NAME}: insert_lineage parent-2: {e}"));

    let parents = lineage_parents(&mut client, "lineage-child").await.unwrap_or_else(|e| panic!("{TEST_NAME}: lineage_parents: {e}"));
    println!("{TEST_NAME}: lineage-child's recorded (parent_asset_id, job_id) pairs = {parents:?}");
    assert_eq!(parents, vec![("lineage-parent-1".to_string(), "job-42".to_string()), ("lineage-parent-2".to_string(), "job-42".to_string())]);
}

/// 8. The running container's image id equals `services/catalog/IMAGE_DIGEST.md`'s own
///    recorded digest -- an EXPLICIT assertion (question 212(a)), not merely trusting [`gate`].
#[tokio::test]
async fn running_container_image_id_equals_the_recorded_digest() {
    const TEST_NAME: &str = "running_container_image_id_equals_the_recorded_digest";
    gate_or_skip!(TEST_NAME);
    let (image_ref, recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (fixture, mut client) = start_fixture(&image_ref, TEST_NAME).await;

    let running_image_id = docker(&["inspect", "--format", "{{.Image}}", &fixture.container.container_id]);
    println!("{TEST_NAME}: running container image id = {running_image_id:?}, recorded id (services/catalog/IMAGE_DIGEST.md) = {recorded_id:?}");
    assert_eq!(running_image_id, recorded_id, "{TEST_NAME}: the RUNNING PostGIS container's own image id must equal the digest recorded in services/catalog/IMAGE_DIGEST.md");

    // A cheap, real proof this fixture's client can actually talk to the container whose image
    // id was just checked -- not load-bearing for the assertion above, but confirms this is a
    // live, addressable container, not a lucky id match on something that never actually
    // started serving.
    Migrator::apply_pending(&mut client, TEST_EPOCH_TAI_NS).await.unwrap_or_else(|e| panic!("{TEST_NAME}: apply_pending: {e}"));
}

/// 9. SQLSTATE surfaces: violating `assets_sha256_check` (the `sha256 ~ '^[0-9a-f]{64}$'`
///    constraint, `migrations/0001_init.sql`) is a typed `CatalogError::Server` with SQLSTATE
///    `23514` (`check_violation`).
#[tokio::test]
async fn sha256_check_constraint_violation_surfaces_as_server_error_23514() {
    const TEST_NAME: &str = "sha256_check_constraint_violation_surfaces_as_server_error_23514";
    gate_or_skip!(TEST_NAME);
    let (image_ref, _recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (_fixture, mut client) = start_fixture(&image_ref, TEST_NAME).await;
    Migrator::apply_pending(&mut client, TEST_EPOCH_TAI_NS).await.unwrap_or_else(|e| panic!("{TEST_NAME}: apply_pending: {e}"));

    let mut bad_asset = make_asset("bad-sha256", "UNCLASSIFIED", vec![]);
    bad_asset.sha256 = "not-a-valid-sha256-hex-digest".to_string();

    let err = bad_asset.insert(&mut client).await.unwrap_err();
    println!("{TEST_NAME}: real CatalogError::Server Display: {err}");
    match err {
        CatalogError::Server(server_error) => {
            assert_eq!(server_error.sqlstate, "23514", "{TEST_NAME}: expected SQLSTATE 23514 (check_violation), got {:?} (full ServerError: {server_error:?})", server_error.sqlstate);
        }
        other => panic!("{TEST_NAME}: expected CatalogError::Server, got {other:?}"),
    }
}
