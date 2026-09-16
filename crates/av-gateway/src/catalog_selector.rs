//! H2c (`docs/heavy-plan.md` H2: "The av-gateway learns a `catalog` selector through the same
//! authentication it already has"): [`query_catalog`], the `GATEWAY_SELECTOR_CATALOG` handler
//! `crate::gateway::authenticated_query` routes to -- see that function's own module-doc
//! section, "H2c", for exactly where and why the fork happens before [`crate::gateway::
//! GatewayCore::query`] is ever reached.
//!
//! # The refusal chain, ordered, mirroring `GatewayCore::query`'s own
//!
//! This function is self-contained -- it does not assume its own caller already validated
//! anything about `req` beyond routing on `req.selector`'s raw wire value (`crate::gateway`'s
//! own `authenticated_query` does no more than that one peek) -- so, exactly like
//! [`crate::gateway::GatewayCore::query`] itself, every check below re-derives what it needs
//! from `req` directly:
//!
//! 1. **Caller-supplied path** (`req.caller_supplied_products_uri` non-empty) -- THE EXISTING
//!    RULE, [`crate::gateway::RefusalReason::CallerSuppliedPath`], reused verbatim (same type,
//!    same counter code `gateway_caller_supplied_path`) at this second call site -- never a
//!    second, independently-typed variant for the identical fact. This has to be re-checked
//!    HERE, not merely trusted to have already been checked by `GatewayCore::query`, because a
//!    `GATEWAY_SELECTOR_CATALOG` request never reaches that function at all (see this module's
//!    own "Run identity" section below, and `crate::gateway`'s own "H2c" doc) -- if this
//!    function did not check it, a catalog request carrying a caller-supplied path would sail
//!    straight through, the one gap D1's whole design exists to close.
//! 2. **An unspecified (or, defensively, any non-`GATEWAY_SELECTOR_CATALOG`) selector** --
//!    reuses [`crate::gateway::RefusalReason::MalformedRequest`] (same type, same counter code
//!    `gateway_malformed_request`). In normal operation this function is only ever called once
//!    `crate::gateway::authenticated_query` has already confirmed `req.selector ==
//!    GATEWAY_SELECTOR_CATALOG`, so this arm is not expected to fire in production -- it exists
//!    so this function stays correct and safe to call directly (this crate's own tests do),
//!    never assuming a caller already checked what this function can trivially re-derive
//!    itself.
//! 3. **A malformed `CatalogQuery`** -- FOUR typed, NEWLY-counted reasons ([`CatalogRefusal`]),
//!    checked in this order: `req.catalog_query` itself unset
//!    ([`CatalogRefusal::NoCatalogQuery`]); `limit` zero or above [`av_catalog::MAX_QUERY_LIMIT`]
//!    ([`CatalogRefusal::LimitOutOfRange`] -- refused, never silently clamped the way
//!    [`av_catalog::query::find_assets`]'s own `AssetQuery.limit` is for ITS OWN, already-
//!    reviewed internal callers; a wire caller gets a typed refusal instead, since silently
//!    reinterpreting "0" or "a million" as "the maximum" would hide a caller's own mistake);
//!    `bbox` set with `min_lon >= max_lon` or `min_lat >= max_lat`
//!    ([`CatalogRefusal::BboxInvalid`]); `time` set with `end_tai_ns <= start_tai_ns`
//!    ([`CatalogRefusal::TimeRangeInvalid`]).
//! 4. **No catalog configured** ([`CatalogRefusal::CatalogNotConfigured`]) -- checked only
//!    AFTER the request itself is known well-formed (so a malformed request is always refused
//!    for what it itself got wrong, never masked by an unrelated deployment fact), but still
//!    BEFORE any network I/O: [`crate::gateway::GatewayCore::catalog_handle`] returning `None`
//!    is a typed, counted refusal here -- never an empty `Ok` response (which would be
//!    indistinguishable, to a caller, from "the catalog has no matching assets") and never a
//!    panic.
//! 5. **The query itself** -- connects (see "Connection lifecycle" below), runs
//!    [`av_catalog::query::find_assets`] (whose own module doc states the label filter's exact
//!    SQL-level, never-a-post-filter semantics -- reused unchanged, never reimplemented here),
//!    and any `av_catalog::CatalogError` this step raises (a connection failure, a SCRAM
//!    failure, a real `ErrorResponse` from the server) becomes [`CatalogRefusal::QueryFailed`],
//!    typed and counted, never an unwrapped panic.
//!
//! # The caller's clearance -- through `AuthContext`, never a second path
//!
//! `req.caller_clearance` here is ALREADY the verified, token-derived marking by the time this
//! function ever sees it: `crate::gateway::authenticated_query` (this function's one caller in
//! production) overwrites `GatewayQueryRequest.caller_clearance` with
//! `crate::auth::AuthContext::authenticate_query`'s own return value BEFORE the routing peek
//! that calls this function -- the identical rule every other selector already gets, restated
//! here because it is the single most important invariant a future reader of this file must
//! never weaken: there is no second authentication path, no second verifier, and no relaxed
//! check for this selector. `GatewayQueryRequest.caller_clearance` (the wire field) is checked
//! for agreement against the verified marking, or is empty and defaults to it, by
//! `AuthContext::authenticate_query` itself -- exactly as documented there -- before this
//! function is ever reached; this function's own job is only to pass the (already-verified)
//! `req.caller_clearance` on to [`av_catalog::query::find_assets`]'s own `caller_clearance`
//! parameter, which independently ranks it against [`av_catalog::labels::ClearanceLadder`] (H2's
//! OWN copy of the clearance-ladder convention, not `crate::labels::ClearanceLadder` --
//! `crate::catalog_selector::CatalogHandle::ladder`'s own doc explains why a second, structurally
//! distinct instance of an otherwise-identical configured list is correct here, not a violation
//! of "one ladder").
//!
//! # Run identity: never required for this selector, unlike every other one
//!
//! `GATEWAY_SELECTOR_CATALOG` carries no `run` (`GatewayQueryRequest.run` stays `None` on every
//! real catalog request -- `crates/av-gateway/tests/catalog_selector.rs`'s own docker-free
//! tests assert this by constructing exactly such a request and observing it is never refused
//! for a missing run). This is the fork `crate::gateway`'s own module doc names: every OTHER
//! selector still goes through [`crate::gateway::GatewayCore::query`], whose OWN first
//! structural check after caller-supplied-path is "`run` (`RunIdentity`) is required" -- that
//! check is completely unmodified by this task (`crates/av-gateway/tests/catalog_selector.rs`'s
//! own docker-free tests assert the mirror image too: a NON-catalog selector with no `run` is
//! still refused `MalformedRequest`, exactly as before H2c).
//!
//! # Connection lifecycle: connect fresh per call, no pool
//!
//! [`query_catalog`] calls `av_catalog::client::PgClient::connect` once per
//! `GATEWAY_SELECTOR_CATALOG` call and lets the connection close at the end of this function
//! (`PgClient::close`, best-effort -- a failed `Terminate` is not itself treated as this query's
//! own failure, since by that point `find_assets` has already returned its real result). No
//! pooled or persistent connection: `av-gateway` is explicitly not a hot-path crate
//! (`docs/heavy-plan.md` H1 names `av-ingest`/`av-track`/`av-command` as the hot path, and this
//! crate's own `Cargo.toml` comment on its new `av-catalog` dependency restates why depending on
//! the catalog tier at all does not touch that claim-check boundary), and this round's own
//! acceptance line is a real label-filtered read proof, not a load/throughput one -- a
//! connection pool is a real, reasonable follow-up named in this task's own final report for
//! the manager to schedule, not something this round invents unasked.

use av_cdm::pb::{CatalogRecord, GatewayQueryRequest, GatewayQueryResponse, GatewaySelector};
use av_catalog::{find_assets, AssetQuery, GeoBbox as CatalogGeoBbox, PgClient, PgConfig, TimeRange as CatalogTimeRange};

use crate::counters::{Counted, Counters};
use crate::gateway::{GatewayCore, RefusalReason};
use crate::query_id::compute_catalog_query_id;

/// What [`GatewayCore`] needs to serve `GATEWAY_SELECTOR_CATALOG`: how to reach `crates/
/// av-catalog`'s own PostgreSQL+PostGIS wire client, and this deployment's own clearance
/// ladder FOR THE CATALOG specifically.
///
/// `ladder` is [`av_catalog::labels::ClearanceLadder`] -- a STRUCTURALLY DIFFERENT type from
/// `crate::labels::ClearanceLadder` (the one [`GatewayCore`] itself already holds, for
/// ranking a caller against a run's own `product_label`), even though a correctly-configured
/// deployment gives both the identical ordered marking list. This is not a missed
/// deduplication: `av-catalog`'s own `src/labels.rs` module doc names this explicitly as the
/// fourth, deliberate copy of the clearance-ladder convention in this workspace (after
/// `av-edge`, `av-gateway`, `av-store`) -- `av-catalog` must not depend on `av-gateway` (this
/// crate is the read-path CONSUMER sitting above the catalog tier; a dependency the other way
/// would be backwards), so it cannot reuse `crate::labels::ClearanceLadder` even though the
/// two types are near-identical. A shared `av-labels`-shaped crate both could depend on
/// remains a real opportunity (`av-catalog`'s own module doc already names it as a proposal
/// for the manager) -- this task's own final report repeats that proposal, not invents a
/// second one, since extracting shared code across four crates this task did not open is not
/// this task's call to make unilaterally.
#[derive(Debug, Clone)]
pub struct CatalogHandle {
    pub pg_config: PgConfig,
    pub ladder: av_catalog::labels::ClearanceLadder,
}

/// Every NEW way [`query_catalog`] can refuse a `GATEWAY_SELECTOR_CATALOG` request -- see this
/// module's own doc, "The refusal chain", for the two EXISTING [`crate::gateway::RefusalReason`]
/// variants this function also reaches (never re-typed here).
///
/// `PartialEq` only, no `Eq` -- [`CatalogRefusal::BboxInvalid`] carries `f64` fields, which have
/// no total equality (`crate::gateway::RefusalReason`'s own doc comment states the identical
/// reason for nesting this type).
#[derive(Debug, Clone, PartialEq)]
pub enum CatalogRefusal {
    /// `GatewayQueryRequest.catalog_query` was unset on a `GATEWAY_SELECTOR_CATALOG` request.
    NoCatalogQuery,
    /// `CatalogQuery.limit` was `0` or greater than [`av_catalog::MAX_QUERY_LIMIT`] -- refused,
    /// never silently clamped (this module's own doc, "The refusal chain", item 3).
    LimitOutOfRange { limit: u32 },
    /// `CatalogQuery.bbox` was set with `min_lon >= max_lon` or `min_lat >= max_lat`.
    BboxInvalid { min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64 },
    /// `CatalogQuery.time` was set with `end_tai_ns <= start_tai_ns`.
    TimeRangeInvalid { start_tai_ns: i64, end_tai_ns: i64 },
    /// This `GatewayCore` was built with no catalog handle at all
    /// (`crate::gateway::GatewayCore::catalog_handle` returned `None`).
    CatalogNotConfigured,
    /// The underlying `av_catalog` call itself failed (connecting, or the query) --
    /// `detail` is the real `av_catalog::error::CatalogError`'s own `Display` text.
    QueryFailed { detail: String },
}

/// Counter-code prefix `catalog_` (this family) is deliberately distinct from the
/// pre-existing `catalogue_` prefix ([`crate::catalogue::ResolveError`]'s own codes,
/// e.g. `catalogue_unknown_run`): "catalogue" (British spelling, matching
/// [`crate::catalogue::RunCatalogue`]'s own name) is this crate's configured RUN catalogue;
/// "catalog" (matching the `av-catalog` crate's own name) is H2's PostgreSQL+PostGIS asset
/// catalog -- two different things this workspace already spells two different ways, restated
/// here as a counter-naming rule so the two families stay greppably distinct.
impl Counted for CatalogRefusal {
    fn code(&self) -> &'static str {
        match self {
            CatalogRefusal::NoCatalogQuery => "catalog_no_catalog_query",
            CatalogRefusal::LimitOutOfRange { .. } => "catalog_limit_out_of_range",
            CatalogRefusal::BboxInvalid { .. } => "catalog_bbox_invalid",
            CatalogRefusal::TimeRangeInvalid { .. } => "catalog_time_range_invalid",
            CatalogRefusal::CatalogNotConfigured => "catalog_not_configured",
            CatalogRefusal::QueryFailed { .. } => "catalog_query_failed",
        }
    }
}

impl std::fmt::Display for CatalogRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogRefusal::NoCatalogQuery => write!(f, "GATEWAY_SELECTOR_CATALOG requires GatewayQueryRequest.catalog_query, which was not set"),
            CatalogRefusal::LimitOutOfRange { limit } => {
                write!(f, "CatalogQuery.limit {limit} is out of range (must be 1..={}, refused rather than clamped)", av_catalog::MAX_QUERY_LIMIT)
            }
            CatalogRefusal::BboxInvalid { min_lon, min_lat, max_lon, max_lat } => {
                write!(f, "CatalogQuery.bbox is invalid: min ({min_lon}, {min_lat}) must be strictly less than max ({max_lon}, {max_lat}) on both axes")
            }
            CatalogRefusal::TimeRangeInvalid { start_tai_ns, end_tai_ns } => {
                write!(f, "CatalogQuery.time is invalid: end_tai_ns ({end_tai_ns}) must be strictly greater than start_tai_ns ({start_tai_ns})")
            }
            CatalogRefusal::CatalogNotConfigured => write!(f, "this gateway deployment has no catalog tier configured (GatewayCore::with_catalog was never called) -- GATEWAY_SELECTOR_CATALOG is refused, never an empty success"),
            CatalogRefusal::QueryFailed { detail } => write!(f, "the catalog query itself failed: {detail}"),
        }
    }
}

fn refuse(counters: &Counters, reason: RefusalReason) -> RefusalReason {
    counters.record(&reason);
    reason
}

/// `GATEWAY_SELECTOR_CATALOG`'s handler -- see this module's own doc for the full ordered
/// refusal chain, the connection lifecycle, and why the caller's clearance is never taken from
/// anywhere but the already-verified `req.caller_clearance`.
pub async fn query_catalog(core: &GatewayCore, req: &GatewayQueryRequest, counters: &Counters) -> Result<GatewayQueryResponse, RefusalReason> {
    // 1. Caller-supplied path -- the existing rule, reused (this module's own doc, item 1).
    if !req.caller_supplied_products_uri.is_empty() {
        return Err(refuse(counters, RefusalReason::CallerSuppliedPath));
    }

    // 2. Selector must genuinely be GATEWAY_SELECTOR_CATALOG -- defensive self-containment
    // (this module's own doc, item 2): in production this function is only ever reached once
    // crate::gateway::authenticated_query has already confirmed this, but this function does
    // not trust that -- it re-derives the same fact itself, exactly like GatewayCore::query
    // does for every other selector.
    let selector = GatewaySelector::try_from(req.selector).unwrap_or(GatewaySelector::Unspecified);
    if selector != GatewaySelector::Catalog {
        return Err(refuse(
            counters,
            RefusalReason::MalformedRequest {
                detail: format!(
                    "crate::catalog_selector::query_catalog was called for selector {} (raw value {}), not GATEWAY_SELECTOR_CATALOG",
                    selector.as_str_name(),
                    req.selector
                ),
            },
        ));
    }

    // 3. A malformed CatalogQuery (this module's own doc, item 3).
    let Some(query) = req.catalog_query.as_ref() else {
        return Err(refuse(counters, RefusalReason::Catalog(CatalogRefusal::NoCatalogQuery)));
    };
    if query.limit == 0 || query.limit > av_catalog::MAX_QUERY_LIMIT {
        return Err(refuse(counters, RefusalReason::Catalog(CatalogRefusal::LimitOutOfRange { limit: query.limit })));
    }
    if let Some(bbox) = query.bbox.as_ref() {
        if bbox.min_lon >= bbox.max_lon || bbox.min_lat >= bbox.max_lat {
            return Err(refuse(
                counters,
                RefusalReason::Catalog(CatalogRefusal::BboxInvalid { min_lon: bbox.min_lon, min_lat: bbox.min_lat, max_lon: bbox.max_lon, max_lat: bbox.max_lat }),
            ));
        }
    }
    if let Some(time) = query.time.as_ref() {
        if time.end_tai_ns <= time.start_tai_ns {
            return Err(refuse(counters, RefusalReason::Catalog(CatalogRefusal::TimeRangeInvalid { start_tai_ns: time.start_tai_ns, end_tai_ns: time.end_tai_ns })));
        }
    }

    // 4. No catalog configured (this module's own doc, item 4) -- checked only once the
    // request itself is known well-formed, but still before any network I/O.
    let Some(handle) = core.catalog_handle() else {
        return Err(refuse(counters, RefusalReason::Catalog(CatalogRefusal::CatalogNotConfigured)));
    };

    // 5. The query itself (this module's own doc, item 5 and "Connection lifecycle").
    let asset_query = AssetQuery {
        bbox: query.bbox.as_ref().map(|b| CatalogGeoBbox { min_lon: b.min_lon, min_lat: b.min_lat, max_lon: b.max_lon, max_lat: b.max_lat }),
        time: query.time.as_ref().map(|t| CatalogTimeRange { start_tai_ns: t.start_tai_ns, end_tai_ns: t.end_tai_ns }),
        media_type: if query.media_type.is_empty() { None } else { Some(query.media_type.clone()) },
        job_id: if query.job_id.is_empty() { None } else { Some(query.job_id.clone()) },
        limit: query.limit,
    };

    let mut client = PgClient::connect(&handle.pg_config)
        .await
        .map_err(|e| refuse(counters, RefusalReason::Catalog(CatalogRefusal::QueryFailed { detail: e.to_string() })))?;
    let assets = find_assets(&mut client, &asset_query, &req.caller_clearance, &handle.ladder)
        .await
        .map_err(|e| refuse(counters, RefusalReason::Catalog(CatalogRefusal::QueryFailed { detail: e.to_string() })))?;
    // Best-effort: the query already succeeded above, so a failed Terminate here is a
    // connection-teardown detail, never this call's own result.
    let _ = client.close().await;

    let catalog_records: Vec<CatalogRecord> = assets
        .into_iter()
        .map(|a| CatalogRecord { asset_id: a.asset_id.clone(), asset: Some(a.to_asset_ref()), job_id: a.job_id.unwrap_or_default(), created_tai_ns: a.created_tai_ns, footprint_wkt: a.footprint_wkt.unwrap_or_default() })
        .collect();

    // D5, extended (H2c): crate::query_id's own convention, over this catalog query's own
    // canonical content -- see that module's "H2c" doc for why this is a new, additive entry
    // point rather than a change to compute_query_id's own preimage.
    let query_id = compute_catalog_query_id(query, &req.caller_clearance);

    Ok(GatewayQueryResponse {
        run: None,
        product_label: None,
        trajectories: Default::default(),
        events: Vec::new(),
        scores: Default::default(),
        measurements: Vec::new(),
        query_id,
        catalog_records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::{CatalogQuery, GeoBbox as WireGeoBbox, TemporalExtent};
    use std::time::Duration;

    fn empty_core() -> GatewayCore {
        GatewayCore::new(crate::catalogue::RunCatalogue::new(std::collections::BTreeMap::new()), crate::labels::ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()]), std::sync::Arc::new(Counters::new()))
    }

    fn dummy_pg_config() -> PgConfig {
        // Never actually dialled by any docker-free test below: every one of them is refused
        // before PgClient::connect is ever called. A real (reachable) config is
        // crates/av-gateway/tests/catalog_selector.rs's own job.
        PgConfig { host: "127.0.0.1".to_string(), port: 1, user: "nobody".to_string(), password: String::new(), database: "nonexistent".to_string(), application_name: "av-gateway-catalog-selector-test".to_string(), connect_timeout: Duration::from_millis(1), tls: av_catalog::PgTls::Disabled }
    }

    fn core_with_catalog() -> GatewayCore {
        empty_core().with_catalog(CatalogHandle { pg_config: dummy_pg_config(), ladder: av_catalog::labels::ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()]) })
    }

    fn catalog_req(catalog_query: Option<CatalogQuery>, caller_clearance: &str) -> GatewayQueryRequest {
        GatewayQueryRequest {
            run: None,
            caller_clearance: caller_clearance.to_string(),
            selector: GatewaySelector::Catalog as i32,
            caller_supplied_products_uri: String::new(),
            caller_token: String::new(),
            catalog_query,
        }
    }

    fn valid_query() -> CatalogQuery {
        CatalogQuery { bbox: None, time: None, media_type: String::new(), job_id: String::new(), limit: 10 }
    }

    /// This module's own doc, "Run identity: never required for this selector": a real
    /// GATEWAY_SELECTOR_CATALOG request carries no `run` at all, and is never refused for
    /// that -- it fails, instead, on the very next check this test is not exercising
    /// (CatalogNotConfigured, since `empty_core()` has none), proving `run`'s absence itself
    /// was never the reason.
    #[tokio::test]
    async fn a_catalog_request_with_no_run_identity_is_never_refused_for_missing_run() {
        let core = empty_core();
        let counters = Counters::new();
        let err = query_catalog(&core, &catalog_req(Some(valid_query()), "CUI"), &counters).await.unwrap_err();
        assert!(!matches!(err, RefusalReason::MalformedRequest { ref detail } if detail.contains("run")), "must never be refused for a missing run identity: {err}");
        assert!(matches!(err, RefusalReason::Catalog(CatalogRefusal::CatalogNotConfigured)), "{err:?}");
    }

    #[tokio::test]
    async fn caller_supplied_products_uri_is_refused_first_and_counted() {
        let core = core_with_catalog();
        let counters = Counters::new();
        let mut req = catalog_req(Some(valid_query()), "CUI");
        req.caller_supplied_products_uri = "/etc/passwd".to_string();
        let err = query_catalog(&core, &req, &counters).await.unwrap_err();
        assert_eq!(err, RefusalReason::CallerSuppliedPath);
        assert_eq!(counters.get("gateway_caller_supplied_path"), 1, "the EXISTING counter, reused, must move -- not a new one");
    }

    #[tokio::test]
    async fn no_catalog_query_is_refused_and_counted() {
        let core = core_with_catalog();
        let counters = Counters::new();
        let err = query_catalog(&core, &catalog_req(None, "CUI"), &counters).await.unwrap_err();
        assert!(matches!(err, RefusalReason::Catalog(CatalogRefusal::NoCatalogQuery)), "{err:?}");
        assert_eq!(counters.get("catalog_no_catalog_query"), 1);
    }

    #[tokio::test]
    async fn a_zero_limit_is_refused_never_silently_clamped() {
        let core = core_with_catalog();
        let counters = Counters::new();
        let mut q = valid_query();
        q.limit = 0;
        let err = query_catalog(&core, &catalog_req(Some(q), "CUI"), &counters).await.unwrap_err();
        assert!(matches!(err, RefusalReason::Catalog(CatalogRefusal::LimitOutOfRange { limit: 0 })), "{err:?}");
        assert_eq!(counters.get("catalog_limit_out_of_range"), 1);
    }

    #[tokio::test]
    async fn a_limit_above_the_maximum_is_refused_never_silently_clamped() {
        let core = core_with_catalog();
        let counters = Counters::new();
        let mut q = valid_query();
        q.limit = av_catalog::MAX_QUERY_LIMIT + 1;
        let err = query_catalog(&core, &catalog_req(Some(q), "CUI"), &counters).await.unwrap_err();
        assert!(matches!(err, RefusalReason::Catalog(CatalogRefusal::LimitOutOfRange { .. })), "{err:?}");
        assert_eq!(counters.get("catalog_limit_out_of_range"), 1);
    }

    #[tokio::test]
    async fn a_bbox_with_min_greater_than_or_equal_to_max_is_refused() {
        let core = core_with_catalog();
        let counters = Counters::new();
        let mut q = valid_query();
        q.bbox = Some(WireGeoBbox { min_lon: 10.0, min_lat: 0.0, max_lon: 10.0, max_lat: 5.0 }); // min_lon == max_lon
        let err = query_catalog(&core, &catalog_req(Some(q), "CUI"), &counters).await.unwrap_err();
        assert!(matches!(err, RefusalReason::Catalog(CatalogRefusal::BboxInvalid { .. })), "{err:?}");
        assert_eq!(counters.get("catalog_bbox_invalid"), 1);
    }

    #[tokio::test]
    async fn a_time_range_with_end_at_or_before_start_is_refused() {
        let core = core_with_catalog();
        let counters = Counters::new();
        let mut q = valid_query();
        q.time = Some(TemporalExtent { start_tai_ns: 200, end_tai_ns: 200 }); // end == start
        let err = query_catalog(&core, &catalog_req(Some(q), "CUI"), &counters).await.unwrap_err();
        assert!(matches!(err, RefusalReason::Catalog(CatalogRefusal::TimeRangeInvalid { .. })), "{err:?}");
        assert_eq!(counters.get("catalog_time_range_invalid"), 1);
    }

    /// This module's own doc, item 4: a well-formed request against a `GatewayCore` with no
    /// catalog handle is refused typed and counted -- never an empty `Ok` response (which a
    /// caller could never distinguish from "the catalog genuinely has no matches") and never a
    /// panic.
    #[tokio::test]
    async fn no_catalog_configured_is_refused_typed_and_counted_never_an_empty_success() {
        let core = empty_core();
        let counters = Counters::new();
        let err = query_catalog(&core, &catalog_req(Some(valid_query()), "CUI"), &counters).await.unwrap_err();
        assert!(matches!(err, RefusalReason::Catalog(CatalogRefusal::CatalogNotConfigured)), "{err:?}");
        assert_eq!(counters.get("catalog_not_configured"), 1);
    }

    /// A malformed request (bad limit) is refused for THAT reason even when the deployment
    /// also happens to have no catalog configured -- proves the ordering (malformed request
    /// before "not configured"), not merely that some refusal happens.
    #[tokio::test]
    async fn a_malformed_catalog_query_is_refused_before_catalog_not_configured_is_ever_checked() {
        let core = empty_core(); // also has no catalog configured
        let counters = Counters::new();
        let mut q = valid_query();
        q.limit = 0;
        let err = query_catalog(&core, &catalog_req(Some(q), "CUI"), &counters).await.unwrap_err();
        assert!(matches!(err, RefusalReason::Catalog(CatalogRefusal::LimitOutOfRange { .. })), "malformed-request checks must run before the not-configured check: {err:?}");
        assert_eq!(counters.get("catalog_not_configured"), 0, "not-configured must never be counted when the request itself was already malformed");
    }
}
