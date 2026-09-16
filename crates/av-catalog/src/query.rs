//! The catalog's read queries: [`find_assets`] (extent/time/media-type/job-id filtered, with
//! the label filter always evaluated IN SQL) plus the small lineage read/write pair
//! [`insert_lineage`]/[`lineage_parents`] H2's own job-lineage requirement needs.
//!
//! # The label filter is a `WHERE` clause, never a post-filter
//!
//! [`find_assets`] computes [`crate::labels::ClearanceLadder::markings_at_or_below`] once, up
//! front, and binds the whole resulting set as ONE array parameter to `marking = ANY($1)` --
//! the label check happens INSIDE the SQL the database actually executes, never after rows
//! have already been fetched, counted, or paged with `LIMIT`. This is not a style preference:
//! a post-filter (fetch every row matching the other predicates, then drop the ones the caller
//! is not cleared for in Rust) would leak three things to a caller through channels that have
//! nothing to do with the actual data returned --
//!
//! 1. **Row count.** A caller who queries the same bbox/time window twice, once before and
//!    once after a higher-labelled asset is added inside it, sees the SAME returned-row count
//!    either way if the filter runs in SQL (that asset was never counted at all) -- but a
//!    Rust-side post-filter would fetch `LIMIT` rows first, INCLUDING the over-clearance one,
//!    and only then drop it, meaning the caller's own returned count (and, more subtly, HOW
//!    MANY other rows got silently pushed out of the `LIMIT` window to make room for a row the
//!    caller was never going to see) changes based on data the caller cannot see. That is an
//!    inference channel, not a hypothetical one.
//! 2. **Query cost.** `EXPLAIN ANALYZE` timing (or, on a shared/contended database, ordinary
//!    latency) for an otherwise-identical query differs depending on how many over-clearance
//!    rows the bbox/time predicates matched, even though every one of those rows is later
//!    discarded -- a caller who can measure latency learns something about data outside their
//!    own clearance.
//! 3. **Existence, through `LIMIT` itself.** [`MAX_QUERY_LIMIT`] below exists precisely so no
//!    query can ask for "the whole table" -- but a `LIMIT` applied BEFORE the label filter (the
//!    post-filter shape) can be filled entirely by over-clearance rows, so a caller sees FEWER
//!    results than the lower-labelled rows actually present would justify, silently signalling
//!    "something else matched this query that you are not shown" -- exactly the existence leak
//!    `crate::labels`'s own module doc names. Filtering markings in SQL, before `LIMIT` is
//!    applied, removes all three leaks at once: the database's own query plan, row count and
//!    `LIMIT` accounting only ever see rows the caller was already entitled to.
//!
//! # A stored marking absent from the ladder is refused, at every clearance
//!
//! `crate::labels::ClearanceLadder::markings_at_or_below` returns markings FROM THE LADDER
//! ITSELF -- never a computed "everything except what's above me" set -- so `marking =
//! ANY(...)` can only ever match a stored `assets.marking` value that is literally one of the
//! ladder's own entries. An asset whose stored `marking` is not on the ladder at ALL therefore
//! matches `= ANY(...)` under no caller clearance whatsoever, the top rung included -- proven
//! for real, against a real PostGIS container, by `tests/catalog_postgis.rs`.
//!
//! # Extent and time: NULL handling and boundary semantics
//!
//! **Extent.** An asset with a `NULL` `footprint` is EXCLUDED from a bbox-filtered query (a
//! `NULL` footprint cannot be said to intersect, or not intersect, anything) and INCLUDED when
//! no bbox filter is given at all (having no geodetic meaning is not itself a reason to hide an
//! asset from an unfiltered listing) -- `migrations/0001_init.sql`'s own `COMMENT ON COLUMN
//! assets.footprint` states this identical rule.
//!
//! **Time**, half-open `[start, end)` on TAI nanoseconds, standard half-open-interval overlap:
//! an asset `[a_start, a_end)` matches a query `[q_start, q_end)` iff `a_start < q_end AND
//! a_end > q_start`. At the boundary this means an asset whose OWN `end_tai_ns` equals the
//! query's `start_tai_ns` does NOT match (`a_end > q_start` is false when they are equal) --
//! the asset's own validity interval is defined to have already ended at that instant. The
//! identical NULL rule as extent: an asset with `NULL` `start_tai_ns`/`end_tai_ns` is EXCLUDED
//! from a time-filtered query and INCLUDED when no time filter is given.

use crate::client::{Param, PgClient};
use crate::error::CatalogError;
use crate::labels::ClearanceLadder;
use crate::model::CatalogAsset;
use crate::pgtext::encode_text_array;

/// A geodetic bounding box, WGS84 degrees -- `crate::query`'s own module doc, "Extent", states
/// the exact `ST_Intersects`/NULL-footprint semantics this drives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoBbox {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

/// A half-open TAI-nanosecond time range `[start_tai_ns, end_tai_ns)` -- `crate::query`'s own
/// module doc, "Time", states the exact overlap/boundary semantics this drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start_tai_ns: i64,
    pub end_tai_ns: i64,
}

/// The maximum `AssetQuery.limit` [`find_assets`] will ever honour -- `crate::query`'s own
/// module doc, item 3 of "The label filter is a WHERE clause", explains why a bound is
/// mandatory at all: without one, `AssetQuery.limit` alone could ask for the entire `assets`
/// table in one round trip. 1000 is generous for a catalog metadata query (this is not a bulk
/// telemetry or tile-data path -- `crate::client`'s own module doc makes the identical
/// observation about this crate's TEXT-format choice) while still being a real bound a caller
/// cannot raise.
pub const MAX_QUERY_LIMIT: u32 = 1000;

/// One `find_assets` query. Every field is optional except `limit`, which is always clamped
/// into `1..=MAX_QUERY_LIMIT` (never zero, never unlimited) -- see [`MAX_QUERY_LIMIT`]'s own
/// doc.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetQuery {
    pub bbox: Option<GeoBbox>,
    pub time: Option<TimeRange>,
    pub media_type: Option<String>,
    pub job_id: Option<String>,
    pub limit: u32,
}

/// Finds every `assets` row matching `query`, filtered to the markings `caller_clearance`'s own
/// rank on `ladder` is entitled to see -- filtered IN SQL, per this module's own doc. Ordered by
/// `asset_id` (a deterministic order -- ADR-004 -- so `LIMIT` always returns the SAME rows for
/// the same query against the same data, never an order that happens to depend on the
/// database's own scan order). `Err(CatalogError::CallerMarkingNotOnLadder)` before any SQL runs
/// at all if `caller_clearance` itself is not on `ladder`.
pub async fn find_assets(client: &mut PgClient, query: &AssetQuery, caller_clearance: &str, ladder: &ClearanceLadder) -> Result<Vec<CatalogAsset>, CatalogError> {
    let allowed_markings = ladder.markings_at_or_below(caller_clearance)?;

    let mut params: Vec<Param> = Vec::new();
    let mut conditions: Vec<String> = Vec::new();

    params.push(Param::Text(encode_text_array(&allowed_markings)));
    conditions.push(format!("marking = ANY(${}::text[])", params.len()));

    if let Some(bbox) = &query.bbox {
        params.push(Param::F64(bbox.min_lon));
        let p_min_lon = params.len();
        params.push(Param::F64(bbox.min_lat));
        let p_min_lat = params.len();
        params.push(Param::F64(bbox.max_lon));
        let p_max_lon = params.len();
        params.push(Param::F64(bbox.max_lat));
        let p_max_lat = params.len();
        conditions.push(format!(
            "(footprint IS NOT NULL AND ST_Intersects(footprint, ST_MakeEnvelope(${p_min_lon}::double precision,${p_min_lat}::double precision,${p_max_lon}::double precision,${p_max_lat}::double precision,4326)::geography))"
        ));
    }

    if let Some(time) = &query.time {
        params.push(Param::I64(time.start_tai_ns));
        let p_start = params.len();
        params.push(Param::I64(time.end_tai_ns));
        let p_end = params.len();
        // Half-open overlap: a_start < q_end AND a_end > q_start (this module's own doc,
        // "Time"). NULL start/end is excluded, matching the extent rule above.
        conditions.push(format!("(start_tai_ns IS NOT NULL AND end_tai_ns IS NOT NULL AND start_tai_ns < ${p_end}::bigint AND end_tai_ns > ${p_start}::bigint)"));
    }

    if let Some(media_type) = &query.media_type {
        params.push(Param::Text(media_type.clone()));
        conditions.push(format!("media_type = ${}::text", params.len()));
    }

    if let Some(job_id) = &query.job_id {
        params.push(Param::Text(job_id.clone()));
        conditions.push(format!("job_id = ${}::text", params.len()));
    }

    let limit = query.limit.clamp(1, MAX_QUERY_LIMIT);
    let sql = format!(
        "SELECT asset_id, sha256, uri, size_bytes, media_type, frame_id, extent_min, extent_max, \
         ST_AsText(footprint) AS footprint_wkt, start_tai_ns, end_tai_ns, provenance, label_bytes, job_id, created_tai_ns \
         FROM assets WHERE {} ORDER BY asset_id LIMIT {limit}",
        conditions.join(" AND ")
    );

    let rows = client.query(&sql, &params).await?;
    rows.iter().map(CatalogAsset::from_row).collect()
}

/// Records one job-lineage edge: `asset_id` was derived from `parent_asset_id` by `job_id`
/// (`migrations/0001_init.sql`'s own `asset_lineage` table doc).
pub async fn insert_lineage(client: &mut PgClient, asset_id: &str, parent_asset_id: &str, job_id: &str) -> Result<(), CatalogError> {
    let params = vec![Param::Text(asset_id.to_string()), Param::Text(parent_asset_id.to_string()), Param::Text(job_id.to_string())];
    client.execute("INSERT INTO asset_lineage (asset_id, parent_asset_id, job_id) VALUES ($1,$2,$3)", &params).await?;
    Ok(())
}

/// Every `(parent_asset_id, job_id)` pair recorded for `asset_id`, ordered by `parent_asset_id`
/// then `job_id` (deterministic order, same rationale as [`find_assets`]'s own `ORDER BY
/// asset_id`).
pub async fn lineage_parents(client: &mut PgClient, asset_id: &str) -> Result<Vec<(String, String)>, CatalogError> {
    let params = vec![Param::Text(asset_id.to_string())];
    let rows = client.query("SELECT parent_asset_id, job_id FROM asset_lineage WHERE asset_id = $1 ORDER BY parent_asset_id, job_id", &params).await?;
    rows.iter().map(|row| Ok((row.get_str("parent_asset_id")?.to_string(), row.get_str("job_id")?.to_string()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `AssetQuery.limit` is always clamped into `1..=MAX_QUERY_LIMIT` -- pure, docker-free
    /// (the clamp itself, not the SQL it produces, which `tests/catalog_postgis.rs` proves
    /// against a real server).
    #[test]
    fn limit_clamp_never_produces_zero_or_a_value_over_the_maximum() {
        assert_eq!(0u32.clamp(1, MAX_QUERY_LIMIT), 1, "a zero limit must clamp up to 1, never stay 0 (an unbounded-looking LIMIT 0 is not what a caller asking for '0' should get either way)");
        assert_eq!(50u32.clamp(1, MAX_QUERY_LIMIT), 50);
        assert_eq!((MAX_QUERY_LIMIT + 1).clamp(1, MAX_QUERY_LIMIT), MAX_QUERY_LIMIT);
        assert_eq!(u32::MAX.clamp(1, MAX_QUERY_LIMIT), MAX_QUERY_LIMIT, "no caller-supplied limit can ever ask for the whole table");
    }
}
