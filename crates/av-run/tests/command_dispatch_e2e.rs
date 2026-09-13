//! A3.2 end-to-end acceptance (`docs/aiplane-plan.md` milestone A3): a real
//! `CommandAuthorityService` (a real loopback gRPC socket, a real minted OIDC token) driving
//! a live `av_kernel::drm::execute` run of the `demo_attitude_command` DRM through
//! `av_run::command_adapter::KernelCommandAdapter`.
//!
//! # Threading, and why this file's tests are plain `#[test]`, not `#[tokio::test]`
//!
//! `KernelCommandAdapter::report`'s own `block_on` (`src/command_adapter.rs`'s module doc)
//! is refused by `tokio` when called from a thread that is itself currently being polled as
//! part of the same runtime -- and `av_kernel::drm::execute` is a long, synchronous,
//! GMAT-singleton-holding call that this file must run somewhere. Rather than push it onto
//! `tokio::task::spawn_blocking` (which would need `Gmat`/`gmat_sys::EngineGuard` to be
//! `Send`, unproven and not needed), every test here is a plain, synchronous `#[test]` that
//! builds its own `tokio::runtime::Runtime` and drives every gRPC call through `rt.block_on`
//! from the *original* OS thread -- a thread the runtime itself never treats as one of its
//! own worker threads, so calling back into `rt.handle().block_on` from inside `execute`
//! (via the adapter's `report`) is exactly the supported "synchronous caller, async runtime"
//! pattern, not a nested `block_on`. The server's own request handling still runs
//! concurrently on the runtime's other worker threads throughout.
//!
//! # No network at test time (question 154); no test mutates the process environment (199)
//!
//! Every server here binds `127.0.0.1:0` (an OS-assigned ephemeral port) and is spawned
//! in-process by the very test that talks to it -- mirrors `crates/av-command/tests/
//! grpc_service.rs`'s own module doc, which makes the identical record. No test calls
//! `std::env::set_var`/`remove_var`; every clock is a `TestClock` this test constructs and
//! never advances by sleeping.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use av_cdm::pb::{
    AckLevel, Command, CommandProposal, CommandState, DesignReferenceMission, EventKind, PortDirection, PortTrafficLog, Provenance,
    SosConfiguration, SystemDefinition,
};
use av_command::audit::{AuditSinkConfig, AuditWriter};
use av_command::authz::{DelegationTable, RoleTable, ServiceRoleTable};
use av_command::clock::{Clock, TestClock};
use av_command::counters::Counters;
use av_command::ledger::Ledger;
use av_command::oidc::IssuerConfig;
use av_command::pb::command_authority_service_client::CommandAuthorityServiceClient;
use av_command::pb::command_authority_service_server::CommandAuthorityServiceServer;
use av_command::policy::PolicyBundle;
use av_command::service::{AuthzConfig, CommandAuthorityServiceImpl, DispatchSink};
use av_command::test_support::{claims_with_roles_and_mfa, valid_claims, RoleAndMfaClaims, TestIssuer};
use av_kernel::drm::command_source::{pack_double_value, ExternalCommandSource};
use av_kernel::drm::{execute, schema, RunConfig};
use av_run::command_adapter::KernelCommandAdapter;
use gmat_sys::Gmat;
use prost::Message as _;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, Endpoint, Server};
use tonic::Code;

const TEST_ISSUER: &str = "https://sso.test.example/";
const TEST_AUDIENCE: &str = "av-command";
const TOKEN_NOW_UNIX_S: i64 = 1_760_000_000;
const TOKEN_TTL_S: i64 = 3_600;
/// `drms/demo_attitude_command.drm.yaml`'s own declared `scenario.start_tai_ns`.
const START_TAI_NS: i64 = 1_767_225_637_000_000_000;

fn real_policy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/policies/authority")
}

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-run-command-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn default_roles() -> BTreeMap<String, Vec<String>> {
    let mut roles = BTreeMap::new();
    roles.insert("operators".to_string(), vec!["mode".to_string()]);
    roles
}

/// R3.1: the service-role table this test's `TestService` grants -- `"ground-segment"` may
/// call all four service RPCs. Disjoint from [`default_roles`]'s `"operators"` by
/// construction, matching `av_command::authz::check_service_roles_disjoint`'s own requirement.
fn default_service_roles() -> BTreeMap<String, Vec<String>> {
    let mut roles = BTreeMap::new();
    roles.insert("ground-segment".to_string(), vec!["dispatch".to_string(), "ack".to_string(), "expire".to_string(), "fail".to_string()]);
    roles
}

// -- DRM bundle loading, mirrors crates/av-kernel/tests/drm_attitude_command.rs exactly (a
// separate crate's own tests/ directory cannot import that file's helpers) -----------------

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn read_drm_file(name: &str) -> String {
    std::fs::read_to_string(drms_path(name)).unwrap_or_else(|e| panic!("reading drms/{name}: {e}"))
}

fn load_system(stem: &str) -> SystemDefinition {
    schema::parse_system_definition_yaml(&read_drm_file(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"))
}

fn load_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&read_drm_file("demo_attitude_command.drm.yaml")).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&read_drm_file("demo_attitude_command.sos.yaml")).expect("SosConfiguration parses");
    let truth = load_system("demo_attitude_control_truth");
    let star = load_system("demo_attitude_control_startracker");
    let imu = load_system("demo_attitude_control_imu");
    let controller = load_system("demo_attitude_command_controller");
    let ground = load_system("demo_ground_command_ground");
    let mut systems = BTreeMap::new();
    systems.insert(truth.id.clone(), truth);
    systems.insert(star.id.clone(), star);
    systems.insert(imu.id.clone(), imu);
    systems.insert(controller.id.clone(), controller);
    systems.insert(ground.id.clone(), ground);
    (drm, sos, systems)
}

fn mode_command(id: &str, value: f64, not_before_tai_ns: i64, deadline_tai_ns: i64) -> Command {
    Command {
        id: id.to_string(),
        idempotency_key: format!("{id}-key"),
        entity_id: "controller".to_string(),
        command_class: "mode".to_string(),
        hazardous: false,
        payload: Some(pack_double_value(value)),
        deadline_tai_ns,
        not_before_tai_ns,
        provenance: Some(Provenance { attributes: BTreeMap::from([("from".to_string(), "ground".to_string())]), ..Default::default() }),
        ..Default::default()
    }
}

fn pointing_error_at_end(products: &av_kernel::drm::RunProducts) -> f64 {
    products.scores.get("pointing_error_end").unwrap_or_else(|| panic!("demo_attitude_command.drm.yaml declares measure \"pointing_error_end\"")).value
}

// -- The real service, standing up exactly like crates/av-command/tests/grpc_service.rs's
// own TestServer, plus a KernelCommandAdapter wired as its DispatchSink -----------------------

struct TestService {
    client: CommandAuthorityServiceClient<Channel>,
    adapter: Arc<KernelCommandAdapter>,
    ledger_dir: PathBuf,
    issuer: TestIssuer,
    shutdown_tx: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl TestService {
    /// Binds the listener and builds a *lazy* `Channel` to it (`Endpoint::connect_lazy`,
    /// never `.connect().await`) before the server task is even spawned -- the one way to
    /// hand `KernelCommandAdapter` a client pointed at this service's own address without a
    /// bind-before-listen chicken-and-egg (the adapter's client needs an address; the
    /// service needs the adapter, as its `DispatchSink`, before it can be constructed at
    /// all). A lazy channel defers the actual TCP connect to the first real RPC, by which
    /// point the server below is already serving.
    async fn spawn(name: &str, start_tai_ns: i64) -> Self {
        let ledger_dir = tmp_dir(name);
        let ledger = Arc::new(Ledger::open(&ledger_dir).expect("open ledger"));
        let bundle = Arc::new(PolicyBundle::load(real_policy_dir()).expect("load the shipped policy bundle"));
        let clock = Arc::new(TestClock::new(start_tai_ns));
        let issuer = TestIssuer::new();
        let issuer_config =
            Arc::new(IssuerConfig::from_public_key_pem(TEST_ISSUER, TEST_AUDIENCE, issuer.public_key_pem()).expect("a freshly-generated test issuer key parses"));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port");
        let addr: SocketAddr = listener.local_addr().expect("local_addr");
        let channel = Endpoint::from_shared(format!("http://{addr}")).expect("valid endpoint URI").connect_lazy();

        // R3.1: the adapter's own service_token, minted under a service role
        // (default_service_roles's "ground-segment") the servicer's own service_role_table
        // below actually grants -- see crates/av-command/src/service.rs's module doc, "R3.1: a
        // service principal, verified like a human token". KERNEL_PRINCIPAL ("kernel" --
        // av_run::command_adapter's own module doc) is the label the adapter declares on
        // every Ack/Expire/Fail, so this token's own verified sub must equal it or every one
        // of those calls would be refused PrincipalMismatch.
        let adapter_service_token =
            issuer.mint(&claims_with_roles_and_mfa(TEST_ISSUER, TEST_AUDIENCE, av_run::command_adapter::KERNEL_PRINCIPAL, TOKEN_NOW_UNIX_S, TOKEN_TTL_S, RoleAndMfaClaims { groups: &["ground-segment"], amr: &[], acr: "" }));
        let adapter = Arc::new(KernelCommandAdapter::new(CommandAuthorityServiceClient::new(channel.clone()), adapter_service_token, tokio::runtime::Handle::current()));

        let audit_path = ledger_dir.join("audit.log");
        let audit = Arc::new(AuditWriter::open(&AuditSinkConfig::File(audit_path.clone())).expect("open the test audit sink file"));
        let authz = AuthzConfig {
            role_table: Arc::new(RoleTable::from_config(&default_roles())),
            delegations: Arc::new(DelegationTable::from_delegations(vec![])),
            mfa_amr_methods: Arc::new(vec![]),
            mfa_acr: Arc::new(String::new()),
            audit,
            service_role_table: Arc::new(ServiceRoleTable::from_config(&default_service_roles()).expect("this file's own service-role fixture always uses recognized rpc names")),
            counters: Arc::new(Counters::new()),
        };

        let servicer = CommandAuthorityServiceImpl::new(
            ledger,
            bundle,
            3_600_000_000_000,
            clock.clone() as Arc<dyn Clock>,
            adapter.clone() as Arc<dyn DispatchSink>,
            issuer_config,
            authz,
        )
        .expect("rebuild the duplicate-dispatch guard from the ledger at construction");

        let incoming = TcpListenerStream::new(listener);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            Server::builder()
                .add_service(CommandAuthorityServiceServer::new(servicer))
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("server exits cleanly");
        });

        let client = CommandAuthorityServiceClient::new(channel);
        Self { client, adapter, ledger_dir, issuer, shutdown_tx, handle }
    }

    fn mint(&self, sub: &str) -> String {
        self.issuer.mint(&valid_claims(TEST_ISSUER, TEST_AUDIENCE, sub, TOKEN_NOW_UNIX_S, TOKEN_TTL_S))
    }

    /// R3.1: mints a real, fully-valid service token carrying `groups` -- for this test's own
    /// direct `Dispatch` RPC calls (`Self::client`, never the adapter's own client, which is
    /// wired to a separate, fixed service_token at construction -- see `Self::spawn`'s own
    /// comment).
    fn mint_service(&self, sub: &str, groups: &[&str]) -> String {
        self.issuer.mint(&claims_with_roles_and_mfa(TEST_ISSUER, TEST_AUDIENCE, sub, TOKEN_NOW_UNIX_S, TOKEN_TTL_S, RoleAndMfaClaims { groups, amr: &[], acr: "" }))
    }

    async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        self.handle.await.expect("server task joins cleanly at test end");
        let _ = std::fs::remove_dir_all(&self.ledger_dir);
    }
}

fn propose_request(command: Command, principal: &str) -> av_cdm::pb::ProposeRequest {
    av_cdm::pb::ProposeRequest { proposal: Some(CommandProposal { command: Some(command), rationale: "av-run e2e test".to_string(), evidence_ids: vec![] }), principal: principal.to_string() }
}

/// Drives one command through `Propose`/`Check`/`Authorize`/`Dispatch` over the real gRPC
/// client, exactly the literal `Command` this test built -- returns its id (the caller
/// already knows it, but returning it keeps every call site symmetrical).
async fn propose_check_authorize_dispatch(service: &mut TestService, command: Command) {
    let id = command.id.clone();
    service.client.propose(propose_request(command, "model-x")).await.expect("Propose");
    let checked = service.client.check(av_cdm::pb::CheckRequest { command_id: id.clone() }).await.expect("Check").into_inner();
    assert_eq!(checked.command.expect("command present").state, CommandState::Checked as i32, "the shipped policy admits the \"mode\" class");
    let token = service.mint("operator-1");
    service
        .client
        .authorize(av_cdm::pb::AuthorizeRequest { command_id: id.clone(), principal_token: token, delegation_id: String::new() })
        .await
        .expect("Authorize");
    let dispatch_token = service.mint_service("ground-segment-1", &["ground-segment"]);
    service.client.dispatch(av_cdm::pb::DispatchRequest { command_id: id, service_token: dispatch_token }).await.expect("Dispatch");
}

fn read_port_traffic_log(path: &Path) -> PortTrafficLog {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    PortTrafficLog::decode(bytes.as_slice()).expect("decode PortTrafficLog")
}

/// **Acceptance evidence 1**: the demo attitude DRM's mode command proposed, checked,
/// authorized and dispatched over the real gRPC surface, executing on the asset through the
/// real kernel telecommand path, acking at all three real levels, with the ledger showing
/// the full trail and `VerifyLedger` clean -- plus the physical effect (SAFE changes the
/// pointing error against a REGULATE baseline), never merely an event-name check.
#[test]
fn full_command_trail_through_the_real_service_and_kernel_acks_at_all_three_levels_with_a_physical_effect() {
    let rt = tokio::runtime::Runtime::new().expect("build a tokio runtime");
    let mut service = rt.block_on(TestService::spawn("full-trail", 1_000));

    let safe_at = START_TAI_NS + 10_000_000_000;
    rt.block_on(propose_check_authorize_dispatch(&mut service, mode_command("safe1", 0.0, safe_at, 0)));

    // -- Baseline: no command source at all -- the controller stays REGULATE its whole life.
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let baseline = execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "av-run-e2e-baseline".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: None })
        .expect("baseline run executes");
    let baseline_error = pointing_error_at_end(&baseline);

    // -- The live kernel run, driven by the adapter's own queue (the literal Command the
    // service just dispatched) -- this call is what makes `KernelCommandAdapter::report`'s
    // own `block_on` calls happen, on this same OS thread (see this file's own module doc).
    let commanded = execute(RunConfig {
        gmat: &gmat,
        drm: &drm,
        sos: &sos,
        systems: &systems,
        run_id: "av-run-e2e-commanded".to_string(),
        error_mode: Default::default(),
        products_dir: None,
        replay: None,
        command_source: Some(service.adapter.as_ref()),
    })
    .expect("commanded run executes");
    let commanded_error = pointing_error_at_end(&commanded);

    // -- The kernel's own event trail agrees with what was actually dispatched. --
    let transitions: Vec<_> = commanded.events.iter().filter(|e| e.kind == EventKind::CommandTransition as i32 && e.reference_id == "safe1").collect();
    let states: Vec<&str> = transitions.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(states, vec!["COMMAND_STATE_PROPOSED", "COMMAND_STATE_CHECKED", "COMMAND_STATE_AUTHORIZED", "COMMAND_STATE_DISPATCHED", "COMMAND_STATE_ACKED"], "{transitions:#?}");

    // -- The ledger, over the real gRPC surface: PROPOSED, CHECKED, AUTHORIZED, DISPATCHED,
    // then three ACKED transitions with strictly increasing ack levels, and VerifyLedger
    // clean. Queried after the kernel run, so this proves the adapter's own report() calls
    // actually landed. --
    let queried = rt
        .block_on(service.client.query(av_cdm::pb::QueryRequest { selector: Some(av_cdm::pb::query_request::Selector::CommandId("safe1".to_string())) }))
        .expect("Query")
        .into_inner();
    let final_command = queried.commands.first().expect("safe1 must be queryable").clone();
    assert_eq!(final_command.state, CommandState::Acked as i32, "{final_command:#?}");
    let ledger_states: Vec<CommandState> = final_command.transitions.iter().map(|t| CommandState::try_from(t.state).unwrap()).collect();
    assert_eq!(
        ledger_states,
        vec![CommandState::Proposed, CommandState::Checked, CommandState::Authorized, CommandState::Dispatched, CommandState::Acked, CommandState::Acked, CommandState::Acked],
        "the ledger must show DISPATCHED, then three ACKED transitions (A3.2/D2's own tenth edge) -- one per real ack level"
    );
    let ack_levels: Vec<i32> = final_command.transitions[4..].iter().map(|t| t.ack_level).collect();
    assert_eq!(ack_levels, vec![AckLevel::Edge as i32, AckLevel::AssetReceived as i32, AckLevel::AssetExecuted as i32], "ack levels must be strictly increasing, in this exact real order");

    let verify = rt.block_on(service.client.verify_ledger(av_cdm::pb::VerifyLedgerRequest { partition: "controller".to_string() })).expect("VerifyLedger").into_inner();
    assert!(verify.ok, "{verify:?}");

    // -- The physical effect: SAFE measurably changes the pointing-error trajectory versus
    // the identical run with no command at all -- not merely an event-name check. --
    eprintln!("[command_dispatch_e2e] baseline pointing_error_rad@end={baseline_error}, SAFE-commanded pointing_error_rad@end={commanded_error}");
    assert!((commanded_error - baseline_error).abs() > 1e-4, "baseline={baseline_error}, commanded={commanded_error}");
    assert!(commanded_error > baseline_error, "SAFE (zero torque) must leave a larger residual pointing error; baseline={baseline_error}, commanded={commanded_error}");

    rt.block_on(service.shutdown());
}

/// **Acceptance evidence 2**: a deadline that expires undispatched, end to end. The service
/// dispatches the command over real gRPC (`AUTHORIZED -> DISPATCHED` on the ledger, at the
/// service's own clock reading); the kernel, at its own scenario-start epoch, finds the
/// deadline already passed and reports `CommandOutcome::Expired`, which the adapter turns
/// into a real `Expire` RPC -- `DISPATCHED -> EXPIRED` on the ledger, with the deadline and
/// the kernel epoch in the reason text -- and asserts against the *recorded port traffic*
/// that no command frame ever reached the wire, not merely an absent event.
#[test]
fn a_deadline_expires_undispatched_end_to_end() {
    let rt = tokio::runtime::Runtime::new().expect("build a tokio runtime");
    let mut service = rt.block_on(TestService::spawn("deadline-expiry", 1_000));

    // deadline_tai_ns == not_before_tai_ns == the run's own t0: decide_disposition's own
    // boundary convention (`>=`) makes this EXPIRED at the very first epoch dispatch could
    // ever have been attempted.
    rt.block_on(propose_check_authorize_dispatch(&mut service, mode_command("expire1", 0.0, 0, START_TAI_NS)));

    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products_dir = tmp_dir("deadline-expiry-products");
    let products = execute(RunConfig {
        gmat: &gmat,
        drm: &drm,
        sos: &sos,
        systems: &systems,
        run_id: "av-run-e2e-deadline".to_string(),
        error_mode: Default::default(),
        products_dir: Some(products_dir.clone()),
        replay: None,
        command_source: Some(service.adapter.as_ref()),
    })
    .expect("a run with an already-expired command must still execute cleanly");

    // No COMMAND_STATE_DISPATCHED/ACKED event for this command anywhere in the kernel's own
    // event trail -- it never entered the dispatch loop at all.
    let transitions: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::CommandTransition as i32 && e.reference_id == "expire1").collect();
    assert!(transitions.is_empty(), "an expired-before-dispatch command must produce no kernel-side CommandTransition event at all: {transitions:#?}");

    // The recorded port traffic itself: no OUT frame on the ground instance's own dispatch
    // port, and no IN frame on the controller's own mode_in port -- the frame genuinely
    // never reached the wire, not merely "no event says so".
    let log = read_port_traffic_log(&products_dir.join("port_traffic.pb"));
    let dispatch_frames: Vec<_> = log.records.iter().filter(|r| r.instance == "ground" && r.port == "cmd_out" && r.direction == PortDirection::Out as i32).collect();
    assert!(dispatch_frames.is_empty(), "no cmd_out OUT frame must exist for an expired-before-dispatch command: {dispatch_frames:#?}");
    let mode_in_frames: Vec<_> = log.records.iter().filter(|r| r.instance == "controller" && r.port == "mode_in").collect();
    assert!(mode_in_frames.is_empty(), "no mode_in frame of either direction must exist for an expired-before-dispatch command: {mode_in_frames:#?}");

    // The ledger, over the real gRPC surface: DISPATCHED -> EXPIRED, with the deadline and
    // the kernel epoch named in the reason text.
    let queried = rt
        .block_on(service.client.query(av_cdm::pb::QueryRequest { selector: Some(av_cdm::pb::query_request::Selector::CommandId("expire1".to_string())) }))
        .expect("Query")
        .into_inner();
    let final_command = queried.commands.first().expect("expire1 must be queryable").clone();
    assert_eq!(final_command.state, CommandState::Expired as i32, "{final_command:#?}");
    let last = final_command.transitions.last().expect("at least one transition");
    assert_eq!(CommandState::try_from(last.state).unwrap(), CommandState::Expired);
    assert!(last.reason.contains(&format!("deadline_tai_ns={START_TAI_NS}")), "reason must name the deadline: {}", last.reason);
    assert!(last.reason.contains(&format!("kernel_epoch_tai_ns={START_TAI_NS}")), "reason must name the kernel epoch it was evaluated at: {}", last.reason);

    let _ = std::fs::remove_dir_all(&products_dir);
    rt.block_on(service.shutdown());
}

/// **Acceptance evidence 3 (the service's own guard)**: a duplicate `idempotency_key` is
/// refused on the *second* `Dispatch` RPC for the same command, over the real gRPC surface,
/// and the ledger shows only one `DISPATCHED` record. This is the *service-layer* guard
/// (`CommandAuthorityServiceImpl::dispatch`'s own `dispatched_idempotency_keys` set) --
/// already covered end to end by `crates/av-command/tests/grpc_service.rs`'s own
/// `dispatch_refuses_a_duplicate_idempotency_key_and_appends_no_second_ledger_record`; this
/// test exercises the identical guard through this crate's own adapter-backed service to
/// prove the adapter changes nothing about it.
///
/// **The kernel's own guard** (`av_kernel::drm::command_source::DuplicateIdempotencyKeyTracker`
/// -- "a source handing the kernel two commands with one key dispatches one frame") is a
/// different guard at a different layer, and is **not** reachable through this same
/// service+adapter pipeline: the service's own guard above already refuses a *second*
/// `Dispatch` RPC for a repeated `idempotency_key` before that second command could ever
/// reach the adapter's queue at all -- there is no way, through one service instance, to
/// hand the kernel two DIFFERENT commands sharing one key. That guard is exercised directly
/// against `RecordingCommandSource` in `crates/av-kernel/tests/external_command_source.rs`
/// (A3.1, unmodified by A3.2) -- see this task's own final report for that test's exact,
/// pasted output.
#[test]
fn a_duplicate_idempotency_key_is_refused_by_the_service_on_a_second_dispatch_rpc() {
    let rt = tokio::runtime::Runtime::new().expect("build a tokio runtime");
    let mut service = rt.block_on(TestService::spawn("duplicate-key", 1_000));

    let command = mode_command("dup1", 0.0, START_TAI_NS + 1_000_000_000, 0);
    rt.block_on(async {
        service.client.propose(propose_request(command.clone(), "model-x")).await.expect("Propose");
        service.client.check(av_cdm::pb::CheckRequest { command_id: "dup1".to_string() }).await.expect("Check");
        let token = service.mint("operator-1");
        service.client.authorize(av_cdm::pb::AuthorizeRequest { command_id: "dup1".to_string(), principal_token: token, delegation_id: String::new() }).await.expect("Authorize");
        let dispatch_token1 = service.mint_service("ground-segment-1", &["ground-segment"]);
        service.client.dispatch(av_cdm::pb::DispatchRequest { command_id: "dup1".to_string(), service_token: dispatch_token1 }).await.expect("first Dispatch must succeed");

        // A second Propose/Check/Authorize for a fresh command id, but the identical
        // idempotency_key -- the second Dispatch must be refused ALREADY_EXISTS.
        let mut second = mode_command("dup2", 0.0, START_TAI_NS + 2_000_000_000, 0);
        second.idempotency_key = command.idempotency_key.clone();
        service.client.propose(propose_request(second, "model-x")).await.expect("Propose");
        service.client.check(av_cdm::pb::CheckRequest { command_id: "dup2".to_string() }).await.expect("Check");
        let token = service.mint("operator-1");
        service.client.authorize(av_cdm::pb::AuthorizeRequest { command_id: "dup2".to_string(), principal_token: token, delegation_id: String::new() }).await.expect("Authorize");
        let dispatch_token2 = service.mint_service("ground-segment-1", &["ground-segment"]);
        let err = service.client.dispatch(av_cdm::pb::DispatchRequest { command_id: "dup2".to_string(), service_token: dispatch_token2 }).await.expect_err("a second Dispatch sharing dup1's own idempotency_key must be refused");
        assert_eq!(err.code(), Code::AlreadyExists, "{err:?}");
    });

    // Only ONE Command ever reached the adapter's own dispatch queue -- a plain synchronous
    // call, no `block_on` needed (`ExternalCommandSource::poll` never awaits anything).
    let queued = service.adapter.poll(0);
    assert_eq!(queued.len(), 1, "the refused second Dispatch must never have reached the DispatchSink at all: {queued:#?}");
    assert_eq!(queued[0].id, "dup1");

    rt.block_on(service.shutdown());
}
