//! The ClickHouse tables of the study store: their DDL, their expected live shape, the migration
//! that creates them, and the verification that refuses a store whose live tables do not match.
//!
//! # Columnar, not a blob
//!
//! Every column is a field of the proto (`proto/altavista/v1/run.proto`, `core.proto`'s
//! `Provenance`); nothing stores an encoded `SweepResults`. A blob would make "the ClickHouse copy
//! equals the file copy" vacuous (the same bytes in, the same bytes out) and would leave ClickHouse
//! unable to answer the MoE query in SQL. Each proto `map<string, T>` is its own table keyed by the
//! map key, because `SweepSample.scores` is keyed by one string and carries another
//! (`ScoreResult.name`), and `aggregate()` groups on the *key*: the two must be separate columns.
//! `Provenance` is columns on the study header (its scalar fields) plus a table for its
//! `attributes` map, rather than an encoded blob, for the same reason.
//!
//! | table | one row per | proto |
//! |---|---|---|
//! | `av_sweep_schema_version` | applied schema version | (bookkeeping) |
//! | `av_sweep_studies` | written study (the header, inserted **last**) | `SweepResults` scalars + `Provenance` scalars |
//! | `av_sweep_samples` | `SweepResults.samples[i]` | `SweepSample` scalars |
//! | `av_sweep_sample_axis_values` | `SweepSample.axis_values` entry | `map<string,double>` |
//! | `av_sweep_sample_seeds` | `SweepSample.seeds` entry | `map<string,uint64>` |
//! | `av_sweep_sample_scores` | `SweepSample.scores` entry | `map<string,ScoreResult>` |
//! | `av_sweep_aggregates` | `SweepResults.aggregates[i]` | `ScoreAggregate` |
//! | `av_sweep_provenance_attributes` | `Provenance.attributes` entry | `map<string,string>` |
//!
//! Bookkeeping columns (`write_id`, `ordinal`, `sample_ordinal`, `sample_count`,
//! `aggregate_count`, `content_sha256`, `created_at`) are not proto fields: they are what makes
//! an all-or-nothing write and an order-preserving read possible on a database without
//! transactions (see [`crate::clickhouse`]).
//!
//! # Migration and verification
//!
//! [`migrate`] is `CREATE TABLE IF NOT EXISTS` for every table plus one idempotent version row; run
//! twice, `SHOW CREATE TABLE` is unchanged. [`verify`] reads `system.columns` (name, type,
//! position) and compares it with [`TABLES`]; any difference is a typed
//! [`StoreError::SchemaMismatch`] naming table, column, expected and found. An opened store never
//! silently writes into a table whose shape it does not know.

use std::collections::BTreeMap;

use crate::store::StoreError;

use super::client::ChClient;
use super::rowbinary::Decoder;

/// The schema version this build writes and expects. A live version table holding any other
/// value is a [`StoreError::SchemaMismatch`].
pub const SCHEMA_VERSION: u32 = 1;

pub const SCHEMA_VERSION_TABLE: &str = "av_sweep_schema_version";
pub const STUDIES: &str = "av_sweep_studies";
pub const SAMPLES: &str = "av_sweep_samples";
pub const AXIS_VALUES: &str = "av_sweep_sample_axis_values";
pub const SEEDS: &str = "av_sweep_sample_seeds";
pub const SCORES: &str = "av_sweep_sample_scores";
pub const AGGREGATES: &str = "av_sweep_aggregates";
pub const PROVENANCE_ATTRIBUTES: &str = "av_sweep_provenance_attributes";

/// One table: name, columns in order (name, ClickHouse type as `system.columns` reports it, and
/// an optional `DEFAULT` expression that is part of the DDL but not of the reported type), and the
/// `ORDER BY` key.
pub struct TableSpec {
    pub name: &'static str,
    pub columns: &'static [Column],
    pub order_by: &'static [&'static str],
}

pub struct Column {
    pub name: &'static str,
    pub ty: &'static str,
    pub default: Option<&'static str>,
}

const fn col(name: &'static str, ty: &'static str) -> Column {
    Column { name, ty, default: None }
}

impl TableSpec {
    /// The column names an `INSERT` supplies, in order: every column without a server-side
    /// `DEFAULT`.
    pub fn insert_columns(&self) -> Vec<&'static str> {
        self.columns.iter().filter(|c| c.default.is_none()).map(|c| c.name).collect()
    }

    /// The ClickHouse type of each [`Self::insert_columns`] entry, in the same order.
    pub fn insert_types(&self) -> Vec<&'static str> {
        self.columns.iter().filter(|c| c.default.is_none()).map(|c| c.ty).collect()
    }

    /// `CREATE TABLE IF NOT EXISTS ...` for this table.
    pub fn create_sql(&self) -> String {
        let mut sql = format!("CREATE TABLE IF NOT EXISTS {} (\n", quote_ident(self.name));
        for (i, c) in self.columns.iter().enumerate() {
            sql.push_str("    ");
            sql.push_str(&quote_ident(c.name));
            sql.push(' ');
            sql.push_str(c.ty);
            if let Some(d) = c.default {
                sql.push_str(" DEFAULT ");
                sql.push_str(d);
            }
            sql.push_str(if i + 1 < self.columns.len() { ",\n" } else { "\n" });
        }
        let key: Vec<String> = self.order_by.iter().map(|c| quote_ident(c)).collect();
        sql.push_str(&format!(") ENGINE = MergeTree ORDER BY ({})", key.join(", ")));
        sql
    }
}

/// Every table the store owns, in creation order.
pub const TABLES: &[TableSpec] = &[
    TableSpec {
        name: SCHEMA_VERSION_TABLE,
        columns: &[col("version", "UInt32"), col("description", "String")],
        order_by: &["version"],
    },
    TableSpec {
        name: STUDIES,
        columns: &[
            col("sweep_id", "String"),
            col("write_id", "String"),
            col("sweep_hash", "String"),
            col("drm_hash", "String"),
            col("sample_count", "UInt64"),
            col("aggregate_count", "UInt64"),
            col("content_sha256", "String"),
            col("has_provenance", "Bool"),
            col("provenance_author_kind", "Int32"),
            col("provenance_principal", "String"),
            col("provenance_tool", "String"),
            col("provenance_config_hash", "String"),
            col("provenance_data_pack_hash", "String"),
            col("provenance_dataset_hash", "String"),
            col("provenance_created_tai_ns", "Int64"),
            col("provenance_run_id", "String"),
            Column { name: "created_at", ty: "DateTime64(6)", default: Some("now64(6)") },
        ],
        order_by: &["sweep_id", "created_at", "write_id"],
    },
    TableSpec {
        name: SAMPLES,
        columns: &[
            col("sweep_id", "String"),
            col("write_id", "String"),
            col("ordinal", "UInt64"),
            col("point_index", "UInt32"),
            col("draw_index", "UInt32"),
            col("run_id", "String"),
            col("config_hash", "String"),
            col("products_uri", "String"),
            col("error", "String"),
        ],
        order_by: &["sweep_id", "write_id", "ordinal"],
    },
    TableSpec {
        name: AXIS_VALUES,
        columns: &[col("sweep_id", "String"), col("write_id", "String"), col("sample_ordinal", "UInt64"), col("key", "String"), col("value", "Float64")],
        order_by: &["sweep_id", "write_id", "sample_ordinal", "key"],
    },
    TableSpec {
        name: SEEDS,
        columns: &[col("sweep_id", "String"), col("write_id", "String"), col("sample_ordinal", "UInt64"), col("key", "String"), col("seed", "UInt64")],
        order_by: &["sweep_id", "write_id", "sample_ordinal", "key"],
    },
    TableSpec {
        name: SCORES,
        columns: &[
            col("sweep_id", "String"),
            col("write_id", "String"),
            col("sample_ordinal", "UInt64"),
            col("key", "String"),
            col("name", "String"),
            col("value", "Float64"),
            col("unit", "Int32"),
            col("passed", "Nullable(Bool)"),
        ],
        order_by: &["sweep_id", "write_id", "sample_ordinal", "key"],
    },
    TableSpec {
        name: AGGREGATES,
        columns: &[
            col("sweep_id", "String"),
            col("write_id", "String"),
            col("ordinal", "UInt64"),
            col("name", "String"),
            col("point_index", "UInt32"),
            col("draws", "UInt32"),
            col("mean", "Float64"),
            col("std_dev", "Float64"),
            col("min", "Float64"),
            col("max", "Float64"),
            col("pass_fraction", "Nullable(Float64)"),
        ],
        order_by: &["sweep_id", "write_id", "ordinal"],
    },
    TableSpec {
        name: PROVENANCE_ATTRIBUTES,
        columns: &[col("sweep_id", "String"), col("write_id", "String"), col("key", "String"), col("value", "String")],
        order_by: &["sweep_id", "write_id", "key"],
    },
];

/// The table spec named `name`. Panics on an unknown name: callers pass this module's constants.
pub fn table(name: &str) -> &'static TableSpec {
    TABLES.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no table named {name:?} in the study schema"))
}

/// A backtick-quoted ClickHouse identifier. Every identifier in this module is a compile-time
/// constant of `[a-z_]`, so no escaping is needed; the debug assertion keeps it that way.
pub fn quote_ident(ident: &str) -> String {
    debug_assert!(ident.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit()), "{ident:?}");
    format!("`{ident}`")
}

/// What [`verify`] found when nothing was wrong with the tables that exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaState {
    /// Every table exists, matches [`TABLES`], and the version row says [`SCHEMA_VERSION`].
    Ready,
    /// These tables do not exist yet (and no table that does exist disagrees with the spec), or
    /// the version table exists but holds no row: [`migrate`] would complete the schema.
    Incomplete { missing: Vec<&'static str> },
}

/// Creates every missing table and records the schema version, idempotently. Needs DDL
/// privileges; a read-only user's server refusal comes back as the typed server exception.
pub fn migrate(client: &ChClient) -> Result<(), StoreError> {
    for t in TABLES {
        client.command(&t.create_sql(), &[], &format!("creating table {}", t.name))?;
    }
    // Exactly one row per applied version, written only if none exists: running this twice, or
    // two stores opening at once, leaves the table valid (duplicates of the same version are
    // accepted by `verify`).
    let sql = format!(
        "INSERT INTO {v} (version, description) SELECT {ver}, 'sweep study store: columnar SweepResults' WHERE (SELECT count() FROM {v}) = 0",
        v = quote_ident(SCHEMA_VERSION_TABLE),
        ver = SCHEMA_VERSION
    );
    client.command(&sql, &[], "recording the schema version")?;
    Ok(())
}

/// Compares the live tables with [`TABLES`] through `system.columns`.
///
/// Errors with [`StoreError::SchemaMismatch`] on the first difference found, table by table in
/// creation order, column by column in position order: a column with a different name or type at
/// a position, a column missing from the live table, or an extra live column. A table with no
/// columns at all does not exist; that is reported through [`SchemaState::Incomplete`], not as a
/// mismatch, unless another table already disagrees.
pub fn verify(client: &ChClient) -> Result<SchemaState, StoreError> {
    let names: Vec<String> = TABLES.iter().map(|t| format!("'{}'", t.name)).collect();
    let sql = format!(
        "SELECT `table`, `name`, `type`, `position` FROM system.columns WHERE database = currentDatabase() AND `table` IN ({}) ORDER BY `table`, `position` FORMAT RowBinary",
        names.join(", ")
    );
    let bytes = client.query(&sql, &[], "reading system.columns")?;
    let mut live: BTreeMap<String, Vec<(String, String, u64)>> = BTreeMap::new();
    let mut d = Decoder::new(&bytes);
    while !d.is_done() {
        let decoded = (|| Ok::<_, super::rowbinary::RowBinaryError>((d.string()?, d.string()?, d.string()?, d.u64()?)))();
        let (t, name, ty, position) = decoded.map_err(|e| client.corrupt("reading system.columns", e.to_string()))?;
        live.entry(t).or_default().push((name, ty, position));
    }

    let mut missing = Vec::new();
    for spec in TABLES {
        let Some(found) = live.get(spec.name) else {
            missing.push(spec.name);
            continue;
        };
        for (i, expected) in spec.columns.iter().enumerate() {
            let want_position = (i + 1) as u64;
            match found.get(i) {
                None => {
                    return Err(StoreError::SchemaMismatch {
                        table: spec.name.to_string(),
                        column: expected.name.to_string(),
                        expected: format!("{} at position {want_position}", expected.ty),
                        found: "no such column".to_string(),
                    })
                }
                Some((name, ty, position)) => {
                    if name != expected.name || ty != expected.ty || *position != want_position {
                        return Err(StoreError::SchemaMismatch {
                            table: spec.name.to_string(),
                            column: expected.name.to_string(),
                            expected: format!("{} `{}` {} at position {want_position}", "column", expected.name, expected.ty),
                            found: format!("column `{name}` {ty} at position {position}"),
                        });
                    }
                }
            }
        }
        if found.len() > spec.columns.len() {
            let (name, ty, position) = &found[spec.columns.len()];
            return Err(StoreError::SchemaMismatch {
                table: spec.name.to_string(),
                column: name.clone(),
                expected: "no such column".to_string(),
                found: format!("column `{name}` {ty} at position {position}"),
            });
        }
    }

    if !missing.contains(&SCHEMA_VERSION_TABLE) {
        let sql = format!("SELECT `version` FROM {} ORDER BY `version` FORMAT RowBinary", quote_ident(SCHEMA_VERSION_TABLE));
        let bytes = client.query(&sql, &[], "reading the schema version")?;
        let mut d = Decoder::new(&bytes);
        let mut versions = Vec::new();
        while !d.is_done() {
            versions.push(d.u32().map_err(|e| client.corrupt("reading the schema version", e.to_string()))?);
        }
        if versions.is_empty() {
            if !missing.contains(&SCHEMA_VERSION_TABLE) {
                missing.push(SCHEMA_VERSION_TABLE);
            }
        } else if let Some(other) = versions.iter().find(|v| **v != SCHEMA_VERSION) {
            return Err(StoreError::SchemaMismatch {
                table: SCHEMA_VERSION_TABLE.to_string(),
                column: "version".to_string(),
                expected: SCHEMA_VERSION.to_string(),
                found: other.to_string(),
            });
        }
    }

    if missing.is_empty() {
        Ok(SchemaState::Ready)
    } else {
        Ok(SchemaState::Incomplete { missing })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ddl_is_pinned() {
        let studies = table(STUDIES).create_sql();
        assert_eq!(
            studies,
            "CREATE TABLE IF NOT EXISTS `av_sweep_studies` (\n    `sweep_id` String,\n    `write_id` String,\n    `sweep_hash` String,\n    `drm_hash` String,\n    `sample_count` UInt64,\n    `aggregate_count` UInt64,\n    `content_sha256` String,\n    `has_provenance` Bool,\n    `provenance_author_kind` Int32,\n    `provenance_principal` String,\n    `provenance_tool` String,\n    `provenance_config_hash` String,\n    `provenance_data_pack_hash` String,\n    `provenance_dataset_hash` String,\n    `provenance_created_tai_ns` Int64,\n    `provenance_run_id` String,\n    `created_at` DateTime64(6) DEFAULT now64(6)\n) ENGINE = MergeTree ORDER BY (`sweep_id`, `created_at`, `write_id`)"
        );
        assert_eq!(table(PROVENANCE_ATTRIBUTES).create_sql(), "CREATE TABLE IF NOT EXISTS `av_sweep_provenance_attributes` (\n    `sweep_id` String,\n    `write_id` String,\n    `key` String,\n    `value` String\n) ENGINE = MergeTree ORDER BY (`sweep_id`, `write_id`, `key`)");
    }

    #[test]
    fn a_default_column_is_not_an_insert_column() {
        let studies = table(STUDIES);
        assert!(!studies.insert_columns().contains(&"created_at"));
        assert_eq!(studies.insert_columns().len(), studies.columns.len() - 1);
        assert_eq!(studies.insert_columns().len(), studies.insert_types().len());
    }

    #[test]
    fn every_table_is_distinct_and_every_order_by_column_exists() {
        let mut names: Vec<&str> = TABLES.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TABLES.len());
        for t in TABLES {
            for k in t.order_by {
                assert!(t.columns.iter().any(|c| c.name == *k), "{}: ORDER BY column {k} is not a column", t.name);
            }
        }
    }
}
