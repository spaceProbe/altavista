//! Integration tests for `altavista.v1.CommandAuthorityService` (A1.3, `crates/av-command/
//! src/service.rs`): drive the **real service over a real loopback socket with a real tonic
//! client** (`tonic`'s `"channel"` feature, a `[dev-dependencies]` entry -- see `Cargo.toml`'s
//! and `build.rs`'s own comments for why that is the cleanest way to get a usable generated
//! client stub without this crate's production `tonic` dependency ever gaining the
//! `"channel"`/`"transport"` feature). Every test binds `127.0.0.1:0` (an OS-assigned
//! ephemeral port), never a fixed one, and shuts its server task down cleanly before
//! returning ([`TestServer::shutdown`]) -- no test here leaves a listening task running past
//! its own end.
//!
//! # No network at test time (question 154)
//!
//! Binding and connecting to `127.0.0.1` here is **not** "network at test time" -- question
//! 154's rule is about a test reaching *outside* the host (an image pull, a real DNS lookup,
//! a call to an external service); a loopback socket never leaves the host's own kernel
//! network stack, and every server this file spawns is spawned in-process by the very test
//! that connects to it. Recorded explicitly here (mirroring `crates/av-command/src/
//! policy.rs`'s module doc, which makes the identical record for its own, narrower "no
//! network builtin" claim) so a later reader does not have to re-derive it.
//!
//! # No test mutates the process environment (question 199)
//!
//! Every test constructs its own [`av_command::clock::TestClock`], ledger temp directory and
//! [`av_command::policy::PolicyBundle`] directly; none of them calls `std::env::set_var`/
//! `remove_var`, and no test sleeps -- every "later" is a fresh `TestClock` value or an
//! explicit `advance`/`set` (`TestServer::spawn`'s own clock is exposed on the struct for a
//! test that needs one, though none of the tests below need to move it forward: the
//! `CommandTransition.tai_ns` at each edge only needs to be *some* deterministic value, never
//! a wall-clock read).
//!
//! # Reading/writing the raw ledger file
//!
//! [`ledger_file_path`]/[`read_ledger_records`]/[`write_ledger_records`] below recompute the
//! on-disk partition filename and framing exactly as `crates/av-command/src/ledger.rs`'s
//! module doc documents it (SHA-256 hex of the partition name plus `.ledger`; a big-endian
//! `u32` length prefix then that many `LedgerRecord` bytes) rather than calling any private
//! function of that module -- this file is a separate crate (an integration test) and can
//! only see `av_command`'s public API, so it exercises the *documented* on-disk contract the
//! same way an external auditor would, not an internal shortcut.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use av_cdm::pb::{
    query_request::Selector, AckLevel, AckRequest, AuthorizeRequest, CheckRequest, Command, CommandProposal, CommandState,
    Delegation, DispatchRequest, LedgerRecord, ProposeRequest, QueryByEntity, QueryRequest, VerifyLedgerRequest,
};
use av_cdm::time::Tai;
use av_command::audit::{AuditSinkConfig, AuditWriter};
use av_command::authz::{DelegationTable, RoleTable, WILDCARD};
use av_command::clock::{Clock, TestClock};
use av_command::ledger::Ledger;
use av_command::oidc::IssuerConfig;
use av_command::pb::command_authority_service_client::CommandAuthorityServiceClient;
use av_command::pb::command_authority_service_server::CommandAuthorityServiceServer;
use av_command::policy::PolicyBundle;
use av_command::service::{
    resolve_loopback_bind_address, AuthzConfig, BindAddressError, CommandAuthorityServiceImpl, DispatchSink, RecordingDispatchSink,
};
use av_command::test_support::{claims_with_roles_and_mfa, valid_claims, RoleAndMfaClaims, TestIssuer};
use prost::Message as _;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, Endpoint, Server};
use tonic::Code;

/// A2.1: the OIDC issuer/audience every `TestServer` below configures its
/// `CommandAuthorityServiceImpl` with. Arbitrary strings -- no production meaning, just
/// something `TestServer::mint` and `TestServer::spawn_over`'s `IssuerConfig` agree on.
const TEST_ISSUER: &str = "https://sso.test.example/";
const TEST_AUDIENCE: &str = "av-command";
/// A fixed, arbitrary Unix-seconds "now" for every minted token's `iat`/`exp` -- unrelated to
/// (and always vastly larger than) the small `start_tai_ns` values (e.g. `1_000`) this file's
/// `TestServer::spawn` calls use for the *service's own* `TestClock`, so a minted token's
/// `exp_tai_ns` is always far in this service's own clock's "future" and never spuriously
/// expired -- see `av_command::test_support::valid_claims`'s own doc for why no test here
/// sets `nbf` at all (a default `nbf` would spuriously trigger `NotYetValid` against those
/// same small `TestClock` values).
const TOKEN_NOW_UNIX_S: i64 = 1_760_000_000;
const TOKEN_TTL_S: i64 = 3_600;

/// The real policy directory `profiles/execution.yaml`'s `authority.policy_dir` names,
/// resolved relative to this crate's manifest (matches `tests/policy_fixture.rs`'s identical
/// helper) -- these tests run the shipped `command.rego`, not a private fixture copy.
fn real_policy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/policies/authority")
}

fn tmp_ledger_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-command-grpc-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// See this file's module doc: recomputes `crate::ledger`'s documented (not private)
/// filename scheme.
fn ledger_file_path(ledger_dir: &Path, partition: &str) -> PathBuf {
    let digest = openssl::sha::sha256(partition.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    ledger_dir.join(format!("{hex}.ledger"))
}

fn read_ledger_records(ledger_dir: &Path, partition: &str) -> Vec<LedgerRecord> {
    let bytes = std::fs::read(ledger_file_path(ledger_dir, partition)).expect("read partition file");
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        out.push(LedgerRecord::decode(&bytes[offset..offset + len]).expect("decode LedgerRecord frame"));
        offset += len;
    }
    out
}

fn write_ledger_records(ledger_dir: &Path, partition: &str, records: &[LedgerRecord]) {
    let mut bytes = Vec::new();
    for r in records {
        let body = r.encode_to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&body);
    }
    std::fs::write(ledger_file_path(ledger_dir, partition), bytes).expect("write partition file");
}

fn base_command(id: &str, entity_id: &str, command_class: &str, idempotency_key: &str) -> Command {
    Command {
        id: id.to_string(),
        idempotency_key: idempotency_key.to_string(),
        entity_id: entity_id.to_string(),
        command_class: command_class.to_string(),
        ..Command::default()
    }
}

fn propose_request(command: Command, principal: &str) -> ProposeRequest {
    ProposeRequest { proposal: Some(CommandProposal { command: Some(command), rationale: "integration test".to_string(), evidence_ids: vec![] }), principal: principal.to_string() }
}

/// A wildcard delegation covering every class/entity for `subject`, expiring far in the
/// future -- used only to keep this file's *pre-A2.2* tests (which pass a non-empty
/// `delegation_id` purely to exercise the field being carried, not to test delegation
/// enforcement itself) authorizing exactly as they did before A2.2 added real delegation
/// enforcement. Tests that exercise delegation enforcement itself
/// ([`delegation_expiry_is_refused_at_the_boundary_second_over_the_wire`],
/// [`a_delegation_grants_a_class_the_role_does_not_and_reaches_the_ledger`]) build their own,
/// narrower delegation instead of relying on this one.
fn wildcard_delegation(id: &str, subject: &str) -> Delegation {
    Delegation {
        id: id.to_string(),
        subject: subject.to_string(),
        command_classes: vec![WILDCARD.to_string()],
        entity_ids: vec![WILDCARD.to_string()],
        not_before_tai_ns: 0,
        expires_tai_ns: i64::MAX,
        granted_by: "test-fixture".to_string(),
        reason: "backward-compatible test fixture delegation".to_string(),
    }
}

/// A2.2's default test role table: `"operators"` grants `"mode"`, `"burn-authorizers"` grants
/// `"mode"`/`"burn"` -- matching `av_command::test_support::valid_claims`'s own default
/// `groups` (`["operators", "burn-authorizers"]`), so every pre-A2.2 test in this file (which
/// mints tokens via [`TestServer::mint`], always using those default claims) keeps
/// authorizing its `"mode"`-class commands exactly as before, now through a real role check
/// rather than an unconditional pass.
fn default_roles() -> BTreeMap<String, Vec<String>> {
    let mut roles = BTreeMap::new();
    roles.insert("operators".to_string(), vec!["mode".to_string()]);
    roles.insert("burn-authorizers".to_string(), vec!["mode".to_string(), "burn".to_string()]);
    roles
}

/// A running `CommandAuthorityServiceImpl` behind a real loopback socket, plus everything a
/// test needs to inspect what happened: the ledger directory (for
/// [`read_ledger_records`]/[`write_ledger_records`]), the [`RecordingDispatchSink`] (A3's
/// seam -- see `crate::service`'s module doc), the shared clock, and (A2.2) the audit sink
/// file path. [`Self::shutdown`] must be called at the end of every test that constructs one.
struct TestServer {
    client: CommandAuthorityServiceClient<Channel>,
    ledger_dir: PathBuf,
    clock: Arc<TestClock>,
    dispatch_sink: Arc<RecordingDispatchSink>,
    /// A2.1: the same local test issuer the server's own `IssuerConfig` was built from --
    /// [`Self::mint`] mints tokens this server's `Authorize` will actually verify.
    issuer: TestIssuer,
    /// A2.2: the file this server's `AuditWriter` was configured with -- read back by
    /// this file's audit-line tests.
    audit_path: PathBuf,
    shutdown_tx: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    /// Spawns over a **fresh** ledger directory, named `name` (wiped first if it somehow
    /// already exists -- see [`tmp_ledger_dir`]), with the default role table
    /// ([`default_roles`]) and two wildcard fixture delegations (`"delegation-1"` for
    /// `"operator-1"`, `"delegation-9"` for `"astronaut-jane"` -- see [`wildcard_delegation`]),
    /// no MFA methods configured. The overwhelming majority of tests want this.
    async fn spawn(name: &str, start_tai_ns: i64) -> Self {
        Self::spawn_over(tmp_ledger_dir(name), start_tai_ns).await
    }

    /// As [`Self::spawn`], but over `ledger_dir` **as it already is** -- never wiped, never
    /// created fresh -- so a test can build a *second* `TestServer` over the exact directory
    /// a *first* one (already shut down via [`Self::shutdown_keep_ledger`]) wrote to, proving
    /// a property survives a real process restart rather than merely surviving within one
    /// process's own `Ledger`/`CommandAuthorityServiceImpl` handles.
    async fn spawn_over(ledger_dir: PathBuf, start_tai_ns: i64) -> Self {
        let roles = default_roles();
        let delegations = vec![wildcard_delegation("delegation-1", "operator-1"), wildcard_delegation("delegation-9", "astronaut-jane")];
        Self::spawn_over_with_authz(ledger_dir, start_tai_ns, roles, delegations, vec![], "").await
    }

    /// The general constructor every other one delegates to: full control over the role
    /// table, the delegation set, and the MFA configuration (`mfa_amr_methods`/`mfa_acr`) --
    /// used by this file's dedicated A2.2 tests (wrong role, missing MFA, expired delegation,
    /// a delegation granting a class the role does not).
    async fn spawn_over_with_authz(
        ledger_dir: PathBuf,
        start_tai_ns: i64,
        roles: BTreeMap<String, Vec<String>>,
        delegations: Vec<Delegation>,
        mfa_amr_methods: Vec<String>,
        mfa_acr: &str,
    ) -> Self {
        let ledger = Arc::new(Ledger::open(&ledger_dir).expect("open ledger"));
        let bundle = Arc::new(PolicyBundle::load(real_policy_dir()).expect("load the shipped policy bundle"));
        let clock = Arc::new(TestClock::new(start_tai_ns));
        let dispatch_sink = Arc::new(RecordingDispatchSink::new());
        let issuer = TestIssuer::new();
        let issuer_config = Arc::new(
            IssuerConfig::from_public_key_pem(TEST_ISSUER, TEST_AUDIENCE, issuer.public_key_pem())
                .expect("a freshly-generated test issuer key parses as a valid public key"),
        );

        let audit_path = ledger_dir.join("audit.log");
        let audit = Arc::new(AuditWriter::open(&AuditSinkConfig::File(audit_path.clone())).expect("open the test audit sink file"));
        let authz = AuthzConfig {
            role_table: Arc::new(RoleTable::from_config(&roles)),
            delegations: Arc::new(DelegationTable::from_delegations(delegations)),
            mfa_amr_methods: Arc::new(mfa_amr_methods),
            mfa_acr: Arc::new(mfa_acr.to_string()),
            audit,
        };

        let servicer = CommandAuthorityServiceImpl::new(
            ledger,
            bundle,
            3_600_000_000_000, // matches profiles/execution.yaml's authority.rate_window_ns
            clock.clone() as Arc<dyn Clock>,
            dispatch_sink.clone() as Arc<dyn DispatchSink>,
            issuer_config,
            authz,
        )
        .expect("rebuild the duplicate-dispatch guard from the ledger at construction");

        // Bind an OS-assigned ephemeral port -- never a fixed one -- and read the real
        // address back, with no bind-then-drop-then-rebind race (the listener stays open,
        // handed straight to the server).
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port");
        let addr: SocketAddr = listener.local_addr().expect("local_addr");
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

        let channel = Endpoint::from_shared(format!("http://{addr}"))
            .expect("valid endpoint URI")
            .connect()
            .await
            .expect("connect to the just-spawned server over its real loopback socket");
        let client = CommandAuthorityServiceClient::new(channel);

        Self { client, ledger_dir, clock, dispatch_sink, issuer, audit_path, shutdown_tx, handle }
    }

    /// Mints a real RS256-signed token this server's own `Authorize` will verify: `sub`,
    /// the fixed `TEST_ISSUER`/`TEST_AUDIENCE`/`TOKEN_NOW_UNIX_S`/`TOKEN_TTL_S` this file uses
    /// throughout, and no `nbf` (see `TOKEN_NOW_UNIX_S`'s own doc comment for why). Carries the
    /// default `groups`/`amr`/`acr` from `valid_claims` -- [`Self::mint_with_claims`] for a
    /// test that needs to override them.
    fn mint(&self, sub: &str) -> String {
        self.issuer.mint(&valid_claims(TEST_ISSUER, TEST_AUDIENCE, sub, TOKEN_NOW_UNIX_S, TOKEN_TTL_S))
    }

    /// As [`Self::mint`], but with caller-chosen `groups`/`amr`/`acr` -- for a test that needs
    /// a specific role or a specific (or absent) MFA claim.
    fn mint_with_claims(&self, sub: &str, groups: &[&str], amr: &[&str], acr: &str) -> String {
        let claims = claims_with_roles_and_mfa(TEST_ISSUER, TEST_AUDIENCE, sub, TOKEN_NOW_UNIX_S, TOKEN_TTL_S, RoleAndMfaClaims { groups, amr, acr });
        self.issuer.mint(&claims)
    }

    /// Every line in this server's audit sink file so far, in order -- read straight from
    /// disk, the real artifact `crate::audit::AuditWriter` wrote to.
    fn audit_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.audit_path).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
    }

    /// Shuts the server task down cleanly, **joins** it (proves the task actually stopped,
    /// rather than merely asking it to), and removes its ledger directory -- the ordinary
    /// end-of-test teardown.
    async fn shutdown(self) {
        let ledger_dir = self.shutdown_keep_ledger().await;
        let _ = std::fs::remove_dir_all(&ledger_dir);
    }

    /// Shuts the server task down cleanly and joins it, like [`Self::shutdown`], but
    /// deliberately leaves the ledger directory on disk and returns its path -- for the one
    /// test that needs a *second* `TestServer` to reopen exactly what this instance wrote
    /// ([`Self::spawn_over`]). The caller is responsible for eventually removing the
    /// directory once no further instance needs it.
    async fn shutdown_keep_ledger(self) -> PathBuf {
        let _ = self.shutdown_tx.send(());
        self.handle.await.expect("server task joins cleanly at test end");
        self.ledger_dir
    }
}

/// **Acceptance test 1**: the full legal path end to end, asserting every response's state,
/// the ledger's exact on-disk record sequence, and `VerifyLedger`.
#[tokio::test]
async fn full_legal_path_propose_check_authorize_dispatch_ack_end_to_end() {
    let mut server = TestServer::spawn("legal-path", 1_000).await;

    let proposed = server.client.propose(propose_request(base_command("cmd-1", "sat-1", "mode", "idem-1"), "model-x")).await.expect("Propose").into_inner();
    let proposed_command = proposed.command.expect("command present");
    assert_eq!(proposed_command.state, CommandState::Proposed as i32);
    assert!(proposed.decision.is_none(), "Propose never runs a policy decision");
    // The injected clock, not the wall clock: every transition's own tai_ns traces back to
    // the TestClock this server was spawned with.
    assert_eq!(proposed_command.transitions[0].tai_ns, server.clock.now_tai_ns());

    let checked = server.client.check(CheckRequest { command_id: "cmd-1".to_string() }).await.expect("Check").into_inner();
    let checked_command = checked.command.expect("command present");
    assert_eq!(checked_command.state, CommandState::Checked as i32);
    let decision = checked.decision.expect("Check attaches the PolicyDecision");
    assert!(decision.allow, "{decision:?}");

    let token = server.mint("operator-1");
    let authorized = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-1".to_string(), principal_token: token, delegation_id: "delegation-1".to_string() })
        .await
        .expect("Authorize")
        .into_inner();
    let authorized_command = authorized.command.expect("command present");
    assert_eq!(authorized_command.state, CommandState::Authorized as i32);
    // A2.1: the recorded principal is the token's verified sub claim, never the raw token.
    assert_eq!(authorized_command.transitions.last().unwrap().principal, "operator-1");
    assert_eq!(authorized_command.transitions.last().unwrap().delegation_id, "delegation-1");

    let dispatched = server.client.dispatch(DispatchRequest { command_id: "cmd-1".to_string() }).await.expect("Dispatch").into_inner();
    let dispatched_command = dispatched.command.expect("command present");
    assert_eq!(dispatched_command.state, CommandState::Dispatched as i32);
    assert_eq!(server.dispatch_sink.dispatched().len(), 1);
    assert_eq!(server.dispatch_sink.dispatched()[0].id, "cmd-1");

    let acked = server
        .client
        .ack(AckRequest { command_id: "cmd-1".to_string(), ack_level: AckLevel::AssetExecuted as i32, principal: "flight-software".to_string(), reason: "executed".to_string() })
        .await
        .expect("Ack")
        .into_inner();
    let acked_command = acked.command.expect("command present");
    assert_eq!(acked_command.state, CommandState::Acked as i32);
    assert_eq!(acked_command.transitions.last().unwrap().ack_level, AckLevel::AssetExecuted as i32);

    // The ledger on disk holds exactly the expected five records, in order.
    let records = read_ledger_records(&server.ledger_dir, "sat-1");
    let states: Vec<CommandState> = records.iter().map(|r| CommandState::try_from(r.transition.as_ref().unwrap().state).unwrap()).collect();
    assert_eq!(
        states,
        vec![CommandState::Proposed, CommandState::Checked, CommandState::Authorized, CommandState::Dispatched, CommandState::Acked]
    );
    assert_eq!(records[0].seq, 1);
    assert_eq!(records[4].seq, 5);
    assert!(records[1].decision.is_some(), "the CHECKED record carries the PolicyDecision");

    let verify = server.client.verify_ledger(VerifyLedgerRequest { partition: "sat-1".to_string() }).await.expect("VerifyLedger").into_inner();
    assert!(verify.ok);
    assert_eq!(verify.results.len(), 1);
    assert!(verify.results[0].ok);
    assert_eq!(verify.results[0].checked, 5);

    server.shutdown().await;
}

/// **Acceptance test 2**: a `payload`-class command is `REJECTED` at `Check`, with the
/// policy's exact reason text in the response, over the wire.
#[tokio::test]
async fn check_denies_a_payload_class_command_with_the_policys_exact_reason() {
    let mut server = TestServer::spawn("policy-deny", 1_000).await;

    server.client.propose(propose_request(base_command("cmd-2", "sat-1", "payload", ""), "model-x")).await.expect("Propose");
    let checked = server.client.check(CheckRequest { command_id: "cmd-2".to_string() }).await.expect("Check").into_inner();
    let checked_command = checked.command.expect("command present");
    assert_eq!(checked_command.state, CommandState::Rejected as i32);
    let decision = checked.decision.expect("Check attaches the PolicyDecision even for a denial");
    assert!(!decision.allow);
    assert_eq!(decision.reasons, vec!["command_class payload is not admitted by policy".to_string()]);

    server.shutdown().await;
}

/// **Acceptance test 3**: every illegal edge over the wire is refused `FAILED_PRECONDITION`
/// with the typed `state::CommandError::IllegalTransition` message preserved.
#[tokio::test]
async fn illegal_edges_over_the_wire_are_refused_failed_precondition_with_the_typed_message() {
    let mut server = TestServer::spawn("illegal-edges", 1_000).await;

    // Authorize-before-Check. A verifiable token (A2.1 verifies identity before attempting
    // the state edge) -- this must fail on the *state machine's* own edge check, not on
    // token verification, or this test would no longer be testing what its name says.
    server.client.propose(propose_request(base_command("cmd-a", "sat-1", "mode", ""), "model-x")).await.unwrap();
    let token = server.mint("operator-1");
    let err = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-a".to_string(), principal_token: token, delegation_id: String::new() })
        .await
        .expect_err("Authorize before Check must be refused");
    assert_eq!(err.code(), Code::FailedPrecondition, "{err:?}");
    assert!(err.message().contains("illegal command transition"), "{}", err.message());

    // Dispatch-before-Authorize.
    server.client.propose(propose_request(base_command("cmd-b", "sat-1", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-b".to_string() }).await.unwrap();
    let err = server.client.dispatch(DispatchRequest { command_id: "cmd-b".to_string() }).await.expect_err("Dispatch before Authorize must be refused");
    assert_eq!(err.code(), Code::FailedPrecondition, "{err:?}");
    assert!(err.message().contains("illegal command transition"), "{}", err.message());

    // Ack-before-Dispatch.
    server.client.propose(propose_request(base_command("cmd-c", "sat-1", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-c".to_string() }).await.unwrap();
    let token = server.mint("operator-1");
    server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-c".to_string(), principal_token: token, delegation_id: String::new() })
        .await
        .unwrap();
    let err = server
        .client
        .ack(AckRequest { command_id: "cmd-c".to_string(), ack_level: AckLevel::AssetExecuted as i32, principal: "p".to_string(), reason: "r".to_string() })
        .await
        .expect_err("Ack before Dispatch must be refused");
    assert_eq!(err.code(), Code::FailedPrecondition, "{err:?}");
    assert!(err.message().contains("illegal command transition"), "{}", err.message());

    server.shutdown().await;
}

/// **Acceptance test 4** (question 53): `Propose` with a non-empty `envelope_id` is refused
/// `INVALID_ARGUMENT`, over the wire, with the typed message.
#[tokio::test]
async fn propose_with_a_nonempty_envelope_id_is_refused_over_the_wire() {
    let mut server = TestServer::spawn("envelope-refused", 1_000).await;

    let mut command = base_command("cmd-env", "sat-1", "mode", "");
    command.envelope_id = "env-station-keeping".to_string();
    let err = server.client.propose(propose_request(command, "model-x")).await.expect_err("a non-empty envelope_id must be refused");
    assert_eq!(err.code(), Code::InvalidArgument, "{err:?}");
    assert!(err.message().contains("propose refuses a non-empty envelope_id"), "{}", err.message());
    assert!(err.message().contains("question 53"), "{}", err.message());

    server.shutdown().await;
}

/// **Acceptance test 5**: two commands sharing one `idempotency_key` -- the first dispatches;
/// the second is refused `ALREADY_EXISTS`, and appends no second ledger record.
#[tokio::test]
async fn dispatch_refuses_a_duplicate_idempotency_key_and_appends_no_second_ledger_record() {
    let mut server = TestServer::spawn("idempotency", 1_000).await;
    let key = "idem-shared";

    for id in ["cmd-x", "cmd-y"] {
        server.client.propose(propose_request(base_command(id, "sat-1", "mode", key), "model-x")).await.unwrap();
        server.client.check(CheckRequest { command_id: id.to_string() }).await.unwrap();
        let token = server.mint("operator-1");
        server.client.authorize(AuthorizeRequest { command_id: id.to_string(), principal_token: token, delegation_id: String::new() }).await.unwrap();
    }

    let first = server.client.dispatch(DispatchRequest { command_id: "cmd-x".to_string() }).await.expect("the first dispatch of this key succeeds").into_inner();
    assert_eq!(first.command.unwrap().state, CommandState::Dispatched as i32);

    let records_before = read_ledger_records(&server.ledger_dir, "sat-1").len();

    let err = server.client.dispatch(DispatchRequest { command_id: "cmd-y".to_string() }).await.expect_err("a second command sharing the dispatched key must be refused");
    assert_eq!(err.code(), Code::AlreadyExists, "{err:?}");
    assert!(err.message().contains(&format!("idempotency_key {key:?}")), "{}", err.message());
    assert!(err.message().contains("twice"), "{}", err.message());

    let records_after = read_ledger_records(&server.ledger_dir, "sat-1").len();
    assert_eq!(records_before, records_after, "the refused dispatch must append no second ledger record");
    assert_eq!(server.dispatch_sink.dispatched().len(), 1, "only the first command ever reached the DispatchSink");

    server.shutdown().await;
}

/// **The cross-restart acceptance test** (defect the manager's review found: the
/// duplicate-dispatch guard was built empty at every construction, so the guarantee
/// evaporated across a process restart). Dispatches a key through one `TestServer` instance,
/// shuts that instance down keeping its ledger directory, then builds a **second**,
/// independent `TestServer` over that same directory (a different `Ledger` handle, a
/// different `CommandAuthorityServiceImpl`, a fresh in-memory `BTreeSet` before `new` rebuilds
/// it) and asserts the second instance refuses the same key with `ALREADY_EXISTS`, appending
/// no record for the refused attempt -- proving the guard is rebuilt from the ledger itself
/// (`Ledger::scan_dispatched_idempotency_keys`), not carried over by any in-process state.
#[tokio::test]
async fn dispatch_refuses_a_key_already_dispatched_by_a_prior_process_lifetime() {
    let key = "idem-across-restart";

    let mut server1 = TestServer::spawn("idempotency-restart", 1_000).await;
    server1.client.propose(propose_request(base_command("cmd-r1", "sat-r", "mode", key), "model-x")).await.unwrap();
    server1.client.check(CheckRequest { command_id: "cmd-r1".to_string() }).await.unwrap();
    let token1 = server1.mint("operator-1");
    server1
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-r1".to_string(), principal_token: token1, delegation_id: String::new() })
        .await
        .unwrap();
    let dispatched = server1
        .client
        .dispatch(DispatchRequest { command_id: "cmd-r1".to_string() })
        .await
        .expect("the first process lifetime's dispatch succeeds")
        .into_inner();
    assert_eq!(dispatched.command.unwrap().state, CommandState::Dispatched as i32);
    let ledger_dir = server1.shutdown_keep_ledger().await;

    let records_before = read_ledger_records(&ledger_dir, "sat-r").len();

    // A second, independent process lifetime: a fresh Ledger handle, a fresh
    // CommandAuthorityServiceImpl, over the SAME on-disk ledger directory. Its own
    // in-memory idempotency set starts empty and must be rebuilt by `new` before this
    // assertion can pass.
    let mut server2 = TestServer::spawn_over(ledger_dir.clone(), 2_000).await;
    server2.client.propose(propose_request(base_command("cmd-r2", "sat-r", "mode", key), "model-x")).await.unwrap();
    server2.client.check(CheckRequest { command_id: "cmd-r2".to_string() }).await.unwrap();
    let token2 = server2.mint("operator-1");
    server2.client.authorize(AuthorizeRequest { command_id: "cmd-r2".to_string(), principal_token: token2, delegation_id: String::new() }).await.unwrap();

    let err = server2
        .client
        .dispatch(DispatchRequest { command_id: "cmd-r2".to_string() })
        .await
        .expect_err("a second process lifetime must still refuse a key the first already dispatched");
    assert_eq!(err.code(), Code::AlreadyExists, "{err:?}");
    assert!(err.message().contains(&format!("idempotency_key {key:?}")), "{}", err.message());

    // The refused dispatch appended PROPOSED/CHECKED/AUTHORIZED for cmd-r2 (three more
    // records than server1 left behind) but no fourth, DISPATCHED one.
    let records_after = read_ledger_records(&ledger_dir, "sat-r").len();
    assert_eq!(records_after, records_before + 3, "cmd-r2 reached AUTHORIZED but never DISPATCHED: {records_after} vs {records_before}");
    assert_eq!(server2.dispatch_sink.dispatched().len(), 0, "the second process lifetime's DispatchSink was never reached");

    server2.shutdown().await;
}

/// **Acceptance test 6**: `Query` by `command_id`, and by `entity_id` with an optional
/// `CommandState` filter.
#[tokio::test]
async fn query_by_id_and_by_entity_with_a_state_filter() {
    let mut server = TestServer::spawn("query", 1_000).await;

    server.client.propose(propose_request(base_command("cmd-q1", "sat-q", "mode", ""), "model-x")).await.unwrap();
    server.client.propose(propose_request(base_command("cmd-q2", "sat-q", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-q2".to_string() }).await.unwrap(); // cmd-q2 -> CHECKED; cmd-q1 stays PROPOSED

    let by_id = server.client.query(QueryRequest { selector: Some(Selector::CommandId("cmd-q1".to_string())) }).await.expect("Query by id").into_inner();
    assert_eq!(by_id.commands.len(), 1);
    assert_eq!(by_id.commands[0].id, "cmd-q1");

    let by_entity_unfiltered = server
        .client
        .query(QueryRequest { selector: Some(Selector::Entity(QueryByEntity { entity_id: "sat-q".to_string(), state_filter: CommandState::Unspecified as i32 })) })
        .await
        .expect("Query by entity, unfiltered")
        .into_inner();
    let mut ids: Vec<String> = by_entity_unfiltered.commands.iter().map(|c| c.id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["cmd-q1".to_string(), "cmd-q2".to_string()]);

    let by_entity_checked = server
        .client
        .query(QueryRequest { selector: Some(Selector::Entity(QueryByEntity { entity_id: "sat-q".to_string(), state_filter: CommandState::Checked as i32 })) })
        .await
        .expect("Query by entity, filtered to CHECKED")
        .into_inner();
    assert_eq!(by_entity_checked.commands.len(), 1);
    assert_eq!(by_entity_checked.commands[0].id, "cmd-q2");

    server.shutdown().await;
}

/// **Acceptance test 7**: `VerifyLedger` reports a tampered partition as broken, at the
/// exact sequence number the tamper is at.
#[tokio::test]
async fn verify_ledger_reports_a_tampered_partition_as_broken_at_the_right_sequence() {
    let mut server = TestServer::spawn("verify-tamper", 1_000).await;

    server.client.propose(propose_request(base_command("cmd-t", "sat-t", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-t".to_string() }).await.unwrap();
    let token = server.mint("operator-1");
    server.client.authorize(AuthorizeRequest { command_id: "cmd-t".to_string(), principal_token: token, delegation_id: String::new() }).await.unwrap();

    let mut records = read_ledger_records(&server.ledger_dir, "sat-t");
    assert_eq!(records.len(), 3, "sanity: PROPOSED, CHECKED, AUTHORIZED");

    // Tamper record 2's body, leaving prev_hash/hash untouched -- the same technique
    // crates/av-command/src/ledger.rs's own tamper-detection test uses.
    records[1].command_id = "tampered-command-id".to_string();
    write_ledger_records(&server.ledger_dir, "sat-t", &records);

    let resp = server.client.verify_ledger(VerifyLedgerRequest { partition: "sat-t".to_string() }).await.expect("VerifyLedger").into_inner();
    assert!(!resp.ok);
    assert_eq!(resp.results.len(), 1);
    assert!(!resp.results[0].ok);
    assert_eq!(resp.results[0].broken_at_sequence, 2, "must name the exact sequence number the chain broke at");
    assert_eq!(resp.results[0].checked, 1, "the record before the tamper (seq 1) still verifies as good");
    assert!(resp.results[0].detail.contains("tampered"), "{}", resp.results[0].detail);

    server.shutdown().await;
}

/// **Acceptance test 8** (question 155): a non-loopback bind address is refused with a typed
/// error, at this crate's own public bind-address boundary -- no server is spawned for this
/// test at all, since the whole point is that a bad bind address must never reach a real
/// `TcpListener::bind` call.
#[test]
fn non_loopback_bind_addresses_are_refused_with_a_typed_error_naming_question_155() {
    for raw in ["0.0.0.0:50070", "10.1.2.3:50070", "example.com:50070", "[::]:50070"] {
        let err = resolve_loopback_bind_address(raw).expect_err(&format!("{raw:?} must be refused"));
        assert!(matches!(err, BindAddressError::NotLoopback { .. }), "{raw:?} -> {err:?}");
        assert!(err.to_string().contains("question 155"), "{err}");
        assert!(err.to_string().contains(raw), "{err}");
    }

    // Sanity: the loopback spellings this same boundary must accept -- proves the refusal
    // above is a real discriminator, not every address being refused unconditionally.
    for raw in ["127.0.0.1:50070", "localhost:50070", "[::1]:50070"] {
        resolve_loopback_bind_address(raw).unwrap_or_else(|e| panic!("{raw:?} must be accepted: {e}"));
    }
}

/// **Acceptance test 9** (A2.1): `Authorize` with an unverifiable `principal_token` is
/// refused `UNAUTHENTICATED`, before any state transition happens and before any ledger
/// record is appended -- see `crate::service`'s module doc, "A2.1: `Authorize` verifies
/// *who*, not *whether*".
#[tokio::test]
async fn authorize_with_an_unverifiable_token_is_refused_unauthenticated_and_appends_no_record() {
    let mut server = TestServer::spawn("a2-unverifiable-token", 1_000).await;
    server.client.propose(propose_request(base_command("cmd-bad-tok", "sat-a2", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-bad-tok".to_string() }).await.unwrap();

    let records_before = read_ledger_records(&server.ledger_dir, "sat-a2").len();

    // Wrong issuer -- a real, correctly-signed token from this server's own issuer, but
    // minted with an iss the server's IssuerConfig does not match. Not a hand-edited string:
    // a real signed token whose *claims* are wrong, exactly per this task's brief.
    let bad_token = server.issuer.mint(&valid_claims("https://not-the-configured-issuer/", TEST_AUDIENCE, "operator-1", TOKEN_NOW_UNIX_S, TOKEN_TTL_S));
    let err = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-bad-tok".to_string(), principal_token: bad_token, delegation_id: String::new() })
        .await
        .expect_err("an unverifiable token must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");
    assert!(err.message().contains("token verification failed"), "{}", err.message());
    assert!(err.message().contains("does not match the configured issuer"), "{}", err.message());

    let records_after = read_ledger_records(&server.ledger_dir, "sat-a2").len();
    assert_eq!(records_before, records_after, "a refused Authorize must append no ledger record at all");

    // The command itself is unaffected: still CHECKED, not AUTHORIZED and not anything else.
    let queried = server
        .client
        .query(QueryRequest { selector: Some(Selector::CommandId("cmd-bad-tok".to_string())) })
        .await
        .expect("Query")
        .into_inner();
    assert_eq!(queried.commands[0].state, CommandState::Checked as i32);

    server.shutdown().await;
}

/// **Acceptance test 10** (A2.1): the refusal message for an unverifiable token contains
/// neither the raw bearer token nor its raw signature bytes -- `crate::oidc`'s module doc's
/// and `crate::service`'s module doc's own claim, asserted directly here against a real
/// refusal over the real wire.
#[tokio::test]
async fn authorize_refusal_message_never_contains_the_token_or_signature() {
    let mut server = TestServer::spawn("a2-no-leak", 1_000).await;
    server.client.propose(propose_request(base_command("cmd-leak-check", "sat-a2", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-leak-check".to_string() }).await.unwrap();

    // Wrong audience this time (a different refusal reason from test 9, same principle): a
    // real, correctly-signed token, deliberately minted for the wrong audience.
    let bad_token = server.issuer.mint(&valid_claims(TEST_ISSUER, "some-other-audience", "operator-1", TOKEN_NOW_UNIX_S, TOKEN_TTL_S));
    let signature_b64 = bad_token.rsplit_once('.').expect("a JWS has a signature segment").1.to_string();

    let err = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-leak-check".to_string(), principal_token: bad_token.clone(), delegation_id: String::new() })
        .await
        .expect_err("a wrong-audience token must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");
    assert!(!err.message().contains(bad_token.as_str()), "refusal message must not contain the raw token: {}", err.message());
    assert!(!err.message().contains(signature_b64.as_str()), "refusal message must not contain the raw signature: {}", err.message());
    assert!(err.message().contains("does not contain the configured audience"), "{}", err.message());

    server.shutdown().await;
}

/// **Acceptance test 11** (A2.1): a real, correctly-signed, fully-valid token succeeds, and
/// the **verified** `sub` claim -- not the raw bearer token -- is what lands in
/// `CommandTransition.principal`, over the real wire, read back from the durable ledger.
#[tokio::test]
async fn authorize_with_a_verified_token_records_the_verified_sub_not_the_raw_token() {
    let mut server = TestServer::spawn("a2-verified-principal", 1_000).await;
    server.client.propose(propose_request(base_command("cmd-verified", "sat-a2", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-verified".to_string() }).await.unwrap();

    let token = server.mint("astronaut-jane");
    let authorized = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-verified".to_string(), principal_token: token.clone(), delegation_id: "delegation-9".to_string() })
        .await
        .expect("a fresh, correctly-signed token must be accepted")
        .into_inner();
    let authorized_command = authorized.command.expect("command present");
    assert_eq!(authorized_command.state, CommandState::Authorized as i32);
    let last = authorized_command.transitions.last().unwrap();
    assert_eq!(last.principal, "astronaut-jane");
    assert_ne!(last.principal, token, "the recorded principal must never be the raw token string");
    assert_eq!(last.delegation_id, "delegation-9");
    // A2.2: the reason now names how authorization was granted (crate::authz), not A2.1's old
    // "principal_token verified ..." text -- the delegation-9 fixture (see
    // `wildcard_delegation`) is what actually granted this, since "astronaut-jane" has no
    // configured role.
    assert!(last.reason.contains("via=delegation=\"delegation-9\""), "{}", last.reason);
    assert!(last.reason.contains("mfa=not_required"), "{}", last.reason);

    // Read back from the durable ledger, not just the in-memory response.
    let records = read_ledger_records(&server.ledger_dir, "sat-a2");
    let authorized_record = records.iter().find(|r| r.transition.as_ref().unwrap().state == CommandState::Authorized as i32).expect("an AUTHORIZED record exists");
    assert_eq!(authorized_record.transition.as_ref().unwrap().principal, "astronaut-jane");

    server.shutdown().await;
}

// -------------------------------------------------------------------------------------------
// A2.2: the role gate, the MFA gate, delegation enforcement, and the audit line for every
// outcome (`docs/aiplane-plan.md` milestone A2's second half). This crate's own
// `crate::authz` unit tests already pin every refusal shape and the delegation expiry
// boundary at the library level (no wire, no ledger, no audit sink); the tests below are this
// task's named acceptance tests, each run **over the wire, end to end**, against the real
// `CommandAuthorityServiceImpl`, its real ledger, and its real audit sink file.
// -------------------------------------------------------------------------------------------

/// A fixed, arbitrary "now", chosen so every audit-line test below can assert a real,
/// human-legible RFC 5424 `TIMESTAMP` rather than a value derived from the leap-second table's
/// pre-1972 clamped offset (a `TestClock` seeded at a small raw value like `1_000` -- what
/// this file's earlier, pre-A2.2 tests use -- converts through `av_cdm::time::Tai::
/// to_utc_nanos` to a UTC instant within a second of the Unix epoch, which is real and
/// correct but not a timestamp worth pasting into a report by hand). Cross-checked against
/// `date -u -r 1760000000` (`2025-10-09 08:53:20 UTC`) on this host -- the identical value
/// `crates/av-command/src/audit.rs`'s own `format_timestamp_matches_known_reference_dates`
/// test pins.
fn audit_test_start_tai_ns() -> i64 {
    Tai::from_utc_nanos(1_760_000_000_000_000_000).as_nanos()
}

/// **Acceptance test 1: the right role authorizes, over the wire, end to end, with the
/// ledger asserted.** `"operator-1"`'s default `groups` (`["operators", "burn-authorizers"]`,
/// `av_command::test_support::valid_claims`) include `"operators"`, which
/// [`default_roles`] grants `"mode"` -- no delegation is claimed at all (`delegation_id`
/// empty), so this is a pure role-gate acceptance.
#[tokio::test]
async fn authorize_with_the_right_role_authorizes_over_the_wire_with_the_ledger_asserted() {
    let mut server = TestServer::spawn("right-role", 1_000).await;
    server.client.propose(propose_request(base_command("cmd-right-role", "sat-role", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-right-role".to_string() }).await.unwrap();

    let token = server.mint("operator-1");
    let authorized = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-right-role".to_string(), principal_token: token, delegation_id: String::new() })
        .await
        .expect("operators grants mode: this must be authorized")
        .into_inner();
    let command = authorized.command.expect("command present");
    assert_eq!(command.state, CommandState::Authorized as i32);
    let last = command.transitions.last().unwrap();
    assert_eq!(last.principal, "operator-1");
    assert_eq!(last.delegation_id, "", "no delegation was claimed");
    assert_eq!(last.reason, "authorize: command_class=\"mode\" via=role=\"operators\" mfa=not_required (crate::authz)");

    let records = read_ledger_records(&server.ledger_dir, "sat-role");
    let authorized_record = records.iter().find(|r| r.transition.as_ref().unwrap().state == CommandState::Authorized as i32).expect("an AUTHORIZED record exists");
    assert_eq!(authorized_record.transition.as_ref().unwrap().principal, "operator-1");
    assert_eq!(authorized_record.transition.as_ref().unwrap().reason, last.reason);

    server.shutdown().await;
}

/// **Acceptance test 2: the wrong role is refused with the reason, exact.**
#[tokio::test]
async fn authorize_with_the_wrong_role_is_refused_with_the_exact_reason_over_the_wire() {
    let mut server = TestServer::spawn("wrong-role", 1_000).await;
    server.client.propose(propose_request(base_command("cmd-wrong-role", "sat-role", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-wrong-role".to_string() }).await.unwrap();
    let records_before = read_ledger_records(&server.ledger_dir, "sat-role").len();

    // "viewers" is not in the default role table at all -- deny by default.
    let token = server.mint_with_claims("viewer-1", &["viewers"], &[], "");
    let err = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-wrong-role".to_string(), principal_token: token, delegation_id: String::new() })
        .await
        .expect_err("a role that does not grant mode must be refused");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    assert_eq!(
        err.message(),
        "authorize: role gate refused -- none of groups [\"viewers\"] is a role granting command_class \"mode\" \
         (crate::authz, deny-by-default: an unlisted role, or a role that does not list this class, is refused)"
    );

    let records_after = read_ledger_records(&server.ledger_dir, "sat-role").len();
    assert_eq!(records_before, records_after, "a denied Authorize must append no ledger record at all");

    server.shutdown().await;
}

/// **Acceptance test 3: hazardous without MFA is refused, exact reason, distinct from the
/// wrong-role reason.**
#[tokio::test]
async fn authorize_of_a_hazardous_class_without_mfa_is_refused_with_the_exact_reason_over_the_wire() {
    let mut roles = BTreeMap::new();
    roles.insert("operators".to_string(), vec!["mode".to_string()]);
    let mut server = TestServer::spawn_over_with_authz(tmp_ledger_dir("hazardous-no-mfa"), 1_000, roles, vec![], vec!["otp".to_string()], "").await;

    let mut command = base_command("cmd-hazardous", "sat-hazard", "mode", "");
    command.hazardous = true;
    server.client.propose(propose_request(command, "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-hazardous".to_string() }).await.unwrap();

    // The right role, but no amr/acr at all -- the MFA gate, not the role gate, must refuse.
    let token = server.mint_with_claims("operator-1", &["operators"], &[], "");
    let err = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-hazardous".to_string(), principal_token: token, delegation_id: String::new() })
        .await
        .expect_err("a hazardous command_class without a satisfying amr/acr must be refused");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    let mfa_message = "authorize: MFA gate refused -- command_class \"mode\" is hazardous and requires one of amr [\"otp\"] or acr \"\"; \
                        principal amr was [] and acr was \"\"";
    assert_eq!(err.message(), mfa_message);

    let wrong_role_message = "authorize: role gate refused -- none of groups [\"viewers\"] is a role granting command_class \"mode\" \
                               (crate::authz, deny-by-default: an unlisted role, or a role that does not list this class, is refused)";
    assert_ne!(mfa_message, wrong_role_message, "the MFA refusal and the wrong-role refusal must read differently");

    server.shutdown().await;
}

/// **Acceptance test 4: an expired delegation is refused at the boundary second -- both
/// sides asserted exactly, with a `TestClock`, never a sleep.**
#[tokio::test]
async fn authorize_under_a_delegation_is_refused_at_the_expiry_boundary_second_one_nanosecond_earlier_is_not() {
    let expires_tai_ns: i64 = 50_000;
    let delegation = Delegation {
        id: "delegation-exp".to_string(),
        subject: "operator-1".to_string(),
        command_classes: vec!["mode".to_string()],
        entity_ids: vec!["sat-exp".to_string()],
        not_before_tai_ns: 0,
        expires_tai_ns,
        granted_by: "ops-lead".to_string(),
        reason: "contingency".to_string(),
    };
    let mut server = TestServer::spawn_over_with_authz(tmp_ledger_dir("delegation-expiry"), 1_000, BTreeMap::new(), vec![delegation], vec![], "").await;

    // One nanosecond before expiry: accepted.
    server.client.propose(propose_request(base_command("cmd-before-expiry", "sat-exp", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-before-expiry".to_string() }).await.unwrap();
    server.clock.set(expires_tai_ns - 1);
    let token = server.mint("operator-1");
    let ok = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-before-expiry".to_string(), principal_token: token, delegation_id: "delegation-exp".to_string() })
        .await
        .expect("one nanosecond before expires_tai_ns must be accepted")
        .into_inner();
    assert_eq!(ok.command.unwrap().state, CommandState::Authorized as i32);

    // Exactly at expires_tai_ns: refused.
    server.client.propose(propose_request(base_command("cmd-at-expiry", "sat-exp", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-at-expiry".to_string() }).await.unwrap();
    server.clock.set(expires_tai_ns);
    let token = server.mint("operator-1");
    let err = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-at-expiry".to_string(), principal_token: token, delegation_id: "delegation-exp".to_string() })
        .await
        .expect_err("at expires_tai_ns exactly, the delegation must be refused");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    assert_eq!(err.message(), format!("authorize: delegation \"delegation-exp\" expired -- expires_tai_ns={expires_tai_ns} is at or before now_tai_ns={expires_tai_ns}"));

    server.shutdown().await;
}

/// **Acceptance test 6: a delegation grants a class the role does not, and it reaches the
/// ledger as `CommandTransition.delegation_id`.**
#[tokio::test]
async fn a_delegation_grants_a_class_the_role_does_not_and_reaches_the_ledger_as_delegation_id() {
    let mut roles = BTreeMap::new();
    roles.insert("operators".to_string(), vec!["mode".to_string()]); // no "burn"
    let delegation = Delegation {
        id: "delegation-burn".to_string(),
        subject: "operator-1".to_string(),
        command_classes: vec!["burn".to_string()],
        entity_ids: vec![WILDCARD.to_string()],
        not_before_tai_ns: 0,
        expires_tai_ns: i64::MAX,
        granted_by: "ops-lead".to_string(),
        reason: "contingency burn authority".to_string(),
    };
    let mut server = TestServer::spawn_over_with_authz(tmp_ledger_dir("delegation-extra-class"), 1_000, roles, vec![delegation], vec![], "").await;

    server.client.propose(propose_request(base_command("cmd-burn", "sat-burn", "burn", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-burn".to_string() }).await.unwrap();

    let token = server.mint("operator-1");
    let authorized = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-burn".to_string(), principal_token: token, delegation_id: "delegation-burn".to_string() })
        .await
        .expect("the delegation must grant burn even though the role does not")
        .into_inner();
    let command = authorized.command.expect("command present");
    assert_eq!(command.state, CommandState::Authorized as i32);
    assert_eq!(command.transitions.last().unwrap().delegation_id, "delegation-burn");

    let records = read_ledger_records(&server.ledger_dir, "sat-burn");
    let authorized_record = records.iter().find(|r| r.transition.as_ref().unwrap().state == CommandState::Authorized as i32).expect("an AUTHORIZED record exists");
    assert_eq!(authorized_record.transition.as_ref().unwrap().delegation_id, "delegation-burn", "CommandTransition.delegation_id must reach the ledger");

    server.shutdown().await;
}

/// **Acceptance test 5a: the audit line for a successful authorization, exact.**
#[tokio::test]
async fn audit_line_for_a_successful_authorization_is_exact() {
    let mut server = TestServer::spawn("audit-success", audit_test_start_tai_ns()).await;
    server.client.propose(propose_request(base_command("cmd-audit-ok", "sat-audit", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-audit-ok".to_string() }).await.unwrap();
    let token = server.mint("operator-1");
    server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-audit-ok".to_string(), principal_token: token, delegation_id: String::new() })
        .await
        .expect("authorized");

    let lines = server.audit_lines();
    let authorized_line = lines.iter().find(|l| l.contains("- AUTHORIZED ")).expect("an AUTHORIZED audit line exists");
    assert_eq!(
        authorized_line,
        "<134>1 2025-10-09T08:53:20.000000Z - av-command - AUTHORIZED [avCommand@32473 commandId=\"cmd-audit-ok\" entity=\"sat-audit\" \
         class=\"mode\" state=\"AUTHORIZED\" principal=\"operator-1\"] authorize: command_class=\"mode\" via=role=\"operators\" \
         mfa=not_required (crate::authz)"
    );

    server.shutdown().await;
}

/// **Acceptance test 5b: the audit line for a wrong-role refusal, exact.**
#[tokio::test]
async fn audit_line_for_a_wrong_role_refusal_is_exact() {
    let mut server = TestServer::spawn("audit-wrong-role", audit_test_start_tai_ns()).await;
    server.client.propose(propose_request(base_command("cmd-audit-role", "sat-audit", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-audit-role".to_string() }).await.unwrap();
    let token = server.mint_with_claims("viewer-1", &["viewers"], &[], "");
    let _ = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-audit-role".to_string(), principal_token: token, delegation_id: String::new() })
        .await
        .expect_err("refused");

    let lines = server.audit_lines();
    let refused_line = lines.iter().rev().find(|l| l.contains("REFUSED")).expect("a REFUSED audit line exists");
    assert_eq!(
        refused_line,
        "<132>1 2025-10-09T08:53:20.000000Z - av-command - REFUSED [avCommand@32473 commandId=\"cmd-audit-role\" entity=\"sat-audit\" \
         class=\"mode\" state=\"REFUSED\" principal=\"viewer-1\"] authorize: role gate refused -- none of groups [\"viewers\"] is a role \
         granting command_class \"mode\" (crate::authz, deny-by-default: an unlisted role, or a role that does not list this class, is refused)"
    );

    server.shutdown().await;
}

/// **Acceptance test 5c: the audit line for a missing-MFA refusal, exact.**
#[tokio::test]
async fn audit_line_for_a_missing_mfa_refusal_is_exact() {
    let mut roles = BTreeMap::new();
    roles.insert("operators".to_string(), vec!["mode".to_string()]);
    let mut server = TestServer::spawn_over_with_authz(tmp_ledger_dir("audit-missing-mfa"), audit_test_start_tai_ns(), roles, vec![], vec!["otp".to_string()], "").await;

    let mut command = base_command("cmd-audit-mfa", "sat-audit", "mode", "");
    command.hazardous = true;
    server.client.propose(propose_request(command, "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-audit-mfa".to_string() }).await.unwrap();
    let token = server.mint_with_claims("operator-1", &["operators"], &[], "");
    let _ = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-audit-mfa".to_string(), principal_token: token, delegation_id: String::new() })
        .await
        .expect_err("refused");

    let lines = server.audit_lines();
    let refused_line = lines.iter().rev().find(|l| l.contains("REFUSED")).expect("a REFUSED audit line exists");
    assert_eq!(
        refused_line,
        "<132>1 2025-10-09T08:53:20.000000Z - av-command - REFUSED [avCommand@32473 commandId=\"cmd-audit-mfa\" entity=\"sat-audit\" \
         class=\"mode\" state=\"REFUSED\" principal=\"operator-1\"] authorize: MFA gate refused -- command_class \"mode\" is hazardous and \
         requires one of amr [\"otp\"] or acr \"\"; principal amr was [] and acr was \"\""
    );

    server.shutdown().await;
}

/// **Acceptance test 5d: the audit line for an expired-delegation refusal, exact.**
#[tokio::test]
async fn audit_line_for_an_expired_delegation_refusal_is_exact() {
    let start = audit_test_start_tai_ns();
    let delegation = Delegation {
        id: "delegation-exp-audit".to_string(),
        subject: "operator-1".to_string(),
        command_classes: vec!["mode".to_string()],
        entity_ids: vec!["sat-audit".to_string()],
        not_before_tai_ns: 0,
        expires_tai_ns: start, // already at the boundary -- expired
        granted_by: "ops-lead".to_string(),
        reason: "test".to_string(),
    };
    let mut server = TestServer::spawn_over_with_authz(tmp_ledger_dir("audit-expired-delegation"), start, BTreeMap::new(), vec![delegation], vec![], "").await;

    server.client.propose(propose_request(base_command("cmd-audit-exp", "sat-audit", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-audit-exp".to_string() }).await.unwrap();
    let token = server.mint("operator-1");
    let _ = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-audit-exp".to_string(), principal_token: token, delegation_id: "delegation-exp-audit".to_string() })
        .await
        .expect_err("refused");

    let lines = server.audit_lines();
    let refused_line = lines.iter().rev().find(|l| l.contains("REFUSED")).expect("a REFUSED audit line exists");
    assert_eq!(
        refused_line,
        &format!(
            "<132>1 2025-10-09T08:53:20.000000Z - av-command - REFUSED [avCommand@32473 commandId=\"cmd-audit-exp\" entity=\"sat-audit\" \
             class=\"mode\" state=\"REFUSED\" principal=\"operator-1\" delegationId=\"delegation-exp-audit\"] authorize: delegation \
             \"delegation-exp-audit\" expired -- expires_tai_ns={start} is at or before now_tai_ns={start}"
        )
    );

    server.shutdown().await;
}
