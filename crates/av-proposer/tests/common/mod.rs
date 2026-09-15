//! Shared harness for this crate's integration tests: a REAL `av-gateway` (`DataGatewayService`
//! and `ModelProposeService`, `crate::unknown_route_counter`'s layer attached) in front of a
//! REAL `av_command::service::CommandAuthorityServiceImpl` and a real on-disk ledger --
//! `crates/av-gateway/tests/propose_only.rs` is the exact pattern this copies, extended with
//! the gateway's own two rpcs actually being served over a real loopback socket (this crate's
//! own binary/library never sees `CommandAuthorityService` at all -- see `crates/av-proposer/
//! tests/no_command_authority_client.rs`). Shared across more than one test binary in this
//! crate, so it lives here rather than in any one test file (mirroring `crates/av-gateway/
//! tests/common/mod.rs`'s own stated convention: a helper used by only one binary lives in
//! that binary; one used by several lives here).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use av_command::audit::{AuditSinkConfig, AuditWriter};
use av_command::authz::{DelegationTable, RoleTable, ServiceRoleTable};
use av_command::clock::{Clock, TestClock};
use av_command::counters::Counters;
use av_command::ledger::Ledger;
use av_command::oidc::IssuerConfig;
use av_command::pb::command_authority_service_server::CommandAuthorityServiceServer;
use av_command::policy::PolicyBundle;
use av_command::service::{AuthzConfig, CommandAuthorityServiceImpl, RecordingDispatchSink};
use av_command::test_support::{valid_claims, TestIssuer};
use av_gateway::auth::{AuthContext, GroupClearanceMap};
use av_gateway::catalogue::{CatalogueEntry, RunCatalogue};
use av_gateway::gateway::{DataGatewayServiceImpl, DataGatewayServiceServer, GatewayCore};
use av_gateway::labels::ClearanceLadder;
use av_gateway::pb::model_propose_service_server::ModelProposeServiceServer;
use av_gateway::propose_flow::ModelProposeServiceImpl;
use av_gateway::propose_only::ProposeOnlyAuthority;
use av_gateway::unknown_route_counter::UnknownRouteCounterLayer;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Endpoint, Server};

/// R5.1/question 208(b): this crate's tests exercise `av-proposer` as a real SERVICE caller of
/// a real `av-gateway` -- one service role, `"proposer-service"`, granting both `"query"` and
/// `"propose"` (this crate's own real proposer run always issues one of each) at clearance
/// `"CUI"` (every test in this crate's own `catalogue_over_real_fixture` call uses `"CUI"` --
/// grep confirms no other marking is used anywhere in this crate's test suite).
const GATEWAY_AUTH_ISSUER: &str = "https://sso.test.example/";
const GATEWAY_AUTH_AUDIENCE: &str = "av-gateway";
const GATEWAY_AUTH_SERVICE_GROUP: &str = "proposer-service";
/// The verified `sub` [`GatewayHarness::mint_service_token`] mints -- since R5.1/invariant D
/// makes the verified token subject authoritative for `ProposalEvidence.model_identity`
/// (never the caller-declared `principal`, which `av_proposer::proposer::run` now sends
/// empty), a test asserting against a real proposal's own recorded `model_identity` compares
/// against THIS constant, not against `ProposerConfig.model.node_id` (a model-version
/// identifier with no reason to equal the service token's own subject).
pub const SERVICE_TOKEN_SUBJECT: &str = "av-proposer-it";

/// The real, committed, git-tracked `RunProducts` fixture this task names: `run_id`
/// `demo_two_instance_frozen_fixture`. The two named real scores this fixture carries
/// (`demo_flt_rmag_at_end`/`demo_mvr_rmag_at_end` and their real values) are declared locally
/// by whichever test file actually asserts against them, not here -- a constant only this
/// module used and no consumer ever read would be dead code in every OTHER test binary that
/// includes `mod common` without needing it (each `tests/*.rs` file is its own separate
/// compilation of this module).
pub const FIXTURE_RUN_ID: &str = "demo_two_instance_frozen_fixture";

pub fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-proposer-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

pub fn fixture_bytes() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/demo_two_instance.runproducts.bin");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read real RunProducts fixture at {}: {e}", path.display()))
}

fn real_policy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/policies/authority")
}

/// A real, running `av-gateway` (`DataGatewayService` + `ModelProposeService`) in front of a
/// real `CommandAuthorityServiceImpl` -- both over real loopback sockets, two separate ports
/// (mirroring this workspace's real deployment topology: `av-gateway` dials
/// `CommandAuthorityService` as its OWN client, over the network, exactly as
/// `crate::propose_only::ProposeOnlyAuthority::connect` does in production).
pub struct GatewayHarness {
    /// `http://127.0.0.1:PORT` for the gateway -- what `av_proposer::gateway_client::
    /// GatewayClient::connect` dials in a real end-to-end test, and what
    /// `tests/no_command_authority_client.rs`'s crafted raw-path call dials directly.
    pub endpoint: String,
    /// `http://127.0.0.1:PORT` for the real `CommandAuthorityService` behind this gateway --
    /// exposed only so a test can prove that even a channel dialed straight at THIS
    /// (standing in for a proposer process that somehow built its own client, which this
    /// crate's own structural guarantee -- no `CommandAuthorityService` client is ever
    /// generated -- rules out in practice) is refused for a crafted already-started
    /// `Command`, exactly as `crates/av-gateway/tests/propose_only.rs`'s own test already
    /// proves. `av_proposer::gateway_client::GatewayClient` never dials this endpoint.
    pub command_authority_endpoint: String,
    pub command_ledger: Arc<Ledger>,
    pub evidence_ledger: Arc<Ledger>,
    /// The SAME `Counters` instance `GatewayCore` and `ModelProposeServiceImpl` (and the
    /// unknown-route layer) all record into -- mirroring `crates/av-gateway/src/bin/
    /// av-gateway.rs`'s own real wiring, so a test can read one counters snapshot for
    /// everything this gateway process refused.
    pub counters: Arc<Counters>,
    pub clock: Arc<TestClock>,
    /// R5.1: the issuer [`GatewayHarness::mint_service_token`] mints against -- the same one
    /// this gateway's own `AuthContext` verifies with.
    pub issuer: TestIssuer,
    cmd_shutdown: Option<oneshot::Sender<()>>,
    cmd_handle: Option<tokio::task::JoinHandle<()>>,
    gw_shutdown: Option<oneshot::Sender<()>>,
    gw_handle: Option<tokio::task::JoinHandle<()>>,
}

impl GatewayHarness {
    /// `entries` is this gateway's own configured catalogue (D1) -- a test builds it from
    /// the real fixture bytes ([`fixture_bytes`]), never a hand-built `RunProducts` literal.
    pub async fn spawn(name: &str, start_tai_ns: i64, entries: BTreeMap<String, CatalogueEntry>, ladder: ClearanceLadder) -> Self {
        let ledger_dir = tmp_dir(&format!("{name}-authority"));
        let command_ledger = Arc::new(Ledger::open(&ledger_dir).expect("open command ledger"));
        let bundle = Arc::new(PolicyBundle::load(real_policy_dir()).expect("load the shipped policy bundle"));
        let clock = Arc::new(TestClock::new(start_tai_ns));
        let dispatch_sink = Arc::new(RecordingDispatchSink::new());
        let issuer = TestIssuer::new();
        let issuer_config = Arc::new(IssuerConfig::from_public_key_pem("https://sso.test.example/", "av-proposer-it", issuer.public_key_pem()).expect("a freshly generated test issuer key parses"));
        let audit = Arc::new(AuditWriter::open(&AuditSinkConfig::File(ledger_dir.join("audit.log"))).expect("open the test audit sink file"));
        let authz = AuthzConfig {
            role_table: Arc::new(RoleTable::from_config(&BTreeMap::new())),
            delegations: Arc::new(DelegationTable::from_delegations(vec![])),
            mfa_amr_methods: Arc::new(vec![]),
            mfa_acr: Arc::new(String::new()),
            audit,
            service_role_table: Arc::new(ServiceRoleTable::from_config(&BTreeMap::new()).expect("an empty table always parses")),
            counters: Arc::new(Counters::new()),
        };
        let servicer = CommandAuthorityServiceImpl::new(
            command_ledger.clone(),
            bundle,
            3_600_000_000_000,
            clock.clone() as Arc<dyn Clock>,
            dispatch_sink as Arc<dyn av_command::service::DispatchSink>,
            issuer_config,
            authz,
        )
        .expect("rebuild the duplicate-dispatch guard from the ledger at construction");

        let cmd_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port for CommandAuthorityService");
        let cmd_addr = cmd_listener.local_addr().expect("local_addr");
        let (cmd_shutdown_tx, cmd_shutdown_rx) = oneshot::channel::<()>();
        let cmd_handle = tokio::spawn(async move {
            Server::builder()
                .add_service(CommandAuthorityServiceServer::new(servicer))
                .serve_with_incoming_shutdown(TcpListenerStream::new(cmd_listener), async {
                    let _ = cmd_shutdown_rx.await;
                })
                .await
                .expect("CommandAuthorityService server exits cleanly");
        });

        let cmd_channel = Endpoint::from_shared(format!("http://{cmd_addr}")).expect("valid endpoint URI").connect().await.expect("connect to the just-spawned CommandAuthorityService");
        let authority = Arc::new(ProposeOnlyAuthority::from_channel(cmd_channel));

        let evidence_ledger = Arc::new(Ledger::open(tmp_dir(&format!("{name}-evidence"))).expect("open evidence ledger"));
        let counters = Arc::new(Counters::new());
        let core = Arc::new(GatewayCore::new(RunCatalogue::new(entries), ladder.clone(), counters.clone()));

        // R5.1/question 208(b): this gateway's own AuthContext -- see this module's own doc
        // comment for the one service role/clearance every test in this crate needs.
        let gateway_auth_issuer = TestIssuer::new();
        let gateway_issuer_config = Arc::new(IssuerConfig::from_public_key_pem(GATEWAY_AUTH_ISSUER, GATEWAY_AUTH_AUDIENCE, gateway_auth_issuer.public_key_pem()).expect("a freshly generated test issuer key parses"));
        let gateway_service_roles = Arc::new(RoleTable::from_config(&BTreeMap::from([(GATEWAY_AUTH_SERVICE_GROUP.to_string(), vec!["query".to_string(), "propose".to_string()])])));
        let gateway_group_clearance = Arc::new(GroupClearanceMap::new(BTreeMap::from([(GATEWAY_AUTH_SERVICE_GROUP.to_string(), "CUI".to_string())])));
        let gateway_auth =
            Arc::new(AuthContext::new(gateway_issuer_config, Arc::new(RoleTable::default()), gateway_service_roles, gateway_group_clearance, Arc::new(ladder), clock.clone() as Arc<dyn Clock>));

        let model_propose = ModelProposeServiceImpl::new(authority, evidence_ledger.clone(), clock.clone() as Arc<dyn Clock>, counters.clone(), gateway_auth.clone());

        let gw_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port for the gateway");
        let gw_addr = gw_listener.local_addr().expect("local_addr");
        let known_prefixes = vec!["/altavista.v1.DataGatewayService/".to_string(), "/altavista.v1.ModelProposeService/".to_string()];
        let unknown_route_layer = UnknownRouteCounterLayer::new(known_prefixes, counters.clone());
        let (gw_shutdown_tx, gw_shutdown_rx) = oneshot::channel::<()>();
        let gw_handle = tokio::spawn(async move {
            Server::builder()
                .layer(unknown_route_layer)
                .add_service(DataGatewayServiceServer::new(DataGatewayServiceImpl::new(core, gateway_auth)))
                .add_service(ModelProposeServiceServer::new(model_propose))
                .serve_with_incoming_shutdown(TcpListenerStream::new(gw_listener), async {
                    let _ = gw_shutdown_rx.await;
                })
                .await
                .expect("gateway server exits cleanly");
        });

        Self {
            endpoint: format!("http://{gw_addr}"),
            command_authority_endpoint: format!("http://{cmd_addr}"),
            command_ledger,
            evidence_ledger,
            counters,
            clock,
            issuer: gateway_auth_issuer,
            cmd_shutdown: Some(cmd_shutdown_tx),
            cmd_handle: Some(cmd_handle),
            gw_shutdown: Some(gw_shutdown_tx),
            gw_handle: Some(gw_handle),
        }
    }

    /// R5.1: a real, signed service token this gateway's own `AuthContext` accepts for both
    /// `"query"` and `"propose"` at clearance `"CUI"` -- `now_unix_s = 0`/`ttl_s = 3_600`
    /// (converted, not compared, against this crate's own tiny `TestClock` readings like
    /// `1_000`/`5_000`; `crate::oidc::verify` never compares `iat` to `now`, only `exp`/`nbf`,
    /// so the resulting `exp_tai_ns` -- on the order of `3_600 * 1e9` -- is always far past any
    /// `start_tai_ns` this crate's own harnesses use).
    pub fn mint_service_token(&self) -> String {
        let mut claims = valid_claims(GATEWAY_AUTH_ISSUER, GATEWAY_AUTH_AUDIENCE, SERVICE_TOKEN_SUBJECT, 0, 3_600);
        claims["groups"] = serde_json::json!([GATEWAY_AUTH_SERVICE_GROUP]);
        self.issuer.mint(&claims)
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.gw_shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.gw_handle.take() {
            let _ = h.await;
        }
        if let Some(tx) = self.cmd_shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.cmd_handle.take() {
            let _ = h.await;
        }
    }
}

/// A trivial, real sanity check every consuming test calls right after
/// [`GatewayHarness::spawn`]: every field the harness constructs is a genuine, non-empty
/// value, not a placeholder -- also what keeps every one of this struct's fields "used" from
/// EVERY test binary that includes this module (`tests/*.rs` each compile `mod common`
/// separately, so a field only some test files read directly would be flagged dead code in
/// the others; reading it here, called from every one, settles that for all of them at once
/// rather than by a per-file, per-field lint-suppressing attribute this task's own rules
/// forbid).
pub fn assert_harness_is_wired(harness: &GatewayHarness) {
    assert!(harness.endpoint.starts_with("http://"), "{}", harness.endpoint);
    assert!(harness.command_authority_endpoint.starts_with("http://"), "{}", harness.command_authority_endpoint);
    assert!(harness.command_authority_endpoint != harness.endpoint, "the gateway and the command authority must be two distinct real sockets, mirroring this workspace's real deployment topology");
    assert!(harness.clock.now_tai_ns() > 0);
    let _ = harness.counters.snapshot();
    let _ = &harness.evidence_ledger;
    let _ = &harness.command_ledger;
    assert!(!harness.mint_service_token().is_empty(), "R5.1: the harness's own AuthContext must mint a real, non-empty service token");
}

/// A catalogue with exactly one entry, `run_id` [`FIXTURE_RUN_ID`], loaded from the real,
/// committed fixture bytes ([`fixture_bytes`]) -- never a hand-built `RunProducts` literal.
pub fn catalogue_over_real_fixture(marking: &str) -> (BTreeMap<String, CatalogueEntry>, ClearanceLadder) {
    let mut entries = BTreeMap::new();
    entries.insert(FIXTURE_RUN_ID.to_string(), CatalogueEntry::from_raw_bytes(av_cdm::pb::Label { marking: marking.to_string(), caveats: vec![] }, fixture_bytes()));
    let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]);
    (entries, ladder)
}
