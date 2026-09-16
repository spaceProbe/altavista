//! `CatalogAsset`: the Rust-side record for one `assets` row (`migrations/0001_init.sql`),
//! with conversions to and from `av_cdm::pb::AssetRef` (the wire claim-check H1's own
//! `av-store` crate already defines) plus the catalog-only fields no `AssetRef` carries:
//! `asset_id` (this table's own primary key -- `migrations/0001_init.sql`'s own COMMENT
//! explains why it is not `sha256`), `job_id`, `created_tai_ns`, and the geodetic footprint as
//! WKT text (`ST_AsText(footprint)`, never a binary PostGIS format this crate would have to
//! parse itself -- the same TEXT-format-throughout discipline `crate::client`'s own module doc
//! states for every other value this crate ever sends or reads).
//!
//! # Why `AssetRef.spatial_extent`/`temporal_extent` round-trip asymmetrically
//!
//! `assets.frame_id`/`extent_min`/`extent_max` are `NOT NULL` columns (`migrations/
//! 0001_init.sql`'s own doc: this schema represents "no spatial extent" as an empty
//! `frame_id`/empty `extent_min`/`extent_max` arrays, never a SQL `NULL`), so
//! [`CatalogAsset::from_asset_ref`] on a source `AssetRef` with no `spatial_extent` at all
//! writes exactly that -- and [`CatalogAsset::to_asset_ref`] therefore reconstructs
//! `Some(SpatialExtent { frame_id: "", min: [], max: [] })`, never `None`: this schema has no
//! way to distinguish "no `SpatialExtent` was ever set" from "a `SpatialExtent` with an empty
//! frame and no bounds", so it does not pretend to. `assets.start_tai_ns`/`end_tai_ns` ARE
//! nullable columns (`migrations/0001_init.sql`'s own `assets_time_pairing_check`), so
//! `temporal_extent`'s `None`/`Some` round-trips EXACTLY -- this is the one sub-message this
//! schema can and does represent losslessly. Both facts are proven by this module's own
//! `spatial_extent_absent_round_trips_to_an_empty_present_value`/
//! `temporal_extent_none_round_trips_to_none` tests below, not merely asserted here.

use av_cdm::pb::{AssetRef, Label, Provenance, SpatialExtent, TemporalExtent};
use prost::Message;

use crate::client::{Param, PgClient, Row};
use crate::error::CatalogError;
use crate::pgtext::{decode_bytea, encode_bytea, encode_f64_array, encode_text_array, parse_pg_f64_array};

/// One `assets` row. See this module's own doc for the `AssetRef` conversion's exact
/// round-trip guarantees.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogAsset {
    /// This catalog's own identifier -- NOT `sha256` (`migrations/0001_init.sql`'s own
    /// COMMENT ON COLUMN explains why). Caller-supplied: this crate mints no id of its own
    /// (`src/migrate.rs`'s "no clock, no id generation" convention, restated here for
    /// identity).
    pub asset_id: String,
    pub sha256: String,
    pub uri: String,
    pub size_bytes: u64,
    pub media_type: String,
    /// The canonical `Label` -- `assets.marking`/`assets.caveats` are derived from this at
    /// insert time ([`Self::insert`]), never an independently-settable fact through this
    /// crate's own API.
    pub label: Label,
    pub frame_id: String,
    pub extent_min: Vec<f64>,
    pub extent_max: Vec<f64>,
    /// The geodetic footprint as WKT (e.g. `"POLYGON((...))"`), or `None` for an asset with no
    /// geodetic meaning. `crate::query`'s own module doc states the bbox-query rule this field
    /// drives.
    pub footprint_wkt: Option<String>,
    /// Half-open `[start_tai_ns, end_tai_ns)`, both `Some` or both `None` (`migrations/
    /// 0001_init.sql`'s `assets_time_pairing_check`) -- TAI nanoseconds.
    pub start_tai_ns: Option<i64>,
    pub end_tai_ns: Option<i64>,
    pub provenance: Provenance,
    /// The job that produced this asset, if any (H2's job lineage, output side).
    pub job_id: Option<String>,
    /// Catalog-entry creation epoch, TAI nanoseconds -- caller-supplied (this crate never
    /// reads a clock; `src/migrate.rs`'s identical convention for `applied_tai_ns`).
    pub created_tai_ns: i64,
}

impl CatalogAsset {
    /// Builds a [`CatalogAsset`] from an `AssetRef` (H1's own claim-check) plus the
    /// catalog-only fields no `AssetRef` carries. `Err(CatalogError::AssetRefMissingField)` if
    /// `asset_ref.label`/`asset_ref.provenance` is unset -- both back `NOT NULL` columns
    /// (`label_bytes`, `provenance`), so there is no default value this function could
    /// silently substitute instead. See this module's own doc for exactly how
    /// `spatial_extent`/`temporal_extent` absence is represented.
    pub fn from_asset_ref(asset_ref: &AssetRef, asset_id: impl Into<String>, footprint_wkt: Option<String>, job_id: Option<String>, created_tai_ns: i64) -> Result<Self, CatalogError> {
        let label = asset_ref.label.clone().ok_or(CatalogError::AssetRefMissingField { field: "label" })?;
        let provenance = asset_ref.provenance.clone().ok_or(CatalogError::AssetRefMissingField { field: "provenance" })?;
        let (frame_id, extent_min, extent_max) = match &asset_ref.spatial_extent {
            Some(se) => (se.frame_id.clone(), se.min.clone(), se.max.clone()),
            None => (String::new(), Vec::new(), Vec::new()),
        };
        let (start_tai_ns, end_tai_ns) = match &asset_ref.temporal_extent {
            Some(te) => (Some(te.start_tai_ns), Some(te.end_tai_ns)),
            None => (None, None),
        };
        Ok(Self {
            asset_id: asset_id.into(),
            sha256: asset_ref.sha256.clone(),
            uri: asset_ref.uri.clone(),
            size_bytes: asset_ref.size_bytes,
            media_type: asset_ref.media_type.clone(),
            label,
            frame_id,
            extent_min,
            extent_max,
            footprint_wkt,
            start_tai_ns,
            end_tai_ns,
            provenance,
            job_id,
            created_tai_ns,
        })
    }

    /// The `AssetRef` this catalog row represents -- see this module's own doc for the exact
    /// (asymmetric) round-trip guarantee against [`Self::from_asset_ref`]. `attributes` is
    /// always empty: nothing in this table stores `AssetRef.attributes` today (a future column
    /// can add that the same way `marking`/`caveats` were added to `migrations/0001_init.sql`,
    /// without changing this function's own contract for the fields it already handles).
    pub fn to_asset_ref(&self) -> AssetRef {
        AssetRef {
            uri: self.uri.clone(),
            sha256: self.sha256.clone(),
            size_bytes: self.size_bytes,
            media_type: self.media_type.clone(),
            label: Some(self.label.clone()),
            spatial_extent: Some(SpatialExtent { frame_id: self.frame_id.clone(), min: self.extent_min.clone(), max: self.extent_max.clone() }),
            temporal_extent: match (self.start_tai_ns, self.end_tai_ns) {
                (Some(start_tai_ns), Some(end_tai_ns)) => Some(TemporalExtent { start_tai_ns, end_tai_ns }),
                _ => None,
            },
            provenance: Some(self.provenance.clone()),
            attributes: Default::default(),
        }
    }

    /// Decodes one `assets` row (`crate::query::find_assets`'s own `SELECT` list -- see that
    /// module for the exact column list this expects) into a [`CatalogAsset`]. `provenance`/
    /// `label_bytes` are decoded via `prost::Message::decode` after [`decode_bytea`] strips
    /// PostgreSQL's own `\x` hex `bytea` TEXT-format prefix (`crate::pgtext`'s own module doc).
    pub fn from_row(row: &Row) -> Result<Self, CatalogError> {
        let size_bytes_i64 = row.get_i64("size_bytes")?;
        let size_bytes: u64 = size_bytes_i64.try_into().map_err(|_| CatalogError::ColumnParse { column: "size_bytes".to_string(), expected: "a non-negative i64 (the assets.size_bytes >= 0 CHECK)", raw: size_bytes_i64.to_string() })?;

        let extent_min = parse_pg_f64_array(row.get_str("extent_min")?, "extent_min")?;
        let extent_max = parse_pg_f64_array(row.get_str("extent_max")?, "extent_max")?;

        let provenance_bytes = decode_bytea(row.get_str("provenance")?, "provenance")?;
        let provenance = Provenance::decode(provenance_bytes.as_slice()).map_err(|e| CatalogError::ColumnParse { column: "provenance".to_string(), expected: "a valid prost-encoded altavista.v1.Provenance message", raw: e.to_string() })?;

        let label_bytes = decode_bytea(row.get_str("label_bytes")?, "label_bytes")?;
        let label = Label::decode(label_bytes.as_slice()).map_err(|e| CatalogError::ColumnParse { column: "label_bytes".to_string(), expected: "a valid prost-encoded altavista.v1.Label message", raw: e.to_string() })?;

        let footprint_wkt = if row.is_null("footprint_wkt")? { None } else { Some(row.get_str("footprint_wkt")?.to_string()) };
        let start_tai_ns = if row.is_null("start_tai_ns")? { None } else { Some(row.get_i64("start_tai_ns")?) };
        let end_tai_ns = if row.is_null("end_tai_ns")? { None } else { Some(row.get_i64("end_tai_ns")?) };
        let job_id = if row.is_null("job_id")? { None } else { Some(row.get_str("job_id")?.to_string()) };

        Ok(Self {
            asset_id: row.get_str("asset_id")?.to_string(),
            sha256: row.get_str("sha256")?.to_string(),
            uri: row.get_str("uri")?.to_string(),
            size_bytes,
            media_type: row.get_str("media_type")?.to_string(),
            label,
            frame_id: row.get_str("frame_id")?.to_string(),
            extent_min,
            extent_max,
            footprint_wkt,
            start_tai_ns,
            end_tai_ns,
            provenance,
            job_id,
            created_tai_ns: row.get_i64("created_tai_ns")?,
        })
    }

    /// Inserts this row into `assets`. `marking`/`caveats` are written from `self.label` (never
    /// an independent value a caller could set inconsistently -- `migrations/0001_init.sql`'s
    /// own COMMENT ON COLUMN for `assets.marking` states this same rule); `footprint` is set via
    /// `ST_GeogFromText($11)`, which PostGIS itself returns `NULL` for a `NULL` argument (a
    /// `STRICT` function), so [`Self::footprint_wkt`] being `None` needs no separate SQL branch
    /// -- the one parameterised statement below handles both cases.
    pub async fn insert(&self, client: &mut PgClient) -> Result<(), CatalogError> {
        const SQL: &str = "INSERT INTO assets \
            (asset_id, sha256, uri, size_bytes, media_type, marking, caveats, frame_id, extent_min, extent_max, footprint, start_tai_ns, end_tai_ns, provenance, label_bytes, job_id, created_tai_ns) \
            VALUES ($1,$2,$3,$4,$5,$6,$7::text[],$8,$9::double precision[],$10::double precision[],ST_GeogFromText($11::text),$12,$13,$14,$15,$16,$17)";

        let params = vec![
            Param::Text(self.asset_id.clone()),
            Param::Text(self.sha256.clone()),
            Param::Text(self.uri.clone()),
            Param::I64(self.size_bytes as i64),
            Param::Text(self.media_type.clone()),
            Param::Text(self.label.marking.clone()),
            Param::Text(encode_text_array(&self.label.caveats)),
            Param::Text(self.frame_id.clone()),
            Param::Text(encode_f64_array(&self.extent_min)),
            Param::Text(encode_f64_array(&self.extent_max)),
            match &self.footprint_wkt {
                Some(wkt) => Param::Text(wkt.clone()),
                None => Param::Null,
            },
            match self.start_tai_ns {
                Some(v) => Param::I64(v),
                None => Param::Null,
            },
            match self.end_tai_ns {
                Some(v) => Param::I64(v),
                None => Param::Null,
            },
            Param::Text(encode_bytea(&self.provenance.encode_to_vec())),
            Param::Text(encode_bytea(&self.label.encode_to_vec())),
            match &self.job_id {
                Some(v) => Param::Text(v.clone()),
                None => Param::Null,
            },
            Param::I64(self.created_tai_ns),
        ];

        client.execute(SQL, &params).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn fixture_asset_ref() -> AssetRef {
        AssetRef {
            uri: "s3://altavista-heavy/imagery/scene.tif".to_string(),
            sha256: "a".repeat(64),
            size_bytes: 12_345,
            media_type: "image/tiff".to_string(),
            label: Some(Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
            spatial_extent: Some(SpatialExtent { frame_id: "EPSG:4326".to_string(), min: vec![-10.0, -20.0], max: vec![10.0, 20.0] }),
            temporal_extent: Some(TemporalExtent { start_tai_ns: 1_000, end_tai_ns: 2_000 }),
            provenance: Some(Provenance { principal: "svc-tiler".to_string(), tool: "av-catalog-test".to_string(), attributes: BTreeMap::from([("mission".to_string(), "h2-model-test".to_string())]), ..Default::default() }),
            attributes: Default::default(),
        }
    }

    /// A fully-populated `AssetRef` round-trips through `from_asset_ref`/`to_asset_ref`
    /// field-for-field -- docker-free (no database involved at all).
    #[test]
    fn fully_populated_asset_ref_round_trips() {
        let asset_ref = fixture_asset_ref();
        let catalog_asset = CatalogAsset::from_asset_ref(&asset_ref, "asset-1", Some("POLYGON((-10 -20,10 -20,10 20,-10 20,-10 -20))".to_string()), Some("job-1".to_string()), 555).unwrap();
        assert_eq!(catalog_asset.asset_id, "asset-1");
        assert_eq!(catalog_asset.job_id, Some("job-1".to_string()));
        assert_eq!(catalog_asset.created_tai_ns, 555);

        let round_tripped = catalog_asset.to_asset_ref();
        assert_eq!(round_tripped, asset_ref, "every AssetRef field from_asset_ref reads must come back unchanged from to_asset_ref");
    }

    /// This module's own doc: a source `AssetRef` with no `spatial_extent` becomes an
    /// empty-frame catalog row, and reading it back gives `Some(SpatialExtent{..})` with empty
    /// contents -- never `None`, and never a panic.
    #[test]
    fn spatial_extent_absent_round_trips_to_an_empty_present_value() {
        let mut asset_ref = fixture_asset_ref();
        asset_ref.spatial_extent = None;
        let catalog_asset = CatalogAsset::from_asset_ref(&asset_ref, "asset-2", None, None, 1).unwrap();
        assert_eq!(catalog_asset.frame_id, "");
        assert!(catalog_asset.extent_min.is_empty());
        assert!(catalog_asset.extent_max.is_empty());

        let round_tripped = catalog_asset.to_asset_ref();
        assert_eq!(round_tripped.spatial_extent, Some(SpatialExtent { frame_id: String::new(), min: Vec::new(), max: Vec::new() }));
    }

    /// This module's own doc: `temporal_extent`'s `None` DOES round-trip exactly (unlike
    /// `spatial_extent`'s `None`, above) -- `start_tai_ns`/`end_tai_ns` are nullable columns.
    #[test]
    fn temporal_extent_none_round_trips_to_none() {
        let mut asset_ref = fixture_asset_ref();
        asset_ref.temporal_extent = None;
        let catalog_asset = CatalogAsset::from_asset_ref(&asset_ref, "asset-3", None, None, 1).unwrap();
        assert_eq!(catalog_asset.start_tai_ns, None);
        assert_eq!(catalog_asset.end_tai_ns, None);
        assert_eq!(catalog_asset.to_asset_ref().temporal_extent, None);
    }

    /// A source `AssetRef` with no `label` is refused, typed, at conversion time -- never a
    /// silently-defaulted `Label`.
    #[test]
    fn missing_label_is_refused() {
        let mut asset_ref = fixture_asset_ref();
        asset_ref.label = None;
        let err = CatalogAsset::from_asset_ref(&asset_ref, "asset-4", None, None, 1).unwrap_err();
        assert!(matches!(err, CatalogError::AssetRefMissingField { field: "label" }), "{err:?}");
    }

    /// The identical refusal for a missing `provenance`.
    #[test]
    fn missing_provenance_is_refused() {
        let mut asset_ref = fixture_asset_ref();
        asset_ref.provenance = None;
        let err = CatalogAsset::from_asset_ref(&asset_ref, "asset-5", None, None, 1).unwrap_err();
        assert!(matches!(err, CatalogError::AssetRefMissingField { field: "provenance" }), "{err:?}");
    }
}
