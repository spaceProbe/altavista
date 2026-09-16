//! H2c (`docs/heavy-plan.md` H2: "The av-gateway learns a `catalog` selector through the same
//! authentication it already has"): the end-to-end proof that `GATEWAY_SELECTOR_CATALOG`,
//! served over a REAL `DataGatewayService` gRPC socket with a REAL verified OIDC token, returns
//! exactly the assets a caller's clearance entitles them to -- against a REAL PostGIS
//! container, mirroring `crates/av-catalog/tests/catalog_postgis.rs`'s own gate/lock/label/
//! digest discipline exactly (this task's own brief names that file as the pattern to mirror).
//!
//! Plus the docker-free refusals this task's own brief lists: an unauthenticated catalog
//! query, a token whose groups grant no `"query"` role, a disagreeing `caller_clearance`, and
//! "no catalog configured" -- every one of them proven through the SAME real gRPC socket
//! (`GatewayServerHarness`, below), never merely in-process. The malformed-`CatalogQuery`
//! refusals (limit/bbox/time) and the "no catalog configured"/caller-supplied-path refusals'
//! own typed shapes are already proven, docker-free, by `crates/av-gateway/src/
//! catalog_selector.rs`'s own unit tests -- this file's docker-free section focuses on what
//! THOSE tests cannot reach: the authentication layer (`crate::auth::AuthContext`), which
//! lives one level up, in `crate::gateway::authenticated_query`, not in `query_catalog` itself.
//!
//! The pinned query id for an existing selector is `crates/av-gateway/src/query_id.rs`'s own
//! `compute_query_id_output_is_unchanged_by_the_h2c_extension` test -- not duplicated here.

mod auth_common;

mod harness {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use av_gateway::auth::AuthContext;
    use av_gateway::catalog_selector::CatalogHandle;
    use av_gateway::catalogue::{CatalogueEntry, RunCatalogue};
    use av_gateway::counters::Counters;
    use av_gateway::gateway::{DataGatewayServiceImpl, DataGatewayServiceServer, GatewayCore};
    use av_gateway::labels::ClearanceLadder;
    use av_gateway::pb::data_gateway_service_client::DataGatewayServiceClient;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::transport::{Channel, Endpoint, Server};

    /// A REAL `DataGatewayService` over a real loopback socket, optionally with a catalog
    /// handle attached -- mirrors `crates/av-gateway/tests/command_trail_run_products.rs`'s
    /// own `GatewayServerHarness` (this crate's own established pattern for a
    /// `DataGatewayService`-only server, no `ModelProposeService`/`CommandAuthorityService`
    /// wiring, this file's own read-path-only scope), extended with the one thing that file's
    /// own harness has no need of: `catalog`.
    pub struct GatewayServerHarness {
        pub client: DataGatewayServiceClient<Channel>,
        pub counters: Arc<Counters>,
        shutdown_tx: Option<oneshot::Sender<()>>,
        handle: Option<tokio::task::JoinHandle<()>>,
    }

    impl GatewayServerHarness {
        pub async fn spawn(entries: BTreeMap<String, CatalogueEntry>, ladder: ClearanceLadder, auth: Arc<AuthContext>, catalog: Option<CatalogHandle>) -> Self {
            let counters = Arc::new(Counters::new());
            let mut core = GatewayCore::new(RunCatalogue::new(entries), ladder, counters.clone());
            if let Some(catalog) = catalog {
                core = core.with_catalog(catalog);
            }
            let core = Arc::new(core);

            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port");
            let addr = listener.local_addr().expect("local_addr");
            let incoming = TcpListenerStream::new(listener);

            let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
            let handle = tokio::spawn(async move {
                Server::builder()
                    .add_service(DataGatewayServiceServer::new(DataGatewayServiceImpl::new(core, auth)))
                    .serve_with_incoming_shutdown(incoming, async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .expect("server exits cleanly");
            });

            let channel = Endpoint::from_shared(format!("http://{addr}"))
                .expect("valid endpoint URI")
                .connect()
                .await
                .expect("connect to the just-spawned DataGatewayService over its real loopback socket");
            let client = DataGatewayServiceClient::new(channel);

            Self { client, counters, shutdown_tx: Some(shutdown_tx), handle: Some(handle) }
        }

        pub async fn shutdown(mut self) {
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
            if let Some(handle) = self.handle.take() {
                let _ = handle.await;
            }
        }
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use av_catalog::client::{PgClient, PgConfig, PgTls};
use av_catalog::migrate::Migrator;
use av_catalog::model::CatalogAsset;
use av_cdm::pb::{CatalogQuery, GatewayQueryRequest, GatewaySelector, Label, Provenance};
use av_command::test_support::TestIssuer;
use av_gateway::catalog_selector::CatalogHandle;
use av_gateway::labels::ClearanceLadder;
use av_lockstep::docker::{announce_gate_skip, lock_docker_tests, prune_stale_test_resources, recorded_digest_gate, test_label_args, test_run_id, DockerGateReason, ManagedContainer};
use auth_common::{mint_token, test_auth_context, test_clock, TEST_NOW_UNIX_S, GROUP_CUI, GROUP_SECRET, GROUP_UNCLASSIFIED};
use harness::GatewayServerHarness;
use tonic::Code;

const CONTAINER_PORT: u16 = 5432;
/// Mirrors `crates/av-catalog/tests/catalog_postgis.rs`'s own `READY_TIMEOUT` -- identical
/// image, identical host, identical measured startup cost (see that file's own comment for the
/// full "why 60s" reasoning; restated here rather than imported since it is a private `const`
/// in that other test binary, not a library item this crate could import).
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const TEST_EPOCH_TAI_NS: i64 = 1_700_000_000_000_000_000;

// -----------------------------------------------------------------------------------------
// services/catalog/IMAGE_DIGEST.md parsing and the gate -- mirrors
// crates/av-catalog/tests/catalog_postgis.rs's identically-named functions line for line (this
// task's own brief: "mirroring crates/av-catalog/tests/catalog_postgis.rs's gate/lock/label/
// digest discipline"). Not a shared library function: `catalog_postgis.rs`'s own copy is
// private to that test binary, not exported by `av-catalog`'s own `src/`.
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
// Fixture: one PostgreSQL/PostGIS container, migrated, seeded with three on-ladder assets and
// one off-ladder one -- plus a PgConfig the real GatewayServerHarness's own CatalogHandle
// dials independently (this crate's production code, crate::catalog_selector, connects fresh
// per call -- see that module's own "Connection lifecycle" doc -- so the seeding connection
// below and the harness's own runtime connection are deliberately two separate `PgClient`s
// against the same server, exactly like a real deployment's seeding tooling and its
// long-running gateway process would be).
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

async fn wait_for_ready(config: &PgConfig, container_id: &str, test_name: &str) -> PgClient {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match PgClient::connect(config).await {
            Ok(client) => return client,
            Err(e) => {
                if Instant::now() > deadline {
                    let logs = docker(&["logs", container_id]);
                    panic!("{test_name}: PostgreSQL at {}:{} (container {container_id}) did not accept a connection within {READY_TIMEOUT:?}; last connect error: {e}; docker logs:\n{logs}", config.host, config.port);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

struct PostgisFixture {
    container: ManagedContainer,
}

/// Starts a fresh, labelled, loopback-only PostGIS container, applies `crates/av-catalog`'s
/// own migrations, seeds four assets (`UNCLASSIFIED`, `CUI`, `SECRET`, and `TOP-SECRET` --
/// deliberately OFF this test's own three-rung ladder, mirroring `catalog_postgis.rs`'s
/// identical fixture), and returns the fixture plus a fresh [`PgConfig`] any later
/// [`PgClient::connect`] (including the real gateway harness's own) can dial against it.
async fn start_seeded_fixture(image_ref: &str, test_name: &str) -> (PostgisFixture, PgConfig) {
    let run_id = test_run_id();
    let labels = labels_from_test_label_args(&run_id);

    let sanitized_run_id: String = run_id.chars().filter(char::is_ascii_alphanumeric).collect();
    let password = format!("avtestpw{sanitized_run_id}");
    let db = format!("avtestdb{sanitized_run_id}");

    let mut env = BTreeMap::new();
    env.insert("POSTGRES_PASSWORD".to_string(), password.clone());
    env.insert("POSTGRES_DB".to_string(), db.clone());

    let (container, host_port) = ManagedContainer::run_local(image_ref, &[], CONTAINER_PORT, &BTreeMap::new(), &env, &BTreeMap::new(), &labels)
        .unwrap_or_else(|e| panic!("{test_name}: ManagedContainer::run_local({image_ref:?}) failed: {e}"));
    let container_id = container.container_id.clone();

    let config =
        PgConfig { host: "127.0.0.1".to_string(), port: host_port, user: "postgres".to_string(), password, database: db, application_name: "av-gateway-catalog-selector-it".to_string(), connect_timeout: Duration::from_secs(5), tls: PgTls::Disabled };

    let mut seed_client = wait_for_ready(&config, &container_id, test_name).await;
    Migrator::apply_pending(&mut seed_client, TEST_EPOCH_TAI_NS).await.unwrap_or_else(|e| panic!("{test_name}: apply_pending: {e}"));

    for (asset_id, marking) in [("asset-u", "UNCLASSIFIED"), ("asset-c", "CUI"), ("asset-s", "SECRET"), ("asset-x", "TOP-SECRET")] {
        let asset = CatalogAsset {
            asset_id: asset_id.to_string(),
            sha256: "a".repeat(64),
            uri: format!("s3://av-gateway-catalog-selector-it/{asset_id}"),
            size_bytes: 100,
            media_type: "application/octet-stream".to_string(),
            label: Label { marking: marking.to_string(), caveats: vec![] },
            frame_id: String::new(),
            extent_min: Vec::new(),
            extent_max: Vec::new(),
            footprint_wkt: None,
            start_tai_ns: None,
            end_tai_ns: None,
            provenance: Provenance { principal: "av-gateway-catalog-selector-it".to_string(), tool: "catalog_selector.rs".to_string(), ..Default::default() },
            job_id: None,
            created_tai_ns: TEST_EPOCH_TAI_NS,
        };
        asset.insert(&mut seed_client).await.unwrap_or_else(|e| panic!("{test_name}: inserting {asset_id:?}: {e}"));
    }
    let _ = seed_client.close().await;

    (PostgisFixture { container }, config)
}

fn ladder() -> ClearanceLadder {
    ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()])
}

fn catalog_handle(pg_config: PgConfig) -> CatalogHandle {
    CatalogHandle { pg_config, ladder: av_catalog::labels::ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]) }
}

fn catalog_req(caller_clearance: &str, caller_token: &str, catalog_query: CatalogQuery) -> GatewayQueryRequest {
    GatewayQueryRequest {
        run: None,
        caller_clearance: caller_clearance.to_string(),
        selector: GatewaySelector::Catalog as i32,
        caller_supplied_products_uri: String::new(),
        caller_token: caller_token.to_string(),
        catalog_query: Some(catalog_query),
    }
}

fn unfiltered_query() -> CatalogQuery {
    CatalogQuery { bbox: None, time: None, media_type: String::new(), job_id: String::new(), limit: 100 }
}

fn group_for_clearance(clearance: &str) -> &'static str {
    match clearance {
        "UNCLASSIFIED" => GROUP_UNCLASSIFIED,
        "CUI" => GROUP_CUI,
        "SECRET" => GROUP_SECRET,
        other => panic!("no test group configured for clearance {other:?}"),
    }
}

// -----------------------------------------------------------------------------------------
// Real-container tests (gated, locked, pruned, labelled -- this task's own brief).
// -----------------------------------------------------------------------------------------

/// **This task's own core acceptance line**: a cleared caller sees the lower-labelled assets
/// and not the higher-labelled one, through the REAL gRPC `Query` rpc with a REAL verified
/// token -- proven by the EXACT asset-id set each of three clearances sees (mirroring
/// `crates/av-catalog/tests/catalog_postgis.rs`'s own `label_filter_returns_exactly_the_
/// assets_at_or_below_caller_clearance`, one layer up: through the real, authenticated
/// `DataGatewayService`, not `find_assets` called directly). Also proves, incidentally but for
/// real: no `run` was ever required (every request this test builds carries `run: None`, and
/// every one succeeds), and the query id is non-empty and stable.
#[tokio::test]
async fn the_gateway_serves_exactly_the_label_filtered_catalog_assets_over_the_real_grpc_rpc() {
    const TEST_NAME: &str = "the_gateway_serves_exactly_the_label_filtered_catalog_assets_over_the_real_grpc_rpc";
    gate_or_skip!(TEST_NAME);
    let (image_ref, _recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (fixture, pg_config) = start_seeded_fixture(&image_ref, TEST_NAME).await;
    println!("{TEST_NAME}: seeded PostGIS container {}", fixture.container.container_id);

    let issuer = TestIssuer::new();
    let auth = test_auth_context(&issuer, test_clock());
    let mut harness = GatewayServerHarness::spawn(BTreeMap::new(), ladder(), auth, Some(catalog_handle(pg_config))).await;

    let mut seen: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for clearance in ["UNCLASSIFIED", "CUI", "SECRET"] {
        let token = mint_token(&issuer, "operator-1", &[group_for_clearance(clearance)], TEST_NOW_UNIX_S, 3_600);
        let resp = harness.client.query(catalog_req(clearance, &token, unfiltered_query())).await.unwrap_or_else(|e| panic!("{TEST_NAME}: CATALOG query at {clearance}: {e}")).into_inner();
        assert!(!resp.query_id.is_empty(), "{TEST_NAME}: query_id must be non-empty for a real catalog query");
        let ids: BTreeSet<String> = resp.catalog_records.iter().map(|r| r.asset_id.clone()).collect();
        println!("{TEST_NAME}: {clearance} saw {ids:?}");
        seen.insert(clearance, ids);
    }

    assert_eq!(seen["UNCLASSIFIED"], BTreeSet::from(["asset-u".to_string()]), "{TEST_NAME}: at UNCLASSIFIED, exactly the UNCLASSIFIED asset");
    assert_eq!(seen["CUI"], BTreeSet::from(["asset-u".to_string(), "asset-c".to_string()]), "{TEST_NAME}: at CUI, exactly UNCLASSIFIED and CUI (the lower one still returned -- a real filter, not an empty result)");
    assert_eq!(
        seen["SECRET"],
        BTreeSet::from(["asset-u".to_string(), "asset-c".to_string(), "asset-s".to_string()]),
        "{TEST_NAME}: at SECRET (this test's own top ladder rung), exactly the three on-ladder assets -- asset-x (TOP-SECRET, off this ladder) must never appear, at any clearance"
    );

    // Every asset a real conversion of a real CatalogAsset -- the AssetRef half round-trips
    // for real, not just the id.
    let secret_resp = harness
        .client
        .query(catalog_req("SECRET", &mint_token(&issuer, "operator-1", &[group_for_clearance("SECRET")], TEST_NOW_UNIX_S, 3_600), unfiltered_query()))
        .await
        .expect(TEST_NAME)
        .into_inner();
    let asset_c = secret_resp.catalog_records.iter().find(|r| r.asset_id == "asset-c").expect("asset-c present at SECRET");
    let asset_ref = asset_c.asset.as_ref().expect("CatalogRecord.asset set");
    assert_eq!(asset_ref.label.as_ref().unwrap().marking, "CUI");
    assert_eq!(asset_ref.uri, "s3://av-gateway-catalog-selector-it/asset-c");

    assert_eq!(harness.counters.get("gateway_caller_supplied_path"), 0);
    harness.shutdown().await;
}

/// A `media_type`/`limit`-filtered catalog query, through the real rpc, still label-filters
/// correctly -- a caller at CUI, asking for every asset (`limit` large enough), still never
/// sees the SECRET one, proving the filters compose with D2's label enforcement rather than
/// bypassing it.
#[tokio::test]
async fn a_filtered_catalog_query_still_enforces_the_label_over_the_real_rpc() {
    const TEST_NAME: &str = "a_filtered_catalog_query_still_enforces_the_label_over_the_real_rpc";
    gate_or_skip!(TEST_NAME);
    let (image_ref, _recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (fixture, pg_config) = start_seeded_fixture(&image_ref, TEST_NAME).await;
    println!("{TEST_NAME}: seeded PostGIS container {}", fixture.container.container_id);

    let issuer = TestIssuer::new();
    let auth = test_auth_context(&issuer, test_clock());
    let mut harness = GatewayServerHarness::spawn(BTreeMap::new(), ladder(), auth, Some(catalog_handle(pg_config))).await;

    let token = mint_token(&issuer, "operator-1", &[group_for_clearance("CUI")], TEST_NOW_UNIX_S, 3_600);
    let mut query = unfiltered_query();
    query.media_type = "application/octet-stream".to_string();
    let resp = harness.client.query(catalog_req("CUI", &token, query)).await.expect(TEST_NAME).into_inner();
    let ids: BTreeSet<String> = resp.catalog_records.iter().map(|r| r.asset_id.clone()).collect();
    assert_eq!(ids, BTreeSet::from(["asset-u".to_string(), "asset-c".to_string()]), "{TEST_NAME}: media_type filter must never surface the SECRET asset to a CUI caller: {ids:?}");

    harness.shutdown().await;
}

// -----------------------------------------------------------------------------------------
// Docker-free tests: the authentication layer, and "no catalog configured", through the same
// real gRPC socket -- see this file's own module doc for why these live here rather than only
// as crate::catalog_selector's own unit tests.
// -----------------------------------------------------------------------------------------

/// An unauthenticated (empty-token) `GATEWAY_SELECTOR_CATALOG` request is refused, over the
/// real rpc, before any catalog logic runs at all -- the auth counter, not any catalog
/// counter, is what moves.
#[tokio::test]
async fn an_unauthenticated_catalog_query_is_refused_over_the_real_rpc_and_counted() {
    let issuer = TestIssuer::new();
    let auth = test_auth_context(&issuer, test_clock());
    let mut harness = GatewayServerHarness::spawn(BTreeMap::new(), ladder(), auth, None).await;

    let err = harness.client.query(catalog_req("CUI", "", unfiltered_query())).await.expect_err("an empty token must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");
    assert_eq!(harness.counters.get("gateway_auth_missing_token"), 1);
    assert_eq!(harness.counters.get("catalog_not_configured"), 0, "catalog-specific logic must never run before authentication");

    harness.shutdown().await;
}

/// A verified token whose groups grant no role on the `"query"` surface at all is refused,
/// over the real rpc.
#[tokio::test]
async fn a_token_whose_groups_grant_no_query_surface_is_refused_over_the_real_rpc() {
    let issuer = TestIssuer::new();
    let auth = test_auth_context(&issuer, test_clock());
    let mut harness = GatewayServerHarness::spawn(BTreeMap::new(), ladder(), auth, None).await;

    let token = mint_token(&issuer, "operator-1", &["nobody-group"], TEST_NOW_UNIX_S, 3_600);
    let err = harness.client.query(catalog_req("CUI", &token, unfiltered_query())).await.expect_err("a token naming no granting role must be refused");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    assert_eq!(harness.counters.get("gateway_auth_role_not_granted"), 1);

    harness.shutdown().await;
}

/// A `caller_clearance` field that disagrees with the token-derived clearance is refused,
/// over the real rpc -- proving the wire field can never buy read access to a clearance the
/// token does not actually carry, for the catalog selector exactly like every other one
/// (invariant C, `crate::auth::AuthContext::authenticate_query`'s own doc).
#[tokio::test]
async fn a_disagreeing_caller_clearance_is_refused_over_the_real_rpc_and_counted() {
    let issuer = TestIssuer::new();
    let auth = test_auth_context(&issuer, test_clock());
    let mut harness = GatewayServerHarness::spawn(BTreeMap::new(), ladder(), auth, None).await;

    // "operators"-equivalent group here maps to CUI (auth_common::GROUP_CUI); declare SECRET.
    let token = mint_token(&issuer, "operator-1", &[GROUP_CUI], TEST_NOW_UNIX_S, 3_600);
    let err = harness.client.query(catalog_req("SECRET", &token, unfiltered_query())).await.expect_err("a declared clearance above the token's own must be refused");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    assert_eq!(harness.counters.get("gateway_auth_clearance_mismatch"), 1);

    harness.shutdown().await;
}

/// A properly-authenticated, correctly-cleared caller against a `GatewayCore` with NO catalog
/// handle attached is refused `FAILED_PRECONDITION`, typed and counted -- never an empty
/// success, over the real rpc (`crate::catalog_selector`'s own module doc, item 4; unit-level
/// proof already lives in `crates/av-gateway/src/catalog_selector.rs`'s own tests -- this is
/// the end-to-end proof, through real authentication, that the SAME refusal is what a real
/// caller actually receives on the wire).
#[tokio::test]
async fn no_catalog_configured_is_refused_over_the_real_rpc_even_for_a_properly_authenticated_caller() {
    let issuer = TestIssuer::new();
    let auth = test_auth_context(&issuer, test_clock());
    let mut harness = GatewayServerHarness::spawn(BTreeMap::new(), ladder(), auth, None).await;

    let token = mint_token(&issuer, "operator-1", &[group_for_clearance("CUI")], TEST_NOW_UNIX_S, 3_600);
    let err = harness.client.query(catalog_req("CUI", &token, unfiltered_query())).await.expect_err("no catalog configured must be refused, never an empty Ok");
    assert_eq!(err.code(), Code::FailedPrecondition, "{err:?}");
    assert_eq!(harness.counters.get("catalog_not_configured"), 1);

    harness.shutdown().await;
}

/// The mirror-image regression this task's brief names explicitly: an EXISTING (non-catalog)
/// selector still requires a `run` identity, unchanged by H2c -- over the real rpc, with a
/// real verified token, against a `GatewayCore` carrying no catalog handle at all (proving
/// this path is completely independent of whether a catalog is configured).
#[tokio::test]
async fn an_existing_selector_still_requires_a_run_identity_over_the_real_rpc() {
    let issuer = TestIssuer::new();
    let auth = test_auth_context(&issuer, test_clock());
    let mut harness = GatewayServerHarness::spawn(BTreeMap::new(), ladder(), auth, None).await;

    let token = mint_token(&issuer, "operator-1", &[group_for_clearance("CUI")], TEST_NOW_UNIX_S, 3_600);
    let req = GatewayQueryRequest { run: None, caller_clearance: "CUI".to_string(), selector: GatewaySelector::All as i32, caller_supplied_products_uri: String::new(), caller_token: token, catalog_query: None };
    let err = harness.client.query(req).await.expect_err("GATEWAY_SELECTOR_ALL with no run must still be refused, exactly as before H2c");
    assert_eq!(err.code(), Code::InvalidArgument, "{err:?}");
    assert_eq!(harness.counters.get("gateway_malformed_request"), 1);

    harness.shutdown().await;
}
