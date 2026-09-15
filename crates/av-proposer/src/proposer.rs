//! One propose-run (D5): connect (done by the caller, see `crate::gateway_client`), issue
//! exactly one `DataGatewayService.Query` for the run's scores, evaluate `crate::rule` over
//! them, and either propose exactly one command through `ModelProposeService.ProposeCommand`
//! or report [`RunOutcome::NoProposalNeeded`] -- never both, never neither, never a loop.

use av_cdm::pb::{CommandState, GatewayQueryRequest, GatewaySelector, ProposeCommandRequest, RunIdentity};

use av_command::counters::{Counted, Counters};

use crate::command_id::{compute_command_id, compute_idempotency_key, ProposalInputs};
use crate::gateway_client::GatewayClient;
use crate::model_service::ModelIdentity;
use crate::rule::{evaluate, RuleConfig, RuleOutcome, RuleRefusal};

/// Every knob one run needs beyond the gateway endpoint itself (D3: every one a command-line
/// argument on `av-proposer`, never an environment variable).
#[derive(Debug, Clone, PartialEq)]
pub struct ProposerConfig {
    pub run_id: String,
    pub config_hash: String,
    pub caller_clearance: String,
    pub entity_id: String,
    pub command_class: String,
    pub rule: RuleConfig,
    pub model: ModelIdentity,
    pub rationale_prefix: String,
    /// R5.1/question 208(b): a compact-serialization JWS for this proposer's SERVICE subject,
    /// carried on both `GatewayQueryRequest.caller_token` and `ProposeCommandRequest.
    /// caller_token` -- `crates/av-proposer/src/bin/av-proposer.rs`'s own `--service-token-
    /// file` flag reads it off disk (never a flag VALUE -- a flag value is visible in `ps` and
    /// `docker inspect`; invariant G).
    pub service_token: String,
}

/// Every way one run can be refused -- see the module doc. Every variant [`Counted`].
#[derive(Debug)]
pub enum RunRefusal {
    /// `DataGatewayService.Query` itself failed (transport, or a typed gateway refusal --
    /// label, unknown run, missing product -- reported by `av-gateway` as a `tonic::Status`;
    /// the underlying detail survives in this variant's own message, not re-classified a
    /// second time here).
    GatewayQuery { detail: String },
    /// D3's own rule refused the named score (absent / wrong unit / not finite).
    Rule(RuleRefusal),
    /// `ModelProposeService.ProposeCommand` itself failed.
    Propose { detail: String },
}

impl Counted for RunRefusal {
    fn code(&self) -> &'static str {
        match self {
            RunRefusal::GatewayQuery { .. } => "proposer_gateway_query_failed",
            RunRefusal::Rule(r) => r.code(),
            RunRefusal::Propose { .. } => "proposer_propose_failed",
        }
    }
}

impl std::fmt::Display for RunRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunRefusal::GatewayQuery { detail } => write!(f, "gateway query failed: {detail}"),
            RunRefusal::Rule(r) => write!(f, "{r}"),
            RunRefusal::Propose { detail } => write!(f, "propose_command failed: {detail}"),
        }
    }
}

/// What one run decided.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    /// A burn was proposed and accepted -- question 209(a): `Propose` now checks
    /// automatically, so a successful call lands at `COMMAND_STATE_CHECKED`, not
    /// `COMMAND_STATE_PROPOSED`.
    Proposed { command_id: String, idempotency_key: String, drift_m: f64, burn_mps: f64 },
    /// The rule found nothing to propose -- D5's "no proposal, and why" line, the typed
    /// non-error outcome (drift within `threshold_m`, not a refusal).
    NoProposalNeeded { drift_m: f64 },
}

/// One complete run. `client` is already connected (D5's binary owns that step, so a test can
/// substitute a client dialed against an in-process test server without this function needing
/// to know an endpoint string at all).
pub async fn run(client: &mut GatewayClient, config: &ProposerConfig, counters: &Counters) -> Result<RunOutcome, RunRefusal> {
    let request = GatewayQueryRequest {
        run: Some(RunIdentity { run_id: config.run_id.clone(), config_hash: config.config_hash.clone() }),
        caller_clearance: config.caller_clearance.clone(),
        selector: GatewaySelector::Scores as i32,
        caller_supplied_products_uri: String::new(),
        caller_token: config.service_token.clone(),
    };
    let response = client.query(request).await.map_err(|status| {
        let err = RunRefusal::GatewayQuery { detail: status.to_string() };
        counters.record(&err);
        err
    })?;
    let query_id = response.query_id.clone();
    let resolved_config_hash = response.run.as_ref().map(|r| r.config_hash.clone()).unwrap_or_else(|| config.config_hash.clone());

    let outcome = evaluate(&response.scores, &config.rule).map_err(|refusal| {
        counters.record(&refusal);
        RunRefusal::Rule(refusal)
    })?;

    let (drift_m, burn_mps) = match outcome {
        RuleOutcome::NoProposalNeeded { drift_m } => return Ok(RunOutcome::NoProposalNeeded { drift_m }),
        RuleOutcome::Burn { drift_m, burn_mps } => (drift_m, burn_mps),
    };

    // Already checked finite by `evaluate` (it is the same value the rule refused on
    // otherwise) -- read straight back off the response rather than re-deriving it, so the
    // id is derived from exactly what the gateway actually returned.
    let score_value = response.scores.get(&config.rule.score_name).map(|s| s.value).unwrap_or(config.rule.reference_radius_m);

    let inputs = ProposalInputs {
        run_id: &config.run_id,
        config_hash: &resolved_config_hash,
        score_name: &config.rule.score_name,
        score_value,
        reference_radius_m: config.rule.reference_radius_m,
        threshold_m: config.rule.threshold_m,
        gain_per_s: config.rule.gain_per_s,
        max_burn_mps: config.rule.max_burn_mps,
        entity_id: &config.entity_id,
        command_class: &config.command_class,
        model_node_id: &config.model.node_id,
        model_version: &config.model.version,
    };
    let command_id = compute_command_id(&inputs);
    let idempotency_key = compute_idempotency_key(&inputs);

    let propose_request = ProposeCommandRequest {
        command_id: command_id.clone(),
        entity_id: config.entity_id.clone(),
        command_class: config.command_class.clone(),
        hazardous: false,
        envelope_id: String::new(),
        idempotency_key: idempotency_key.clone(),
        rationale: format!("{}: score {:?} drifted {drift_m:.6} m past reference {:.6} m (threshold {:.6} m); proposing a {burn_mps:.9} m/s correction (gain {:.9} (m/s)/m, clamp +-{:.6} m/s)", config.rationale_prefix, config.rule.score_name, config.rule.reference_radius_m, config.rule.threshold_m, config.rule.gain_per_s, config.rule.max_burn_mps),
        evidence_ids: vec![],
        // R5.1/invariant D: `principal` is a caller-DECLARED label that must agree with the
        // gateway's own verified token subject or be refused -- `config.model.node_id` (e.g.
        // a Kalman-filter version string) has no reason to equal the service token's own
        // `sub`, so this is left empty ("declares nothing", always accepted); the gateway's
        // `ProposalEvidence.model_identity` now records the VERIFIED service subject, not this
        // model-version identifier -- `config.model` is still recorded on `model_version`
        // below, which invariant D leaves untouched.
        principal: String::new(),
        model_version: config.model.version.clone(),
        run: Some(RunIdentity { run_id: config.run_id.clone(), config_hash: resolved_config_hash }),
        query_ids: vec![query_id],
        caller_token: config.service_token.clone(),
    };

    let response = client.propose_command(propose_request).await.map_err(|status| {
        let err = RunRefusal::Propose { detail: status.to_string() };
        counters.record(&err);
        err
    })?;
    let command = response.command.ok_or_else(|| {
        let err = RunRefusal::Propose { detail: "ProposeCommand succeeded but returned no command".to_string() };
        counters.record(&err);
        err
    })?;
    // Question 209(a): `Propose` now checks automatically, so a successful `ProposeCommand`
    // response is CHECKED, never a bare PROPOSED one. This was a `debug_assert_eq!` -- an
    // invariant that compiles OUT of a release build, exactly the shape this track has
    // already been burned by once (`docs/open-questions.md`) -- now a real, always-on, typed
    // and counted check instead, so a server that ever regressed this guarantee is refused
    // loudly in every profile, not silently trusted in release.
    if command.state != CommandState::Checked as i32 {
        let state_name = CommandState::try_from(command.state).map(|s| s.as_str_name().to_string()).unwrap_or_else(|_| command.state.to_string());
        let err = RunRefusal::Propose {
            detail: format!("a successful ProposeCommand response must be CHECKED (question 209(a): Propose now checks automatically); got {state_name}"),
        };
        counters.record(&err);
        return Err(err);
    }

    Ok(RunOutcome::Proposed { command_id: command.id, idempotency_key, drift_m, burn_mps })
}
