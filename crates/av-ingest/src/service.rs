//! The `tonic` `EdgeIngest` service implementation (question 202, E3b's charter).
//!
//! # The manifest handshake, and where identity fits
//!
//! [`EdgeIngestService::announce`] is the one place a plugin's certificate identity is
//! actually verified this round (question 202: "Identity: ... `verify_identity` runs once
//! per connection (or per announce -- choose and justify)"). **This service chooses "once
//! per `Announce` call", not "once per connection" and not "once per batch":**
//!
//! - Not per connection: `tonic`'s built-in server exposes no ready-made "same TCP
//!   connection" identity a service trait method can key state on without adding
//!   `serve_with_incoming` plumbing and a `ConnectInfo` extractor this round has no other
//!   need for, and a real nginx front (question 155/202) may itself multiplex several
//!   plugins' mTLS connections onto fewer, reused backend connections -- so "per
//!   connection" would be either unenforceable or, worse, silently wrong the moment the
//!   front's own connection pooling changes shape. This service instead scopes
//!   "announced" to **this service instance's lifetime**, per `producer_id` (tracked in
//!   [`ServiceState::manifests`]) -- simpler, and exactly what every `wire_*.rs` test
//!   (each with its own fresh service) can observe and assert on directly.
//! - Not per batch: `crate::ingest::Ingest::submit`'s own `Signer::Certificate` path
//!   re-verifies the full certificate chain and validity window on **every** batch, which
//!   is the right amortization for E1/E2's own in-process tests (no connection concept
//!   exists there at all) but the wrong one for a live stream at any real rate -- ADR-004's
//!   "measure, then decide" instinct (`docs/edge-plan.md`'s own latency-budget language)
//!   argues for paying the chain-walk cost once. `announce` calls
//!   `crate::ingest::Ingest::verify_identity_once` exactly once, caches the resulting
//!   `EdgeIdentity`'s public key in [`ServiceState::verify_keys`], and every subsequent
//!   `Submit` batch from that producer is checked with `Signer::Key(cached_key)` --
//!   `av_edge::verify::verify_batch`'s own ECDSA check still runs on **every single
//!   batch** (nothing about per-batch cryptographic integrity is weakened), only the
//!   *certificate* chain-and-time walk is amortized to once per `Announce`.
//!
//! # What `Announce` enforces versus what it only records
//!
//! Checked, in this fixed order, each a distinct [`pb::ManifestRefusal`] (ADR-004: exactly
//! one reason per refusal):
//!
//! 1. **`IDENTITY_REFUSED`** -- if [`EdgeIngestConfig::require_client_certificate`], the
//!    forwarded-certificate header (`crate::forwarded_cert`) must be present, decode, and
//!    verify (`Ingest::verify_identity_once`); if a certificate identity was verified and
//!    `PluginManifest.leaf_fingerprint_sha256` is non-empty, it must match.
//! 2. **`UNKNOWN_PLUGIN`** -- if [`EdgeIngestConfig::known_plugins`] is configured,
//!    `producer_id` must be on it.
//! 3. **`LABEL_NOT_PERMITTED`** -- if [`EdgeIngestConfig::permitted_labels`] is configured,
//!    the declared label must be one of them.
//! 4. **`CLEARANCE_ABOVE_LADDER`** -- `av_edge::policy::ProducerPolicy::new` with this
//!    deployment's configured `clearance_ladder`/`max_age_ns` must succeed for the
//!    declared `clearance`.
//! 5. **`MANIFEST_MISMATCH`** -- a manifest already accepted for this `producer_id` (this
//!    service instance's lifetime) that is not `==` this one.
//!
//! `PluginManifest.output_schemas`/`frame_ids`/`shard_keys`/`plugin_version` are recorded
//! (kept in [`ServiceState::manifests`], returned unchanged by a repeat `Announce`) but
//! enforce nothing this round -- see `edge.proto`'s own `MeasurementSchema` doc comment.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use openssl::ec::EcKey;
use openssl::pkey::Public;
use tonic::{Request, Response, Status};

use av_edge::identity::TrustAnchors;
use av_edge::pb;
use av_edge::policy::ProducerPolicy;

use crate::forwarded_cert;
use crate::ingest::{Ingest, IngestOutcome, Signer};
use crate::pb::edge_ingest_server::EdgeIngest;

/// The clock this service is injected with -- **never** a live read of any clock anywhere
/// on the accept path (question 199's sibling rule for this track). Tests inject a fixed
/// or scripted closure (e.g. a `Mutex<i64>` a test advances by hand between calls); a
/// deployed binary supplies its own TAI-nanosecond reader at its single `main` call site
/// (the platform's UTC-to-TAI conversion lives at the GMAT/time boundary, question 8),
/// never inside this crate.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Deployment configuration for one [`EdgeIngestService`] -- everything a manifest is
/// checked against that is **not** carried on the wire (the clearance ladder and
/// staleness budget are a deployment concern, `docs/edge-plan.md`'s "profiles select
/// components, never change model behaviour" rule, question 11 -- a plugin cannot declare
/// its own ladder).
pub struct EdgeIngestConfig {
    /// If `Some`, `PluginManifest.producer_id` must be a member or `Announce` refuses
    /// `UNKNOWN_PLUGIN`. `None` (the default) is permissive: any `producer_id` may
    /// announce. A production deployment should set this; a single-plugin dev/test setup
    /// reasonably leaves it `None`.
    pub known_plugins: Option<Vec<String>>,
    /// If `Some`, `PluginManifest.label` (marking + caveat set, order-independent) must
    /// equal one of these or `Announce` refuses `LABEL_NOT_PERMITTED`. `None` (the
    /// default) is permissive.
    pub permitted_labels: Option<Vec<pb::Label>>,
    /// This deployment's ordered clearance ladder, passed to `av_edge::policy::
    /// ProducerPolicy::new` for every accepted manifest -- rank = index, exactly as that
    /// module documents.
    pub clearance_ladder: Vec<String>,
    /// This deployment's staleness budget (`ProducerPolicy::max_age_ns`), applied to every
    /// producer this service registers.
    pub max_age_ns: i64,
    /// **Safe default: `true`.** When set, `Announce` requires the forwarded-certificate
    /// header (`crate::forwarded_cert`) and refuses `IDENTITY_REFUSED` if it is absent,
    /// malformed, or does not verify against `anchors` -- the cross-host, nginx-mTLS-
    /// fronted deployment ADR-004 assumes by default. Only a deployment that has
    /// deliberately decided it has no front at all (loopback-only, single host, `crate::
    /// server::bind_loopback`'s own address never leaving that host) should set this to
    /// `false`, at which point `Submit` instead checks every batch's signature against
    /// `verify_keys` -- a configured key, not a certificate -- exactly like E1's own
    /// no-certificate-in-the-loop tests. Defaulting this to `false` would mean a
    /// deployment that forgot to configure it silently accepted unauthenticated batches
    /// the moment it was reachable from anywhere but localhost; defaulting to `true`
    /// means the failure mode of forgetting is "plugins cannot connect" (loud, immediate,
    /// safe), not "batches are accepted from anyone" (silent, unsafe) -- ADR-004's own
    /// "refuse by default" instinct, applied to this one flag.
    pub require_client_certificate: bool,
    /// Verifying keys used only when `require_client_certificate` is `false` -- keyed by
    /// `producer_id`, exactly like E1's own `Signer::Key` tests. Ignored (never consulted)
    /// when `require_client_certificate` is `true`: that path's keys come from the
    /// certificate `Announce` itself just verified, never from this map.
    pub verify_keys: HashMap<String, EcKey<Public>>,
    /// The Intermediate certificate(s) (PEM, possibly concatenated) every leaf this
    /// deployment's seccert Intermediate issues chains through, used as
    /// `chain_pem` on **every** [`Ingest::verify_identity_once`] call this service makes.
    /// **A deployment-wide constant, deliberately not carried on the wire per request**:
    /// nginx's own `$ssl_client_escaped_cert` variable forwards only the leaf certificate
    /// (this crate's `forwarded_cert` module doc, quoting nginx's documentation directly)
    /// -- there is no corresponding "`$ssl_client_escaped_chain`" variable to forward an
    /// Intermediate with, and even if there were, an Intermediate is exactly the kind of
    /// fact a deployment fixes once (every plugin's leaf is issued by the *same*
    /// Intermediate under ADR-004's two-tier PKI), not something that varies request to
    /// request or that a plugin should get to assert about itself. `None` when the
    /// deployment's Root signs leaves directly (a single-tier PKI, or a test hierarchy
    /// with no separate Intermediate at all) -- `av_edge::identity::verify_identity`
    /// accepts `chain_pem: None` for exactly that shape already.
    pub intermediate_chain_pem: Option<Vec<u8>>,
}

impl EdgeIngestConfig {
    /// The safe default described on [`EdgeIngestConfig::require_client_certificate`]'s
    /// own doc comment: certificates required, no allow-lists configured (permissive on
    /// plugin identity/label, strict on cryptographic identity), an empty clearance
    /// ladder and zero staleness budget (both of which a caller must set explicitly --
    /// there is no sane non-empty default for either, so leaving them empty here is a
    /// deliberate "every real deployment must configure this" signal, not a working
    /// default on its own).
    pub fn new(clearance_ladder: Vec<String>, max_age_ns: i64) -> Self {
        Self {
            known_plugins: None,
            permitted_labels: None,
            clearance_ladder,
            max_age_ns,
            require_client_certificate: true,
            verify_keys: HashMap::new(),
            intermediate_chain_pem: None,
        }
    }
}

/// The manifest-handshake bookkeeping [`EdgeIngestService::announce`]/[`EdgeIngestService::
/// submit`] need beyond `Ingest` itself. Kept in its own `Mutex`, separate from
/// [`EdgeIngestService::ingest`], specifically so [`crate::admin`]'s `/admin/api/evidence`
/// HTTP surface can share the very same `Arc<Mutex<Ingest>>` this service mutates
/// (`EdgeIngestService::ingest_handle`) without also needing to know anything about the
/// handshake state a plain evidence reader has no reason to touch. **Lock order, to avoid
/// a deadlock**: every RPC that needs both locks (`announce`, `submit`) always takes
/// `handshake` first, `ingest` second -- documented once, here, rather than re-derived at
/// each call site; nothing in this crate ever takes them in the opposite order.
struct Handshake {
    /// Every manifest ever accepted, by `producer_id`, for this service instance's
    /// lifetime -- see the module doc's "not per connection" reasoning. Also what a
    /// repeat `Announce` is compared against (`MANIFEST_MISMATCH`).
    manifests: HashMap<String, pb::PluginManifest>,
    /// The verifying key each announced producer's batches are checked against on
    /// `Submit`: either a certificate's own public key (cached once, at `Announce`, when
    /// `require_client_certificate` is set) or `EdgeIngestConfig::verify_keys`'s
    /// configured entry (when it is not). Never both -- exactly one path populates this
    /// per producer, per `EdgeIngestConfig::require_client_certificate`.
    verify_keys: HashMap<String, EcKey<Public>>,
}

/// The `EdgeIngest` `tonic` service. Nothing here is held across an `.await` point, so
/// both locks below are ordinary, uncontended `std::sync::Mutex`es, not `tokio::sync::
/// Mutex` -- matching `crates/av-dynamics-service`'s own worker-handoff style of keeping
/// synchronous work off any lock an async call point would need to hold live.
pub struct EdgeIngestService {
    ingest: Arc<Mutex<Ingest>>,
    handshake: Mutex<Handshake>,
    config: EdgeIngestConfig,
    clock: Clock,
}

impl EdgeIngestService {
    /// `log_dir` is `crate::ingest::Ingest::new`'s own log directory; `anchors` is handed
    /// straight through to that same `Ingest` (which already owns exactly this concept --
    /// see [`Ingest::verify_identity_once`]) rather than duplicated as a second field on
    /// this struct. Must be `Some` whenever `config.require_client_certificate` is `true`
    /// -- checked, not assumed, via [`Ingest::verify_identity_once`]'s own
    /// `IngestError::NoTrustAnchors`, surfaced by [`EdgeIngestService::announce`] as a
    /// typed `IDENTITY_REFUSED` refusal rather than a panic.
    pub fn new(log_dir: impl Into<std::path::PathBuf>, anchors: Option<TrustAnchors>, config: EdgeIngestConfig, clock: Clock) -> Self {
        Self {
            ingest: Arc::new(Mutex::new(Ingest::new(log_dir, anchors))),
            handshake: Mutex::new(Handshake { manifests: HashMap::new(), verify_keys: HashMap::new() }),
            config,
            clock,
        }
    }

    fn now_tai_ns(&self) -> i64 {
        (self.clock)()
    }

    /// The same `Arc<Mutex<Ingest>>` this service mutates, for `crate::admin::AdminState`
    /// (or a test) to share -- so `GET /admin/api/evidence` and this service's own
    /// `GetEvidence` RPC are two views onto **one** `Ingest`, never two independently
    /// constructed ones that could silently drift apart.
    pub fn ingest_handle(&self) -> Arc<Mutex<Ingest>> {
        self.ingest.clone()
    }

    /// This deployment's evidence, as `EvidenceResponse` -- built directly from
    /// `crate::ingest::Ingest`'s own accessors (the same state `crate::evidence::
    /// evidence` reads), so the wire response and the in-process JSON evidence surface
    /// can never structurally diverge (`tests/wire_evidence.rs` asserts they agree on
    /// content, not merely on shape).
    pub fn evidence_snapshot(&self) -> pb::EvidenceResponse {
        let ingest = self.ingest.lock().unwrap_or_else(|p| p.into_inner());
        build_evidence_response(&ingest)
    }

    /// This deployment's per-partition chain verification, as `VerifyResponse` -- exactly
    /// `crate::evidence::verify_all`'s own map.
    pub fn verify_snapshot(&self) -> pb::VerifyResponse {
        let ingest = self.ingest.lock().unwrap_or_else(|p| p.into_inner());
        pb::VerifyResponse { partitions: crate::evidence::verify_all(&ingest).into_iter().collect() }
    }
}

fn build_evidence_response(ingest: &Ingest) -> pb::EvidenceResponse {
    let mut producers = std::collections::BTreeMap::new();
    let mut accepted_total: u64 = 0;
    let mut rejected_total: u64 = 0;
    for producer_id in ingest.producer_ids() {
        let counters = ingest.producer_counters(producer_id);
        accepted_total += counters.accepted;
        rejected_total += counters.unsigned_count
            + counters.bad_signature_count
            + counters.chain_gap_count
            + counters.chain_break_count
            + counters.mislabeled_count
            + counters.over_clearance_count
            + counters.stale_count
            + counters.duplicate_count
            + counters.shard_mismatch_count;
        producers.insert(producer_id.clone(), counters);
    }
    let mut partitions = std::collections::BTreeMap::new();
    for (shard_key, log) in ingest.partitions() {
        partitions.insert(shard_key.clone(), pb::PartitionEvidence { chain_head: log.tip_hash(), record_count: log.record_count() });
    }
    let identity = ingest.identity_counters();
    pb::EvidenceResponse {
        accepted_total,
        rejected_total,
        identity: Some(pb::IdentityCounters {
            accepted: identity.accepted,
            malformed_pem: identity.malformed_pem,
            not_p384: identity.not_p384,
            issuer_not_trusted: identity.issuer_not_trusted,
            expired: identity.expired,
            not_yet_valid: identity.not_yet_valid,
            openssl_error: identity.openssl_error,
        }),
        producers: producers.into_iter().collect(),
        partitions: partitions.into_iter().collect(),
    }
}

/// `pb::Label` equality as a **set** of caveats (order-independent), matching
/// `av_edge::policy::ProducerPolicy`'s own caveat-set convention rather than `pb::Label`'s
/// derived, order-sensitive `PartialEq`.
fn label_matches(a: &pb::Label, b: &pb::Label) -> bool {
    if a.marking != b.marking {
        return false;
    }
    let mut a_sorted = a.caveats.clone();
    let mut b_sorted = b.caveats.clone();
    a_sorted.sort();
    b_sorted.sort();
    a_sorted == b_sorted
}

fn refuse(refusal: pb::ManifestRefusal, detail: impl Into<String>) -> pb::ManifestAck {
    pb::ManifestAck { accepted: false, refusal: refusal as i32, detail: detail.into(), chain_head: Vec::new() }
}

#[tonic::async_trait]
impl EdgeIngest for EdgeIngestService {
    async fn announce(&self, request: Request<pb::PluginManifest>) -> Result<Response<pb::ManifestAck>, Status> {
        let header = request.metadata().get(forwarded_cert::FORWARDED_CLIENT_CERT_HEADER).map(|v| v.to_str().unwrap_or_default().to_string());
        // Question 202's own observability requirement, and `crate::forwarded_cert`'s
        // module doc's "what was actually checked, and what was not": print exactly what
        // arrived on this header, so a caller running this service behind a real nginx
        // front (`tests/test_edge_ingest_mtls.py`) can capture this process's own stderr
        // and compare nginx's documented `$ssl_client_escaped_cert` escaping against what
        // actually showed up here -- rather than trusting the documentation alone. Always
        // printed when the header is present at all (never gated on any config flag or
        // environment variable -- question 199 -- and never on whether the value later
        // decodes or verifies): a certificate is not secret material, and a defect in
        // nginx's own escaping is exactly the kind of thing this line exists to surface.
        if let Some(escaped) = &header {
            let prefix: String = escaped.chars().take(120).collect();
            eprintln!("av-ingest: received {} header, first 120 chars: {prefix:?} (total length {})", forwarded_cert::FORWARDED_CLIENT_CERT_HEADER, escaped.len());
        }
        let manifest = request.into_inner();
        let now = self.now_tai_ns();

        // Lock order: handshake first, ingest second (see `Handshake`'s own doc comment).
        let mut handshake = self.handshake.lock().unwrap_or_else(|p| p.into_inner());
        let mut ingest = self.ingest.lock().unwrap_or_else(|p| p.into_inner());

        // 1. IDENTITY_REFUSED.
        let mut certificate_verified_key: Option<EcKey<Public>> = None;
        if self.config.require_client_certificate {
            let Some(escaped) = header else {
                return Ok(Response::new(refuse(pb::ManifestRefusal::IdentityRefused, "no forwarded-client-certificate header presented, and this deployment requires one (EdgeIngestConfig::require_client_certificate)")));
            };
            let Some(leaf_pem) = forwarded_cert::percent_decode(&escaped) else {
                return Ok(Response::new(refuse(pb::ManifestRefusal::IdentityRefused, "the forwarded-client-certificate header could not be decoded")));
            };
            let identity = match ingest.verify_identity_once(&leaf_pem, self.config.intermediate_chain_pem.as_deref(), now) {
                Ok(identity) => identity,
                Err(e) => return Ok(Response::new(refuse(pb::ManifestRefusal::IdentityRefused, format!("{e}")))),
            };
            if !manifest.leaf_fingerprint_sha256.is_empty() && manifest.leaf_fingerprint_sha256 != identity.fingerprint_sha256 {
                return Ok(Response::new(refuse(
                    pb::ManifestRefusal::IdentityRefused,
                    format!("manifest declares leaf_fingerprint_sha256={:?}, but the verified certificate's own fingerprint is {:?}", manifest.leaf_fingerprint_sha256, identity.fingerprint_sha256),
                )));
            }
            certificate_verified_key = Some(identity.public_key);
        }

        // 2. UNKNOWN_PLUGIN.
        if let Some(known) = &self.config.known_plugins {
            if !known.iter().any(|p| p == &manifest.producer_id) {
                return Ok(Response::new(refuse(pb::ManifestRefusal::UnknownPlugin, format!("producer_id {:?} is not on this deployment's known_plugins list", manifest.producer_id))));
            }
        }

        // 3. LABEL_NOT_PERMITTED.
        let declared_label = manifest.label.clone().unwrap_or_default();
        if let Some(permitted) = &self.config.permitted_labels {
            if !permitted.iter().any(|l| label_matches(l, &declared_label)) {
                return Ok(Response::new(refuse(pb::ManifestRefusal::LabelNotPermitted, format!("label marking={:?} caveats={:?} is not on this deployment's permitted_labels list", declared_label.marking, declared_label.caveats))));
            }
        }

        // 4. CLEARANCE_ABOVE_LADDER.
        let policy = match ProducerPolicy::new(manifest.producer_id.clone(), declared_label.marking.clone(), declared_label.caveats.clone(), self.config.clearance_ladder.clone(), manifest.clearance.clone(), self.config.max_age_ns) {
            Ok(p) => p,
            Err(e) => return Ok(Response::new(refuse(pb::ManifestRefusal::ClearanceAboveLadder, format!("{e}")))),
        };

        // 5. MANIFEST_MISMATCH.
        if let Some(existing) = handshake.manifests.get(&manifest.producer_id) {
            if existing != &manifest {
                return Ok(Response::new(refuse(pb::ManifestRefusal::ManifestMismatch, format!("producer_id {:?} already announced a different manifest on this service instance", manifest.producer_id))));
            }
        }

        // Accepted: register the policy (idempotent -- Ingest::register_producer simply
        // replaces it, which is fine here since we already proved the manifest is
        // unchanged from any prior accepted one), cache the verifying key, and remember
        // the manifest.
        ingest.register_producer(policy);
        let verify_key = match certificate_verified_key {
            Some(k) => k,
            None => match self.config.verify_keys.get(&manifest.producer_id) {
                Some(k) => k.clone(),
                None => return Ok(Response::new(refuse(pb::ManifestRefusal::IdentityRefused, format!("no certificate presented and no configured verify_keys entry for producer_id {:?}", manifest.producer_id)))),
            },
        };
        handshake.verify_keys.insert(manifest.producer_id.clone(), verify_key);
        let chain_head = ingest.producer_counters(&manifest.producer_id).chain_head;
        handshake.manifests.insert(manifest.producer_id.clone(), manifest);

        Ok(Response::new(pb::ManifestAck { accepted: true, refusal: pb::ManifestRefusal::Unspecified as i32, detail: "accepted".to_string(), chain_head }))
    }

    type SubmitStream = std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<pb::BatchVerdict, Status>> + Send + 'static>>;

    async fn submit(&self, request: Request<tonic::Streaming<pb::MeasurementBatch>>) -> Result<Response<Self::SubmitStream>, Status> {
        let mut stream = request.into_inner();
        let mut verdicts: Vec<Result<pb::BatchVerdict, Status>> = Vec::new();

        while let Some(batch) = stream.message().await? {
            let now = self.now_tai_ns();
            // Lock order: handshake first, ingest second (see `Handshake`'s own doc) --
            // the two are never held simultaneously here, so this scoped block just
            // returns the one thing `submit` needs from `handshake` before it is dropped.
            let verify_key = {
                let handshake = self.handshake.lock().unwrap_or_else(|p| p.into_inner());
                handshake.verify_keys.get(&batch.producer_id).cloned()
            };
            let Some(verify_key) = verify_key else {
                verdicts.push(Err(Status::failed_precondition(format!("producer_id {:?} never announced on this serving process -- Announce must succeed before Submit", batch.producer_id))));
                continue;
            };
            let mut ingest = self.ingest.lock().unwrap_or_else(|p| p.into_inner());
            let outcome = ingest.submit(&batch, Signer::Key(verify_key), now).map_err(|e| Status::internal(format!("{e}")))?;
            match outcome {
                IngestOutcome::Verdict(v) => verdicts.push(Ok(v)),
                IngestOutcome::UnknownProducer(p) => verdicts.push(Err(Status::failed_precondition(format!("producer_id {p:?} has no registered policy (this should be unreachable once Announce has succeeded)")))),
                IngestOutcome::IdentityRejected(e) => verdicts.push(Err(Status::internal(format!("unexpected identity rejection on the Signer::Key path: {e}")))),
            }
        }

        let out = futures_util::stream::iter(verdicts);
        Ok(Response::new(Box::pin(out)))
    }

    async fn get_evidence(&self, _request: Request<pb::EvidenceRequest>) -> Result<Response<pb::EvidenceResponse>, Status> {
        Ok(Response::new(self.evidence_snapshot()))
    }

    async fn verify_ledger(&self, _request: Request<pb::VerifyRequest>) -> Result<Response<pb::VerifyResponse>, Status> {
        Ok(Response::new(self.verify_snapshot()))
    }
}
