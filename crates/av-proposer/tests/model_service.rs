//! Acceptance evidence 5: "`ModelService` served for real" -- over a real loopback gRPC
//! channel, `ModelInfo` returns the declared identity and `Predict` returns a genuinely
//! propagated belief. Plus: `ModelInfo`'s `node_id`/`version` are exactly what
//! `ProposalEvidence` carries for a real proposal (D2's "one declaration, two consumers").
//!
//! Driven through `spoore_ml::ModelClient` -- the REAL calling half of this exact contract
//! (`build.rs` deliberately compiles this crate's own server with `build_client(false)`,
//! mirroring `spoore-ml/build.rs`'s own `build_server(false)`; neither side duplicates the
//! other's half), a dev-dependency only, never linked into the shipped `av-proposer` binary.

mod common;

use nalgebra::{DMatrix, DVector};
use spoore_cdm::{Belief, Epoch, GaussianState};
use spoore_ml::ModelClient;

use av_command::counters::Counters;
use av_proposer::gateway_client::GatewayClient;
use av_proposer::model_service::{ModelIdentity, ModelServiceImpl};
use av_proposer::model_service_pb::model_service_server::ModelServiceServer;
use av_proposer::proposer::{run, ProposerConfig, RunOutcome};
use av_proposer::rule::RuleConfig;

/// Spawns a real `ModelServiceImpl` (D2) over a real loopback socket, returning a connected
/// `spoore_ml::ModelClient` -- this file's own harness (used only here, mirroring `crates/
/// av-gateway/tests/propose_only.rs`'s convention of a per-file harness for a real server
/// only that file's tests need).
async fn spawn_real_model_service(identity: ModelIdentity) -> (ModelClient, tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let service = ModelServiceImpl::new(identity, 1e-3, "radar_0".to_string()).expect("a Position3dSensor over air_3d_state_space always constructs");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("local_addr");
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(ModelServiceServer::new(service))
            .serve_with_incoming_shutdown(incoming, async {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("ModelService server exits cleanly");
    });
    let client = ModelClient::connect(format!("http://{addr}")).await.expect("connect to the just-spawned real ModelService (ModelInfo included, at connect)");
    (client, shutdown_tx, handle)
}

fn belief_at(mean: [f64; 6], epoch_ns: i64) -> Belief {
    let state = GaussianState::new(DVector::from_row_slice(&mean), DMatrix::identity(6, 6), "air_3d", Epoch::from_nanos(epoch_ns)).expect("a well-formed identity-covariance state always constructs");
    Belief::single("test-node", state)
}

#[tokio::test]
async fn model_info_returns_the_declared_identity_over_a_real_loopback_channel() {
    let identity = ModelIdentity { node_id: "av-proposer.station-keeping".to_string(), version: "1.2.3".to_string() };
    let (client, shutdown_tx, handle) = spawn_real_model_service(identity.clone()).await;

    let info = client.info();
    assert_eq!(info.node_id, identity.node_id);
    assert_eq!(info.version, identity.version);
    assert_eq!(info.state_space_id, "air_3d");
    assert_eq!(info.sensor_ids, vec!["radar_0".to_string()]);
    assert!(info.supports_update);
    assert_eq!(info.epistemic_method, "exact");
    assert!(info.covariance_inflation.is_finite() && info.covariance_inflation > 0.0);

    let _ = shutdown_tx.send(());
    let _ = handle.await;
}

/// D2's own acceptance line: a served `Predict` returns a genuinely propagated `Belief`, not
/// an echo -- the mean must have moved by exactly `velocity * dt` (a real closed-form Kalman
/// predict step under `ConstantVelocity3d`, over a real network round trip, decoded through
/// `spoore_ml`'s own fallible `TryFrom` boundary).
#[tokio::test]
async fn predict_over_a_real_loopback_channel_genuinely_propagates_the_belief() {
    let identity = ModelIdentity { node_id: "av-proposer.station-keeping".to_string(), version: "1.0.0".to_string() };
    let (mut client, shutdown_tx, handle) = spawn_real_model_service(identity).await;

    let mean = [1_000.0, 2_000.0, 3_000.0, 10.0, -5.0, 2.0]; // pos_xyz, vel_xyz
    let belief = belief_at(mean, 0);

    let dt_ns = 4_000_000_000i64; // 4 seconds
    let predicted = client.predict(&belief, dt_ns).await.expect("Predict rpc");
    let state = &predicted.components()[0].state;

    let dt_secs = dt_ns as f64 * 1e-9;
    let expected = [mean[0] + mean[3] * dt_secs, mean[1] + mean[4] * dt_secs, mean[2] + mean[5] * dt_secs];
    for (i, exp) in expected.iter().enumerate() {
        assert!((state.mean()[i] - exp).abs() < 1e-6, "component {i}: {} vs {exp}", state.mean()[i]);
    }
    // Velocity is unchanged under constant-velocity propagation.
    for (i, v) in mean.iter().enumerate().skip(3) {
        assert!((state.mean()[i] - v).abs() < 1e-9, "velocity component {i} must not change under constant-velocity predict");
    }
    assert_eq!(state.epoch(), Epoch::from_nanos(dt_ns), "predict must stamp the target epoch, not leave the source epoch");
    // A real predict step must GROW the covariance (uncertainty increases with time), not
    // merely echo the identity matrix this test seeded it with.
    assert!(state.cov()[(0, 0)] > 1.0, "position variance must have grown past the seeded 1.0: {}", state.cov()[(0, 0)]);

    let _ = shutdown_tx.send(());
    let _ = handle.await;
}

/// D2's own acceptance line, restated as the cross-check this task's brief specifically
/// asks for: `ModelInfo`'s `node_id`/`version`, served over a real channel, are EXACTLY the
/// two strings `ProposalEvidence.model_identity`/`model_version` carry for a real proposal
/// -- one declaration ([`ModelIdentity`]), two consumers, never independently drifting.
#[tokio::test]
async fn model_infos_identity_is_exactly_what_a_real_proposals_evidence_carries() {
    let identity = ModelIdentity { node_id: "av-proposer.station-keeping".to_string(), version: "7.7.7".to_string() };
    let (model_client, model_shutdown_tx, model_handle) = spawn_real_model_service(identity.clone()).await;
    let served_version = model_client.info().version.clone();
    // Sanity: the ModelService really did serve the identity this test declared (D2's own
    // "one declaration" line still holds for `ModelInfo` itself) -- `ProposalEvidence.
    // model_identity` is never compared against this below (see the R5.1 comment further
    // down); `ProposalEvidence.model_node_id` IS (R5.1b, defect 2's own fix, below).
    assert_eq!(model_client.info().node_id, identity.node_id);

    let (entries, ladder) = common::catalogue_over_real_fixture("CUI");
    let harness = common::GatewayHarness::spawn("model-info-matches-evidence", 1_000, entries, ladder).await;
    common::assert_harness_is_wired(&harness);

    let mut gateway_client = GatewayClient::connect(harness.endpoint.clone()).await.expect("connect to the real gateway");
    let counters = Counters::new();
    let config = ProposerConfig {
        run_id: common::FIXTURE_RUN_ID.to_string(),
        config_hash: String::new(),
        caller_clearance: "CUI".to_string(),
        entity_id: "sat-1".to_string(),
        command_class: "burn".to_string(),
        rule: RuleConfig { score_name: "demo_flt_rmag_at_end".to_string(), reference_radius_m: 6_871_000.0, threshold_m: 100.0, gain_per_s: 0.001, max_burn_mps: 5.0 },
        // The SAME ModelIdentity value the served ModelService above answered ModelInfo
        // with -- this is the "one declaration" D2 requires; a real binary would build both
        // from the identical --model-node-id/--model-version CLI arguments.
        model: identity.clone(),
        rationale_prefix: "model-info-matches-evidence test".to_string(),
        service_token: harness.mint_service_token(),
    };

    let outcome = run(&mut gateway_client, &config, &counters).await.expect("real fixture proposes");
    let command_id = match outcome {
        RunOutcome::Proposed { command_id, .. } => command_id,
        other => panic!("expected Proposed, got {other:?}"),
    };

    let evidence = av_gateway::evidence::EvidenceRecorder::new(&harness.evidence_ledger).read_back(&command_id).expect("read_back").expect("evidence present");
    // R5.1/question 208(b), invariant D (a deliberate, documented behaviour change from this
    // test's own pre-R5.1 acceptance line): `ProposalEvidence.model_identity` now records the
    // VERIFIED service token subject, never a caller-declared identity string --
    // `av_proposer::proposer::run` sends `principal: String::new()` precisely because
    // `ModelIdentity.node_id` (a model-version identifier, e.g. a Kalman-filter variant) has
    // no reason to equal the service account authenticating the call, and invariant D forbids
    // trusting a declared value that disagrees with the verified one. `model_version` is
    // UNCHANGED by R5.1 (it is not a proposal-identity field) and still equals `ModelInfo.
    // version` exactly, preserving the rest of D2's "one declaration, two consumers" line.
    //
    // R5.1b, defect 2 resolves the tension the paragraph above left open: R5.1 made
    // `model_identity` the verified subject but, in doing so, dropped `ModelIdentity.node_id`
    // (WHICH model produced this proposal) from the evidence record entirely -- nothing failed
    // on it, because nothing asserted it either way. `ProposalEvidence.model_node_id` is the
    // additive fix (`docs/aiplane-plan.md` milestone A4 / ADR-004's AI-plane section: the
    // evidence topic must attribute a proposal to "the model identity and version"): a real
    // proposal's evidence now carries the verified subject (WHO submitted it), the
    // caller-declared model node id (WHICH model produced it), and the model version, all
    // three read back off the real ledger below -- never a constructed `ProposalEvidence`
    // literal standing in for what the gateway actually wrote.
    assert_eq!(evidence.model_identity, common::SERVICE_TOKEN_SUBJECT, "ProposalEvidence.model_identity must equal the verified service token subject (R5.1 invariant D), not the declared model node_id");
    assert_eq!(evidence.model_node_id, identity.node_id, "ProposalEvidence.model_node_id must equal the caller-declared ModelIdentity.node_id (R5.1b defect 2) -- unverified, unlike model_identity above");
    assert_eq!(evidence.model_version, served_version, "ProposalEvidence.model_version must equal the served ModelInfo.version exactly");
    assert_eq!(evidence.model_version, config.model.version);

    let _ = model_shutdown_tx.send(());
    let _ = model_handle.await;
    harness.shutdown().await;
}
