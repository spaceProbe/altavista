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
    query_request::Selector, AckLevel, AckRequest, AuthorizeRequest, AuthorKind, CheckRequest, Command, CommandProposal,
    CommandState, Delegation, DispatchRequest, ExpireRequest, FailRequest, Label, LedgerRecord, Provenance, ProposeRequest,
    QueryByEntity, QueryRequest, VerifyLedgerRequest,
};
use av_cdm::time::Tai;
use av_command::admin;
use base64::Engine as _;
use av_command::audit::{AuditSinkConfig, AuditWriter};
use av_command::authz::{DelegationTable, RoleTable, ServiceRoleTable, WILDCARD};
use av_command::clock::{Clock, TestClock};
use av_command::counters::Counters;
use av_command::evidence::AdminState;
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
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
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

/// Like [`base_command`], but with every field question 203(a)'s restart fix actually needed
/// `LedgerRecord.command` to carry (`payload`, `deadline_tai_ns`, `not_before_tai_ns`, `label`,
/// `provenance`) given a real, non-default value -- these are exactly the fields the old
/// `LedgerRecord` (only `partition`/`command_id`/`command_class`/`idempotency_key`) could not
/// carry, so a test that only checked `id`/`state` across a restart would pass against the
/// unfixed code too. `envelope_id` is deliberately left empty (question 53: propose-only
/// forbids a non-empty one).
fn full_command(id: &str, entity_id: &str, command_class: &str, idempotency_key: &str) -> Command {
    Command {
        id: id.to_string(),
        idempotency_key: idempotency_key.to_string(),
        entity_id: entity_id.to_string(),
        command_class: command_class.to_string(),
        hazardous: false,
        payload: Some(prost_types::Any { type_url: "type.googleapis.com/altavista.v1.TestPayload".to_string(), value: vec![1, 2, 3, 4, 5] }),
        deadline_tai_ns: 9_999_999,
        not_before_tai_ns: 500,
        label: Some(Label { marking: "CUI".to_string(), caveats: vec!["NOFORN".to_string(), "FEDCON".to_string()] }),
        provenance: Some(Provenance {
            author_kind: AuthorKind::Agent as i32,
            principal: "model-x".to_string(),
            tool: "grpc_service.rs integration test".to_string(),
            config_hash: "config-hash-abc".to_string(),
            data_pack_hash: "data-pack-hash-def".to_string(),
            dataset_hash: "dataset-hash-ghi".to_string(),
            created_tai_ns: 42,
            run_id: "run-restart-test".to_string(),
            attributes: BTreeMap::new(),
        }),
        ..Command::default()
    }
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

/// R3.1's default test service-role table: `"dispatchers"` grants all four service RPCs
/// (`"dispatch"`/`"ack"`/`"expire"`/`"fail"`) -- disjoint from [`default_roles`]'s keys by
/// construction (`"dispatchers"` is not `"operators"` or `"burn-authorizers"`), matching
/// `av_command::authz::check_service_roles_disjoint`'s own requirement. [`TestServer::
/// mint_service`] mints tokens carrying this group by default.
fn default_service_roles() -> BTreeMap<String, Vec<String>> {
    let mut roles = BTreeMap::new();
    roles.insert("dispatchers".to_string(), vec!["dispatch".to_string(), "ack".to_string(), "expire".to_string(), "fail".to_string()]);
    roles
}

/// A running `CommandAuthorityServiceImpl` behind a real loopback socket, plus a real
/// `/admin/api/evidence*` HTTP server sharing the same [`Counters`] (R3.1), plus everything a
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
    /// R3.1: the same [`Counters`] instance the servicer records every refusal into --
    /// shared, not a second one, with the admin server below, exactly as
    /// `src/bin/av-command.rs` wires the real binary.
    counters: Arc<Counters>,
    /// R3.1: the real `/admin/api/evidence*` HTTP server's own bound address.
    admin_addr: SocketAddr,
    shutdown_tx: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
    /// R3.1: `av_command::admin::serve` has no graceful-shutdown signal of its own (unlike the
    /// gRPC server above) -- aborted, not joined, at [`Self::shutdown_keep_ledger`]. Aborting
    /// a task that is only ever `.await`ing `accept()` on a socket this test process owns
    /// leaves nothing else to clean up.
    admin_handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    /// Spawns over a **fresh** ledger directory, named `name` (wiped first if it somehow
    /// already exists -- see [`tmp_ledger_dir`]), with the default role table
    /// ([`default_roles`]), the default service-role table ([`default_service_roles`]), and
    /// two wildcard fixture delegations (`"delegation-1"` for `"operator-1"`, `"delegation-9"`
    /// for `"astronaut-jane"` -- see [`wildcard_delegation`]), no MFA methods configured. The
    /// overwhelming majority of tests want this.
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

    /// As [`Self::spawn_over_with_service_roles`], but with [`default_service_roles`] -- used
    /// by this file's dedicated A2.2 tests (wrong role, missing MFA, expired delegation, a
    /// delegation granting a class the role does not), none of which exercise R3.1's service
    /// role gate itself and so want the default granting table rather than repeating it.
    async fn spawn_over_with_authz(
        ledger_dir: PathBuf,
        start_tai_ns: i64,
        roles: BTreeMap<String, Vec<String>>,
        delegations: Vec<Delegation>,
        mfa_amr_methods: Vec<String>,
        mfa_acr: &str,
    ) -> Self {
        Self::spawn_over_with_service_roles(ledger_dir, start_tai_ns, roles, delegations, mfa_amr_methods, mfa_acr, default_service_roles()).await
    }

    /// The general constructor every other one delegates to: full control over the human role
    /// table, the delegation set, the MFA configuration (`mfa_amr_methods`/`mfa_acr`), and
    /// (R3.1) the service-role table -- used by this file's dedicated R3.1 tests (deny by
    /// default, a role granting only some RPCs, a purely human token, the disjointness-at-
    /// runtime shape).
    async fn spawn_over_with_service_roles(
        ledger_dir: PathBuf,
        start_tai_ns: i64,
        roles: BTreeMap<String, Vec<String>>,
        delegations: Vec<Delegation>,
        mfa_amr_methods: Vec<String>,
        mfa_acr: &str,
        service_roles: BTreeMap<String, Vec<String>>,
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
        let counters = Arc::new(Counters::new());
        let authz = AuthzConfig {
            role_table: Arc::new(RoleTable::from_config(&roles)),
            delegations: Arc::new(DelegationTable::from_delegations(delegations)),
            mfa_amr_methods: Arc::new(mfa_amr_methods),
            mfa_acr: Arc::new(mfa_acr.to_string()),
            audit,
            service_role_table: Arc::new(ServiceRoleTable::from_config(&service_roles).expect("this file's own service-role fixtures always use recognized rpc names")),
            counters: counters.clone(),
        };

        let servicer = CommandAuthorityServiceImpl::new(
            ledger.clone(),
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

        // R3.1: the real admin HTTP server, sharing the identical `counters` `Arc` the gRPC
        // servicer above records every refusal into -- so `/admin/api/evidence` can report a
        // refusal this test provokes over the real gRPC surface. `av_command::admin::serve`
        // binds its own listener (unlike the gRPC server above, which takes an already-bound
        // one) -- bind-then-drop-then-rebind to discover a free ephemeral port first; the
        // window between drop and rebind is negligible for a single local test process on
        // loopback.
        let admin_addr: SocketAddr = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port for the admin probe").local_addr().expect("local_addr");
        let admin_state = Arc::new(AdminState { ledger, run_id: "grpc-service-it".to_string(), version: "0.1.0".to_string(), counters: counters.clone() });
        let admin_handle = tokio::spawn(async move {
            let _ = admin::serve(admin_addr, admin_state).await;
        });

        Self { client, ledger_dir, clock, dispatch_sink, issuer, audit_path, counters, admin_addr, shutdown_tx, handle, admin_handle }
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

    /// R3.1: mints a real, fully-valid token carrying `groups` and nothing else notable (no
    /// `amr`/`acr` -- a service call is not a human-authorization gate, so this file's R3.1
    /// tests never need MFA claims) -- for a caller presenting a `service_token`. Defaults to
    /// `["dispatchers"]` via [`Self::mint_service`]'s own callers that want the default
    /// granting group; a test exercising the service-role gate itself calls
    /// [`Self::mint_with_claims`] directly with the specific groups it needs (including zero,
    /// for "no service role at all").
    fn mint_service(&self, sub: &str, groups: &[&str]) -> String {
        self.mint_with_claims(sub, groups, &[], "")
    }

    /// Every line in this server's audit sink file so far, in order -- read straight from
    /// disk, the real artifact `crate::audit::AuditWriter` wrote to.
    fn audit_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.audit_path).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
    }

    /// R3.1: a real `GET` against this server's own real admin HTTP surface -- `(status_line,
    /// body)`, mirroring `src/admin.rs`'s own private test helper of the identical shape
    /// (that one is unreachable from this file, a separate integration-test crate).
    async fn admin_get(&self, path: &str) -> (String, String) {
        let mut stream = TcpStream::connect(self.admin_addr).await.expect("connect to the real admin HTTP server");
        stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8(buf).unwrap();
        let mut parts = text.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap_or("").to_string();
        let body = parts.next().unwrap_or("").to_string();
        (head.lines().next().unwrap_or("").to_string(), body)
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
        // R3.1: av_command::admin::serve has no shutdown signal of its own -- see this
        // struct's own `admin_handle` doc comment for why abort (not join) is correct here.
        self.admin_handle.abort();
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

    let dispatch_token = server.mint_service("ground-segment-1", &["dispatchers"]);
    let dispatched = server.client.dispatch(DispatchRequest { command_id: "cmd-1".to_string(), service_token: dispatch_token }).await.expect("Dispatch").into_inner();
    let dispatched_command = dispatched.command.expect("command present");
    assert_eq!(dispatched_command.state, CommandState::Dispatched as i32);
    // R3.1: the recorded principal is the verified service_token subject, not a fixed string.
    assert_eq!(dispatched_command.transitions.last().unwrap().principal, "ground-segment-1");
    assert_eq!(server.dispatch_sink.dispatched().len(), 1);
    assert_eq!(server.dispatch_sink.dispatched()[0].id, "cmd-1");

    let ack_token = server.mint_service("flight-software", &["dispatchers"]);
    let acked = server
        .client
        .ack(AckRequest {
            command_id: "cmd-1".to_string(),
            ack_level: AckLevel::AssetExecuted as i32,
            principal: "flight-software".to_string(),
            reason: "executed".to_string(),
            service_token: ack_token,
        })
        .await
        .expect("Ack")
        .into_inner();
    let acked_command = acked.command.expect("command present");
    assert_eq!(acked_command.state, CommandState::Acked as i32);
    assert_eq!(acked_command.transitions.last().unwrap().ack_level, AckLevel::AssetExecuted as i32);
    // R3.1: the verified service_token subject, not the declared label, is authoritative.
    assert_eq!(acked_command.transitions.last().unwrap().principal, "flight-software");

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

    // Dispatch-before-Authorize. A verifiable, granting service_token (R3.1 verifies the
    // service principal before attempting the state edge, exactly as A2.1 does for
    // Authorize's own principal_token above) -- this must fail on the *state machine's* own
    // edge check, not on service-principal verification, or this test would no longer be
    // testing what its name says.
    server.client.propose(propose_request(base_command("cmd-b", "sat-1", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-b".to_string() }).await.unwrap();
    let dispatch_token = server.mint_service("ground-segment-1", &["dispatchers"]);
    let err = server
        .client
        .dispatch(DispatchRequest { command_id: "cmd-b".to_string(), service_token: dispatch_token })
        .await
        .expect_err("Dispatch before Authorize must be refused");
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
    let ack_token = server.mint_service("flight-software", &["dispatchers"]);
    let err = server
        .client
        .ack(AckRequest {
            command_id: "cmd-c".to_string(),
            ack_level: AckLevel::AssetExecuted as i32,
            principal: "flight-software".to_string(),
            reason: "r".to_string(),
            service_token: ack_token,
        })
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

    let dispatch_token = server.mint_service("ground-segment-1", &["dispatchers"]);
    let first = server
        .client
        .dispatch(DispatchRequest { command_id: "cmd-x".to_string(), service_token: dispatch_token })
        .await
        .expect("the first dispatch of this key succeeds")
        .into_inner();
    assert_eq!(first.command.unwrap().state, CommandState::Dispatched as i32);

    let records_before = read_ledger_records(&server.ledger_dir, "sat-1").len();

    let dispatch_token2 = server.mint_service("ground-segment-1", &["dispatchers"]);
    let err = server
        .client
        .dispatch(DispatchRequest { command_id: "cmd-y".to_string(), service_token: dispatch_token2 })
        .await
        .expect_err("a second command sharing the dispatched key must be refused");
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
    let dispatch_token1 = server1.mint_service("ground-segment-1", &["dispatchers"]);
    let dispatched = server1
        .client
        .dispatch(DispatchRequest { command_id: "cmd-r1".to_string(), service_token: dispatch_token1 })
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

    let dispatch_token2 = server2.mint_service("ground-segment-1", &["dispatchers"]);
    let err = server2
        .client
        .dispatch(DispatchRequest { command_id: "cmd-r2".to_string(), service_token: dispatch_token2 })
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

/// `Query` for a `command_id` this service's index has never held (never proposed, in this
/// process or any prior one) is still refused `NOT_FOUND` -- the plain gRPC meaning
/// (`crate::service`'s own module doc, "Unknown `command_id`") -- exactly the same typed
/// refusal it was before question 203(a)'s restart fix (rebuilding `commands` from the ledger
/// does not turn "genuinely never seen" into anything other than `NOT_FOUND`).
#[tokio::test]
async fn query_for_an_unknown_id_is_still_not_found() {
    let mut server = TestServer::spawn("query-unknown", 1_000).await;

    let err = server
        .client
        .query(QueryRequest { selector: Some(Selector::CommandId("cmd-never-seen".to_string())) })
        .await
        .expect_err("an id this service has never held must be refused, not answered");
    assert_eq!(err.code(), Code::NotFound, "{err:?}");
    assert!(err.message().contains("cmd-never-seen"), "{}", err.message());

    server.shutdown().await;
}

/// **The cross-restart acceptance test for question 203(a).** Drives a command with a full,
/// non-default payload/deadline/not_before/label/provenance through `Propose` -> `Check` ->
/// `Authorize` -> `Dispatch` over the real gRPC surface, drops that `TestServer` (keeping its
/// ledger directory), builds a **second**, independent `TestServer` over the exact same
/// directory (a fresh `Ledger` handle, a fresh `CommandAuthorityServiceImpl`, a fresh
/// in-memory `commands` `BTreeMap` before `new` rebuilds it), and asserts `Query` on the
/// second instance returns the **full** `Command` field-for-field equal to what the first
/// instance's own `Dispatch` response already returned -- not merely a `Command` sharing the
/// same `id`/`state`, which the unfixed code (an empty `commands` map at construction) could
/// never have produced at all: before this fix, this exact `Query` call was refused
/// `NOT_FOUND`, full stop.
#[tokio::test]
async fn query_across_a_restart_returns_the_full_command_field_for_field() {
    let command_id = "cmd-restart-q";
    let entity_id = "sat-restart-q";

    let mut server1 = TestServer::spawn("query-restart", 1_000).await;
    server1.client.propose(propose_request(full_command(command_id, entity_id, "mode", "idem-restart-q"), "model-x")).await.unwrap();
    server1.client.check(CheckRequest { command_id: command_id.to_string() }).await.unwrap();
    let token1 = server1.mint("operator-1");
    server1
        .client
        .authorize(AuthorizeRequest { command_id: command_id.to_string(), principal_token: token1, delegation_id: String::new() })
        .await
        .unwrap();
    let dispatch_token = server1.mint_service("ground-segment-1", &["dispatchers"]);
    let dispatched = server1
        .client
        .dispatch(DispatchRequest { command_id: command_id.to_string(), service_token: dispatch_token })
        .await
        .expect("the first process lifetime's dispatch succeeds")
        .into_inner();
    let expected = dispatched.command.expect("Dispatch always returns the command");
    assert_eq!(expected.state, CommandState::Dispatched as i32, "sanity: reached DISPATCHED before the restart");
    assert_eq!(expected.transitions.len(), 4, "sanity: PROPOSED, CHECKED, AUTHORIZED, DISPATCHED all recorded before the restart");

    let ledger_dir = server1.shutdown_keep_ledger().await;

    // A second, independent process lifetime: a fresh Ledger handle, a fresh
    // CommandAuthorityServiceImpl, over the SAME on-disk ledger directory. Its own in-memory
    // `commands` map starts empty and must be rebuilt by `new` (`Ledger::scan_commands`)
    // before this Query can answer at all.
    let mut server2 = TestServer::spawn_over(ledger_dir.clone(), 2_000).await;
    let queried = server2
        .client
        .query(QueryRequest { selector: Some(Selector::CommandId(command_id.to_string())) })
        .await
        .expect("Query must answer for a command a PRIOR process lifetime proposed, after this fix")
        .into_inner();
    assert_eq!(queried.commands.len(), 1, "{queried:?}");
    let actual = queried.commands.into_iter().next().unwrap();

    // Exactly the fields the old, narrow LedgerRecord could not carry (no payload, no
    // deadline_tai_ns, no not_before_tai_ns, no label, no provenance) -- asserted individually
    // so a partial reconstruction (e.g. id/state right, everything else defaulted) is caught,
    // not just "some Command with this id came back".
    assert_eq!(actual.payload, expected.payload, "payload must survive the restart");
    assert_eq!(actual.deadline_tai_ns, expected.deadline_tai_ns, "deadline_tai_ns must survive the restart");
    assert_eq!(actual.not_before_tai_ns, expected.not_before_tai_ns, "not_before_tai_ns must survive the restart");
    assert_eq!(actual.label, expected.label, "label must survive the restart");
    assert_eq!(actual.provenance, expected.provenance, "provenance must survive the restart");
    assert_eq!(actual.envelope_id, expected.envelope_id, "envelope_id must survive the restart");

    // And the whole message, field-for-field -- the strongest form of this assertion: the
    // second process lifetime's Query answer is not merely "close enough" to the first
    // process's own Dispatch response, it is identical.
    assert_eq!(actual, expected, "Query across a restart must return the exact same Command the first process lifetime already produced");

    server2.shutdown().await;
}

/// **R3.5a's own acceptance test**: `Propose`'s `rationale`/`evidence_ids` -- previously
/// accepted on the wire and never persisted anywhere -- now come back through `Query`'s new
/// `proposals` map, and `Check`'s own `PolicyDecision` comes back through the new `decisions`
/// map, for both an allowed and a denied command. Then a **second** `TestServer` over the
/// same ledger directory (a real process restart, mirroring
/// `query_across_a_restart_returns_the_full_command_field_for_field` above) proves both maps
/// survive it, exactly like `commands` already does.
#[tokio::test]
async fn query_returns_the_proposal_rationale_and_evidence_and_the_policy_decision_surviving_a_restart() {
    let entity_id = "sat-proposal-q";

    let mut server1 = TestServer::spawn("proposal-decision-query", 1_000).await;

    // "mode" is unconditionally allowed by the shipped policy (profiles/policies/authority/
    // command.rego) -- an ALLOW decision.
    let allowed_id = "cmd-allowed";
    let allowed_request = ProposeRequest {
        proposal: Some(CommandProposal {
            command: Some(base_command(allowed_id, entity_id, "mode", "")),
            rationale: "scored radius drifted past threshold".to_string(),
            evidence_ids: vec!["run-1/query-7".to_string(), "run-1/query-9".to_string()],
        }),
        principal: "model-x".to_string(),
    };
    server1.client.propose(allowed_request).await.expect("propose the allowed command");
    let checked = server1.client.check(CheckRequest { command_id: allowed_id.to_string() }).await.expect("check the allowed command").into_inner();
    let expected_decision = checked.decision.expect("Check always returns a decision");
    assert!(expected_decision.allow, "sanity: mode is unconditionally allowed by the shipped policy");

    // "payload" is unconditionally denied by the shipped policy -- a DENY decision, which
    // must be exactly as recoverable from Query as an allow (LedgerRecord.decision's own doc
    // comment: "a denial must be as reproducible from the ledger as an approval").
    let denied_id = "cmd-denied";
    let denied_request = ProposeRequest {
        proposal: Some(CommandProposal {
            command: Some(base_command(denied_id, entity_id, "payload", "")),
            rationale: "flagged payload for review".to_string(),
            evidence_ids: vec!["run-2/query-3".to_string()],
        }),
        principal: "model-y".to_string(),
    };
    server1.client.propose(denied_request).await.expect("propose the denied command");
    let rejected = server1.client.check(CheckRequest { command_id: denied_id.to_string() }).await.expect("check the denied command").into_inner();
    let expected_rejected_decision = rejected.decision.expect("Check always returns a decision, allow or deny");
    assert!(!expected_rejected_decision.allow, "sanity: payload is unconditionally denied by the shipped policy");

    // A third, still-PROPOSED command -- its proposal must be visible even though it was
    // never Checked, and it must have no entry in `decisions` at all.
    let proposed_only_id = "cmd-proposed-only";
    let proposed_only_request = ProposeRequest {
        proposal: Some(CommandProposal { command: Some(base_command(proposed_only_id, entity_id, "mode", "")), rationale: "awaiting review".to_string(), evidence_ids: vec![] }),
        principal: "model-z".to_string(),
    };
    server1.client.propose(proposed_only_request).await.expect("propose the still-PROPOSED command");

    let assert_query_response = |queried: &av_cdm::pb::QueryResponse| {
        let allowed_proposal = queried.proposals.get(allowed_id).expect("the allowed command's proposal must be in the map");
        assert_eq!(allowed_proposal.rationale, "scored radius drifted past threshold");
        assert_eq!(allowed_proposal.evidence_ids, vec!["run-1/query-7".to_string(), "run-1/query-9".to_string()]);

        let denied_proposal = queried.proposals.get(denied_id).expect("the denied command's proposal must be in the map too");
        assert_eq!(denied_proposal.rationale, "flagged payload for review");
        assert_eq!(denied_proposal.evidence_ids, vec!["run-2/query-3".to_string()]);

        let proposed_only_proposal = queried.proposals.get(proposed_only_id).expect("a still-PROPOSED command's proposal must be in the map too");
        assert_eq!(proposed_only_proposal.rationale, "awaiting review");

        let allow_decision = queried.decisions.get(allowed_id).expect("the allowed command's decision must be in the map");
        assert_eq!(allow_decision.decision_id, expected_decision.decision_id);
        assert_eq!(allow_decision.policy_hash, expected_decision.policy_hash);
        assert!(allow_decision.allow);

        let deny_decision = queried.decisions.get(denied_id).expect("the denied command's decision must be in the map too");
        assert_eq!(deny_decision.decision_id, expected_rejected_decision.decision_id);
        assert!(!deny_decision.allow);
        assert_eq!(deny_decision.reasons, expected_rejected_decision.reasons);

        assert!(!queried.decisions.contains_key(proposed_only_id), "a still-PROPOSED command has no policy decision yet");
    };

    let queried1 = server1
        .client
        .query(QueryRequest { selector: Some(Selector::Entity(QueryByEntity { entity_id: entity_id.to_string(), state_filter: CommandState::Unspecified as i32 })) })
        .await
        .expect("Query by entity")
        .into_inner();
    assert_eq!(queried1.commands.len(), 3, "{queried1:?}");
    assert_query_response(&queried1);

    // QueryByEntity with a PROPOSED filter must return exactly the still-PROPOSED command's
    // own proposal, not the other two's.
    let proposed_filtered = server1
        .client
        .query(QueryRequest { selector: Some(Selector::Entity(QueryByEntity { entity_id: entity_id.to_string(), state_filter: CommandState::Proposed as i32 })) })
        .await
        .expect("Query by entity, filtered to PROPOSED")
        .into_inner();
    assert_eq!(proposed_filtered.commands.len(), 1, "{proposed_filtered:?}");
    assert_eq!(proposed_filtered.commands[0].id, proposed_only_id);
    assert_eq!(proposed_filtered.proposals.len(), 1, "{proposed_filtered:?}");
    assert_eq!(proposed_filtered.proposals.get(proposed_only_id).unwrap().rationale, "awaiting review");
    assert!(proposed_filtered.decisions.is_empty());

    // The restart proof: a second, independent TestServer over the exact same ledger
    // directory must answer the identical proposals/decisions from the ledger alone.
    let ledger_dir = server1.shutdown_keep_ledger().await;
    let mut server2 = TestServer::spawn_over(ledger_dir.clone(), 2_000).await;
    let queried2 = server2
        .client
        .query(QueryRequest { selector: Some(Selector::Entity(QueryByEntity { entity_id: entity_id.to_string(), state_filter: CommandState::Unspecified as i32 })) })
        .await
        .expect("Query by entity, after a restart")
        .into_inner();
    assert_eq!(queried2.commands.len(), 3, "{queried2:?}");
    assert_query_response(&queried2);

    server2.shutdown().await;
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

// =============================================================================================
// R3.1: service principals on Dispatch, Ack, Expire and Fail (`docs/aiplane-plan.md` round 2's
// declared gap; `docs/open-questions.md` question 206's open item). Every test below drives
// the real service over the real loopback socket `TestServer` already sets up; every refusal
// is asserted against a real `tonic::Status` AND a real counted refusal (`server.counters`,
// the identical `Arc<Counters>` the servicer itself records into -- never an exit code,
// question 148).
// =============================================================================================

/// R3.1's own service-role fixture, richer than [`default_service_roles`]: `"dispatchers"`
/// grants all four RPCs (the success-path fixture every test below's scenario 1 uses);
/// `"dispatch-only"`/`"ack-only"` each grant exactly one RPC, used as the "verified service
/// token whose role does not list this RPC" fixture (scenario 5) for the other three RPCs.
fn service_roles_fixture() -> BTreeMap<String, Vec<String>> {
    let mut m = BTreeMap::new();
    m.insert("dispatchers".to_string(), vec!["dispatch".to_string(), "ack".to_string(), "expire".to_string(), "fail".to_string()]);
    m.insert("dispatch-only".to_string(), vec!["dispatch".to_string()]);
    m.insert("ack-only".to_string(), vec!["ack".to_string()]);
    m
}

async fn spawn_r31_server(name: &str) -> TestServer {
    TestServer::spawn_over_with_service_roles(tmp_ledger_dir(name), 1_000, default_roles(), vec![], vec![], "", service_roles_fixture()).await
}

/// Proposes, checks and authorizes a fresh `"mode"`-class command, returning its id at
/// `AUTHORIZED` -- the precondition every RPC below's success scenario needs (`Dispatch`
/// directly; `Ack`/`Expire`/`Fail` via [`dispatched_command`], one real `Dispatch` further).
async fn authorized_command(server: &mut TestServer, id: &str, entity_id: &str) -> String {
    server.client.propose(propose_request(base_command(id, entity_id, "mode", ""), "model-x")).await.expect("Propose");
    server.client.check(CheckRequest { command_id: id.to_string() }).await.expect("Check");
    let token = server.mint("operator-1");
    server.client.authorize(AuthorizeRequest { command_id: id.to_string(), principal_token: token, delegation_id: String::new() }).await.expect("Authorize");
    id.to_string()
}

/// As [`authorized_command`], then one real, successfully-granted `Dispatch` -- the
/// precondition `Ack`/`Expire`/`Fail`'s own success scenario needs.
async fn dispatched_command(server: &mut TestServer, id: &str, entity_id: &str) -> String {
    authorized_command(server, id, entity_id).await;
    let token = server.mint_service("ground-segment-1", &["dispatchers"]);
    server.client.dispatch(DispatchRequest { command_id: id.to_string(), service_token: token }).await.expect("Dispatch");
    id.to_string()
}

/// A fresh, merely-`PROPOSED` command id -- all that scenarios 2-5 below need (an unverified/
/// under-scoped `service_token` is refused *before* the state-machine edge is ever consulted,
/// so these scenarios do not need the command in any particular precondition state).
async fn proposed_command(server: &mut TestServer, id: &str, entity_id: &str) -> String {
    server.client.propose(propose_request(base_command(id, entity_id, "mode", ""), "model-x")).await.expect("Propose");
    id.to_string()
}

/// **`Dispatch`, all five required scenarios.**
#[tokio::test]
async fn dispatch_service_principal_acceptance_and_refusals() {
    let mut server = spawn_r31_server("r31-dispatch").await;

    // 1: a valid service token with a granting role succeeds, and the ledger's transition
    // records the verified service `sub` as its principal.
    let id = authorized_command(&mut server, "d1", "sat-d1").await;
    let token = server.mint_service("ground-segment-1", &["dispatchers"]);
    let dispatched = server.client.dispatch(DispatchRequest { command_id: id.clone(), service_token: token }).await.expect("granting service token must succeed").into_inner();
    assert_eq!(dispatched.command.as_ref().unwrap().state, CommandState::Dispatched as i32);
    assert_eq!(dispatched.command.unwrap().transitions.last().unwrap().principal, "ground-segment-1");
    let records = read_ledger_records(&server.ledger_dir, "sat-d1");
    let dispatched_record = records.iter().find(|r| r.transition.as_ref().unwrap().state == CommandState::Dispatched as i32).unwrap();
    assert_eq!(dispatched_record.transition.as_ref().unwrap().principal, "ground-segment-1", "the real, durable ledger record too, not only the response");

    // 2: no token at all -- refused Unauthenticated, counted.
    let id = proposed_command(&mut server, "d2", "sat-d2").await;
    let before = server.counters.get("token_wrong_segment_count");
    let err = server.client.dispatch(DispatchRequest { command_id: id, service_token: String::new() }).await.expect_err("an empty service_token must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");
    assert_eq!(server.counters.get("token_wrong_segment_count"), before + 1);

    // 3a: a token that fails verification -- expired.
    let id = proposed_command(&mut server, "d3a", "sat-d3a").await;
    let token = server.mint_service("ground-segment-1", &["dispatchers"]);
    let exp_tai_ns = Tai::from_utc_nanos((TOKEN_NOW_UNIX_S + TOKEN_TTL_S) * 1_000_000_000).as_nanos();
    server.clock.set(exp_tai_ns); // now_tai_ns == exp_tai_ns: expired at the boundary
    let before = server.counters.get("token_expired");
    let err = server.client.dispatch(DispatchRequest { command_id: id, service_token: token }).await.expect_err("an expired service token must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");
    assert_eq!(server.counters.get("token_expired"), before + 1);
    server.clock.set(1_000); // restore, for the remaining scenarios below

    // 3b: a token that fails verification -- bad signature (a different issuer's key entirely).
    let id = proposed_command(&mut server, "d3b", "sat-d3b").await;
    let wrong_issuer = TestIssuer::new();
    let bad_token = wrong_issuer.mint(&claims_with_roles_and_mfa(TEST_ISSUER, TEST_AUDIENCE, "ground-segment-1", TOKEN_NOW_UNIX_S, TOKEN_TTL_S, RoleAndMfaClaims { groups: &["dispatchers"], amr: &[], acr: "" }));
    let before = server.counters.get("token_signature_invalid");
    let err = server.client.dispatch(DispatchRequest { command_id: id, service_token: bad_token }).await.expect_err("a token signed by the wrong key must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");
    assert_eq!(server.counters.get("token_signature_invalid"), before + 1);

    // 4: a verified HUMAN token (its groups grant a human command class via `authority.roles`,
    // but no service role at all) is refused -- this is the test that proves "service
    // principal" means something.
    let id = proposed_command(&mut server, "d4", "sat-d4").await;
    let human_token = server.mint_service("operator-1", &["operators"]);
    let before = server.counters.get("service_role_not_granted");
    let err = server.client.dispatch(DispatchRequest { command_id: id, service_token: human_token }).await.expect_err("a purely human token must be refused on Dispatch");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    assert!(err.message().contains("service role gate refused"), "{}", err.message());
    assert_eq!(server.counters.get("service_role_not_granted"), before + 1);

    // 5: a verified service token whose role does not list this RPC ("ack-only" grants only
    // "ack").
    let id = proposed_command(&mut server, "d5", "sat-d5").await;
    let scoped_token = server.mint_service("svc-ack-only", &["ack-only"]);
    let before = server.counters.get("service_role_not_granted");
    let err = server.client.dispatch(DispatchRequest { command_id: id, service_token: scoped_token }).await.expect_err("ack-only must not grant dispatch");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    assert_eq!(server.counters.get("service_role_not_granted"), before + 1);

    server.shutdown().await;
}

/// **`Ack`, all five required scenarios, plus the `principal`-disagreement rule.**
#[tokio::test]
async fn ack_service_principal_acceptance_and_refusals() {
    let mut server = spawn_r31_server("r31-ack").await;

    // 1: valid + granting role succeeds; the verified sub lands as principal, not the
    // caller-declared label (which agrees with it here).
    let id = dispatched_command(&mut server, "a1", "sat-a1").await;
    let token = server.mint_service("flight-software", &["dispatchers"]);
    let acked = server
        .client
        .ack(AckRequest { command_id: id.clone(), ack_level: AckLevel::AssetExecuted as i32, principal: "flight-software".to_string(), reason: "executed".to_string(), service_token: token })
        .await
        .expect("granting service token must succeed")
        .into_inner();
    assert_eq!(acked.command.unwrap().transitions.last().unwrap().principal, "flight-software");

    // 2: no token.
    let id = dispatched_command(&mut server, "a2", "sat-a2").await;
    let before = server.counters.get("token_wrong_segment_count");
    let err = server
        .client
        .ack(AckRequest { command_id: id, ack_level: AckLevel::AssetExecuted as i32, principal: String::new(), reason: "r".to_string(), service_token: String::new() })
        .await
        .expect_err("an empty service_token must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");
    assert_eq!(server.counters.get("token_wrong_segment_count"), before + 1);

    // 3: a tampered signature.
    let id = dispatched_command(&mut server, "a3", "sat-a3").await;
    let token = server.mint_service("flight-software", &["dispatchers"]);
    let parts: Vec<&str> = token.split('.').collect();
    let mut sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
    sig[0] ^= 0xFF;
    let tampered = format!("{}.{}.{}", parts[0], parts[1], base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig));
    let before = server.counters.get("token_signature_invalid");
    let err = server
        .client
        .ack(AckRequest { command_id: id, ack_level: AckLevel::AssetExecuted as i32, principal: String::new(), reason: "r".to_string(), service_token: tampered })
        .await
        .expect_err("a tampered signature must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");
    assert_eq!(server.counters.get("token_signature_invalid"), before + 1);

    // 4: a purely human token.
    let id = dispatched_command(&mut server, "a4", "sat-a4").await;
    let human_token = server.mint_service("operator-1", &["operators"]);
    let err = server
        .client
        .ack(AckRequest { command_id: id, ack_level: AckLevel::AssetExecuted as i32, principal: String::new(), reason: "r".to_string(), service_token: human_token })
        .await
        .expect_err("a purely human token must be refused on Ack");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");

    // 5: a service token scoped to a different rpc ("dispatch-only" grants only "dispatch").
    let id = dispatched_command(&mut server, "a5", "sat-a5").await;
    let scoped_token = server.mint_service("svc-dispatch-only", &["dispatch-only"]);
    let err = server
        .client
        .ack(AckRequest { command_id: id, ack_level: AckLevel::AssetExecuted as i32, principal: String::new(), reason: "r".to_string(), service_token: scoped_token })
        .await
        .expect_err("dispatch-only must not grant ack");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");

    // The principal-disagreement rule: a declared principal that disagrees with the verified
    // service subject is refused INVALID_ARGUMENT and counted, never silently overridden.
    let id = dispatched_command(&mut server, "a6", "sat-a6").await;
    let token = server.mint_service("flight-software", &["dispatchers"]);
    let before = server.counters.get("principal_mismatch");
    let err = server
        .client
        .ack(AckRequest { command_id: id.clone(), ack_level: AckLevel::AssetExecuted as i32, principal: "someone-else".to_string(), reason: "r".to_string(), service_token: token })
        .await
        .expect_err("a declared principal disagreeing with the verified subject must be refused");
    assert_eq!(err.code(), Code::InvalidArgument, "{err:?}");
    assert!(err.message().contains("someone-else"), "{}", err.message());
    assert!(err.message().contains("flight-software"), "{}", err.message());
    assert_eq!(server.counters.get("principal_mismatch"), before + 1);
    // The command itself is unaffected by the refused attempt -- still DISPATCHED.
    let queried = server.client.query(QueryRequest { selector: Some(Selector::CommandId(id)) }).await.expect("Query").into_inner();
    assert_eq!(queried.commands[0].state, CommandState::Dispatched as i32);

    // An EMPTY declared principal is never a disagreement -- accepted, and the verified sub
    // still lands as CommandTransition.principal.
    let id = dispatched_command(&mut server, "a7", "sat-a7").await;
    let token = server.mint_service("flight-software", &["dispatchers"]);
    let acked = server
        .client
        .ack(AckRequest { command_id: id, ack_level: AckLevel::AssetExecuted as i32, principal: String::new(), reason: "r".to_string(), service_token: token })
        .await
        .expect("an empty declared principal must never be refused")
        .into_inner();
    assert_eq!(acked.command.unwrap().transitions.last().unwrap().principal, "flight-software");

    server.shutdown().await;
}

/// **`Expire`, all five required scenarios.** `Expire`'s legal source states are `AUTHORIZED`
/// and `DISPATCHED`; scenarios 2-5 use a merely-`PROPOSED` command (the refusal happens before
/// the state edge is ever consulted, see [`proposed_command`]'s own doc), and scenario 1 uses
/// an `AUTHORIZED` one (the cheaper of the two legal source states to reach).
#[tokio::test]
async fn expire_service_principal_acceptance_and_refusals() {
    let mut server = spawn_r31_server("r31-expire").await;

    let id = authorized_command(&mut server, "e1", "sat-e1").await;
    let token = server.mint_service("kernel-binding-1", &["dispatchers"]);
    let expired = server
        .client
        .expire(ExpireRequest { command_id: id.clone(), reason: "deadline passed".to_string(), principal: "kernel-binding-1".to_string(), service_token: token })
        .await
        .expect("granting service token must succeed")
        .into_inner();
    assert_eq!(expired.command.as_ref().unwrap().state, CommandState::Expired as i32);
    assert_eq!(expired.command.unwrap().transitions.last().unwrap().principal, "kernel-binding-1");
    let records = read_ledger_records(&server.ledger_dir, "sat-e1");
    let expired_record = records.iter().find(|r| r.transition.as_ref().unwrap().state == CommandState::Expired as i32).unwrap();
    assert_eq!(expired_record.transition.as_ref().unwrap().principal, "kernel-binding-1");

    let id = proposed_command(&mut server, "e2", "sat-e2").await;
    let err = server
        .client
        .expire(ExpireRequest { command_id: id, reason: "r".to_string(), principal: String::new(), service_token: String::new() })
        .await
        .expect_err("an empty service_token must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");

    let id = proposed_command(&mut server, "e3", "sat-e3").await;
    let wrong_issuer = TestIssuer::new();
    let bad_token = wrong_issuer.mint(&claims_with_roles_and_mfa(TEST_ISSUER, TEST_AUDIENCE, "kernel-binding-1", TOKEN_NOW_UNIX_S, TOKEN_TTL_S, RoleAndMfaClaims { groups: &["dispatchers"], amr: &[], acr: "" }));
    let err = server
        .client
        .expire(ExpireRequest { command_id: id, reason: "r".to_string(), principal: String::new(), service_token: bad_token })
        .await
        .expect_err("a token signed by the wrong key must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");

    let id = proposed_command(&mut server, "e4", "sat-e4").await;
    let human_token = server.mint_service("operator-1", &["operators"]);
    let err = server
        .client
        .expire(ExpireRequest { command_id: id, reason: "r".to_string(), principal: String::new(), service_token: human_token })
        .await
        .expect_err("a purely human token must be refused on Expire");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");

    let id = proposed_command(&mut server, "e5", "sat-e5").await;
    let scoped_token = server.mint_service("svc-dispatch-only", &["dispatch-only"]);
    let err = server
        .client
        .expire(ExpireRequest { command_id: id, reason: "r".to_string(), principal: String::new(), service_token: scoped_token })
        .await
        .expect_err("dispatch-only must not grant expire");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");

    // The principal-disagreement rule, for Expire too (the identical rule Ack's own test pins
    // in full; asserted here once more to prove it is not Ack-specific).
    let id = authorized_command(&mut server, "e6", "sat-e6").await;
    let token = server.mint_service("kernel-binding-1", &["dispatchers"]);
    let err = server
        .client
        .expire(ExpireRequest { command_id: id, reason: "r".to_string(), principal: "someone-else".to_string(), service_token: token })
        .await
        .expect_err("a disagreeing declared principal must be refused on Expire too");
    assert_eq!(err.code(), Code::InvalidArgument, "{err:?}");

    server.shutdown().await;
}

/// **`Fail`, all five required scenarios.** `Fail`'s only legal source state is `DISPATCHED`;
/// scenario 1 uses [`dispatched_command`], scenarios 2-5 a merely-`PROPOSED` one (see
/// [`proposed_command`]'s own doc).
#[tokio::test]
async fn fail_service_principal_acceptance_and_refusals() {
    let mut server = spawn_r31_server("r31-fail").await;

    let id = dispatched_command(&mut server, "f1", "sat-f1").await;
    let token = server.mint_service("kernel-binding-1", &["dispatchers"]);
    let failed = server
        .client
        .fail(FailRequest { command_id: id.clone(), reason: "kernel refused".to_string(), principal: "kernel-binding-1".to_string(), service_token: token })
        .await
        .expect("granting service token must succeed")
        .into_inner();
    assert_eq!(failed.command.as_ref().unwrap().state, CommandState::Failed as i32);
    assert_eq!(failed.command.unwrap().transitions.last().unwrap().principal, "kernel-binding-1");
    let records = read_ledger_records(&server.ledger_dir, "sat-f1");
    let failed_record = records.iter().find(|r| r.transition.as_ref().unwrap().state == CommandState::Failed as i32).unwrap();
    assert_eq!(failed_record.transition.as_ref().unwrap().principal, "kernel-binding-1");

    let id = proposed_command(&mut server, "f2", "sat-f2").await;
    let err = server
        .client
        .fail(FailRequest { command_id: id, reason: "r".to_string(), principal: String::new(), service_token: String::new() })
        .await
        .expect_err("an empty service_token must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");

    let id = proposed_command(&mut server, "f3", "sat-f3").await;
    let wrong_issuer = TestIssuer::new();
    let bad_token = wrong_issuer.mint(&claims_with_roles_and_mfa(TEST_ISSUER, TEST_AUDIENCE, "kernel-binding-1", TOKEN_NOW_UNIX_S, TOKEN_TTL_S, RoleAndMfaClaims { groups: &["dispatchers"], amr: &[], acr: "" }));
    let err = server
        .client
        .fail(FailRequest { command_id: id, reason: "r".to_string(), principal: String::new(), service_token: bad_token })
        .await
        .expect_err("a token signed by the wrong key must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");

    let id = proposed_command(&mut server, "f4", "sat-f4").await;
    let human_token = server.mint_service("operator-1", &["operators"]);
    let err = server
        .client
        .fail(FailRequest { command_id: id, reason: "r".to_string(), principal: String::new(), service_token: human_token })
        .await
        .expect_err("a purely human token must be refused on Fail");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");

    let id = proposed_command(&mut server, "f5", "sat-f5").await;
    let scoped_token = server.mint_service("svc-dispatch-only", &["dispatch-only"]);
    let err = server
        .client
        .fail(FailRequest { command_id: id, reason: "r".to_string(), principal: String::new(), service_token: scoped_token })
        .await
        .expect_err("dispatch-only must not grant fail");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");

    let id = dispatched_command(&mut server, "f6", "sat-f6").await;
    let token = server.mint_service("kernel-binding-1", &["dispatchers"]);
    let err = server
        .client
        .fail(FailRequest { command_id: id, reason: "r".to_string(), principal: "someone-else".to_string(), service_token: token })
        .await
        .expect_err("a disagreeing declared principal must be refused on Fail too");
    assert_eq!(err.code(), Code::InvalidArgument, "{err:?}");

    server.shutdown().await;
}

/// **The refusal message never contains the raw `service_token` or its raw signature bytes**
/// -- extends `authorize_refusal_message_never_contains_the_token_or_signature`'s identical
/// guarantee (this file's Acceptance test 10) to the new `service_token` verification path;
/// same guarantee, never weakened.
#[tokio::test]
async fn dispatch_refusal_message_never_contains_the_service_token_or_signature() {
    let mut server = spawn_r31_server("r31-no-leak").await;
    let id = proposed_command(&mut server, "leak1", "sat-leak").await;

    let bad_token = server.issuer.mint(&claims_with_roles_and_mfa(TEST_ISSUER, "some-other-audience", "ground-segment-1", TOKEN_NOW_UNIX_S, TOKEN_TTL_S, RoleAndMfaClaims { groups: &["dispatchers"], amr: &[], acr: "" }));
    let signature_b64 = bad_token.rsplit_once('.').expect("a JWS has a signature segment").1.to_string();

    let err = server.client.dispatch(DispatchRequest { command_id: id, service_token: bad_token.clone() }).await.expect_err("a wrong-audience service token must be refused");
    assert_eq!(err.code(), Code::Unauthenticated, "{err:?}");
    assert!(!err.message().contains(bad_token.as_str()), "refusal message must not contain the raw token: {}", err.message());
    assert!(!err.message().contains(signature_b64.as_str()), "refusal message must not contain the raw signature: {}", err.message());

    server.shutdown().await;
}

/// **The admin endpoint reporting counts**: `GET /admin/api/evidence` (the real HTTP surface,
/// `src/admin.rs`) reports a refusal this test just provoked over the real gRPC surface --
/// counters are observable evidence, not merely in-memory state invisible outside this
/// process.
#[tokio::test]
async fn admin_evidence_endpoint_reports_a_refusal_it_just_provoked() {
    let mut server = spawn_r31_server("r31-admin-evidence").await;
    let id = proposed_command(&mut server, "admin1", "sat-admin").await;

    let human_token = server.mint_service("operator-1", &["operators"]);
    let err = server.client.dispatch(DispatchRequest { command_id: id, service_token: human_token }).await.expect_err("a purely human token must be refused");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");

    let (status, body) = server.admin_get("/admin/api/evidence").await;
    assert_eq!(status, "HTTP/1.1 200 OK");
    let json: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");
    assert_eq!(json["refusals"]["service_role_not_granted"], 1, "{json:#?}");

    server.shutdown().await;
}

// =================================================================================================
// R3.1, the manager's review: two properties the task's own tests did not pin.
// =================================================================================================

/// **The existence oracle is closed.** With the command lookup ahead of authentication, a
/// caller presenting *no* credential at all could tell `NOT_FOUND` (this id has never been
/// proposed) from `UNAUTHENTICATED` (it has), and so enumerate the ids this service holds.
/// Authentication now happens first on all four service RPCs, so an unauthenticated caller
/// gets the same answer either way -- asserted here over BOTH an id that exists and one that
/// does not, for each of the four, because a test on only one of the two proves nothing about
/// the pair being indistinguishable.
#[tokio::test]
async fn an_unauthenticated_service_call_cannot_distinguish_an_existing_command_from_a_missing_one() {
    let mut server = spawn_r31_server("r31-no-oracle").await;
    let existing = proposed_command(&mut server, "oracle-exists", "sat-oracle").await;
    let missing = "oracle-does-not-exist".to_string();

    for id in [existing, missing] {
        let d = server.client.dispatch(DispatchRequest { command_id: id.clone(), service_token: String::new() }).await.expect_err("Dispatch with no token");
        assert_eq!(d.code(), Code::Unauthenticated, "Dispatch({id:?}): {d:?}");
        let a = server
            .client
            .ack(AckRequest { command_id: id.clone(), ack_level: AckLevel::Edge as i32, principal: String::new(), reason: String::new(), service_token: String::new() })
            .await
            .expect_err("Ack with no token");
        assert_eq!(a.code(), Code::Unauthenticated, "Ack({id:?}): {a:?}");
        let e = server
            .client
            .expire(ExpireRequest { command_id: id.clone(), reason: String::new(), principal: String::new(), service_token: String::new() })
            .await
            .expect_err("Expire with no token");
        assert_eq!(e.code(), Code::Unauthenticated, "Expire({id:?}): {e:?}");
        let f = server
            .client
            .fail(FailRequest { command_id: id.clone(), reason: String::new(), principal: String::new(), service_token: String::new() })
            .await
            .expect_err("Fail with no token");
        assert_eq!(f.code(), Code::Unauthenticated, "Fail({id:?}): {f:?}");
    }

    server.shutdown().await;
}

/// **A service-principal refusal reaches the audit sink, not only the counters.** Every
/// `Authorize` refusal has written one RFC 5424 line since A2.2 (question 54's SIEM export);
/// a counter lives only in this process and behind `/admin/api/evidence`, so a refused
/// `Dispatch` whose only trace was a counter would be invisible to the sink a SIEM reads.
/// Asserted against the real audit file on disk (question 148), for all three refusal shapes
/// the new gate can produce, and asserting the raw token never reaches the sink either.
#[tokio::test]
async fn every_service_principal_refusal_writes_one_audit_line_naming_the_command_and_the_reason() {
    let mut server = spawn_r31_server("r31-audit-refusals").await;
    let id = proposed_command(&mut server, "audit-svc-1", "sat-audit-svc").await;
    let before = server.audit_lines().len();

    // (a) no token at all.
    server.client.dispatch(DispatchRequest { command_id: id.clone(), service_token: String::new() }).await.expect_err("no token");
    // (b) a verified token with no granting service role.
    let human_token = server.mint_service("operator-1", &["operators"]);
    server.client.dispatch(DispatchRequest { command_id: id.clone(), service_token: human_token.clone() }).await.expect_err("human token");
    // (c) a declared principal disagreeing with the verified subject.
    let service_token = server.mint_service("ground-segment-1", &["dispatchers"]);
    server
        .client
        .expire(ExpireRequest { command_id: id.clone(), reason: "r".to_string(), principal: "somebody-else".to_string(), service_token: service_token.clone() })
        .await
        .expect_err("declared principal disagreement");

    let lines = server.audit_lines();
    let new_lines = &lines[before..];
    assert_eq!(new_lines.len(), 3, "one audit line per refusal, got {new_lines:#?}");
    for line in new_lines {
        assert!(line.contains(&id), "every refusal line names the command id it was about: {line}");
        assert!(line.contains("REFUSED"), "every refusal line is a REFUSED event: {line}");
        assert!(!line.contains(human_token.as_str()) && !line.contains(service_token.as_str()), "no audit line ever contains a raw token: {line}");
    }
    assert!(new_lines[1].contains("ground-segment") || new_lines[1].contains("service_role"), "the role-gate refusal names what was refused: {}", new_lines[1]);
    assert!(new_lines[2].contains("somebody-else"), "the disagreement refusal names the declared label it refused: {}", new_lines[2]);
    // The disagreement refusal is decided AFTER the token verified, so it can and does name
    // the verified subject as its principal -- the two earlier ones cannot and do not.
    assert!(new_lines[2].contains("ground-segment-1"), "the disagreement refusal names the verified subject: {}", new_lines[2]);

    server.shutdown().await;
}
