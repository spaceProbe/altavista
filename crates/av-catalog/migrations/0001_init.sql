-- 0001_init.sql -- H2 (docs/heavy-plan.md): the catalog's core schema. Applied by
-- crate::migrate::Migrator, exactly once, inside one transaction (Migrator::apply_pending
-- wraps this file's text in BEGIN/COMMIT together with its own bookkeeping INSERT -- see that
-- module's own doc for why the bookkeeping table itself is NOT one of these numbered,
-- hashed files).
--
-- IF NOT EXISTS: services/catalog/IMAGE_DIGEST.md records, measured directly against the real
-- imresamu/postgis image, that this image's own init scripts already run `CREATE EXTENSION
-- postgis` in the default database, and that a bare `CREATE EXTENSION postgis` against a
-- database that already has it fails with "duplicate key value violates unique constraint
-- pg_extension_name_index". A fresh database this migration runs against for the first time
-- (no init-script extension yet) still gets the extension created here -- this statement is
-- correct either way, which is the whole point of IF NOT EXISTS.
CREATE EXTENSION IF NOT EXISTS postgis;

-- ------------------------------------------------------------------------------------------
-- assets: one row per catalogued object-store payload (av-store's own claim-check, AssetRef,
-- catalogued here with the extra fields the catalog itself owns: an id independent of content
-- hash, a job id, and a creation epoch).
-- ------------------------------------------------------------------------------------------

CREATE TABLE assets (
    -- The catalog's own identifier -- deliberately NOT `sha256`. The same bytes (same
    -- sha256, same av-store object) can legitimately be catalogued more than once under
    -- different labels or different provenance records: e.g. a SECRET-marked ingest of a
    -- scene and a later UNCLASSIFIED-marked re-release of the identical pixels (same content,
    -- same hash, different handling and a different Provenance.principal/tool that produced
    -- the *catalog entry*) are two distinct facts a caller may need to tell apart -- one row
    -- each, sharing one `sha256`. Making `sha256` the primary key would make that second
    -- catalog entry impossible to represent at all. `asset_id` is supplied by the caller (this
    -- crate mints no id of its own -- av-catalog has no `rand`/`uuid` dependency, the same
    -- "this crate is not the one that generates identity" convention `av-store`'s own module
    -- doc states for its own content-addressed keys).
    asset_id text PRIMARY KEY,
    -- The catalogued object's own content hash (av-store's `AssetRef.sha256`) -- hex, lower
    -- case, exactly 64 characters (SHA-256). Checked, not merely documented: a malformed value
    -- here would silently break every `sha256 = $1` lookup this crate's own `assets_sha256_idx`
    -- exists to serve.
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    -- The object-store URI this asset's bytes live at (av-store's `AssetRef.uri`, e.g.
    -- "s3://altavista-heavy/imagery/...") -- the catalog stores references, never payloads
    -- (the claim-check pattern H1's own module doc states; this table is no exception).
    uri text NOT NULL,
    -- The object's size in bytes, exactly as av-store recorded it at put time.
    size_bytes bigint NOT NULL CHECK (size_bytes >= 0),
    -- IANA media type, or a platform type such as "application/vnd.altavista.tileset+json"
    -- (`AssetRef.media_type`'s own doc comment, entity.proto).
    media_type text NOT NULL,
    -- The handling marking (`Label.marking`, envelope.proto), duplicated here (out of
    -- `label_bytes` below) as a plain, indexed column purely so `crate::query::find_assets`'s
    -- label filter can be a `WHERE marking = ANY($1)` -- an ordinary btree-indexable predicate
    -- -- rather than a per-row `prost::Message::decode` of every candidate row before it is
    -- even known to be relevant. `label_bytes` remains the one canonical encoding; this column
    -- is written from the SAME `Label` value at insert time (`CatalogAsset::insert`) and is
    -- never an independent fact a caller could set inconsistently with `label_bytes` through
    -- this crate's own API.
    marking text NOT NULL,
    -- `Label.caveats`, duplicated out of `label_bytes` for the same reason `marking` is: a
    -- plain, queryable column derived from the one canonical `Label` at insert time. Empty
    -- array, never NULL, for a `Label` with no caveats.
    caveats text[] NOT NULL DEFAULT '{}',
    -- The frame this asset's `extent_min`/`extent_max` bounding volume is expressed in
    -- (`SpatialExtent.frame_id`, entity.proto) -- e.g. a body-fixed frame for a mesh with no
    -- geodetic meaning, or a named geodetic/ECEF frame. Empty string for an asset whose source
    -- `AssetRef` carried no `SpatialExtent` at all (`crate::model::CatalogAsset::from_asset_ref`
    -- doc explains why empty, not NULL: this column is declared NOT NULL so a plain SQL client
    -- never has to NULL-check it, and "no spatial extent" is representable as an empty
    -- min/max pair in an empty-named frame without losing any information a NULL would have
    -- preserved).
    frame_id text NOT NULL,
    -- The bounding volume's minimum corner, one value per axis of `frame_id`'s own frame,
    -- units and axis order exactly as that frame defines them (`SpatialExtent.min`, packed
    -- `repeated double`, entity.proto) -- e.g. metres in a body-fixed frame. Distinct from
    -- `footprint` below: this is an arbitrary-dimension axis-aligned box in ANY declared
    -- frame (a mesh's body-fixed bounding box has no latitude/longitude at all), while
    -- `footprint` is specifically a WGS84 geodetic polygon for the globe-relative extent
    -- query.
    extent_min double precision[] NOT NULL,
    extent_max double precision[] NOT NULL,
    -- The geodetic footprint, for `crate::query::find_assets`'s bbox query
    -- (`ST_Intersects(footprint, ST_MakeEnvelope(...)::geography)`). NULL for an asset with no
    -- geodetic meaning at all (a body-fixed mesh, a non-georeferenced document) -- `crate::
    -- query`'s own doc comment states the resulting query rule explicitly: a NULL footprint is
    -- EXCLUDED from a bbox-filtered query (it cannot be said to intersect or not intersect
    -- anything) and INCLUDED when no bbox filter is given at all (absence of geodetic meaning
    -- is not itself a reason to hide the asset from an unfiltered listing).
    footprint geography(POLYGON,4326),
    -- Half-open temporal extent `[start_tai_ns, end_tai_ns)`, TAI nanoseconds (the platform's
    -- one time base, `TemporalExtent.start_tai_ns`/`end_tai_ns`, entity.proto) -- e.g. an
    -- imagery scene's acquisition window, or a simulation product's validity interval. Both
    -- NULL for an asset with no temporal extent (`crate::model`'s round-trip doc: this is the
    -- one `AssetRef` sub-message this schema CAN represent as a true SQL NULL, because these
    -- two columns -- unlike `extent_min`/`extent_max`/`frame_id` above -- are nullable).
    start_tai_ns bigint,
    end_tai_ns bigint,
    -- Paired nullability: a temporal extent is both bounds or neither -- never a start with no
    -- end or an end with no start, which would be a half-recorded fact no query in
    -- `crate::query` gives any meaning to.
    CONSTRAINT assets_time_pairing_check CHECK ((start_tai_ns IS NULL) = (end_tai_ns IS NULL)),
    -- The prost-encoded `altavista.v1.Provenance` (core.proto) -- who or what produced this
    -- CATALOG ENTRY (which, per `asset_id`'s own doc comment above, is not necessarily unique
    -- per `sha256`). Canonical and complete: unlike `marking`/`caveats`, no plain column
    -- duplicates any part of this value, because nothing in `crate::query` today needs to
    -- filter on a `Provenance` field -- a future query that does can add one the same way
    -- `marking`/`caveats` were added, without touching this column's own meaning.
    provenance bytea NOT NULL,
    -- The prost-encoded `altavista.v1.Label` (envelope.proto) -- the canonical source `marking`
    -- and `caveats` above are both derived from at insert time. Kept in full (not just
    -- reconstructed from those two columns on read) so a future `Label` field this schema does
    -- not yet know to project into its own column is never silently dropped by a round trip
    -- through this table.
    label_bytes bytea NOT NULL,
    -- The job that produced this asset, if any (H2's own "job lineage" -- `asset_lineage`
    -- below records the INPUT side of that same job; this column is the OUTPUT side: "which
    -- job made this asset"). NULL for an asset catalogued outside any job (e.g. an operator's
    -- direct ingest).
    job_id text,
    -- Catalog-entry creation epoch, TAI nanoseconds -- supplied by the caller
    -- (`Migrator::apply_pending`'s own doc states the same rule for `schema_migrations.
    -- applied_tai_ns`: this crate never reads a clock itself, rule 7).
    created_tai_ns bigint NOT NULL
);

COMMENT ON TABLE assets IS 'One catalogued reference to an av-store object (the claim-check pattern): identity, hash, label, spatial/temporal extent, provenance and job lineage. See crate::model::CatalogAsset for the Rust-side record this table backs.';
COMMENT ON COLUMN assets.asset_id IS 'Catalog identifier, caller-supplied, NOT the object''s sha256 -- the same bytes can be catalogued twice under different labels/provenance (see this table''s own COMMENT and the column-level rationale in 0001_init.sql).';
COMMENT ON COLUMN assets.sha256 IS 'SHA-256 of the object-store payload, hex, lower case, exactly 64 characters (checked).';
COMMENT ON COLUMN assets.uri IS 'Object-store URI (av-store AssetRef.uri), e.g. s3://bucket/key. A reference only -- no payload bytes live in this table.';
COMMENT ON COLUMN assets.size_bytes IS 'Payload size in bytes, as recorded by av-store at put time. Never negative.';
COMMENT ON COLUMN assets.media_type IS 'IANA media type, or a platform type such as application/vnd.altavista.tileset+json.';
COMMENT ON COLUMN assets.marking IS 'Handling marking (Label.marking), duplicated out of label_bytes as a plain, btree-indexed column so the label filter in crate::query::find_assets is a WHERE clause, never a post-filter (see that module''s own doc for why that distinction matters).';
COMMENT ON COLUMN assets.caveats IS 'Label.caveats, duplicated out of label_bytes for the same reason marking is. Empty array, never NULL, for a Label with no caveats.';
COMMENT ON COLUMN assets.frame_id IS 'The frame extent_min/extent_max are expressed in (SpatialExtent.frame_id) -- may be a body-fixed or other non-geodetic frame; empty string when the source AssetRef carried no SpatialExtent at all.';
COMMENT ON COLUMN assets.extent_min IS 'Bounding-volume minimum corner, one value per axis of frame_id''s own frame, units/axis order as that frame defines (SpatialExtent.min). Not geodetic -- see footprint for the WGS84 polygon used by the bbox query.';
COMMENT ON COLUMN assets.extent_max IS 'Bounding-volume maximum corner -- see extent_min.';
COMMENT ON COLUMN assets.footprint IS 'Geodetic footprint (WGS84, geography(POLYGON,4326)) for crate::query::find_assets'' bbox query. NULL for an asset with no geodetic meaning (excluded from a bbox-filtered query, included when no bbox filter is given -- see crate::query''s own doc for the exact rule).';
COMMENT ON COLUMN assets.start_tai_ns IS 'Temporal extent start, TAI NANOSECONDS (the platform''s one time base), half-open [start, end). NULL iff end_tai_ns is also NULL.';
COMMENT ON COLUMN assets.end_tai_ns IS 'Temporal extent end (exclusive), TAI NANOSECONDS. NULL iff start_tai_ns is also NULL.';
COMMENT ON COLUMN assets.provenance IS 'prost-encoded altavista.v1.Provenance (core.proto): who/what produced this catalog entry.';
COMMENT ON COLUMN assets.label_bytes IS 'prost-encoded altavista.v1.Label (envelope.proto) -- the canonical source marking/caveats are derived from at insert time.';
COMMENT ON COLUMN assets.job_id IS 'The job that produced this asset (H2''s job lineage, output side), NULL for an asset catalogued outside any job.';
COMMENT ON COLUMN assets.created_tai_ns IS 'Catalog-entry creation epoch, TAI NANOSECONDS, supplied by the caller (this crate never reads a clock itself).';

-- GIST for the geodetic bbox query (ST_Intersects); btree on the half-open time range for
-- crate::query's time filter; btree on marking for the label filter's `= ANY(...)`; btree on
-- sha256 for "every catalog entry of this content hash" lookups (a common av-store-adjacent
-- question this schema should answer in O(log n), not a sequential scan).
CREATE INDEX assets_footprint_gist_idx ON assets USING GIST (footprint);
CREATE INDEX assets_time_range_idx ON assets (start_tai_ns, end_tai_ns);
CREATE INDEX assets_marking_idx ON assets (marking);
CREATE INDEX assets_sha256_idx ON assets (sha256);

-- ------------------------------------------------------------------------------------------
-- asset_lineage: H2's own "job lineage" -- which asset(s) a given asset was derived FROM, and
-- by which job. An asset may have zero, one, or many parents (H3's tiler, for instance, reads
-- one input and writes many output tiles -- each output tile's own lineage row names the one
-- input; a future job that MERGES several inputs into one output would instead give that one
-- output several lineage rows, one per input -- this table's own primary key already allows
-- that shape without any change).
-- ------------------------------------------------------------------------------------------

CREATE TABLE asset_lineage (
    asset_id text NOT NULL REFERENCES assets (asset_id) ON DELETE CASCADE,
    parent_asset_id text NOT NULL REFERENCES assets (asset_id) ON DELETE CASCADE,
    job_id text NOT NULL,
    -- An asset cannot be recorded as its own parent -- a lineage edge with no length is not a
    -- lineage fact, and would make a naive "walk the lineage graph" caller loop forever.
    CONSTRAINT asset_lineage_not_self_referential CHECK (asset_id <> parent_asset_id),
    PRIMARY KEY (asset_id, parent_asset_id, job_id)
);

COMMENT ON TABLE asset_lineage IS 'H2 job lineage: asset_id was derived from parent_asset_id by job_id. An asset may have zero, one or many parents (and may be the parent of many children); see this file''s own comment above this table for the tiler-shaped example.';
COMMENT ON COLUMN asset_lineage.asset_id IS 'The derived (output) asset. ON DELETE CASCADE: a lineage row about an asset that no longer exists in this catalog is not a fact worth keeping either.';
COMMENT ON COLUMN asset_lineage.parent_asset_id IS 'The input asset this row says asset_id was derived from. ON DELETE CASCADE: same rationale as asset_id''s own comment -- a lineage row naming a parent that no longer exists in this catalog is not a fact worth keeping.';
COMMENT ON COLUMN asset_lineage.job_id IS 'The job that performed this derivation (H3''s job runner is the first producer of these rows). Part of the primary key: the SAME asset_id/parent_asset_id pair produced by two different jobs (e.g. a job re-run) is two distinct lineage facts, not a duplicate.';
