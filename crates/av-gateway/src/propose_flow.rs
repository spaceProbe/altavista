//! D1/A4b: the ONE implementation `propose_command` runs through, shared by the two propose
//! surfaces this crate exposes -- the MCP `propose_command` tool ([`crate::mcp`], stdio) and
//! `ModelProposeService.ProposeCommand` ([`ModelProposeServiceImpl`] below, gRPC). Before this
//! module existed, `crate::mcp::McpHandler::handle_propose_command` was the only place this
//! logic lived; extracting it here rather than writing a second copy against the gRPC wire
//! shape is the whole point (this task's own brief): two independently-maintained propose
//! paths is precisely how a propose-only guarantee gets lost on one of them, silently, the
//! day one of the two copies is edited and the other is not.
//!
//! [`propose_command`] does exactly what the old inline body did: build a fresh `Command`
//! (never reading a caller-supplied `state`/`transitions` -- see [`ProposeCommandInput`]'s own
//! doc for why that is structural, not a convention), call the real
//! [`crate::propose_only::ProposeOnlyAuthority::propose`] (so a non-empty `envelope_id` or an
//! already-started `Command` is refused by the same `av_command::state::propose` the real
//! service enforces, never a second, local copy of that check), then record the evidence topic
//! ([`crate::evidence::EvidenceRecorder`]) for the command it actually got back. Both surfaces
//! call this one function; each only translates its own wire shape into
//! [`ProposeCommandInput`] and its own wire shape back out of [`ProposeCommandOutput`] --
//! see `crates/av-gateway/tests/propose_flow_agreement.rs` for the test that the two
//! translations can never disagree in outcome.
//!
//! ## A defect found and fixed while extracting this module
//!
//! The original inline body mapped an [`crate::evidence::EvidenceRecorder::record`] I/O
//! failure straight to an `McpRefusal` with `.map_err(...)?`, bypassing `self.refuse(...)` --
//! so a real evidence-ledger write failure (a full disk, a permissions error) was reported to
//! the caller but never counted, violating ADR-004's "everything rejected is counted" for
//! exactly the failure shape this whole track's review keeps finding: one that leaves no
//! trace anywhere a counter could later be read back. [`ProposeFlowError::EvidenceRecording`]
//! is now `Counted` like every other refusal in this crate, and [`propose_command`] takes the
//! shared `Counters` explicitly so both surfaces record it identically.

use std::sync::Arc;

use av_cdm::pb::{Command, CommandProposal, ProposalEvidence, ProposeCommandRequest, ProposeCommandResponse, RunIdentity};
use tonic::{Request, Response, Status};

use av_command::clock::Clock;
use av_command::ledger::Ledger;

use crate::auth::{to_status as auth_to_status, AuthContext, AuthRefusal};
use crate::counters::{Counted, Counters};
use crate::evidence::EvidenceRecorder;
use crate::pb::model_propose_service_server::ModelProposeService;
use crate::propose_only::{ProposeOnlyAuthority, ProposeRefusal};

/// Everything the shared propose flow needs, translated from either wire shape (the MCP
/// tool's JSON `arguments` object, or [`ProposeCommandRequest`]) into one Rust value. Note
/// what is deliberately absent: no `state`, no `transitions`. Neither surface's own request
/// shape has a field that could populate them -- `AlreadyStarted` (D4's "any other shape") is
/// reachable only by calling [`ProposeOnlyAuthority::propose`] directly with a hand-built
/// `Command`, as `crates/av-gateway/tests/propose_only.rs`'s own crafted-command test does,
/// never through either of this crate's own request schemas.
#[derive(Debug, Clone)]
pub struct ProposeCommandInput {
    pub command_id: String,
    pub entity_id: String,
    pub command_class: String,
    pub hazardous: bool,
    pub envelope_id: String,
    pub idempotency_key: String,
    pub rationale: String,
    pub evidence_ids: Vec<String>,
    /// The model/agent identity proposing this command -- recorded on the `PROPOSED`
    /// transition's `principal` (via `ProposeOnlyAuthority::propose`) AND, verbatim, as
    /// `ProposalEvidence.model_identity`.
    pub principal: String,
    pub model_version: String,
    /// What the model saw: the run identity it read before proposing (D5).
    pub run: Option<RunIdentity>,
    /// Every query id the model's session issued before this proposal, in the order the
    /// gateway served them (D5).
    pub query_ids: Vec<String>,
}

/// What [`propose_command`] returns on success: the real `Command` [`ProposeOnlyAuthority::
/// propose`] returned, and the [`ProposalEvidence`] that was actually written to the evidence
/// ledger for it -- both surfaces render these into their own wire shape, never
/// reconstructing either value independently. Question 209(a)/D6: since the real `Propose`
/// RPC now runs the check edge automatically, this `Command` is `CHECKED` (two transitions,
/// `PROPOSED` then `CHECKED`) on success, never a bare `PROPOSED` one with a single
/// transition -- a policy denial never reaches here at all (it is
/// [`ProposeRefusal::PolicyDenied`], a refusal `propose_command` returns instead, below).
#[derive(Debug, Clone)]
pub struct ProposeCommandOutput {
    pub command: Command,
    pub evidence: ProposalEvidence,
}

/// Every way the shared propose flow can refuse, spanning both the real state-machine
/// refusal ([`ProposeRefusal`], already `Counted`) and this module's own evidence-recording
/// failure (see the module doc's "defect found and fixed" section).
#[derive(Debug)]
pub enum ProposeFlowError {
    Propose(ProposeRefusal),
    /// [`EvidenceRecorder::record`] failed (real ledger I/O) after `propose` had already
    /// succeeded -- the resulting `Command` IS `PROPOSED` on the real ledger; only the
    /// evidence-topic record failed to write. Carried as a `String` (`std::io::Error`'s own
    /// `Display`) rather than the error itself so this type stays `Clone`/`PartialEq`-free of
    /// `io::Error`'s own lack of those impls, matching this crate's other refusal enums.
    EvidenceRecording { detail: String },
}

impl Counted for ProposeFlowError {
    fn code(&self) -> &'static str {
        match self {
            ProposeFlowError::Propose(e) => e.code(),
            ProposeFlowError::EvidenceRecording { .. } => "propose_flow_evidence_recording_failed",
        }
    }
}

impl std::fmt::Display for ProposeFlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProposeFlowError::Propose(e) => write!(f, "{e}"),
            ProposeFlowError::EvidenceRecording { detail } => write!(f, "evidence recording failed: {detail}"),
        }
    }
}

/// The one shared implementation (see the module doc). Builds a fresh `Command` from `input`
/// (never a caller-supplied state/transitions), proposes it through the real
/// [`ProposeOnlyAuthority`], records the evidence topic for the `Command` it actually got
/// back, and counts (via `counters`) either the real state-machine refusal or this module's
/// own evidence-recording failure -- never silently.
pub async fn propose_command(
    authority: &ProposeOnlyAuthority,
    evidence_ledger: &Ledger,
    clock: &dyn Clock,
    counters: &Counters,
    input: ProposeCommandInput,
) -> Result<ProposeCommandOutput, ProposeFlowError> {
    let command = Command {
        id: input.command_id,
        entity_id: input.entity_id,
        command_class: input.command_class,
        hazardous: input.hazardous,
        envelope_id: input.envelope_id,
        idempotency_key: input.idempotency_key,
        ..Default::default() // state = COMMAND_STATE_UNSPECIFIED, transitions = [] -- structural (see ProposeCommandInput's own doc).
    };
    let proposal = CommandProposal { command: Some(command), rationale: input.rationale, evidence_ids: input.evidence_ids };

    let proposed = authority.propose(proposal, input.principal.clone()).await.map_err(|refusal| {
        counters.record(&refusal);
        ProposeFlowError::Propose(refusal)
    })?;

    let evidence = ProposalEvidence {
        command_id: proposed.id.clone(),
        run: input.run,
        query_ids: input.query_ids,
        model_identity: input.principal,
        model_version: input.model_version,
        recorded_tai_ns: clock.now_tai_ns(),
    };
    let recorder = EvidenceRecorder::new(evidence_ledger);
    recorder.record(&evidence, clock).map_err(|e| {
        let err = ProposeFlowError::EvidenceRecording { detail: e.to_string() };
        counters.record(&err);
        err
    })?;

    Ok(ProposeCommandOutput { command: proposed, evidence })
}

/// R5.1/question 208(b): what [`authenticated_propose_command`] can return -- either
/// [`AuthRefusal`] (this crate's own new authentication gate) or the pre-existing
/// [`ProposeFlowError`] (unchanged). See [`crate::gateway::AuthenticatedQueryError`]'s
/// identical doc for why this crate keeps the two kinds as separate variants rather than
/// folding one into the other's shape.
#[derive(Debug)]
pub enum AuthenticatedProposeError {
    Auth(AuthRefusal),
    Flow(ProposeFlowError),
}

impl std::fmt::Display for AuthenticatedProposeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthenticatedProposeError::Auth(e) => write!(f, "{e}"),
            AuthenticatedProposeError::Flow(e) => write!(f, "{e}"),
        }
    }
}

/// R5.1/question 208(b): the ONE authenticated entry point both the gRPC `ProposeCommand` rpc
/// below and the MCP `propose_command` tool (`crate::mcp::McpHandler::handle_propose_command`)
/// call through -- never a second copy of the auth-then-propose sequence. Authenticates `token`
/// for [`crate::auth::Surface::Propose`] (verify -> SERVICE role check -> declared-principal
/// agreement against `input.principal`, invariant D), then overwrites `input.principal` with
/// the VERIFIED subject -- never the caller-declared one -- before calling the existing,
/// unmodified [`propose_command`], so `ProposalEvidence.model_identity` always ends up being
/// the verified subject, exactly invariant D's own requirement.
pub async fn authenticated_propose_command(
    authority: &ProposeOnlyAuthority,
    evidence_ledger: &Ledger,
    clock: &dyn Clock,
    counters: &Counters,
    auth: &AuthContext,
    token: &str,
    mut input: ProposeCommandInput,
) -> Result<ProposeCommandOutput, AuthenticatedProposeError> {
    let principal = auth.authenticate_propose(token, &input.principal, counters).map_err(AuthenticatedProposeError::Auth)?;
    input.principal = principal.sub;
    propose_command(authority, evidence_ledger, clock, counters, input).await.map_err(AuthenticatedProposeError::Flow)
}

/// Maps an [`AuthenticatedProposeError`] to a [`tonic::Status`] for [`ModelProposeServiceImpl`]
/// -- the [`AuthRefusal`] half through [`crate::auth::to_status`], the [`ProposeFlowError`]
/// half through [`to_status`] below, unchanged.
fn authenticated_propose_to_status(err: AuthenticatedProposeError) -> Status {
    match err {
        AuthenticatedProposeError::Auth(e) => auth_to_status(e),
        AuthenticatedProposeError::Flow(e) => to_status(e),
    }
}

/// Maps a [`ProposeFlowError`] to a [`tonic::Status`] for [`ModelProposeServiceImpl`]. The
/// underlying [`ProposeRefusal`] already carries the real `av_command::state::propose`
/// refusal text (question 53's `EnvelopeNotAllowed`, D4's `AlreadyStarted`) as
/// `INVALID_ARGUMENT` -- mirrored here rather than re-derived, so this rpc's status code for a
/// given refusal is identical to what a direct `CommandAuthorityService.Propose` call would
/// have produced. Question 209(a)/D6/D7: [`ProposeRefusal::PolicyDenied`] is mirrored the
/// identical way, as `PERMISSION_DENIED` -- the real `Propose` RPC's own automatic-check
/// denial, typed and counted (`propose_policy_denied`) all the way out to this gRPC surface,
/// not merely to the gateway's internal `ProposeRefusal`. Evidence-recording failure is
/// `INTERNAL`: it is this crate's own I/O, not a property of the caller's request.
fn to_status(err: ProposeFlowError) -> Status {
    let message = err.to_string();
    match &err {
        ProposeFlowError::Propose(ProposeRefusal::EnvelopeNotAllowed { .. } | ProposeRefusal::AlreadyStarted { .. } | ProposeRefusal::InvalidArgument { .. }) => {
            Status::invalid_argument(message)
        }
        ProposeFlowError::Propose(ProposeRefusal::PolicyDenied { .. }) => Status::permission_denied(message),
        ProposeFlowError::Propose(ProposeRefusal::Transport { .. }) => Status::unavailable(message),
        ProposeFlowError::EvidenceRecording { .. } => Status::internal(message),
    }
}

fn input_from_wire(req: ProposeCommandRequest) -> ProposeCommandInput {
    ProposeCommandInput {
        command_id: req.command_id,
        entity_id: req.entity_id,
        command_class: req.command_class,
        hazardous: req.hazardous,
        envelope_id: req.envelope_id,
        idempotency_key: req.idempotency_key,
        rationale: req.rationale,
        evidence_ids: req.evidence_ids,
        principal: req.principal,
        model_version: req.model_version,
        run: req.run,
        query_ids: req.query_ids,
    }
}

/// `ModelProposeService`'s gRPC server (A4b) over [`propose_command`] -- the network propose
/// path a containerised proposer with no stdio channel to the gateway needs. Every field is
/// exactly what [`crate::mcp::McpContext`] already holds; this type exists only because the
/// generated `ModelProposeService` server trait needs its own `Self`, not because the
/// underlying dependencies differ from the MCP surface's.
pub struct ModelProposeServiceImpl {
    authority: Arc<ProposeOnlyAuthority>,
    evidence_ledger: Arc<Ledger>,
    clock: Arc<dyn Clock>,
    counters: Arc<Counters>,
    auth: Arc<AuthContext>,
}

impl ModelProposeServiceImpl {
    pub fn new(authority: Arc<ProposeOnlyAuthority>, evidence_ledger: Arc<Ledger>, clock: Arc<dyn Clock>, counters: Arc<Counters>, auth: Arc<AuthContext>) -> Self {
        Self { authority, evidence_ledger, clock, counters, auth }
    }
}

#[tonic::async_trait]
impl ModelProposeService for ModelProposeServiceImpl {
    /// R5.1/question 208(b): authenticates the proposer's SERVICE subject before any command
    /// is proposed -- see [`authenticated_propose_command`]'s own doc for the full sequence.
    async fn propose_command(&self, request: Request<ProposeCommandRequest>) -> Result<Response<ProposeCommandResponse>, Status> {
        let req = request.into_inner();
        let token = req.caller_token.clone();
        let input = input_from_wire(req);
        let output = authenticated_propose_command(&self.authority, &self.evidence_ledger, &*self.clock, &self.counters, &self.auth, &token, input)
            .await
            .map_err(authenticated_propose_to_status)?;
        Ok(Response::new(ProposeCommandResponse { command: Some(output.command) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propose_flow_error_codes_are_stable_and_distinct_from_the_underlying_refusal() {
        let propose = ProposeFlowError::Propose(ProposeRefusal::AlreadyStarted { detail: "x".to_string() });
        let evidence = ProposeFlowError::EvidenceRecording { detail: "disk full".to_string() };
        assert_eq!(propose.code(), "propose_already_started");
        assert_eq!(evidence.code(), "propose_flow_evidence_recording_failed");
        assert_ne!(propose.code(), evidence.code());
    }

    #[test]
    fn evidence_recording_failure_displays_the_underlying_detail() {
        let err = ProposeFlowError::EvidenceRecording { detail: "disk full".to_string() };
        assert!(err.to_string().contains("disk full"), "{err}");
    }

    /// Not a call through `propose_command` itself (that needs a real authority/ledger,
    /// proven in `crates/av-gateway/tests/propose_flow_agreement.rs`) -- just pins that
    /// `input_from_wire` carries every field through untouched, so a future field added to
    /// one side and forgotten on the other fails here first.
    #[test]
    fn input_from_wire_carries_every_field_through_untouched() {
        let req = ProposeCommandRequest {
            command_id: "cmd-1".to_string(),
            entity_id: "sat-1".to_string(),
            command_class: "burn".to_string(),
            hazardous: true,
            envelope_id: "env-1".to_string(),
            idempotency_key: "idem-1".to_string(),
            rationale: "because".to_string(),
            evidence_ids: vec!["e1".to_string()],
            principal: "model-x".to_string(),
            model_version: "1.0.0".to_string(),
            run: Some(RunIdentity { run_id: "run-1".to_string(), config_hash: "hash-1".to_string() }),
            query_ids: vec!["q1".to_string()],
            caller_token: "irrelevant-to-this-conversion".to_string(),
        };
        let input = input_from_wire(req.clone());
        assert_eq!(input.command_id, req.command_id);
        assert_eq!(input.entity_id, req.entity_id);
        assert_eq!(input.command_class, req.command_class);
        assert_eq!(input.hazardous, req.hazardous);
        assert_eq!(input.envelope_id, req.envelope_id);
        assert_eq!(input.idempotency_key, req.idempotency_key);
        assert_eq!(input.rationale, req.rationale);
        assert_eq!(input.evidence_ids, req.evidence_ids);
        assert_eq!(input.principal, req.principal);
        assert_eq!(input.model_version, req.model_version);
        assert_eq!(input.run, req.run);
        assert_eq!(input.query_ids, req.query_ids);
    }

    // ---- R5.1/question 208(b): authenticated_propose_command's own auth gate ----

    mod authenticated {
        use super::*;
        use crate::auth::{AuthContext, AuthRefusal, GroupClearanceMap};
        use av_command::authz::RoleTable;
        use av_command::clock::TestClock;
        use av_command::oidc::IssuerConfig;
        use av_command::test_support::{valid_claims, TestIssuer};
        use std::collections::BTreeMap;

        const ISSUER: &str = "https://sso.test.example/";
        const AUDIENCE: &str = "av-gateway";
        const NOW_UNIX_S: i64 = 1_760_000_000;

        fn auth_ctx(issuer: &TestIssuer, service_roles: &[(&str, &[&str])]) -> AuthContext {
            let mut m = BTreeMap::new();
            for (role, surfaces) in service_roles {
                m.insert(role.to_string(), surfaces.iter().map(|s| s.to_string()).collect());
            }
            AuthContext::new(
                Arc::new(IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap()),
                Arc::new(RoleTable::default()),
                Arc::new(RoleTable::from_config(&m)),
                Arc::new(GroupClearanceMap::default()),
                Arc::new(TestClock::new(NOW_UNIX_S * 1_000_000_000)),
            )
        }

        fn mint(issuer: &TestIssuer, sub: &str, groups: &[&str]) -> String {
            let mut claims = valid_claims(ISSUER, AUDIENCE, sub, NOW_UNIX_S, 3_600);
            claims["groups"] = serde_json::json!(groups);
            issuer.mint(&claims)
        }

        fn authority_dialing_nothing() -> ProposeOnlyAuthority {
            let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
            ProposeOnlyAuthority::from_channel(channel)
        }

        fn input(principal: &str) -> ProposeCommandInput {
            ProposeCommandInput {
                command_id: "cmd-1".to_string(),
                entity_id: "sat-1".to_string(),
                command_class: "burn".to_string(),
                hazardous: false,
                envelope_id: String::new(),
                idempotency_key: "idem-1".to_string(),
                rationale: "because".to_string(),
                evidence_ids: vec![],
                principal: principal.to_string(),
                model_version: "1.0.0".to_string(),
                run: None,
                query_ids: vec![],
            }
        }

        fn temp_ledger_dir(name: &str) -> std::path::PathBuf {
            let dir = std::env::temp_dir().join(format!("av-gateway-propose-flow-auth-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            dir
        }

        /// An unauthenticated (empty-token) caller is refused before `ProposeOnlyAuthority::
        /// propose` is ever called -- proven by the fact this test's own `authority` dials an
        /// address nothing listens on and the call still completes (it never actually
        /// connects), never hanging or erroring with a transport failure.
        #[tokio::test]
        async fn an_unauthenticated_proposer_is_refused_before_propose_is_ever_called() {
            let dir = temp_ledger_dir("unauth");
            let ledger = Ledger::open(&dir).unwrap();
            let clock = TestClock::new(1_000);
            let counters = Counters::new();
            let auth = auth_ctx(&TestIssuer::new(), &[("proposer-service", &["propose"])]);
            let authority = authority_dialing_nothing();

            let err = authenticated_propose_command(&authority, &ledger, &clock, &counters, &auth, "", input("")).await.unwrap_err();
            assert!(matches!(err, AuthenticatedProposeError::Auth(AuthRefusal::MissingToken { .. })), "{err:?}");
            assert_eq!(counters.get("gateway_auth_missing_token"), 1);
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A purely human role (granted only on the human table) must never authorize the
        /// service `"propose"` surface -- the identical structural guarantee `av_command::
        /// authz`'s own R3.1 gives `Dispatch`/`Ack`/`Expire`/`Fail`.
        #[tokio::test]
        async fn a_human_only_token_cannot_propose() {
            let dir = temp_ledger_dir("human-only");
            let ledger = Ledger::open(&dir).unwrap();
            let clock = TestClock::new(1_000);
            let counters = Counters::new();
            let issuer = TestIssuer::new();
            // "operators" is not in the service_roles table this auth context was built from.
            let auth = auth_ctx(&issuer, &[("proposer-service", &["propose"])]);
            let authority = authority_dialing_nothing();
            let token = mint(&issuer, "human-1", &["operators"]);

            let err = authenticated_propose_command(&authority, &ledger, &clock, &counters, &auth, &token, input("")).await.unwrap_err();
            assert!(matches!(err, AuthenticatedProposeError::Auth(AuthRefusal::RoleNotGranted { .. })), "{err:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A declared `principal` disagreeing with the verified subject is refused before
        /// `propose` is called, and the counter for it moves.
        #[tokio::test]
        async fn a_disagreeing_declared_principal_is_refused_and_counted() {
            let dir = temp_ledger_dir("principal-mismatch");
            let ledger = Ledger::open(&dir).unwrap();
            let clock = TestClock::new(1_000);
            let counters = Counters::new();
            let issuer = TestIssuer::new();
            let auth = auth_ctx(&issuer, &[("proposer-service", &["propose"])]);
            let authority = authority_dialing_nothing();
            let token = mint(&issuer, "the-real-proposer", &["proposer-service"]);

            let err = authenticated_propose_command(&authority, &ledger, &clock, &counters, &auth, &token, input("someone-else")).await.unwrap_err();
            assert!(matches!(err, AuthenticatedProposeError::Auth(AuthRefusal::PrincipalMismatch { .. })), "{err:?}");
            assert_eq!(counters.get("gateway_auth_principal_mismatch"), 1);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
