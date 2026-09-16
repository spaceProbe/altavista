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
//!
//! ## H2c: `GATEWAY_SELECTOR_CATALOG` is routed BEFORE this chain, never through it
//!
//! Every check this module's own doc above describes (`caller-supplied path` through
//! `product missing on this host`) is about ONE `RunIdentity`'s own `RunProducts` -- a
//! `GATEWAY_SELECTOR_CATALOG` request carries no `run` at all (`crate::catalog_selector`'s
//! own module doc: "a run identity is NOT required for it"), so [`GatewayCore::query`] itself
//! is never the function that serves it. [`authenticated_query`] below is the ONE place that
//! decides which of the two functions a request reaches: authentication itself runs first,
//! UNCONDITIONALLY, for every selector including `GATEWAY_SELECTOR_CATALOG` (there is no
//! selector-dependent branch anywhere in [`crate::auth::AuthContext::authenticate_query`]
//! itself, and this function adds none) -- only AFTER that succeeds and `req.caller_clearance`
//! has been overwritten to the verified marking does a raw peek at `req.selector` route
//! `GATEWAY_SELECTOR_CATALOG` to [`crate::catalog_selector::query_catalog`] and every other
//! selector to [`GatewayCore::query`], unchanged. [`GatewayCore::query`]'s own exhaustive
//! `selector` match (below) still
//! has to name `GatewaySelector::Catalog` for the compiler, so it folds that arm into the
//! SAME `empty => false` catch-all `GatewaySelector::All`/`GatewaySelector::Unspecified`
//! already use, as a defensive no-op: a `GATEWAY_SELECTOR_CATALOG` request that somehow
//! reaches [`GatewayCore::query`] directly (bypassing [`authenticated_query`]'s own routing --
//! not a real code path in this crate, but this function is also called directly by this
//! module's own tests) still refuses cleanly as `MalformedRequest` (its `run` field check
//! runs first, and a catalog request carries no `run`), never a panic and never a silent
//! empty-but-Ok(All)-shaped response.

use std::sync::Arc;

use av_cdm::pb::{GatewayQueryRequest, GatewayQueryResponse, GatewaySelector, RunIdentity};
use tonic::{Request, Response, Status};

use crate::pb::data_gateway_service_server::DataGatewayService;
pub use crate::pb::data_gateway_service_server::DataGatewayServiceServer;

use crate::auth::{to_status as auth_to_status, AuthContext, AuthRefusal};
use crate::catalog_selector::{self, CatalogHandle, CatalogRefusal};
use crate::catalogue::{ResolveError, ResolvedRun, RunCatalogue};
use crate::counters::{Counted, Counters};
use crate::labels::{ClearanceLadder, LabelRefusal};
use crate::query_id::compute_query_id;

/// Every way [`GatewayCore::query`] can refuse a request. Every variant is its own typed,
/// counted reason (D1/D2's own acceptance line) -- never one opaque string, and never
/// swallowed: [`GatewayCore::query`] returns `Err` the instant one of these is decided, and
/// the counter for it has already been incremented by then (see that method's own body).
///
/// `PartialEq` only, no `Eq` (H2c): [`RefusalReason::Catalog`] nests [`CatalogRefusal`], which
/// carries `f64` fields (a refused `CatalogQuery.bbox`'s own coordinates) -- `f64` has no
/// total equality, so nothing wrapping it can derive `Eq` either. Every existing test against
/// this enum uses `assert_eq!`/`matches!`, both of which need only `PartialEq`+`Debug`; no
/// code in this crate ever required `RefusalReason: Eq` specifically (checked: it is never a
/// `HashSet`/`BTreeSet` element or map key anywhere in this crate).
#[derive(Debug, Clone, PartialEq)]
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
    /// H2c: `GATEWAY_SELECTOR_CATALOG`'s own refusal chain -- see [`crate::catalog_selector`]'s
    /// own module doc. Nested exactly like [`RefusalReason::Resolve`]/[`RefusalReason::
    /// Label`] above: one sub-module's own typed enum, never flattened into this one.
    Catalog(CatalogRefusal),
}

impl Counted for RefusalReason {
    fn code(&self) -> &'static str {
        match self {
            RefusalReason::CallerSuppliedPath => "gateway_caller_supplied_path",
            RefusalReason::MalformedRequest { .. } => "gateway_malformed_request",
            RefusalReason::Resolve(e) => e.code(),
            // question 218: LabelRefusal is a foreign type (av-label) now, and Counted is
            // also foreign (av-command, R3.1) -- `impl Counted for LabelRefusal` here would
            // be E0117 (neither is local to this crate). `crate::labels::code` is the same
            // mapping as a free function; see that module's own doc.
            RefusalReason::Label(e) => crate::labels::code(e),
            RefusalReason::ProductMissingOnHost { .. } => "gateway_product_missing_on_host",
            RefusalReason::Catalog(e) => e.code(),
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
            RefusalReason::Catalog(e) => write!(f, "{e}"),
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
    /// H2c: how to reach `crates/av-catalog`'s own PostgreSQL+PostGIS tier, or `None` when
    /// this deployment has none configured -- [`Self::new`]'s own signature is UNCHANGED
    /// (every one of this crate's eight existing `GatewayCore::new(...)` call sites, in
    /// `crates/av-gateway/src/{bin/av-gateway.rs,gateway.rs,mcp.rs}` and four integration
    /// test files, keeps compiling with no edit) -- a fresh `GatewayCore` always starts with
    /// no catalog handle; [`Self::with_catalog`] is the one, additive way to attach one. See
    /// [`crate::catalog_selector`]'s own module doc for what "no catalog configured" means at
    /// query time: a typed, counted refusal, never an empty success and never a panic.
    catalog: Option<CatalogHandle>,
}

impl GatewayCore {
    pub fn new(catalogue: RunCatalogue, ladder: ClearanceLadder, counters: Arc<Counters>) -> Self {
        Self { catalogue, ladder, counters, catalog: None }
    }

    /// H2c: attaches this deployment's own catalog handle, additively -- see [`Self::catalog`]
    /// field doc for why this is a separate, consuming builder method rather than a fourth
    /// constructor argument. `crates/av-gateway/src/bin/av-gateway.rs` is this method's one
    /// production call site (when `--catalog-host` is configured); this crate's own
    /// `tests/catalog_selector.rs` is the other.
    pub fn with_catalog(mut self, catalog: CatalogHandle) -> Self {
        self.catalog = Some(catalog);
        self
    }

    pub fn counters(&self) -> &Arc<Counters> {
        &self.counters
    }

    /// H2c: `Some` when this deployment has a catalog tier configured ([`Self::
    /// with_catalog`]), `None` otherwise -- [`crate::catalog_selector::query_catalog`]'s one
    /// way to reach it, and the one place `GATEWAY_SELECTOR_CATALOG`'s "not configured"
    /// refusal is decided.
    pub(crate) fn catalog_handle(&self) -> Option<&CatalogHandle> {
        self.catalog.as_ref()
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
                // GatewaySelector::Catalog never legitimately reaches this line at all: the
                // module doc's "H2c" section names the real path (authenticated_query routes
                // it to crate::catalog_selector::query_catalog before GatewayCore::query is
                // ever called) -- named here, not omitted, purely so this match stays
                // exhaustive and defensive (never a panic) if some future or test call site
                // ever does reach GatewayCore::query directly with this selector: such a
                // request already failed the `run` check above (a catalog request carries
                // none), so this arm's own value is never actually observed.
                GatewaySelector::All | GatewaySelector::Unspecified | GatewaySelector::Catalog => false,
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
            // H2c: never populated by GatewayCore::query itself -- GATEWAY_SELECTOR_CATALOG
            // is routed to crate::catalog_selector::query_catalog before this function is
            // ever reached (see the module doc's "H2c" section), so every response THIS
            // function builds carries an empty catalog_records, exactly like every existing
            // (pre-H2c) selector's response implicitly did before this field existed.
            catalog_records: Vec::new(),
        })
    }
}

/// R5.1: the combined error [`authenticated_query`] can return -- either [`AuthRefusal`]
/// (question 208(b)'s own authentication gate) or the pre-existing [`RefusalReason`] (D1/D2's
/// ordered chain, unchanged). Both surfaces this function serves (the gRPC `Query` rpc below
/// and `crate::mcp::McpHandler::handle_query`) map each side to their own wire shape via
/// [`to_status`]/[`crate::auth::to_status`] or their own MCP equivalent -- never by re-deriving
/// which is which from message prose (both variants stay distinguishable by type).
#[derive(Debug)]
pub enum AuthenticatedQueryError {
    Auth(AuthRefusal),
    Refusal(RefusalReason),
}

impl std::fmt::Display for AuthenticatedQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthenticatedQueryError::Auth(e) => write!(f, "{e}"),
            AuthenticatedQueryError::Refusal(e) => write!(f, "{e}"),
        }
    }
}

/// R5.1/question 208(b): the ONE authenticated entry point both the gRPC `Query` rpc below and
/// the MCP `query` tool (`crate::mcp::McpHandler::handle_query`) call through -- never a second
/// copy of the auth-then-query sequence. Authenticates `token` for [`crate::auth::Surface::
/// Query`] (verify -> human role check -> clearance derivation -> clearance-agreement check
/// against `req.caller_clearance`, invariant C) EXACTLY as before H2c -- this is still the only
/// place a `GatewayQueryRequest` is authenticated, and [`crate::auth::AuthContext::
/// authenticate_query`] itself is entirely unaware `GATEWAY_SELECTOR_CATALOG` exists (there is
/// no second verifier and no relaxed check for it: common.md's binding rule, restated here at
/// the one call site that could have been tempted to add one).
///
/// H2c: `async` (it was not, before this task) purely because [`crate::catalog_selector::
/// query_catalog`] genuinely needs to await a real PostgreSQL round trip -- [`GatewayCore::
/// query`] itself stays perfectly synchronous, unchanged. After authentication succeeds and
/// `req.caller_clearance` is overwritten to the verified, token-derived marking (so BOTH
/// branches below see exactly one clearance value, the authoritative one, never the
/// caller-supplied field directly), a raw peek at `req.selector` decides which of the two
/// functions serves this request: `GATEWAY_SELECTOR_CATALOG` routes to [`crate::
/// catalog_selector::query_catalog`] (which carries no `run` at all and so could never survive
/// [`GatewayCore::query`]'s own `run`-required check -- see that function's own module-doc
/// section, "H2c"); every other selector routes to [`GatewayCore::query`], byte-for-byte the
/// same call this function made before H2c existed.
pub async fn authenticated_query(core: &GatewayCore, auth: &AuthContext, counters: &Counters, token: &str, mut req: GatewayQueryRequest) -> Result<GatewayQueryResponse, AuthenticatedQueryError> {
    let (_principal, verified_clearance) = auth.authenticate_query(token, &req.caller_clearance, counters).map_err(AuthenticatedQueryError::Auth)?;
    req.caller_clearance = verified_clearance;

    if req.selector == GatewaySelector::Catalog as i32 {
        catalog_selector::query_catalog(core, &req, counters).await.map_err(AuthenticatedQueryError::Refusal)
    } else {
        core.query(&req).map_err(AuthenticatedQueryError::Refusal)
    }
}

/// Maps an [`AuthenticatedQueryError`] to a [`tonic::Status`] for [`DataGatewayServiceImpl`] --
/// the [`AuthRefusal`] half through [`crate::auth::to_status`] (UNAUTHENTICATED/
/// PERMISSION_DENIED), the [`RefusalReason`] half through [`to_status`] below, unchanged.
fn authenticated_query_to_status(err: AuthenticatedQueryError) -> Status {
    match err {
        AuthenticatedQueryError::Auth(e) => auth_to_status(e),
        AuthenticatedQueryError::Refusal(e) => to_status(e),
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
        // H2c: a malformed CatalogQuery is the caller's own fault (invalid_argument, mirroring
        // CallerSuppliedPath/MalformedRequest above); "not configured" is this DEPLOYMENT's own
        // state, not something a differently-shaped request could avoid (failed_precondition,
        // mirroring how a client is expected to treat that code: retrying with a different
        // request will not help); a real catalog-tier failure (connect/query) is this
        // deployment's own backend failing, not the caller's (internal, mirroring
        // ResolveError::UndecodableBytes above).
        RefusalReason::Catalog(CatalogRefusal::NoCatalogQuery)
        | RefusalReason::Catalog(CatalogRefusal::LimitOutOfRange { .. })
        | RefusalReason::Catalog(CatalogRefusal::BboxInvalid { .. })
        | RefusalReason::Catalog(CatalogRefusal::TimeRangeInvalid { .. }) => Status::invalid_argument(message),
        RefusalReason::Catalog(CatalogRefusal::CatalogNotConfigured) => Status::failed_precondition(message),
        RefusalReason::Catalog(CatalogRefusal::QueryFailed { .. }) => Status::internal(message),
    }
}

/// The `DataGatewayService` gRPC server, over [`GatewayCore`]. Every rpc is a thin wire
/// adapter -- request validation and resolution logic lives entirely in [`GatewayCore::
/// query`], never duplicated here (the same discipline `crates/av-command/src/service.rs`'s
/// own module doc states for `CommandAuthorityServiceImpl`).
pub struct DataGatewayServiceImpl {
    core: Arc<GatewayCore>,
    auth: Arc<AuthContext>,
}

impl DataGatewayServiceImpl {
    pub fn new(core: Arc<GatewayCore>, auth: Arc<AuthContext>) -> Self {
        Self { core, auth }
    }
}

#[tonic::async_trait]
impl DataGatewayService for DataGatewayServiceImpl {
    /// R5.1/question 208(b): authenticates every caller before touching any product data --
    /// see [`authenticated_query`]'s own doc for the full sequence.
    async fn query(&self, request: Request<GatewayQueryRequest>) -> Result<Response<GatewayQueryResponse>, Status> {
        let req = request.into_inner();
        let token = req.caller_token.clone();
        authenticated_query(&self.core, &self.auth, self.core.counters(), &token, req).await.map(Response::new).map_err(authenticated_query_to_status)
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
            caller_token: String::new(),
            // H2c: this module's own tests never exercise GATEWAY_SELECTOR_CATALOG (that is
            // crates/av-gateway/tests/catalog_selector.rs's own job) -- every request this
            // helper builds carries no catalog_query, exactly as a pre-H2c request did before
            // this field existed.
            catalog_query: None,
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

    // ---- R5.1/question 208(b): authenticated_query, end to end over a real catalogue ----

    mod authenticated {
        use super::*;
        use crate::auth::{AuthContext, GroupClearanceMap};
        use av_command::authz::RoleTable;
        use av_command::clock::TestClock;
        use av_command::oidc::IssuerConfig;
        use av_command::test_support::{valid_claims, TestIssuer};
        use std::collections::BTreeMap;

        const ISSUER: &str = "https://sso.test.example/";
        const AUDIENCE: &str = "av-gateway";
        const NOW_UNIX_S: i64 = 1_760_000_000;

        fn auth_ctx(issuer: &TestIssuer, human_roles: &[(&str, &[&str])], group_clearance: &[(&str, &str)]) -> AuthContext {
            let mut roles = BTreeMap::new();
            for (role, surfaces) in human_roles {
                roles.insert(role.to_string(), surfaces.iter().map(|s| s.to_string()).collect());
            }
            let mut clearance = BTreeMap::new();
            for (group, marking) in group_clearance {
                clearance.insert(group.to_string(), marking.to_string());
            }
            AuthContext::new(
                Arc::new(IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap()),
                Arc::new(RoleTable::from_config(&roles)),
                Arc::new(RoleTable::default()),
                Arc::new(GroupClearanceMap::new(clearance)),
                Arc::new(ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()])),
                Arc::new(TestClock::new(NOW_UNIX_S * 1_000_000_000)),
            )
        }

        fn mint(issuer: &TestIssuer, groups: &[&str]) -> String {
            let mut claims = valid_claims(ISSUER, AUDIENCE, "operator-1", NOW_UNIX_S, 3_600);
            claims["groups"] = serde_json::json!(groups);
            issuer.mint(&claims)
        }

        /// An unauthenticated (empty-token) caller is refused before D1/D2's own chain ever
        /// runs, and the auth counter -- not any `RefusalReason` counter -- is what moved.
        #[tokio::test]
        async fn an_unauthenticated_caller_is_refused_before_gatewaycore_query_runs_and_the_auth_counter_moves() {
            let core = core_with_two_runs();
            let auth = auth_ctx(&TestIssuer::new(), &[("operators", &["query"])], &[("operators", "CUI")]);
            let counters = Counters::new();
            let err = authenticated_query(&core, &auth, &counters, "", req("run-cui", "", GatewaySelector::All)).await.unwrap_err();
            assert!(matches!(err, AuthenticatedQueryError::Auth(crate::auth::AuthRefusal::MissingToken { .. })), "{err:?}");
            assert_eq!(counters.get("gateway_auth_missing_token"), 1);
            // D1/D2's own chain never ran -- no RefusalReason counter incremented.
            assert_eq!(core.counters().get("gateway_malformed_request"), 0);
        }

        /// **Invariant C's own required test, at this crate's real, catalogued layer.** A
        /// token whose group maps to CUI (a genuinely low clearance) sends `caller_clearance =
        /// "SECRET"` against `run-secret`, a run this fixture's own catalogue labels SECRET --
        /// refused, and the counter for the exact refusal moved. Proves the request field can
        /// never buy read access to a SECRET-labelled product a CUI-cleared token does not
        /// actually have.
        #[tokio::test]
        async fn a_low_clearance_token_declaring_secret_for_a_secret_labelled_run_is_refused_and_the_counter_moves() {
            let core = core_with_two_runs(); // run-secret is labelled SECRET, per this module's own fixture above.
            let issuer = TestIssuer::new();
            let auth = auth_ctx(&issuer, &[("operators", &["query"])], &[("operators", "CUI")]);
            let counters = Counters::new();
            let token = mint(&issuer, &["operators"]);

            assert_eq!(counters.get("gateway_auth_clearance_mismatch"), 0);
            let err = authenticated_query(&core, &auth, &counters, &token, req("run-secret", "SECRET", GatewaySelector::All)).await.unwrap_err();
            assert!(matches!(err, AuthenticatedQueryError::Auth(crate::auth::AuthRefusal::ClearanceMismatch { .. })), "{err:?}");
            assert_eq!(counters.get("gateway_auth_clearance_mismatch"), 1, "the counter for this exact refusal must have moved");
            // The over-clearance product read never happened either -- D2's own counter is untouched.
            assert_eq!(core.counters().get("label_over_clearance"), 0);
        }

        /// A properly-authenticated, correctly-cleared caller still gets a real answer --
        /// authentication is additive, not a second way to be refused when everything else is
        /// in order.
        #[tokio::test]
        async fn a_correctly_authenticated_and_cleared_caller_still_gets_a_real_answer() {
            let core = core_with_two_runs();
            let issuer = TestIssuer::new();
            let auth = auth_ctx(&issuer, &[("operators", &["query"])], &[("operators", "CUI")]);
            let counters = Counters::new();
            let token = mint(&issuer, &["operators"]);
            // The request's own caller_clearance is left empty -- the token-derived marking
            // (CUI) is what GatewayCore::query actually sees, per authenticated_query's own doc.
            let resp = authenticated_query(&core, &auth, &counters, &token, req("run-cui", "", GatewaySelector::All)).await.unwrap();
            assert_eq!(resp.product_label.unwrap().marking, "CUI");
        }

        /// **R5.1b, defect 1's own required test, at this crate's real, catalogued layer.** A
        /// caller whose `groups` map to TWO clearances (`operators -> CUI`, `safety-officers ->
        /// SECRET`) is served a REAL SECRET-labelled product, not merely "not refused" -- the
        /// gateway's own `run-secret` fixture, with its own real `run_id` and `product_label`
        /// coming back. Proves the highest-ranked mapped marking is what actually reaches
        /// `GatewayCore::query`, not just what `AuthContext::authenticate_query` returns in
        /// isolation (`crate::auth`'s own unit tests already cover that half).
        #[tokio::test]
        async fn a_caller_whose_highest_mapped_clearance_is_secret_is_served_a_real_secret_product() {
            let core = core_with_two_runs();
            let issuer = TestIssuer::new();
            let auth = auth_ctx(&issuer, &[("operators", &["query"]), ("safety-officers", &["query"])], &[("operators", "CUI"), ("safety-officers", "SECRET")]);
            let counters = Counters::new();
            let token = mint(&issuer, &["operators", "safety-officers"]);

            let resp = authenticated_query(&core, &auth, &counters, &token, req("run-secret", "", GatewaySelector::All))
                .await
                .expect("a caller whose highest mapped clearance is SECRET must be served the SECRET-labelled run");
            assert_eq!(resp.run.unwrap().run_id, "run-secret");
            assert_eq!(resp.product_label.unwrap().marking, "SECRET");
            assert!(!resp.query_id.is_empty());
        }
    }
}
