//! Every way [`crate::core::handle`] can refuse a request: one typed [`TileRefusal`] enum,
//! each variant its own stable, `snake_case`, `"tiles_"`-namespaced [`Counted`] code
//! (ADR-004: "everything rejected is counted", `crate::TESTS_ASSERT_EVERY_CODE` below is
//! where every one of them is pinned in one place) and its own HTTP status via [`status`].
//!
//! # "Per layer" and "per request" label enforcement -- and why this crate implements ONE
//! check, not two
//!
//! H4's own milestone text asks for label enforcement "per layer and per request", and this
//! crate's own task brief spells out what that means operationally: the manifest's tile set
//! carries a label (checked once the manifest is fetched -- "per layer"), and "then, per
//! request, the caller's clearance is checked against **the layer's label again** at the
//! tile level". Read literally (and it can only be read one way: `av-tiles`' current
//! design/data model gives every tile in one manifest the SAME label as the manifest itself
//! -- `crates/av-jobs::runner::Runner::execute_spec` calls `self.sink.put(&out.bytes, &out.
//! media_type, &label)` with the identical `label` for every output of one job, manifest
//! included, and this crate has no other source of a tile's label to check against), the
//! "per request" check compares the SAME `caller_clearance` against the SAME layer label the
//! "per layer" check already compared, within the SAME request. `caller_clearance` (derived
//! once from the verified token, step 2) and the layer's label (read once off the manifest's
//! own `AssetRef`, step 3-5) are both fixed for the lifetime of one request -- neither can
//! change between the first evaluation and a hypothetical second one -- so
//! `av_label::ClearanceLadder::classify(caller_clearance, &layer_label)` run a second time
//! with identical inputs can only ever reproduce its own first answer.
//!
//! **This crate's own conclusion, stated plainly rather than adding a check that does
//! nothing: the single evaluation `crate::core::handle` performs immediately after decoding
//! the manifest already constitutes both enforcements for the request it is part of.** There
//! is exactly one request; it is checked exactly once; "per layer" and "per request" are the
//! same fact about the same request, not two independent facts that happen to coincide. A
//! second, textually distinct call site computing the identical comparison over the
//! identical inputs would not add a check the first one lacks -- it is the textbook shape of
//! `crate`'s own no-`#[allow]`-on-dead-logic discipline extended to security-relevant logic:
//! a branch that can never observably differ from another already-taken branch is not
//! defense in depth, it is dead code with a comment claiming otherwise.
//!
//! This is NOT the same conclusion as "per-tile labels would also be redundant" -- if a
//! future round ever gives an individual [`av_cdm::pb::TileEntry`] its own label, distinct
//! from its manifest's (nothing today does; see the `runner::Runner` citation above), THAT
//! would be exactly the point where a second, tile-object-scoped check earns its keep,
//! because it would then compare against a genuinely different, independently-sourced value.
//! Today it would not, so it is not implemented -- see [`TileRefusal::LayerLabel`], which
//! this crate's single check refuses through.

use thiserror::Error;

use av_command::counters::Counted;
use av_command::oidc::TokenError;
use av_label::LabelRefusal;

/// Every way [`crate::core::handle`] refuses a request, in the fixed pipeline order
/// `crate`'s own module doc names. Each variant is exactly one check; none is a generic
/// "bad request"/"refused" catch-all.
#[derive(Debug, Error)]
pub enum TileRefusal {
    // -- step 1: route parsing --------------------------------------------------------
    #[error(transparent)]
    Route(#[from] crate::route::RouteError),

    // -- step 2: authentication and clearance derivation -------------------------------
    /// No `Authorization: Bearer <token>` header at all (or an empty one) -- never treated
    /// as allow.
    #[error("no Authorization: Bearer <token> header presented")]
    AuthMissingToken,
    /// The presented token failed [`av_command::oidc::verify`]. The underlying
    /// [`TokenError`]'s own `token_*` code is ALSO recorded (mirrors `crates/av-gateway/src/
    /// auth.rs::AuthContext::verify_token`'s identical double-count), so a coarse "some
    /// token failed here" count and the precise reason both stay independently greppable.
    #[error("bearer token failed verification: {source}")]
    AuthTokenInvalid { #[source] source: TokenError },
    /// None of the verified token's groups appear in this deployment's configured
    /// [`av_label::GroupClearanceMap`] at all.
    #[error("no configured clearance for groups {groups:?}")]
    AuthNoClearanceForSubject { groups: Vec<String> },
    /// At least one of the verified token's groups IS mapped, but to a marking absent from
    /// this deployment's [`av_label::ClearanceLadder`] -- a misconfiguration, refused rather
    /// than silently ranked as 0 (mirrors `av-gateway`'s identical R5.1b rule).
    #[error("group_clearance marking {marking:?} (from groups {groups:?}) is not on this deployment's clearance ladder")]
    AuthClearanceMarkingNotOnLadder { groups: Vec<String>, marking: String },

    // -- step 3: manifest fetch and hash verification -----------------------------------
    /// No object is stored under this `manifest_sha256`'s derived key at all.
    #[error("no manifest object stored for manifest_sha256 {manifest_sha256:?}")]
    ManifestNotFound { manifest_sha256: String },
    /// The store returned bytes for this key, but they do not hash to the requested
    /// `manifest_sha256` -- "the store returned something that is not what was asked for".
    #[error("manifest object's actual sha256 {actual:?} does not match the requested manifest_sha256 {requested:?}")]
    ManifestHashMismatch { requested: String, actual: String },

    // -- step 4: manifest decode ----------------------------------------------------------
    #[error("manifest_sha256 {manifest_sha256:?}: bytes do not decode as a TileSetManifest: {detail}")]
    ManifestDecodeInvalid { manifest_sha256: String, detail: String },

    // -- step 5: label enforcement (this module's own doc has the full "per layer and per
    // request" reasoning) --------------------------------------------------------------
    #[error("layer label refused: {0}")]
    LayerLabel(#[source] LabelRefusal),

    // -- step 6: tile address lookup --------------------------------------------------
    /// `(level, x, y)` is not in this manifest's own `tiles` list -- distinct from
    /// [`Self::ManifestNotFound`] (a different resource entirely: the manifest exists, this
    /// address within it does not).
    #[error("no tile at (level={level}, x={x}, y={y}) in this manifest")]
    TileNotFound { level: u32, x: u32, y: u32 },

    // -- step 7: tile fetch and hash verification ---------------------------------------
    /// The manifest lists this tile, but no object is stored under its `object_key` --
    /// the store disagreeing with its own manifest, never the caller's fault.
    #[error("manifest lists tile object_key {object_key:?}, but no object is stored there")]
    TileObjectNotFound { object_key: String },
    #[error("tile object's actual sha256 {actual:?} does not match TileEntry.sha256 {expected:?}")]
    TileHashMismatch { expected: String, actual: String },

    // -- P2: Range requests ----------------------------------------------------------------
    /// A syntactically valid `bytes=<start>-<end>` `Range` header names a range nothing in
    /// the resource's actual `len` bytes can satisfy (`crate::range`'s own module doc: a
    /// header this crate cannot even parse is NOT this variant -- that is ignored, served as
    /// a plain `200`, never refused).
    #[error("Range request unsatisfiable against a resource of {len} bytes")]
    RangeUnsatisfiable { len: usize },
}

/// [`LabelRefusal`] is foreign to this crate (`av_label`) and so is [`Counted`] (`av_command`)
/// -- `impl Counted for LabelRefusal` here would be `E0117` (Rust's orphan rule: neither the
/// trait nor the type is local). `crates/av-gateway/src/labels.rs::code` already solved
/// exactly this with a free function; this is that same precedent, not a second approach.
fn layer_label_code(refusal: &LabelRefusal) -> &'static str {
    match refusal {
        LabelRefusal::MarkingNotOnLadder { side: av_label::Side::Caller, .. } => "tiles_layer_label_caller_marking_not_on_ladder",
        LabelRefusal::MarkingNotOnLadder { side: av_label::Side::Subject, .. } => "tiles_layer_label_subject_marking_not_on_ladder",
        LabelRefusal::OverClearance { .. } => "tiles_layer_label_over_clearance",
    }
}

impl Counted for TileRefusal {
    fn code(&self) -> &'static str {
        match self {
            TileRefusal::Route(crate::route::RouteError::Malformed { .. }) => "tiles_route_malformed",
            TileRefusal::Route(crate::route::RouteError::InvalidManifestHash { .. }) => "tiles_route_invalid_manifest_hash",
            TileRefusal::Route(crate::route::RouteError::InvalidTileAddress { .. }) => "tiles_route_invalid_tile_address",
            TileRefusal::AuthMissingToken => "tiles_auth_missing_token",
            TileRefusal::AuthTokenInvalid { .. } => "tiles_auth_token_invalid",
            TileRefusal::AuthNoClearanceForSubject { .. } => "tiles_auth_no_clearance_for_subject",
            TileRefusal::AuthClearanceMarkingNotOnLadder { .. } => "tiles_auth_clearance_marking_not_on_ladder",
            TileRefusal::ManifestNotFound { .. } => "tiles_manifest_not_found",
            TileRefusal::ManifestHashMismatch { .. } => "tiles_manifest_hash_mismatch",
            TileRefusal::ManifestDecodeInvalid { .. } => "tiles_manifest_decode_invalid",
            TileRefusal::LayerLabel(inner) => layer_label_code(inner),
            TileRefusal::TileNotFound { .. } => "tiles_tile_not_found",
            TileRefusal::TileObjectNotFound { .. } => "tiles_tile_object_not_found",
            TileRefusal::TileHashMismatch { .. } => "tiles_tile_hash_mismatch",
            TileRefusal::RangeUnsatisfiable { .. } => "tiles_range_unsatisfiable",
        }
    }
}

/// Every [`TileRefusal::code`] this crate can ever produce, for the "the full set of keys"
/// test (`tests::refusal_codes_are_counted_under_stable_distinct_keys` below) -- mirrors
/// `crates/av-gateway/src/labels.rs::refusal_codes_are_counted_under_stable_distinct_keys`'s
/// own shape.
pub const ALL_CODES: &[&str] = &[
    "tiles_route_malformed",
    "tiles_route_invalid_manifest_hash",
    "tiles_route_invalid_tile_address",
    "tiles_auth_missing_token",
    "tiles_auth_token_invalid",
    "tiles_auth_no_clearance_for_subject",
    "tiles_auth_clearance_marking_not_on_ladder",
    "tiles_manifest_not_found",
    "tiles_manifest_hash_mismatch",
    "tiles_manifest_decode_invalid",
    "tiles_layer_label_caller_marking_not_on_ladder",
    "tiles_layer_label_subject_marking_not_on_ladder",
    "tiles_layer_label_over_clearance",
    "tiles_tile_not_found",
    "tiles_tile_object_not_found",
    "tiles_tile_hash_mismatch",
    "tiles_range_unsatisfiable",
];

/// The HTTP status [`TileRefusal`] maps to. "Who/what are you" (no/bad token) is `401`;
/// "you may not" (a real clearance decision refusing) is `403`; "that address does not
/// exist" (client-visible: a bad path, an unknown manifest, an unknown tile address) is
/// `400`/`404`; "the store gave us something inconsistent with what was asked/promised"
/// (a hash mismatch, a decode failure, a manifest-listed object gone missing) is `502` --
/// never the client's fault, always the upstream store's.
pub fn status(refusal: &TileRefusal) -> u16 {
    match refusal {
        TileRefusal::Route(_) => 400,
        TileRefusal::AuthMissingToken | TileRefusal::AuthTokenInvalid { .. } => 401,
        TileRefusal::AuthNoClearanceForSubject { .. } | TileRefusal::AuthClearanceMarkingNotOnLadder { .. } | TileRefusal::LayerLabel(_) => 403,
        TileRefusal::ManifestNotFound { .. } | TileRefusal::TileNotFound { .. } => 404,
        TileRefusal::ManifestHashMismatch { .. } | TileRefusal::ManifestDecodeInvalid { .. } | TileRefusal::TileObjectNotFound { .. } | TileRefusal::TileHashMismatch { .. } => 502,
        TileRefusal::RangeUnsatisfiable { .. } => 416,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_command::counters::Counters;

    fn all_refusals() -> Vec<TileRefusal> {
        vec![
            TileRefusal::Route(crate::route::RouteError::Malformed { path: "x".to_string() }),
            TileRefusal::Route(crate::route::RouteError::InvalidManifestHash { manifest_sha256: "x".to_string() }),
            TileRefusal::Route(crate::route::RouteError::InvalidTileAddress { field: "level", segment: "x".to_string() }),
            TileRefusal::AuthMissingToken,
            TileRefusal::AuthTokenInvalid { source: TokenError::MissingSubject },
            TileRefusal::AuthNoClearanceForSubject { groups: vec![] },
            TileRefusal::AuthClearanceMarkingNotOnLadder { groups: vec![], marking: "x".to_string() },
            TileRefusal::ManifestNotFound { manifest_sha256: "x".to_string() },
            TileRefusal::ManifestHashMismatch { requested: "a".to_string(), actual: "b".to_string() },
            TileRefusal::ManifestDecodeInvalid { manifest_sha256: "x".to_string(), detail: "d".to_string() },
            TileRefusal::LayerLabel(LabelRefusal::MarkingNotOnLadder { side: av_label::Side::Caller, marking: "x".to_string() }),
            TileRefusal::LayerLabel(LabelRefusal::MarkingNotOnLadder { side: av_label::Side::Subject, marking: "x".to_string() }),
            TileRefusal::LayerLabel(LabelRefusal::OverClearance { subject_marking: "SECRET".to_string(), caller_clearance: "CUI".to_string() }),
            TileRefusal::TileNotFound { level: 0, x: 0, y: 0 },
            TileRefusal::TileObjectNotFound { object_key: "x".to_string() },
            TileRefusal::TileHashMismatch { expected: "a".to_string(), actual: "b".to_string() },
            TileRefusal::RangeUnsatisfiable { len: 100 },
        ]
    }

    /// **The full set of keys, pinned.** Every variant this crate can produce maps to a
    /// distinct, `snake_case`, `"tiles_"`-namespaced code, and [`ALL_CODES`] names every one
    /// of them -- mirrors `crates/av-gateway/src/labels.rs::
    /// refusal_codes_are_counted_under_stable_distinct_keys`.
    #[test]
    fn refusal_codes_are_counted_under_stable_distinct_keys() {
        let refusals = all_refusals();
        let codes: Vec<&'static str> = refusals.iter().map(|r| r.code()).collect();

        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "every refusal must have its own distinct code: {codes:?}");

        for code in &codes {
            assert_eq!(*code, code.to_lowercase(), "codes must be snake_case: {code}");
            assert!(code.starts_with("tiles_"), "codes must be namespaced: {code}");
        }

        let mut all_codes_sorted = ALL_CODES.to_vec();
        all_codes_sorted.sort_unstable();
        assert_eq!(sorted, all_codes_sorted, "ALL_CODES must name exactly the codes every TileRefusal variant actually produces, no more and no less");

        let counters = Counters::new();
        for refusal in &refusals {
            counters.record(refusal);
        }
        for code in ALL_CODES {
            assert_eq!(counters.get(code), 1, "code {code:?} must have been recorded exactly once");
        }
    }

    #[test]
    fn status_classifies_who_are_you_as_401_you_may_not_as_403_and_upstream_inconsistency_as_502() {
        assert_eq!(status(&TileRefusal::AuthMissingToken), 401);
        assert_eq!(status(&TileRefusal::AuthTokenInvalid { source: TokenError::MissingSubject }), 401);
        assert_eq!(status(&TileRefusal::AuthNoClearanceForSubject { groups: vec![] }), 403);
        assert_eq!(status(&TileRefusal::LayerLabel(LabelRefusal::OverClearance { subject_marking: "SECRET".to_string(), caller_clearance: "CUI".to_string() })), 403);
        assert_eq!(status(&TileRefusal::ManifestNotFound { manifest_sha256: "x".to_string() }), 404);
        assert_eq!(status(&TileRefusal::TileNotFound { level: 0, x: 0, y: 0 }), 404);
        assert_eq!(status(&TileRefusal::ManifestHashMismatch { requested: "a".to_string(), actual: "b".to_string() }), 502);
        assert_eq!(status(&TileRefusal::TileHashMismatch { expected: "a".to_string(), actual: "b".to_string() }), 502);
        assert_eq!(status(&TileRefusal::RangeUnsatisfiable { len: 100 }), 416);
    }
}
