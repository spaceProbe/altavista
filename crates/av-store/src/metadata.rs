//! Encoding an object's [`Label`] and [`Provenance`] into S3 user-metadata headers, and
//! decoding them back.
//!
//! - `x-amz-meta-av-sha256` -- the content hash, hex. Redundant with the object key itself
//!   (`crate::keys::object_key` already shards by this same hash) *deliberately*: an object
//!   reached by any other route (a lifecycle-rule copy, a cross-region replica, a bucket
//!   listed and fetched by a tool that never saw the original `AssetRef`) still carries its
//!   own hash where [`crate::client::StoreClient::head`] can rebuild an `AssetRef` from it
//!   with no other input.
//! - `x-amz-meta-av-marking` -- `Label.marking`, plain ASCII, specifically so a human running
//!   `mc stat` (MinIO's CLI) against the object sees the handling marking directly in the
//!   metadata listing, without having to base64-decode and `prost`-decode
//!   `x-amz-meta-av-label` first.
//! - `x-amz-meta-av-label` -- standard-alphabet, padded base64 of the `prost`-encoded
//!   [`Label`]; `x-amz-meta-av-provenance` -- the same, for [`Provenance`].
//! - `x-amz-meta-av-media-type` -- the IANA (or platform) media type.
//!
//! **Why protobuf-in-base64 rather than one header per field.** `Label.caveats` is a
//! `repeated string` and `Provenance.attributes` is a `map<string, string>` -- either would
//! need its own bespoke escaping scheme to survive as a single HTTP header value (S3 user
//! metadata is one string per header; there is no repeated-header convention MinIO honours
//! for this), and a bespoke escaping scheme is exactly the kind of thing this platform's own
//! `crates/av-command/src/ledger.rs` module doc warns against elsewhere ("a bespoke
//! escaping of caveat lists"). Base64-encoding the exact `prost` wire encoding instead gives
//! an exact round trip of a *versioned* schema for free: a future field added to `Label` or
//! `Provenance` is carried by an object written with an older `av-store` and read back by a
//! newer one (or vice versa, for an additive change) with zero change to this module, because
//! `prost`'s own wire format already handles that -- a per-field header scheme would need a
//! new header, and a decoder that does not yet know about it, every time the message grows.
//!
//! Every header value is checked against S3's per-object user-metadata budget (2 KiB) before
//! it is ever sent -- **both per header and summed across every header** (H1b's review
//! finding: this module used to check only per header, which its own doc here called "the
//! conservative reading of a budget S3 actually shares across the sum of every
//! `x-amz-meta-*` header on one object" -- conservative for a single oversized value, but not
//! actually enforcing the shared budget at all when several headers are each individually
//! under it yet collectively over it. H1a's own brief already asked for the summed check; this
//! module now does both: [`encode`] first refuses any SINGLE header value already over
//! [`USER_METADATA_BUDGET_BYTES`] (cheaper, more specific -- names the one oversized header,
//! most likely `x-amz-meta-av-provenance` if `Provenance.attributes` grows large), then refuses
//! if the SUM of every header's name+value bytes is over the same budget, with the measured
//! total in [`crate::error::StoreError::UserMetadataBudgetExceeded`].

use std::collections::BTreeMap;

use av_cdm::pb::{Label, Provenance};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use prost::Message;

use crate::error::StoreError;

pub const HEADER_SHA256: &str = "x-amz-meta-av-sha256";
pub const HEADER_MARKING: &str = "x-amz-meta-av-marking";
pub const HEADER_LABEL: &str = "x-amz-meta-av-label";
pub const HEADER_PROVENANCE: &str = "x-amz-meta-av-provenance";
pub const HEADER_MEDIA_TYPE: &str = "x-amz-meta-av-media-type";

/// S3's per-object user-metadata budget (2 KiB) -- see this module's own doc for why this
/// crate checks it per header rather than summed across headers.
pub const USER_METADATA_BUDGET_BYTES: usize = 2048;

/// The five headers [`encode`]/[`decode`] carry, as a Rust struct rather than five loose
/// values -- exactly what [`crate::client::StoreClient::head`] rebuilds an `AssetRef` from.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectMetadata {
    pub sha256_hex: String,
    pub label: Label,
    pub provenance: Provenance,
    pub media_type: String,
}

/// Encodes `meta` into the five `x-amz-meta-av-*` header values, refusing a marking with a
/// non-ASCII/non-printable byte or any value over [`USER_METADATA_BUDGET_BYTES`] (with the
/// measured size in the error).
pub fn encode(meta: &ObjectMetadata) -> Result<BTreeMap<String, String>, StoreError> {
    if !meta.label.marking.bytes().all(|b| b.is_ascii_graphic() || b == b' ') {
        return Err(StoreError::InvalidMarking { marking: meta.label.marking.clone() });
    }

    let mut headers = BTreeMap::new();
    headers.insert(HEADER_SHA256.to_string(), meta.sha256_hex.clone());
    headers.insert(HEADER_MARKING.to_string(), meta.label.marking.clone());
    headers.insert(HEADER_LABEL.to_string(), STANDARD.encode(meta.label.encode_to_vec()));
    headers.insert(HEADER_PROVENANCE.to_string(), STANDARD.encode(meta.provenance.encode_to_vec()));
    headers.insert(HEADER_MEDIA_TYPE.to_string(), meta.media_type.clone());

    let mut total_bytes = 0usize;
    for (header, value) in &headers {
        if value.len() > USER_METADATA_BUDGET_BYTES {
            return Err(StoreError::MetadataTooLarge {
                header: header_static_name(header),
                size: value.len(),
                limit: USER_METADATA_BUDGET_BYTES,
            });
        }
        // S3's real budget is shared across the SUM of every `x-amz-meta-*` header's own
        // name+value bytes on the object, not allotted separately per header (review finding --
        // see this module's own doc comment).
        total_bytes += header.len() + value.len();
    }
    if total_bytes > USER_METADATA_BUDGET_BYTES {
        return Err(StoreError::UserMetadataBudgetExceeded { total: total_bytes, limit: USER_METADATA_BUDGET_BYTES });
    }
    Ok(headers)
}

/// The inverse of [`encode`]: given the object's stored `x-amz-meta-av-*` headers (lowercase
/// names -- callers pass whatever their HTTP client already lowercased, since HTTP header
/// names are case-insensitive and S3 always echoes them lowercase), rebuilds an
/// [`ObjectMetadata`]. A missing header is [`StoreError::MetadataMissing`]; a header present
/// but not valid base64 / not a valid `prost` encoding is [`StoreError::MetadataDecode`].
pub fn decode<'a>(headers: impl Iterator<Item = (&'a str, &'a str)>) -> Result<ObjectMetadata, StoreError> {
    let map: BTreeMap<&str, &str> = headers.collect();

    let sha256_hex = require(&map, HEADER_SHA256)?.to_string();
    let media_type = require(&map, HEADER_MEDIA_TYPE)?.to_string();

    let label_b64 = require(&map, HEADER_LABEL)?;
    let label_bytes = STANDARD.decode(label_b64).map_err(|e| StoreError::MetadataDecode { header: HEADER_LABEL, reason: e.to_string() })?;
    let label = Label::decode(label_bytes.as_slice()).map_err(|e| StoreError::MetadataDecode { header: HEADER_LABEL, reason: e.to_string() })?;

    let provenance_b64 = require(&map, HEADER_PROVENANCE)?;
    let provenance_bytes = STANDARD.decode(provenance_b64).map_err(|e| StoreError::MetadataDecode { header: HEADER_PROVENANCE, reason: e.to_string() })?;
    let provenance = Provenance::decode(provenance_bytes.as_slice()).map_err(|e| StoreError::MetadataDecode { header: HEADER_PROVENANCE, reason: e.to_string() })?;

    Ok(ObjectMetadata { sha256_hex, label, provenance, media_type })
}

fn require<'a>(map: &BTreeMap<&str, &'a str>, header: &'static str) -> Result<&'a str, StoreError> {
    map.get(header).copied().ok_or(StoreError::MetadataMissing { header })
}

/// [`encode`]'s own `headers` map is keyed by `String` (built from the `&'static str`
/// constants above), but [`StoreError::MetadataTooLarge::header`] wants the original
/// `&'static str` back rather than an owned copy -- this maps a value back to whichever
/// constant it came from. Exhaustive over the five headers this module defines; a header
/// this function does not recognize is a bug in this module itself (it would only ever be
/// called with one of the five names just inserted above), so it falls back to the sha256
/// header's name rather than panicking a production PUT over a display-string mismatch.
fn header_static_name(header: &str) -> &'static str {
    match header {
        HEADER_MARKING => HEADER_MARKING,
        HEADER_LABEL => HEADER_LABEL,
        HEADER_PROVENANCE => HEADER_PROVENANCE,
        HEADER_MEDIA_TYPE => HEADER_MEDIA_TYPE,
        _ => HEADER_SHA256,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ObjectMetadata {
        ObjectMetadata {
            sha256_hex: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            label: Label { marking: "CUI//SP-EXPT".to_string(), caveats: vec!["SP-EXPT".to_string(), "REL-TO//FVEY".to_string()] },
            provenance: Provenance { author_kind: 3, principal: "svc-tiler".to_string(), tool: "av-store".to_string(), ..Default::default() },
            media_type: "image/tiff".to_string(),
        }
    }

    #[test]
    fn round_trips_a_label_with_caveats_and_a_populated_provenance() {
        let meta = sample();
        let headers = encode(&meta).unwrap();
        let borrowed: BTreeMap<&str, &str> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let decoded = decode(borrowed.into_iter()).unwrap();
        assert_eq!(decoded, meta);
    }

    #[test]
    fn round_trips_empty_provenance_and_no_caveats() {
        let meta = ObjectMetadata {
            sha256_hex: "0".repeat(64),
            label: Label { marking: "UNCLASSIFIED".to_string(), caveats: vec![] },
            provenance: Provenance::default(),
            media_type: "application/octet-stream".to_string(),
        };
        let headers = encode(&meta).unwrap();
        let borrowed: BTreeMap<&str, &str> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(decode(borrowed.into_iter()).unwrap(), meta);
    }

    #[test]
    fn round_trips_a_marking_containing_two_forward_slashes() {
        let meta = ObjectMetadata {
            sha256_hex: "1".repeat(64),
            label: Label { marking: "CUI//SP-EXPT//FOUO".to_string(), caveats: vec![] },
            provenance: Provenance::default(),
            media_type: "application/json".to_string(),
        };
        let headers = encode(&meta).unwrap();
        assert_eq!(headers.get(HEADER_MARKING).unwrap(), "CUI//SP-EXPT//FOUO");
        let borrowed: BTreeMap<&str, &str> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(decode(borrowed.into_iter()).unwrap(), meta);
    }

    #[test]
    fn refuses_a_marking_with_a_non_ascii_byte() {
        let mut meta = sample();
        meta.label.marking = "CUI-\u{00e9}".to_string();
        let err = encode(&meta).unwrap_err();
        assert!(matches!(err, StoreError::InvalidMarking { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_marking_with_a_control_byte() {
        let mut meta = sample();
        meta.label.marking = "CUI\u{0007}".to_string();
        let err = encode(&meta).unwrap_err();
        assert!(matches!(err, StoreError::InvalidMarking { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_header_value_over_the_user_metadata_budget() {
        let mut meta = sample();
        meta.provenance.attributes.insert("k".to_string(), "v".repeat(USER_METADATA_BUDGET_BYTES * 2));
        let err = encode(&meta).unwrap_err();
        match err {
            StoreError::MetadataTooLarge { header, size, limit } => {
                assert_eq!(header, HEADER_PROVENANCE);
                assert!(size > limit);
            }
            other => panic!("{other:?}"),
        }
    }

    /// Review finding (H1b's brief item 2): each of `x-amz-meta-av-label` and
    /// `x-amz-meta-av-provenance` here is, alone, comfortably under
    /// [`USER_METADATA_BUDGET_BYTES`] (2048) -- roughly 1.9 KiB and 1.3 KiB respectively, both
    /// under 2 KiB on their own -- but together (plus the other three headers) the SUM is over
    /// it, which is exactly the gap this module's own doc used to admit: checked per header,
    /// never summed, even though S3's real budget is shared across every `x-amz-meta-*` header
    /// on one object. **What this fails against:** an `encode` that checks only per header
    /// (the pre-fix shape) would return `Ok(_)` here instead of refusing.
    #[test]
    fn encode_refuses_when_the_sum_of_every_header_is_over_budget_even_though_each_is_individually_under_it() {
        let mut meta = sample();
        meta.label.caveats = vec!["C".repeat(1400)];
        meta.provenance.attributes.insert("k".to_string(), "V".repeat(900));

        match encode(&meta) {
            Err(StoreError::UserMetadataBudgetExceeded { total, limit }) => {
                assert_eq!(limit, USER_METADATA_BUDGET_BYTES);
                assert!(total > limit, "measured total {total} must exceed the {limit}-byte shared budget");
            }
            Err(StoreError::MetadataTooLarge { header, size, .. }) => {
                panic!(
                    "expected the SUMMED-budget refusal (UserMetadataBudgetExceeded), got a per-header \
                     MetadataTooLarge for {header} at {size} bytes instead -- this test's fixture sizes must \
                     each stay under the per-header budget so only the summed check can fire"
                );
            }
            other => panic!("expected Err(UserMetadataBudgetExceeded), got {other:?}"),
        }
    }

    #[test]
    fn decode_reports_which_header_is_missing() {
        let err = decode(std::iter::empty()).unwrap_err();
        assert!(matches!(err, StoreError::MetadataMissing { header } if header == HEADER_SHA256), "{err:?}");
    }

    #[test]
    fn decode_reports_a_malformed_base64_label_header() {
        let sha256_hex = "0".repeat(64);
        let headers = [(HEADER_SHA256, sha256_hex.as_str()), (HEADER_MEDIA_TYPE, "x"), (HEADER_LABEL, "not-valid-base64!!"), (HEADER_PROVENANCE, "")];
        let err = decode(headers.into_iter()).unwrap_err();
        assert!(matches!(err, StoreError::MetadataDecode { header: HEADER_LABEL, .. }), "{err:?}");
    }
}
