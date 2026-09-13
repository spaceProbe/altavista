//! The label-aware query resolution D1/D2 both drive ([`GatewayCore`]), and the generated
//! `DataGatewayService` server trait implementation over it ([`DataGatewayServiceImpl`]).
//! [`GatewayCore::query`] is the ONE place this crate's ordered, typed refusal chain lives
//! -- both the gRPC service below and [`crate::mcp`]'s `query` tool call through it, so
//! there is exactly one implementation of the order below for a reviewer (or a future
//! worker) to read: caller-supplied path -> malformed request -> unknown run -> undecodable
//! bytes -> config hash mismatch -> mislabeled -> over-clearance -> product missing on this
//! host.
//!
//! ## Where this order deliberately differs from `altavista/server.py`'s mirrored route
//!
//! D1 names the Python route's order as "malformed request -> unknown run -> product
//! missing on this host -> undecodable bytes" (undecodable bytes LAST). This gateway's own
//! order puts undecodable bytes BEFORE product-missing, for a structural reason, not an
//! oversight: the Python route's "missing on this host" is a filesystem `stat` (`Path::
//! is_file()`) that needs no decode and so can run before attempting one; this gateway has
//! no filesystem step at that point at all -- [`crate::catalogue::RunCatalogue::resolve`]'s
//! "unknown run" is a catalogue key lookup (also no decode needed, so it stays first,
//! exactly like the Python route's), but checking whether a *specific selector's* product is
//! present (e.g. `GATEWAY_SELECTOR_SCORES` against a run with no scores) requires a decoded
//! `RunProducts` value to inspect a field of, so it cannot happen before decoding can even be
//! attempted. `config_hash` mismatch (D1's own addition, absent from the Python route) is
//! likewise checked only after a successful decode, since the value it compares against
//! (`RunProducts.provenance.config_hash`) only exists once decoded.

use std::sync::Arc;

use av_cdm::pb::{GatewayQueryRequest, GatewayQueryResponse, GatewaySelector, RunIdentity};
use tonic::{Request, Response, Status};

use crate::pb::data_gateway_service_server::DataGatewayService;
pub use crate::pb::data_gateway_service_server::DataGatewayServiceServer;

use crate::catalogue::{ResolveError, ResolvedRun, RunCatalogue};
use crate::counters::{Counted, Counters};
use crate::labels::{ClearanceLadder, LabelRefusal};
use crate::query_id::compute_query_id;

/// Every way [`GatewayCore::query`] can refuse a request. Every variant is its own typed,
/// counted reason (D1/D2's own acceptance line) -- never one opaque string, and never
/// swallowed: [`GatewayCore::query`] returns `Err` the instant one of these is decided, and
/// the counter for it has already been incremented by then (see that method's own body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// D1: `GatewayQueryRequest.caller_supplied_products_uri` was non-empty. Checked FIRST,
    /// before any other field of the request is even inspected -- see the module doc's
    /// ordered chain and `crate::catalogue`'s module doc for why this mirrors `altavista/
    /// server.py`'s `POST /api/cdm/sweep/sample`.
    CallerSuppliedPath,
    /// The request is structurally invalid: no `run` (`RunIdentity`), an empty `run_id`, an
    /// empty `caller_clearance`, or `GATEWAY_SELECTOR_UNSPECIFIED`.
    MalformedRequest { detail: String },
    /// D1's identity-resolution refusal chain (unknown run / config hash mismatch /
    /// undecodable bytes) -- see [`crate::catalogue::ResolveError`].
    Resolve(ResolveError),
    /// D2's label refusal chain (mislabeled / over-clearance) -- see [`crate::labels::
    /// LabelRefusal`].
    Label(LabelRefusal),
    /// The run itself is known and its bytes decode, but the specific product this
    /// selector asked for is absent for this run (e.g. `GATEWAY_SELECTOR_SCORES` against a
    /// run whose `RunProducts.scores` is empty) -- the fourth link in D1's ordered chain,
    /// checked only for a non-`ALL` selector (an `ALL` response legitimately has empty
    /// sub-fields for a run that never produced that kind of product at all).
    ProductMissingOnHost { run_id: String, selector: GatewaySelector },
}

impl Counted for RefusalReason {
    fn code(&self) -> &'static str {
        match self {
            RefusalReason::CallerSuppliedPath => "gateway_caller_supplied_path",
            RefusalReason::MalformedRequest { .. } => "gateway_malformed_request",
            RefusalReason::Resolve(e) => e.code(),
            RefusalReason::Label(e) => e.code(),
            RefusalReason::ProductMissingOnHost { .. } => "gateway_product_missing_on_host",
        }
    }
}

impl std::fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefusalReason::CallerSuppliedPath => write!(
                f,
                "caller_supplied_products_uri is set; a caller names a run by identity, never a path or URI (D1) -- this field is never read for resolution"
            ),
            RefusalReason::MalformedRequest { detail } => write!(f, "malformed GatewayQueryRequest: {detail}"),
            RefusalReason::Resolve(e) => write!(f, "{e}"),
            RefusalReason::Label(e) => write!(f, "{e}"),
            RefusalReason::ProductMissingOnHost { run_id, selector } => {
                write!(f, "run {run_id:?} has no product for selector {} on this host", selector.as_str_name())
            }
        }
    }
}

/// The label-aware query resolution shared by the gRPC service and the MCP `query` tool.
pub struct GatewayCore {
    catalogue: RunCatalogue,
    ladder: ClearanceLadder,
    counters: Arc<Counters>,
}

impl GatewayCore {
    pub fn new(catalogue: RunCatalogue, ladder: ClearanceLadder, counters: Arc<Counters>) -> Self {
        Self { catalogue, ladder, counters }
    }

    pub fn counters(&self) -> &Arc<Counters> {
        &self.counters
    }

    fn refuse(&self, reason: RefusalReason) -> RefusalReason {
        self.counters.record(&reason);
        reason
    }

    /// D1/D2's ordered, typed refusal chain: caller-supplied path -> malformed request ->
    /// unknown run / config hash mismatch / undecodable bytes -> mislabeled / over-clearance
    /// -> product missing on this host. Every early return has already incremented its
    /// counter (via [`Self::refuse`]) before this function returns `Err`.
    pub fn query(&self, req: &GatewayQueryRequest) -> Result<GatewayQueryResponse, RefusalReason> {
        if !req.caller_supplied_products_uri.is_empty() {
            return Err(self.refuse(RefusalReason::CallerSuppliedPath));
        }
        let Some(identity) = req.run.as_ref() else {
            return Err(self.refuse(RefusalReason::MalformedRequest { detail: "run (RunIdentity) is required".to_string() }));
        };
        if identity.run_id.is_empty() {
            return Err(self.refuse(RefusalReason::MalformedRequest { detail: "run.run_id must not be empty".to_string() }));
        }
        if req.caller_clearance.is_empty() {
            return Err(self.refuse(RefusalReason::MalformedRequest { detail: "caller_clearance must not be empty".to_string() }));
        }
        let selector = GatewaySelector::try_from(req.selector).unwrap_or(GatewaySelector::Unspecified);
        if selector == GatewaySelector::Unspecified {
            return Err(self.refuse(RefusalReason::MalformedRequest { detail: "selector must not be GATEWAY_SELECTOR_UNSPECIFIED".to_string() }));
        }

        let resolved: ResolvedRun = self.catalogue.resolve(identity).map_err(|e| self.refuse(RefusalReason::Resolve(e)))?;

        if let Some(label_refusal) = self.ladder.classify(&req.caller_clearance, &resolved.label) {
            return Err(self.refuse(RefusalReason::Label(label_refusal)));
        }

        if selector != GatewaySelector::All {
            let empty = match selector {
                GatewaySelector::Trajectories => resolved.run_products.trajectories.is_empty(),
                GatewaySelector::Events => resolved.run_products.events.is_empty(),
                GatewaySelector::Scores => resolved.run_products.scores.is_empty(),
                GatewaySelector::Measurements => resolved.run_products.measurements.is_empty(),
                GatewaySelector::All | GatewaySelector::Unspecified => false,
            };
            if empty {
                return Err(self.refuse(RefusalReason::ProductMissingOnHost { run_id: identity.run_id.clone(), selector }));
            }
        }

        let want_trajectories = matches!(selector, GatewaySelector::All | GatewaySelector::Trajectories);
        let want_events = matches!(selector, GatewaySelector::All | GatewaySelector::Events);
        let want_scores = matches!(selector, GatewaySelector::All | GatewaySelector::Scores);
        let want_measurements = matches!(selector, GatewaySelector::All | GatewaySelector::Measurements);

        // D5: the query id is derived from the REQUEST's own canonical content (what the
        // caller asked for), never from the resolved response -- see crate::query_id's
        // module doc for why (so a replay recomputes the identical id from the same query).
        let query_id = compute_query_id(&identity.run_id, &identity.config_hash, &req.caller_clearance, selector);

        Ok(GatewayQueryResponse {
            run: Some(RunIdentity { run_id: identity.run_id.clone(), config_hash: identity.config_hash.clone() }),
            product_label: Some(resolved.label),
            trajectories: if want_trajectories { resolved.run_products.trajectories } else { Default::default() },
            events: if want_events { resolved.run_products.events } else { Vec::new() },
            scores: if want_scores { resolved.run_products.scores } else { Default::default() },
            measurements: if want_measurements { resolved.run_products.measurements } else { Vec::new() },
            query_id,
        })
    }
}

/// Maps a [`RefusalReason`] to a [`tonic::Status`], deliberately chosen per kind (mirrors
/// `crates/av-command/src/service.rs`'s own `to_status` doc convention: the typed error's
/// `Display` text is always preserved verbatim as the status message).
fn to_status(reason: RefusalReason) -> Status {
    let message = reason.to_string();
    match &reason {
        RefusalReason::CallerSuppliedPath | RefusalReason::MalformedRequest { .. } => Status::invalid_argument(message),
        RefusalReason::Resolve(ResolveError::UnknownRun { .. }) => Status::not_found(message),
        RefusalReason::Resolve(ResolveError::ConfigHashMismatch { .. }) => Status::invalid_argument(message),
        RefusalReason::Resolve(ResolveError::UndecodableBytes { .. }) => Status::internal(message),
        RefusalReason::Label(_) => Status::permission_denied(message),
        RefusalReason::ProductMissingOnHost { .. } => Status::not_found(message),
    }
}

/// The `DataGatewayService` gRPC server, over [`GatewayCore`]. Every rpc is a thin wire
/// adapter -- request validation and resolution logic lives entirely in [`GatewayCore::
/// query`], never duplicated here (the same discipline `crates/av-command/src/service.rs`'s
/// own module doc states for `CommandAuthorityServiceImpl`).
pub struct DataGatewayServiceImpl {
    core: Arc<GatewayCore>,
}

impl DataGatewayServiceImpl {
    pub fn new(core: Arc<GatewayCore>) -> Self {
        Self { core }
    }
}

#[tonic::async_trait]
impl DataGatewayService for DataGatewayServiceImpl {
    async fn query(&self, request: Request<GatewayQueryRequest>) -> Result<Response<GatewayQueryResponse>, Status> {
        let req = request.into_inner();
        self.core.query(&req).map(Response::new).map_err(to_status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::CatalogueEntry;
    use av_cdm::pb::{Label, Provenance, RunProducts};
    use std::collections::BTreeMap;

    fn label(marking: &str) -> Label {
        Label { marking: marking.to_string(), caveats: vec![] }
    }

    fn core_with_two_runs() -> GatewayCore {
        let mut entries = BTreeMap::new();
        entries.insert(
            "run-cui".to_string(),
            CatalogueEntry::from_run_products(
                label("CUI"),
                &RunProducts {
                    run_id: "run-cui".to_string(),
                    provenance: Some(Provenance { config_hash: "hash-cui".to_string(), ..Default::default() }),
                    scores: BTreeMap::new(),
                    ..Default::default()
                },
            ),
        );
        entries.insert(
            "run-secret".to_string(),
            CatalogueEntry::from_run_products(
                label("SECRET"),
                &RunProducts { run_id: "run-secret".to_string(), provenance: Some(Provenance::default()), ..Default::default() },
            ),
        );
        let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]);
        GatewayCore::new(RunCatalogue::new(entries), ladder, Arc::new(Counters::new()))
    }

    fn req(run_id: &str, clearance: &str, selector: GatewaySelector) -> GatewayQueryRequest {
        GatewayQueryRequest {
            run: Some(RunIdentity { run_id: run_id.to_string(), config_hash: String::new() }),
            caller_clearance: clearance.to_string(),
            selector: selector as i32,
            caller_supplied_products_uri: String::new(),
        }
    }

    #[test]
    fn query_allows_a_caller_at_the_products_own_clearance() {
        let core = core_with_two_runs();
        let resp = core.query(&req("run-cui", "CUI", GatewaySelector::All)).unwrap();
        assert_eq!(resp.run.unwrap().run_id, "run-cui");
        assert_eq!(resp.product_label.unwrap().marking, "CUI");
        assert!(!resp.query_id.is_empty());
    }

    #[test]
    fn query_refuses_caller_supplied_path_before_anything_else_and_counts_it() {
        let core = core_with_two_runs();
        let mut r = req("run-cui", "CUI", GatewaySelector::All);
        r.caller_supplied_products_uri = "/etc/passwd".to_string();
        // Even a run_id that does not exist at all must still be refused as
        // CallerSuppliedPath first, proving this check runs before identity resolution.
        r.run = Some(RunIdentity { run_id: "totally-unknown-run".to_string(), config_hash: String::new() });
        let err = core.query(&r).unwrap_err();
        assert_eq!(err, RefusalReason::CallerSuppliedPath);
        assert_eq!(core.counters().get("gateway_caller_supplied_path"), 1);
    }

    #[test]
    fn query_refuses_and_counts_a_malformed_request() {
        let core = core_with_two_runs();
        let mut r = req("", "CUI", GatewaySelector::All);
        r.run = None;
        let err = core.query(&r).unwrap_err();
        assert!(matches!(err, RefusalReason::MalformedRequest { .. }), "{err:?}");
        assert_eq!(core.counters().get("gateway_malformed_request"), 1);
    }

    #[test]
    fn query_refuses_and_counts_an_unknown_run() {
        let core = core_with_two_runs();
        let err = core.query(&req("run-does-not-exist", "SECRET", GatewaySelector::All)).unwrap_err();
        assert_eq!(err, RefusalReason::Resolve(ResolveError::UnknownRun { run_id: "run-does-not-exist".to_string() }));
        assert_eq!(core.counters().get("catalogue_unknown_run"), 1);
    }

    #[test]
    fn query_refuses_and_counts_a_mislabeled_query_marking_not_on_ladder() {
        let core = core_with_two_runs();
        let err = core.query(&req("run-cui", "TOP-SECRET", GatewaySelector::All)).unwrap_err();
        assert!(matches!(err, RefusalReason::Label(LabelRefusal::MarkingNotOnLadder { .. })), "{err:?}");
        assert_eq!(core.counters().get("label_caller_marking_not_on_ladder"), 1);
    }

    #[test]
    fn query_refuses_and_counts_over_clearance() {
        let core = core_with_two_runs();
        let err = core.query(&req("run-secret", "CUI", GatewaySelector::All)).unwrap_err();
        assert!(matches!(err, RefusalReason::Label(LabelRefusal::OverClearance { .. })), "{err:?}");
        assert_eq!(core.counters().get("label_over_clearance"), 1);
    }

    #[test]
    fn query_refuses_and_counts_product_missing_on_host() {
        let core = core_with_two_runs();
        // run-cui has no scores (built with an empty map above).
        let err = core.query(&req("run-cui", "CUI", GatewaySelector::Scores)).unwrap_err();
        assert_eq!(err, RefusalReason::ProductMissingOnHost { run_id: "run-cui".to_string(), selector: GatewaySelector::Scores });
        assert_eq!(core.counters().get("gateway_product_missing_on_host"), 1);
    }

    #[test]
    fn query_id_is_derived_from_the_request_and_is_identical_on_repeat() {
        let core = core_with_two_runs();
        let a = core.query(&req("run-cui", "CUI", GatewaySelector::All)).unwrap();
        let b = core.query(&req("run-cui", "CUI", GatewaySelector::All)).unwrap();
        assert_eq!(a.query_id, b.query_id);
        // run-cui has no events (built with RunProducts::default()'s empty Vec above), so
        // GATEWAY_SELECTOR_EVENTS would legitimately refuse ProductMissingOnHost -- this
        // varies caller_clearance instead (SECRET still clears CUI-labelled run-cui) to
        // prove the id differs by a different query input, not by a different selector's
        // own success/failure.
        let c = core.query(&req("run-cui", "SECRET", GatewaySelector::All)).unwrap();
        assert_ne!(a.query_id, c.query_id);
    }
}
