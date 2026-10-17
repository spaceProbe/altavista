//! The mapping between `altavista.v1.SweepResults` and the study tables' `RowBinary` rows, in both
//! directions.
//!
//! # The mapping is total, and the compiler checks it
//!
//! Every proto message is **destructured with every field named and no `..`** on the way in
//! ([`encode_study`]), and **constructed with every field named and no
//! `..Default::default()`** on the way out ([`decode_samples`], [`decode_aggregates`], ...). A
//! field added to `SweepResults`, `SweepSample`, `ScoreResult`, `ScoreAggregate` or `Provenance`
//! therefore stops this crate compiling until its column is added: a column can never be silently
//! dropped by a schema that lagged the proto. (`tests/clickhouse_store.rs` additionally round-trips
//! a message with every field non-default.)
//!
//! Column order in every encoder below is the order of [`super::schema::TABLES`]'s
//! `insert_columns()`; the unit tests walk each encoded row against the table's declared types.

use std::collections::BTreeMap;

use av_cdm::pb;

use super::rowbinary::{Decoder, Encoder, RowBinaryError};
use super::schema;

/// A request body is split once it reaches this many bytes, at a row boundary.
const CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// An encoder that cuts its output into row-aligned chunks.
struct Chunks {
    done: Vec<Vec<u8>>,
    cur: Encoder,
    limit: usize,
}

impl Chunks {
    fn new(limit: usize) -> Self {
        Self { done: Vec::new(), cur: Encoder::new(), limit }
    }

    fn enc(&mut self) -> &mut Encoder {
        &mut self.cur
    }

    fn end_row(&mut self) {
        if self.cur.len() >= self.limit {
            let full = self.cur.take();
            self.done.push(full);
        }
    }

    fn finish(mut self) -> Vec<Vec<u8>> {
        if !self.cur.is_empty() {
            let last = self.cur.take();
            self.done.push(last);
        }
        self.done
    }
}

/// One study as `RowBinary` bodies, one `Vec` of row-aligned chunks per table. `header` is a
/// single row and is inserted **last**.
pub struct EncodedStudy {
    pub samples: Vec<Vec<u8>>,
    pub axis_values: Vec<Vec<u8>>,
    pub seeds: Vec<Vec<u8>>,
    pub scores: Vec<Vec<u8>>,
    pub aggregates: Vec<Vec<u8>>,
    pub provenance_attributes: Vec<Vec<u8>>,
    pub header: Vec<u8>,
}

/// Encodes `results` for one write: every row carries `write_id`, and the header names it and the
/// content hash.
pub fn encode_study(results: &pb::SweepResults, write_id: &str, content_sha256: &str) -> EncodedStudy {
    encode_study_chunked(results, write_id, content_sha256, CHUNK_BYTES)
}

/// [`encode_study`] with an explicit chunk size (a request body is cut at the first row boundary at
/// or past `limit` bytes).
pub fn encode_study_chunked(results: &pb::SweepResults, write_id: &str, content_sha256: &str, limit: usize) -> EncodedStudy {
    let pb::SweepResults { sweep_id, sweep_hash, drm_hash, samples, aggregates, provenance } = results;

    let mut samples_out = Chunks::new(limit);
    let mut axis_out = Chunks::new(limit);
    let mut seeds_out = Chunks::new(limit);
    let mut scores_out = Chunks::new(limit);
    for (ordinal, sample) in samples.iter().enumerate() {
        let ordinal = ordinal as u64;
        let pb::SweepSample { point_index, draw_index, axis_values, run_id, config_hash, scores, products_uri, error, seeds } = sample;

        let e = samples_out.enc();
        e.string(sweep_id);
        e.string(write_id);
        e.u64(ordinal);
        e.u32(*point_index);
        e.u32(*draw_index);
        e.string(run_id);
        e.string(config_hash);
        e.string(products_uri);
        e.string(error);
        samples_out.end_row();

        for (key, value) in axis_values {
            let e = axis_out.enc();
            e.string(sweep_id);
            e.string(write_id);
            e.u64(ordinal);
            e.string(key);
            e.f64(*value);
            axis_out.end_row();
        }
        for (key, seed) in seeds {
            let e = seeds_out.enc();
            e.string(sweep_id);
            e.string(write_id);
            e.u64(ordinal);
            e.string(key);
            e.u64(*seed);
            seeds_out.end_row();
        }
        for (key, score) in scores {
            let pb::ScoreResult { name, value, unit, passed } = score;
            let e = scores_out.enc();
            e.string(sweep_id);
            e.string(write_id);
            e.u64(ordinal);
            e.string(key);
            e.string(name);
            e.f64(*value);
            e.i32(*unit);
            e.nullable_bool(*passed);
            scores_out.end_row();
        }
    }

    let mut aggregates_out = Chunks::new(limit);
    for (ordinal, aggregate) in aggregates.iter().enumerate() {
        let pb::ScoreAggregate { name, point_index, draws, mean, std_dev, min, max, pass_fraction } = aggregate;
        let e = aggregates_out.enc();
        e.string(sweep_id);
        e.string(write_id);
        e.u64(ordinal as u64);
        e.string(name);
        e.u32(*point_index);
        e.u32(*draws);
        e.f64(*mean);
        e.f64(*std_dev);
        e.f64(*min);
        e.f64(*max);
        e.nullable_f64(*pass_fraction);
        aggregates_out.end_row();
    }

    // The provenance scalars live on the header; its `attributes` map is a table of its own.
    let mut attributes_out = Chunks::new(limit);
    let mut header = Encoder::new();
    header.string(sweep_id);
    header.string(write_id);
    header.string(sweep_hash);
    header.string(drm_hash);
    header.u64(samples.len() as u64);
    header.u64(aggregates.len() as u64);
    header.string(content_sha256);
    match provenance {
        None => {
            header.bool(false);
            header.i32(0);
            for _ in 0..5 {
                header.string("");
            }
            header.i64(0);
            header.string("");
        }
        Some(p) => {
            let pb::Provenance { author_kind, principal, tool, config_hash, data_pack_hash, dataset_hash, created_tai_ns, run_id, attributes } = p;
            header.bool(true);
            header.i32(*author_kind);
            header.string(principal);
            header.string(tool);
            header.string(config_hash);
            header.string(data_pack_hash);
            header.string(dataset_hash);
            header.i64(*created_tai_ns);
            header.string(run_id);
            for (key, value) in attributes {
                let e = attributes_out.enc();
                e.string(sweep_id);
                e.string(write_id);
                e.string(key);
                e.string(value);
                attributes_out.end_row();
            }
        }
    }

    EncodedStudy {
        samples: samples_out.finish(),
        axis_values: axis_out.finish(),
        seeds: seeds_out.finish(),
        scores: scores_out.finish(),
        aggregates: aggregates_out.finish(),
        provenance_attributes: attributes_out.finish(),
        header: header.finish(),
    }
}

/// The columns [`decode_header`] expects, in order (every header column except `sweep_id`, which
/// the reader supplied, and `created_at`, which only orders concurrent headers).
pub const HEADER_SELECT: &str = "`write_id`, `sweep_hash`, `drm_hash`, `sample_count`, `aggregate_count`, `content_sha256`, `has_provenance`, `provenance_author_kind`, `provenance_principal`, `provenance_tool`, `provenance_config_hash`, `provenance_data_pack_hash`, `provenance_dataset_hash`, `provenance_created_tai_ns`, `provenance_run_id`";

/// A study header row, minus `Provenance.attributes` (a table of its own).
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub write_id: String,
    pub sweep_hash: String,
    pub drm_hash: String,
    pub sample_count: u64,
    pub aggregate_count: u64,
    pub content_sha256: String,
    /// `None` when the study had no provenance; otherwise every scalar field, `attributes` empty.
    pub provenance: Option<pb::Provenance>,
}

/// Decodes one [`HEADER_SELECT`] row.
pub fn decode_header(d: &mut Decoder<'_>) -> Result<Header, RowBinaryError> {
    let write_id = d.string()?;
    let sweep_hash = d.string()?;
    let drm_hash = d.string()?;
    let sample_count = d.u64()?;
    let aggregate_count = d.u64()?;
    let content_sha256 = d.string()?;
    let has_provenance = d.bool()?;
    let author_kind = d.i32()?;
    let principal = d.string()?;
    let tool = d.string()?;
    let config_hash = d.string()?;
    let data_pack_hash = d.string()?;
    let dataset_hash = d.string()?;
    let created_tai_ns = d.i64()?;
    let run_id = d.string()?;
    let provenance = has_provenance.then(|| pb::Provenance { author_kind, principal, tool, config_hash, data_pack_hash, dataset_hash, created_tai_ns, run_id, attributes: Default::default() });
    Ok(Header { write_id, sweep_hash, drm_hash, sample_count, aggregate_count, content_sha256, provenance })
}

/// `SELECT` list for [`decode_samples`].
pub const SAMPLES_SELECT: &str = "`ordinal`, `point_index`, `draw_index`, `run_id`, `config_hash`, `products_uri`, `error`";
/// `SELECT` list for [`decode_axis_values`].
pub const AXIS_SELECT: &str = "`sample_ordinal`, `key`, `value`";
/// `SELECT` list for [`decode_seeds`].
pub const SEEDS_SELECT: &str = "`sample_ordinal`, `key`, `seed`";
/// `SELECT` list for [`decode_scores`].
pub const SCORES_SELECT: &str = "`sample_ordinal`, `key`, `name`, `value`, `unit`, `passed`";
/// `SELECT` list for [`decode_aggregates`].
pub const AGGREGATES_SELECT: &str = "`ordinal`, `name`, `point_index`, `draws`, `mean`, `std_dev`, `min`, `max`, `pass_fraction`";
/// `SELECT` list for [`decode_provenance_attributes`].
pub const ATTRIBUTES_SELECT: &str = "`key`, `value`";

/// Why a set of rows did not assemble into a study.
#[derive(Debug, PartialEq, Eq)]
pub enum AssembleError {
    Decode(RowBinaryError),
    /// The rows disagree with the header or with themselves (a gap, a duplicate, an ordinal past
    /// the header's count); the text says how.
    Inconsistent(String),
}

impl From<RowBinaryError> for AssembleError {
    fn from(e: RowBinaryError) -> Self {
        AssembleError::Decode(e)
    }
}

/// Samples in ordinal order, `expected` of them, each with empty maps (filled by the callers
/// below). Ordinals must be exactly `0..expected`.
pub fn decode_samples(bytes: &[u8], expected: u64) -> Result<Vec<pb::SweepSample>, AssembleError> {
    let mut d = Decoder::new(bytes);
    let mut out = Vec::new();
    while !d.is_done() {
        let ordinal = d.u64()?;
        let point_index = d.u32()?;
        let draw_index = d.u32()?;
        let run_id = d.string()?;
        let config_hash = d.string()?;
        let products_uri = d.string()?;
        let error = d.string()?;
        if ordinal != out.len() as u64 {
            return Err(AssembleError::Inconsistent(format!("sample ordinals are not 0..n in order: row {} carries ordinal {ordinal}", out.len())));
        }
        out.push(pb::SweepSample { point_index, draw_index, axis_values: BTreeMap::new(), run_id, config_hash, scores: BTreeMap::new(), products_uri, error, seeds: BTreeMap::new() });
    }
    if out.len() as u64 != expected {
        return Err(AssembleError::Inconsistent(format!("the header says {expected} sample(s) but {} sample row(s) were read", out.len())));
    }
    Ok(out)
}

fn sample_mut<'a>(samples: &'a mut [pb::SweepSample], ordinal: u64, table: &str) -> Result<&'a mut pb::SweepSample, AssembleError> {
    let count = samples.len();
    usize::try_from(ordinal).ok().and_then(|i| samples.get_mut(i)).ok_or_else(|| AssembleError::Inconsistent(format!("{table} row for sample ordinal {ordinal}, but the study has {count} sample(s)")))
}

pub fn decode_axis_values(bytes: &[u8], samples: &mut [pb::SweepSample]) -> Result<(), AssembleError> {
    let mut d = Decoder::new(bytes);
    while !d.is_done() {
        let ordinal = d.u64()?;
        let key = d.string()?;
        let value = d.f64()?;
        if sample_mut(samples, ordinal, schema::AXIS_VALUES)?.axis_values.insert(key.clone(), value).is_some() {
            return Err(AssembleError::Inconsistent(format!("{}: duplicate key {key:?} for sample ordinal {ordinal}", schema::AXIS_VALUES)));
        }
    }
    Ok(())
}

pub fn decode_seeds(bytes: &[u8], samples: &mut [pb::SweepSample]) -> Result<(), AssembleError> {
    let mut d = Decoder::new(bytes);
    while !d.is_done() {
        let ordinal = d.u64()?;
        let key = d.string()?;
        let seed = d.u64()?;
        if sample_mut(samples, ordinal, schema::SEEDS)?.seeds.insert(key.clone(), seed).is_some() {
            return Err(AssembleError::Inconsistent(format!("{}: duplicate key {key:?} for sample ordinal {ordinal}", schema::SEEDS)));
        }
    }
    Ok(())
}

pub fn decode_scores(bytes: &[u8], samples: &mut [pb::SweepSample]) -> Result<(), AssembleError> {
    let mut d = Decoder::new(bytes);
    while !d.is_done() {
        let ordinal = d.u64()?;
        let key = d.string()?;
        let name = d.string()?;
        let value = d.f64()?;
        let unit = d.i32()?;
        let passed = d.nullable_bool()?;
        let score = pb::ScoreResult { name, value, unit, passed };
        if sample_mut(samples, ordinal, schema::SCORES)?.scores.insert(key.clone(), score).is_some() {
            return Err(AssembleError::Inconsistent(format!("{}: duplicate key {key:?} for sample ordinal {ordinal}", schema::SCORES)));
        }
    }
    Ok(())
}

/// Aggregates in ordinal order, `expected` of them.
pub fn decode_aggregates(bytes: &[u8], expected: u64) -> Result<Vec<pb::ScoreAggregate>, AssembleError> {
    let mut d = Decoder::new(bytes);
    let mut out = Vec::new();
    while !d.is_done() {
        let ordinal = d.u64()?;
        let name = d.string()?;
        let point_index = d.u32()?;
        let draws = d.u32()?;
        let mean = d.f64()?;
        let std_dev = d.f64()?;
        let min = d.f64()?;
        let max = d.f64()?;
        let pass_fraction = d.nullable_f64()?;
        if ordinal != out.len() as u64 {
            return Err(AssembleError::Inconsistent(format!("aggregate ordinals are not 0..n in order: row {} carries ordinal {ordinal}", out.len())));
        }
        out.push(pb::ScoreAggregate { name, point_index, draws, mean, std_dev, min, max, pass_fraction });
    }
    if out.len() as u64 != expected {
        return Err(AssembleError::Inconsistent(format!("the header says {expected} aggregate(s) but {} aggregate row(s) were read", out.len())));
    }
    Ok(out)
}

pub fn decode_provenance_attributes(bytes: &[u8]) -> Result<BTreeMap<String, String>, AssembleError> {
    let mut d = Decoder::new(bytes);
    let mut out = BTreeMap::new();
    while !d.is_done() {
        let key = d.string()?;
        let value = d.string()?;
        if out.insert(key.clone(), value).is_some() {
            return Err(AssembleError::Inconsistent(format!("{}: duplicate key {key:?}", schema::PROVENANCE_ATTRIBUTES)));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::rowbinary::row_len_by_types;
    use super::*;

    /// A message with **every** field set to a non-default value, including the hard floats.
    pub(crate) fn every_field_study() -> pb::SweepResults {
        let mut s0 = pb::SweepSample {
            point_index: 7,
            draw_index: 3,
            axis_values: BTreeMap::from([("inst.param".to_string(), -0.0), ("event:e1.v".to_string(), f64::from_bits(1))]),
            run_id: "run-0".to_string(),
            config_hash: "cfg-0".to_string(),
            scores: BTreeMap::new(),
            products_uri: "/tmp/p0".to_string(),
            error: String::new(),
            seeds: BTreeMap::from([("a".to_string(), u64::MAX), ("b".to_string(), 1)]),
        };
        s0.scores.insert("m1".to_string(), pb::ScoreResult { name: "display name 1".to_string(), value: f64::NAN, unit: pb::Unit::Meter as i32, passed: Some(true) });
        s0.scores.insert("m2".to_string(), pb::ScoreResult { name: "m2".to_string(), value: f64::NEG_INFINITY, unit: 12345, passed: Some(false) });
        s0.scores.insert("m3".to_string(), pb::ScoreResult { name: String::new(), value: -0.0, unit: 0, passed: None });
        let s1 = pb::SweepSample { point_index: 0, draw_index: 0, error: "it failed".to_string(), ..Default::default() };
        pb::SweepResults {
            sweep_id: "every-field".to_string(),
            sweep_hash: "sh".to_string(),
            drm_hash: "dh".to_string(),
            samples: vec![s0, s1],
            aggregates: vec![
                pb::ScoreAggregate { name: "m1".to_string(), point_index: 7, draws: 4, mean: f64::INFINITY, std_dev: 0.5, min: -1.0, max: 2.0, pass_fraction: Some(0.25) },
                pb::ScoreAggregate { name: "m3".to_string(), point_index: 0, draws: 1, mean: -0.0, std_dev: -0.0, min: f64::from_bits(1), max: f64::MAX, pass_fraction: None },
            ],
            provenance: Some(pb::Provenance {
                author_kind: pb::AuthorKind::Service as i32,
                principal: "p".to_string(),
                tool: "t".to_string(),
                config_hash: "c".to_string(),
                data_pack_hash: "dp".to_string(),
                dataset_hash: "ds".to_string(),
                created_tai_ns: i64::MIN,
                run_id: "r".to_string(),
                attributes: BTreeMap::from([("k1".to_string(), "v1".to_string()), ("k0".to_string(), "v0".to_string())]),
            }),
        }
    }

    fn rows_of(chunks: &[Vec<u8>], types: &[&str]) -> usize {
        let mut n = 0;
        for chunk in chunks {
            let mut at = 0;
            while at < chunk.len() {
                at += row_len_by_types(types, &chunk[at..]).unwrap_or_else(|e| panic!("row {n} does not walk as {types:?}: {e}"));
                n += 1;
            }
            assert_eq!(at, chunk.len(), "a chunk must end on a row boundary");
        }
        n
    }

    #[test]
    fn every_encoded_row_walks_as_the_tables_declared_types() {
        let enc = encode_study(&every_field_study(), "wid", "sha");
        assert_eq!(rows_of(&enc.samples, &schema::table(schema::SAMPLES).insert_types()), 2);
        assert_eq!(rows_of(&enc.axis_values, &schema::table(schema::AXIS_VALUES).insert_types()), 2);
        assert_eq!(rows_of(&enc.seeds, &schema::table(schema::SEEDS).insert_types()), 2);
        assert_eq!(rows_of(&enc.scores, &schema::table(schema::SCORES).insert_types()), 3);
        assert_eq!(rows_of(&enc.aggregates, &schema::table(schema::AGGREGATES).insert_types()), 2);
        assert_eq!(rows_of(&enc.provenance_attributes, &schema::table(schema::PROVENANCE_ATTRIBUTES).insert_types()), 2);
        assert_eq!(rows_of(std::slice::from_ref(&enc.header), &schema::table(schema::STUDIES).insert_types()), 1);
    }

    /// Cuts the two leading strings (`sweep_id`, `write_id`) off every row of an insert body,
    /// leaving the columns the reader's `SELECT` returns. `types` are the remaining columns.
    fn drop_leading_strings(chunks: &[Vec<u8>], types: &[&str]) -> Vec<u8> {
        let rows = chunks.concat();
        let mut out = Vec::new();
        let mut at = 0;
        while at < rows.len() {
            let mut d = Decoder::new(&rows[at..]);
            d.string().unwrap();
            d.string().unwrap();
            let skip = d.position();
            let n = row_len_by_types(types, &rows[at + skip..]).unwrap();
            out.extend_from_slice(&rows[at + skip..at + skip + n]);
            at += skip + n;
        }
        out
    }

    #[test]
    fn rows_assemble_back_into_the_same_study_bit_for_bit() {
        use prost::Message;
        let study = every_field_study();
        let enc = encode_study(&study, "wid", "sha");
        let after_keys = |name: &str| schema::table(name).insert_types()[2..].to_vec();

        // The header row starts with sweep_id; the reader's SELECT (HEADER_SELECT) starts at write_id.
        let mut d = Decoder::new(&enc.header);
        assert_eq!(d.string().unwrap(), "every-field");
        let header = decode_header(&mut d).unwrap();
        d.finish().unwrap();
        assert_eq!(header.write_id, "wid");
        assert_eq!(header.sample_count, 2);
        assert_eq!(header.aggregate_count, 2);
        assert_eq!(header.content_sha256, "sha");

        let mut samples = decode_samples(&drop_leading_strings(&enc.samples, &after_keys(schema::SAMPLES)), header.sample_count).unwrap();
        decode_axis_values(&drop_leading_strings(&enc.axis_values, &after_keys(schema::AXIS_VALUES)), &mut samples).unwrap();
        decode_seeds(&drop_leading_strings(&enc.seeds, &after_keys(schema::SEEDS)), &mut samples).unwrap();
        decode_scores(&drop_leading_strings(&enc.scores, &after_keys(schema::SCORES)), &mut samples).unwrap();
        let aggregates = decode_aggregates(&drop_leading_strings(&enc.aggregates, &after_keys(schema::AGGREGATES)), header.aggregate_count).unwrap();
        let mut provenance = header.provenance.clone().unwrap();
        provenance.attributes = decode_provenance_attributes(&drop_leading_strings(&enc.provenance_attributes, &after_keys(schema::PROVENANCE_ATTRIBUTES))).unwrap();

        let rebuilt = pb::SweepResults { sweep_id: "every-field".to_string(), sweep_hash: header.sweep_hash, drm_hash: header.drm_hash, samples, aggregates, provenance: Some(provenance) };
        assert_eq!(rebuilt.encode_to_vec(), study.encode_to_vec(), "the canonical encoding (bit-exact floats, NaN payloads included) must match");
    }

    #[test]
    fn a_study_with_no_provenance_keeps_none_distinct_from_an_all_default_provenance() {
        let none = pb::SweepResults { sweep_id: "s".to_string(), ..Default::default() };
        let some_default = pb::SweepResults { sweep_id: "s".to_string(), provenance: Some(pb::Provenance::default()), ..Default::default() };
        let header_of = |r: &pb::SweepResults| {
            let enc = encode_study(r, "w", "h");
            let mut d = Decoder::new(&enc.header);
            d.string().unwrap();
            decode_header(&mut d).unwrap()
        };
        assert_eq!(header_of(&none).provenance, None);
        assert_eq!(header_of(&some_default).provenance, Some(pb::Provenance::default()));
    }

    #[test]
    fn a_gap_a_duplicate_or_a_wrong_count_in_the_rows_is_inconsistent_not_a_shorter_study() {
        let enc = encode_study(&every_field_study(), "wid", "sha");
        let after_keys = |name: &str| schema::table(name).insert_types()[2..].to_vec();
        let samples_body = drop_leading_strings(&enc.samples, &after_keys(schema::SAMPLES));
        assert!(matches!(decode_samples(&samples_body, 3), Err(AssembleError::Inconsistent(_))), "header promises 3 samples, rows hold 2");
        // Keep only the second row: its ordinal is 1, not 0.
        let first_len = {
            let mut d = Decoder::new(&samples_body);
            d.u64().unwrap();
            d.u32().unwrap();
            d.u32().unwrap();
            for _ in 0..4 {
                d.string().unwrap();
            }
            d.position()
        };
        assert!(matches!(decode_samples(&samples_body[first_len..], 1), Err(AssembleError::Inconsistent(_))));
        // A score row for a sample the header never counted.
        let mut samples = decode_samples(&samples_body, 2).unwrap();
        let mut stray = Encoder::new();
        stray.u64(9);
        stray.string("k");
        stray.string("n");
        stray.f64(1.0);
        stray.i32(0);
        stray.nullable_bool(None);
        assert!(matches!(decode_scores(&stray.finish(), &mut samples), Err(AssembleError::Inconsistent(_))));
        // A truncated body is a decode error.
        let scores = drop_leading_strings(&enc.scores, &after_keys(schema::SCORES));
        assert!(matches!(decode_scores(&scores[..scores.len() - 1], &mut samples), Err(AssembleError::Decode(RowBinaryError::Truncated { .. }))));
    }

    #[test]
    fn a_small_chunk_limit_cuts_every_table_at_row_boundaries_without_losing_a_row() {
        let study = every_field_study();
        let whole = encode_study(&study, "wid", "sha");
        let cut = encode_study_chunked(&study, "wid", "sha", 40);
        type Pair<'a> = (&'a Vec<Vec<u8>>, &'a Vec<Vec<u8>>, &'a str);
        let layouts: [Pair<'_>; 6] = [
            (&whole.samples, &cut.samples, schema::SAMPLES),
            (&whole.axis_values, &cut.axis_values, schema::AXIS_VALUES),
            (&whole.seeds, &cut.seeds, schema::SEEDS),
            (&whole.scores, &cut.scores, schema::SCORES),
            (&whole.aggregates, &cut.aggregates, schema::AGGREGATES),
            (&whole.provenance_attributes, &cut.provenance_attributes, schema::PROVENANCE_ATTRIBUTES),
        ];
        let mut any_split = false;
        for (whole_chunks, cut_chunks, table) in layouts {
            let types = schema::table(table).insert_types();
            assert_eq!(rows_of(cut_chunks, &types), rows_of(whole_chunks, &types), "{table}: same row count");
            assert_eq!(cut_chunks.concat(), whole_chunks.concat(), "{table}: same bytes, only cut differently");
            any_split |= cut_chunks.len() > 1;
        }
        assert!(any_split, "a 40-byte limit must actually split something");
    }
}
