//! Step 1 of the request pipeline (`crate`'s own module doc, "The request path"): parses an
//! HTTP request path into a [`Route`], with no I/O and no dependency on anything else in this
//! crate. Every way a path can fail to parse is its own typed [`RouteError`] -- never a
//! generic "bad request" -- so [`crate::refusal::TileRefusal`] can count each one under its
//! own stable key (`crate::refusal`'s own module doc).
//!
//! Exactly two shapes are recognised, both under `/v1/tilesets/{manifest_sha256}/...`:
//!
//! - `GET /v1/tilesets/{manifest_sha256}/manifest`
//! - `GET /v1/tilesets/{manifest_sha256}/tiles/{level}/{x}/{y}`
//!
//! `manifest_sha256` must be exactly 64 lowercase hex characters (the same rule
//! `av_store::keys::validate_sha256_hex` enforces, restated here rather than depended on --
//! this crate needs the check before it has any object to hand `av-store`, and the check
//! itself is two lines with no crypto in it, not something worth a dependency edge for).
//! `level`/`x`/`y` must each parse as a `u32` -- exactly [`av_cdm::pb::TileEntry`]'s own
//! field type for all three.

use thiserror::Error;

/// A successfully parsed route. See this module's own doc for the two recognised shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Manifest { manifest_sha256: String },
    Tile { manifest_sha256: String, level: u32, x: u32, y: u32 },
}

/// Every way [`parse`] can refuse a path -- see [`crate::refusal::TileRefusal`] for how each
/// of these becomes a distinct, counted, `400` refusal.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RouteError {
    /// The path is not one of this module's own doc's two recognised shapes at all (wrong
    /// segment count, wrong literal segments, wrong method target, ...).
    #[error("path {path:?} does not match /v1/tilesets/{{manifest_sha256}}/manifest or /v1/tilesets/{{manifest_sha256}}/tiles/{{level}}/{{x}}/{{y}}")]
    Malformed { path: String },
    /// The path shape matched, but `manifest_sha256` is not exactly 64 lowercase hex
    /// characters.
    #[error("manifest_sha256 {manifest_sha256:?} is not exactly 64 lowercase hex characters")]
    InvalidManifestHash { manifest_sha256: String },
    /// The path shape matched a tile address, but `level`, `x`, or `y` does not parse as a
    /// `u32`.
    #[error("tile address segment {segment:?} (field {field}) does not parse as a u32")]
    InvalidTileAddress { field: &'static str, segment: String },
}

fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn parse_u32_field(field: &'static str, segment: &str) -> Result<u32, RouteError> {
    segment.parse::<u32>().map_err(|_| RouteError::InvalidTileAddress { field, segment: segment.to_string() })
}

/// Parses `path` (the raw HTTP request-target, no query string expected -- neither route
/// this crate serves ever takes one) into a [`Route`]. `path` may or may not carry a leading
/// `/`; a trailing `/` is never accepted (an empty trailing segment is not the same path as
/// without one).
pub fn parse(path: &str) -> Result<Route, RouteError> {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let malformed = || RouteError::Malformed { path: path.to_string() };

    match segments.as_slice() {
        ["v1", "tilesets", manifest_sha256, "manifest"] => {
            if !is_valid_sha256_hex(manifest_sha256) {
                return Err(RouteError::InvalidManifestHash { manifest_sha256: manifest_sha256.to_string() });
            }
            Ok(Route::Manifest { manifest_sha256: manifest_sha256.to_string() })
        }
        ["v1", "tilesets", manifest_sha256, "tiles", level, x, y] => {
            if !is_valid_sha256_hex(manifest_sha256) {
                return Err(RouteError::InvalidManifestHash { manifest_sha256: manifest_sha256.to_string() });
            }
            // Deliberately validated in (level, x, y) order, always: the same "one field
            // fails, name that field" contract this module's own doc promises, and a fixed
            // order so two runs against the same malformed path always name the same field.
            let level = parse_u32_field("level", level)?;
            let x = parse_u32_field("x", x)?;
            let y = parse_u32_field("y", y)?;
            Ok(Route::Tile { manifest_sha256: manifest_sha256.to_string(), level, x, y })
        }
        _ => Err(malformed()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn parses_a_manifest_route() {
        let path = format!("/v1/tilesets/{HASH}/manifest");
        assert_eq!(parse(&path).unwrap(), Route::Manifest { manifest_sha256: HASH.to_string() });
    }

    #[test]
    fn parses_a_tile_route() {
        let path = format!("/v1/tilesets/{HASH}/tiles/3/5/2");
        assert_eq!(parse(&path).unwrap(), Route::Tile { manifest_sha256: HASH.to_string(), level: 3, x: 5, y: 2 });
    }

    #[test]
    fn refuses_a_completely_unrelated_path_as_malformed() {
        let err = parse("/nope").unwrap_err();
        assert!(matches!(err, RouteError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_manifest_path_with_too_few_segments_as_malformed() {
        let err = parse("/v1/tilesets/manifest").unwrap_err();
        assert!(matches!(err, RouteError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_manifest_hash_that_is_too_short() {
        let err = parse("/v1/tilesets/abcd/manifest").unwrap_err();
        assert!(matches!(err, RouteError::InvalidManifestHash { .. }), "{err:?}");
    }

    #[test]
    fn refuses_uppercase_hex_in_the_manifest_hash() {
        let upper = HASH.to_uppercase();
        let err = parse(&format!("/v1/tilesets/{upper}/manifest")).unwrap_err();
        assert!(matches!(err, RouteError::InvalidManifestHash { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_non_hex_character_in_the_manifest_hash() {
        let mut bad = HASH.to_string();
        bad.replace_range(0..1, "g");
        let err = parse(&format!("/v1/tilesets/{bad}/tiles/0/0/0")).unwrap_err();
        assert!(matches!(err, RouteError::InvalidManifestHash { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_non_numeric_level() {
        let err = parse(&format!("/v1/tilesets/{HASH}/tiles/not-a-number/0/0")).unwrap_err();
        assert!(matches!(&err, RouteError::InvalidTileAddress { field, .. } if *field == "level"), "{err:?}");
    }

    #[test]
    fn refuses_a_non_numeric_x() {
        let err = parse(&format!("/v1/tilesets/{HASH}/tiles/0/not-a-number/0")).unwrap_err();
        assert!(matches!(&err, RouteError::InvalidTileAddress { field, .. } if *field == "x"), "{err:?}");
    }

    #[test]
    fn refuses_a_non_numeric_y() {
        let err = parse(&format!("/v1/tilesets/{HASH}/tiles/0/0/not-a-number")).unwrap_err();
        assert!(matches!(&err, RouteError::InvalidTileAddress { field, .. } if *field == "y"), "{err:?}");
    }

    #[test]
    fn refuses_a_negative_level_as_invalid_not_malformed() {
        // "-1" is not a valid u32 -- this is an address-validity refusal, not a route-shape
        // refusal (the path DOES have the right number of segments).
        let err = parse(&format!("/v1/tilesets/{HASH}/tiles/-1/0/0")).unwrap_err();
        assert!(matches!(err, RouteError::InvalidTileAddress { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_trailing_slash_as_malformed() {
        let err = parse(&format!("/v1/tilesets/{HASH}/manifest/")).unwrap_err();
        assert!(matches!(err, RouteError::Malformed { .. }), "{err:?}");
    }
}
