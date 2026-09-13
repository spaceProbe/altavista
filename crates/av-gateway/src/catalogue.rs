//! D1: "A caller names a run by identity, never a path." Mirrors `altavista/server.py`'s
//! `POST /api/cdm/sweep/sample` handler exactly (that route's own docstring: "The caller
//! sends an identity, never a path... This handler resolves that identity against the
//! server's OWN state... never a string the HTTP caller supplied"): [`RunCatalogue`] is
//! this gateway's own configured catalogue of run products, and [`RunCatalogue::resolve`]
//! is the ONLY way a query ever reaches a [`RunProducts`] byte -- there is no code path
//! anywhere in this crate that opens a caller-supplied path or URI. A caller-supplied path
//! ([`GatewayQueryRequest::caller_supplied_products_uri`]) is refused before this module is
//! even consulted (see [`crate::gateway::GatewayCore::query`]).
//!
//! ## Identity (D1)
//!
//! The plan's "run hash" names no field literally called `run_hash` anywhere in this
//! workspace. This crate keys its catalogue on [`RunIdentity::run_id`] (`RunProducts.
//! run_id`, unique per catalogue entry -- the natural primary key `RunProducts` already
//! carries) and treats [`RunIdentity::config_hash`] as an OPTIONAL integrity cross-check
//! against the catalogue entry's own `RunProducts.provenance.config_hash`: empty means "not
//! supplied, skip the check"; non-empty and equal proceeds; non-empty and unequal is its
//! own typed, counted refusal ([`ResolveError::ConfigHashMismatch`]) -- config_hash is
//! **never** itself a lookup key, so a caller cannot resolve a run by guessing a hash
//! without also naming its real `run_id`.
//!
//! ## Ordered, typed refusals (D1), mirroring the Python route's own ordered chain
//!
//! `test_caller_supplied_products_uri_is_never_trusted` and the route's own docstring name
//! this exact order for the identity-resolution portion: malformed request -> unknown run
//! -> product missing on this host -> undecodable bytes. [`RunCatalogue::resolve`] performs
//! the first three (malformed request is the caller's job -- see [`crate::gateway]`); this
//! module raises [`ResolveError::UnknownRun`] then decodes lazily and raises
//! [`ResolveError::UndecodableBytes`] last, exactly mirroring the Python route reading
//! `run_products.pb` off disk and refusing a body that does not decode.
//!
//! ## Why bytes are stored, not a pre-decoded [`RunProducts`]
//!
//! [`CatalogueEntry::encoded_run_products`] is `RunProducts::encode_to_vec()`'s own output,
//! decoded lazily on every [`RunCatalogue::resolve`] call, rather than a `RunProducts`
//! decoded once at load time and cached. This is deliberate, not an efficiency accident:
//! it is what makes "undecodable bytes" a REAL, reachable refusal for this crate's own test
//! suite to exercise (a catalogue entry built with deliberately corrupt bytes), the same
//! way the Python route's refusal is only reachable because it, too, decodes from raw bytes
//! on every request rather than trusting an in-memory cache it never re-validates.

use std::collections::BTreeMap;

use av_cdm::pb::{Label, RunIdentity, RunProducts};
use prost::Message as _;

use crate::counters::Counted;

/// One run's products as this gateway's deployment configured it: the raw, `prost`-encoded
/// `RunProducts` bytes (decoded lazily -- see the module doc), plus the [`Label`] this
/// deployment assigns to the run (D2) -- not read off `RunProducts` itself, which carries
/// no `Label` of its own (`Trajectory`/`Event` do; `ScoreResult`/`Measurement` do not, so
/// the catalogue's own configured label is the one enforcement point common to every
/// selector).
#[derive(Debug, Clone)]
pub struct CatalogueEntry {
    pub label: Label,
    pub encoded_run_products: Vec<u8>,
}

impl CatalogueEntry {
    /// Builds an entry from a real `RunProducts` value, encoding it once with `prost`'s own
    /// deterministic encoding (ADR-004/ADR-001: fixed field order, every generated
    /// `map<..>` a `BTreeMap`) -- the normal construction path for a real catalogue.
    pub fn from_run_products(label: Label, run_products: &RunProducts) -> Self {
        Self { label, encoded_run_products: run_products.encode_to_vec() }
    }

    /// Builds an entry from raw, possibly-corrupt bytes -- this crate's own test suite's
    /// only use, to make [`ResolveError::UndecodableBytes`] a reachable refusal (see the
    /// module doc's "Why bytes are stored" section) rather than a code path nothing ever
    /// exercises.
    pub fn from_raw_bytes(label: Label, encoded_run_products: Vec<u8>) -> Self {
        Self { label, encoded_run_products }
    }
}

/// This gateway's own configured catalogue of run products (D1), keyed by
/// [`RunIdentity::run_id`]. `BTreeMap`, not `HashMap` (ADR-004's determinism rule).
#[derive(Debug, Default)]
pub struct RunCatalogue {
    entries: BTreeMap<String, CatalogueEntry>,
}

/// Every way [`RunCatalogue::resolve`] can refuse a request -- see the module doc's
/// "Ordered, typed refusals" section for the order these are meant to be checked in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// `run_id` names no entry in this gateway's catalogue.
    UnknownRun { run_id: String },
    /// `run_id` names a real entry, but the caller's supplied `config_hash` does not match
    /// that entry's own `RunProducts.provenance.config_hash`.
    ConfigHashMismatch { run_id: String, supplied: String, actual: String },
    /// The entry's stored bytes do not decode as an `altavista.v1.RunProducts`.
    UndecodableBytes { run_id: String, detail: String },
}

impl Counted for ResolveError {
    fn code(&self) -> &'static str {
        match self {
            ResolveError::UnknownRun { .. } => "catalogue_unknown_run",
            ResolveError::ConfigHashMismatch { .. } => "catalogue_config_hash_mismatch",
            ResolveError::UndecodableBytes { .. } => "catalogue_undecodable_bytes",
        }
    }
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::UnknownRun { run_id } => write!(f, "no run named {run_id:?} in this gateway's own configured catalogue"),
            ResolveError::ConfigHashMismatch { run_id, supplied, actual } => {
                write!(f, "run {run_id:?}'s catalogue entry has config_hash {actual:?}, but the caller supplied {supplied:?}")
            }
            ResolveError::UndecodableBytes { run_id, detail } => {
                write!(f, "run {run_id:?}'s catalogue entry does not decode as an altavista.v1.RunProducts: {detail}")
            }
        }
    }
}

/// A successfully resolved run: its real `RunProducts` (decoded fresh from the catalogue's
/// stored bytes) and the label this deployment configured for it.
#[derive(Debug)]
pub struct ResolvedRun {
    pub run_products: RunProducts,
    pub label: Label,
}

impl RunCatalogue {
    pub fn new(entries: BTreeMap<String, CatalogueEntry>) -> Self {
        Self { entries }
    }

    /// D1's one and only identity-resolution entry point. Never reads a caller-supplied
    /// path or URI -- `identity.run_id`/`identity.config_hash` are the sole inputs, matched
    /// against this catalogue's own stored entries. See the module doc for the exact
    /// ordered refusal chain.
    pub fn resolve(&self, identity: &RunIdentity) -> Result<ResolvedRun, ResolveError> {
        let Some(entry) = self.entries.get(&identity.run_id) else {
            return Err(ResolveError::UnknownRun { run_id: identity.run_id.clone() });
        };
        let run_products = RunProducts::decode(entry.encoded_run_products.as_slice())
            .map_err(|e| ResolveError::UndecodableBytes { run_id: identity.run_id.clone(), detail: e.to_string() })?;
        if !identity.config_hash.is_empty() && identity.config_hash != run_products.provenance.as_ref().map(|p| p.config_hash.as_str()).unwrap_or("") {
            return Err(ResolveError::ConfigHashMismatch {
                run_id: identity.run_id.clone(),
                supplied: identity.config_hash.clone(),
                actual: run_products.provenance.as_ref().map(|p| p.config_hash.clone()).unwrap_or_default(),
            });
        }
        Ok(ResolvedRun { run_products, label: entry.label.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::Provenance;

    fn sample_run_products(run_id: &str, config_hash: &str) -> RunProducts {
        RunProducts {
            run_id: run_id.to_string(),
            provenance: Some(Provenance { config_hash: config_hash.to_string(), ..Default::default() }),
            ..Default::default()
        }
    }

    fn label(marking: &str) -> Label {
        Label { marking: marking.to_string(), caveats: vec![] }
    }

    fn catalogue_with_one_entry() -> RunCatalogue {
        let rp = sample_run_products("run-a", "hash-a");
        let mut entries = BTreeMap::new();
        entries.insert("run-a".to_string(), CatalogueEntry::from_run_products(label("CUI"), &rp));
        RunCatalogue::new(entries)
    }

    #[test]
    fn resolve_finds_a_known_run_by_run_id_alone() {
        let cat = catalogue_with_one_entry();
        let resolved = cat.resolve(&RunIdentity { run_id: "run-a".to_string(), config_hash: String::new() }).unwrap();
        assert_eq!(resolved.run_products.run_id, "run-a");
        assert_eq!(resolved.label.marking, "CUI");
    }

    #[test]
    fn resolve_accepts_a_matching_config_hash_cross_check() {
        let cat = catalogue_with_one_entry();
        let resolved = cat.resolve(&RunIdentity { run_id: "run-a".to_string(), config_hash: "hash-a".to_string() }).unwrap();
        assert_eq!(resolved.run_products.run_id, "run-a");
    }

    #[test]
    fn resolve_refuses_an_unknown_run() {
        let cat = catalogue_with_one_entry();
        let err = cat.resolve(&RunIdentity { run_id: "run-does-not-exist".to_string(), config_hash: String::new() }).unwrap_err();
        assert_eq!(err, ResolveError::UnknownRun { run_id: "run-does-not-exist".to_string() });
    }

    #[test]
    fn resolve_refuses_a_config_hash_that_does_not_match() {
        let cat = catalogue_with_one_entry();
        let err = cat.resolve(&RunIdentity { run_id: "run-a".to_string(), config_hash: "wrong-hash".to_string() }).unwrap_err();
        assert_eq!(err, ResolveError::ConfigHashMismatch { run_id: "run-a".to_string(), supplied: "wrong-hash".to_string(), actual: "hash-a".to_string() });
    }

    #[test]
    fn resolve_refuses_undecodable_bytes() {
        let mut entries = BTreeMap::new();
        entries.insert("run-corrupt".to_string(), CatalogueEntry::from_raw_bytes(label("CUI"), vec![0xff, 0x00, 0xff, 0x00, 0xff]));
        let cat = RunCatalogue::new(entries);
        let err = cat.resolve(&RunIdentity { run_id: "run-corrupt".to_string(), config_hash: String::new() }).unwrap_err();
        assert!(matches!(err, ResolveError::UndecodableBytes { .. }), "{err:?}");
    }

    #[test]
    fn every_resolve_error_has_a_distinct_stable_code() {
        let codes: Vec<&'static str> = vec![
            ResolveError::UnknownRun { run_id: "r".to_string() }.code(),
            ResolveError::ConfigHashMismatch { run_id: "r".to_string(), supplied: "s".to_string(), actual: "a".to_string() }.code(),
            ResolveError::UndecodableBytes { run_id: "r".to_string(), detail: "d".to_string() }.code(),
        ];
        let mut sorted = codes.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "codes must be pairwise distinct: {codes:?}");
    }
}
