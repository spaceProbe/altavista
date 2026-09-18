//! The request pipeline: [`handle`] runs every step `crate`'s own crate doc names, in the
//! fixed order the task brief pins, each step its own typed, counted [`crate::refusal::
//! TileRefusal`] on failure. No networking of its own -- an [`ObjectSource`] and a clock
//! reading are both handed in, so this function is directly unit-testable with
//! [`crate::source::InMemoryObjectSource`] and no server, no container, no wall clock.

use av_cdm::pb::TileSetManifest;
use av_command::counters::Counters;
use av_command::oidc::verify;
use prost::Message as _;

use crate::config::{TilesConfig, MANIFEST_MEDIA_TYPE};
use crate::range::RangeHeader;
use crate::refusal::TileRefusal;
use crate::route::{self, Route};
use crate::source::{ObjectSource, ObjectSourceError};

/// P2: content-addressed bytes never change -- the same key/hash always names the same
/// bytes, forever (that IS what content-addressing means) -- so a `200`/`206` response may
/// be cached by any intermediary, indefinitely, with no revalidation ever required.
/// `max-age=31536000` (one year, `Cache-Control`'s own de facto maximum) plus `immutable`
/// (RFC 8246 -- tells a client to skip even a conditional revalidation on reload).
pub const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// A finished response: `crate::server` turns this into real HTTP bytes on the wire; tests
/// call [`handle`] directly and inspect this value with no networking at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileResponse {
    pub status: u16,
    /// Absent exactly when [`Self::body`] is empty AND this is a refusal -- a `200`/`206`/
    /// `304` always sets it (a `304` carries the same `Content-Type` its `200` would have,
    /// per RFC 9110 -- a caller's own cached representation is still that type).
    pub content_type: Option<String>,
    /// Set on every `200`/`206`/`304` -- `"<hex sha256>"`, the exact content hash this crate
    /// already verified the bytes against (P2's own brief: `ETag: "<tile sha256>"`).
    pub etag: Option<String>,
    /// [`IMMUTABLE_CACHE_CONTROL`] on every `200`/`206`/`304`; `None` on a refusal (a `403`/
    /// `404`/`502` is not a cacheable representation of the resource at all).
    pub cache_control: Option<String>,
    /// `"bytes <start>-<end>/<total>"` on a `206`; `"bytes */<len>"` on a `416`; `None`
    /// otherwise.
    pub content_range: Option<String>,
    pub body: Vec<u8>,
}

impl TileResponse {
    fn refused(refusal: &TileRefusal, counters: &Counters) -> Self {
        counters.record(refusal);
        let content_range = match refusal {
            TileRefusal::RangeUnsatisfiable { len } => Some(format!("bytes */{len}")),
            _ => None,
        };
        Self { status: crate::refusal::status(refusal), content_type: None, etag: None, cache_control: None, content_range, body: Vec::new() }
    }
}

/// P2: applies `ETag`/`If-None-Match`/`Range` to a final, already-authorized, already-hash-
/// verified `body` -- the one place both the manifest route and the tile route funnel
/// through on their way to a response, so the caching/range rules are identical for both
/// (both are equally content-addressed, equally immutable). `etag_hex` is the hash this
/// crate already computed and verified `body` against (`manifest_actual_sha256` or
/// `entry.sha256`) -- never recomputed a second time here.
fn serve_content(content_type: String, body: Vec<u8>, etag_hex: &str, range_header: Option<&str>, if_none_match: Option<&str>, counters: &Counters) -> TileResponse {
    let etag = format!("\"{etag_hex}\"");
    let cache_control = Some(IMMUTABLE_CACHE_CONTROL.to_string());

    if let Some(inm) = if_none_match {
        if inm.split(',').any(|tok| { let t = tok.trim(); t == "*" || t == etag }) {
            return TileResponse { status: 304, content_type: Some(content_type), etag: Some(etag), cache_control, content_range: None, body: Vec::new() };
        }
    }

    match crate::range::parse(range_header, body.len()) {
        RangeHeader::Absent | RangeHeader::Unparseable => {
            // Unparseable: RFC 9110 section 14.2 lets a server ignore a Range header it
            // cannot make sense of and serve the full representation instead of refusing --
            // see crate::range's own module doc for the full reasoning.
            TileResponse { status: 200, content_type: Some(content_type), etag: Some(etag), cache_control, content_range: None, body }
        }
        RangeHeader::Unsatisfiable => TileResponse::refused(&TileRefusal::RangeUnsatisfiable { len: body.len() }, counters),
        RangeHeader::Satisfiable { start, end_inclusive } => {
            let total = body.len();
            let slice = body[start..=end_inclusive].to_vec();
            TileResponse {
                status: 206,
                content_type: Some(content_type),
                etag: Some(etag),
                cache_control,
                content_range: Some(format!("bytes {start}-{end_inclusive}/{total}")),
                body: slice,
            }
        }
    }
}

/// `pub(crate)`, not private: `crate::server`'s own tests build manifest/tile fixtures and
/// need to derive the identical hash this function computes, to construct request paths and
/// store keys that agree with each other -- one hash function, never a second copy in tests.
pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    let digest = openssl::sha::sha256(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Extracts the bearer token from an `Authorization` header value (`"Bearer <token>"`).
/// Anything else -- absent, empty, or not the `Bearer` scheme -- yields `""`, which
/// [`handle`] treats identically to no header at all (`TileRefusal::AuthMissingToken`):
/// invariant B (fail closed), the same "an absent OR malformed credential is never treated
/// as allow" rule `crates/av-gateway/src/auth.rs::AuthContext::verify_token` already states.
fn bearer_token(authorization: Option<&str>) -> &str {
    match authorization {
        Some(value) => value.strip_prefix("Bearer ").unwrap_or(""),
        None => "",
    }
}

/// Fetches the object at `key` through `source`, treating a backend error the same as "not
/// found" for THIS refusal (a deliberate simplification for this round: this crate's own
/// `ObjectSourceError` has only two variants, and a caller-visible distinction between "truly
/// absent" and "the store failed to answer" is not something any test in this round's own
/// brief asks for -- both are equally "this crate could not get the bytes it needed").
fn fetch(source: &dyn ObjectSource, key: &str, not_found: impl Fn() -> TileRefusal) -> Result<crate::source::StoredObject, TileRefusal> {
    source.get(key).map_err(|e| match e {
        ObjectSourceError::NotFound { .. } => not_found(),
        ObjectSourceError::Backend { .. } => not_found(),
    })
}

/// The three request headers [`handle`] ever reads, bundled rather than passed as separate
/// parameters (keeps `handle`'s own signature short and typed, not an ever-growing argument
/// list as P2 adds `Range`/`If-None-Match` on top of P1's `Authorization`). `crate::server`
/// is the only place these are ever parsed off the wire; see this crate's own crate doc for
/// why no OTHER header or query parameter is ever read here (no caller-declared clearance).
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestHeaders<'a> {
    pub authorization: Option<&'a str>,
    pub range: Option<&'a str>,
    pub if_none_match: Option<&'a str>,
}

impl<'a> RequestHeaders<'a> {
    /// Convenience for every P1-era call site (and most P2 tests) that only cares about
    /// `Authorization` -- equivalent to `RequestHeaders { authorization, range: None,
    /// if_none_match: None }`.
    pub fn auth_only(authorization: Option<&'a str>) -> Self {
        Self { authorization, range: None, if_none_match: None }
    }
}

/// Runs the full pipeline for one request. `authorization` is the raw `Authorization` header
/// value, if the caller sent one (never parsed by `crate::server` -- see this crate's own
/// module doc: no trusted caller-declared clearance ever enters this function any other way).
/// `now_tai_ns` is an injected clock reading (rule: clocks injected, never slept on) --
/// [`av_command::oidc::verify`]'s own `exp`/`nbf` check is evaluated against exactly this
/// value, nothing read from the wall clock inside this function.
pub fn handle(config: &TilesConfig, source: &dyn ObjectSource, counters: &Counters, path: &str, headers: RequestHeaders<'_>, now_tai_ns: i64) -> TileResponse {
    let RequestHeaders { authorization, range: range_header, if_none_match } = headers;
    // Step 1: parse the route.
    let route = match route::parse(path) {
        Ok(r) => r,
        Err(e) => return TileResponse::refused(&TileRefusal::Route(e), counters),
    };
    let manifest_sha256 = match &route {
        Route::Manifest { manifest_sha256 } => manifest_sha256.clone(),
        Route::Tile { manifest_sha256, .. } => manifest_sha256.clone(),
    };

    // Step 2: authenticate, then derive clearance from the verified token's groups --
    // NEVER from a caller-declared header or query parameter (none of this crate's own
    // routes even has one to trust).
    let token = bearer_token(authorization);
    if token.is_empty() {
        return TileResponse::refused(&TileRefusal::AuthMissingToken, counters);
    }
    let principal = match verify(token, &config.issuer_config, now_tai_ns) {
        Ok(p) => p,
        Err(source_err) => {
            counters.record(&source_err); // the underlying token_* code, recorded alongside the generic one below
            return TileResponse::refused(&TileRefusal::AuthTokenInvalid { source: source_err }, counters);
        }
    };
    let caller_clearance = match config.group_clearance.clearance_for(&principal.groups, &config.ladder) {
        av_label::GroupClearanceOutcome::Marking(m) => m,
        av_label::GroupClearanceOutcome::NoneMapped => {
            return TileResponse::refused(&TileRefusal::AuthNoClearanceForSubject { groups: principal.groups.clone() }, counters);
        }
        av_label::GroupClearanceOutcome::NotOnLadder(marking) => {
            return TileResponse::refused(&TileRefusal::AuthClearanceMarkingNotOnLadder { groups: principal.groups.clone(), marking }, counters);
        }
    };

    // Step 3: fetch the manifest object by its content hash, then verify it.
    let manifest_key = av_store::object_key(config.key_prefix(), &manifest_sha256).expect("route::parse already validated manifest_sha256's shape; key_prefix is deployment configuration validated at startup");
    let manifest_object = match fetch(source, &manifest_key, || TileRefusal::ManifestNotFound { manifest_sha256: manifest_sha256.clone() }) {
        Ok(o) => o,
        Err(refusal) => return TileResponse::refused(&refusal, counters),
    };
    let manifest_actual_sha256 = hex_sha256(&manifest_object.bytes);
    if manifest_actual_sha256 != manifest_sha256 {
        return TileResponse::refused(&TileRefusal::ManifestHashMismatch { requested: manifest_sha256, actual: manifest_actual_sha256 }, counters);
    }

    // Step 4: decode the manifest.
    let manifest = match TileSetManifest::decode(manifest_object.bytes.as_slice()) {
        Ok(m) => m,
        Err(e) => return TileResponse::refused(&TileRefusal::ManifestDecodeInvalid { manifest_sha256, detail: e.to_string() }, counters),
    };

    // Step 5: label enforcement -- see crate::refusal's own module doc, "'Per layer' and
    // 'per request' label enforcement", for why this ONE check is both.
    if let Some(label_refusal) = config.ladder.classify(&caller_clearance, &manifest_object.label) {
        return TileResponse::refused(&TileRefusal::LayerLabel(label_refusal), counters);
    }

    match route {
        Route::Manifest { .. } => serve_content(MANIFEST_MEDIA_TYPE.to_string(), manifest_object.bytes, &manifest_actual_sha256, range_header, if_none_match, counters),
        Route::Tile { level, x, y, .. } => {
            // Step 6: find the TileEntry for (level, x, y).
            let Some(entry) = manifest.tiles.iter().find(|t| t.level == level && t.x == x && t.y == y) else {
                return TileResponse::refused(&TileRefusal::TileNotFound { level, x, y }, counters);
            };

            // Step 7: fetch the tile object by TileEntry.object_key -- already the full,
            // prefixed store key (`TileEntry.object_key`'s own doc comment, `heavy.proto`:
            // constructed at write time as `object_key(object_key_prefix, tile.sha256)`,
            // `crates/av-jobs::tiler::TilerExecutor::run_imagery`) -- never re-joined with
            // `manifest.object_key_prefix` a second time, which would double the prefix and
            // look up a key that was never written.
            let tile_object = match fetch(source, &entry.object_key, || TileRefusal::TileObjectNotFound { object_key: entry.object_key.clone() }) {
                Ok(o) => o,
                Err(refusal) => return TileResponse::refused(&refusal, counters),
            };
            let tile_actual_sha256 = hex_sha256(&tile_object.bytes);
            if tile_actual_sha256 != entry.sha256 {
                return TileResponse::refused(&TileRefusal::TileHashMismatch { expected: entry.sha256.clone(), actual: tile_actual_sha256 }, counters);
            }

            // Step 8: 200 (or 206/304/416 -- P2), the tile's exact bytes.
            serve_content(entry.media_type.clone(), tile_object.bytes, &entry.sha256, range_header, if_none_match, counters)
        }
    }
}

#[cfg(test)]
mod tests {
    //! P1's own acceptance evidence, run directly against [`handle`] -- no networking, no
    //! container, an [`crate::source::InMemoryObjectSource`] standing in for `av-store`.

    use super::*;
    use av_cdm::pb::{Label, TileEntry, TileSetKind, TileSetManifest};
    use av_command::oidc::IssuerConfig;
    use av_command::test_support::{valid_claims, TestIssuer};
    use av_label::{ClearanceLadder, GroupClearanceMap};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    const ISSUER: &str = "https://sso.test.example/";
    const AUDIENCE: &str = "av-tiles";
    const NOW_UNIX_S: i64 = 1_760_000_000;
    const TTL_S: i64 = 3_600;
    const KEY_PREFIX: &str = "imagery/2026";

    fn now_tai_ns() -> i64 {
        av_cdm::time::Tai::from_utc_nanos(NOW_UNIX_S * 1_000_000_000).as_nanos()
    }

    fn ladder() -> Arc<ClearanceLadder> {
        Arc::new(ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]))
    }

    fn tiles_config(issuer: &TestIssuer, group_clearance: &[(&str, &str)]) -> TilesConfig {
        let issuer_config = Arc::new(IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap());
        let mut m = BTreeMap::new();
        for (g, marking) in group_clearance {
            m.insert(g.to_string(), marking.to_string());
        }
        TilesConfig::new(KEY_PREFIX, issuer_config, ladder(), Arc::new(GroupClearanceMap::new(m)))
    }

    fn mint(issuer: &TestIssuer, groups: &[&str], now_unix_s: i64, ttl_s: i64) -> String {
        let mut claims = valid_claims(ISSUER, AUDIENCE, "operator-1", now_unix_s, ttl_s);
        claims["groups"] = serde_json::json!(groups);
        issuer.mint(&claims)
    }

    /// Builds a one-tile manifest (label `label_marking`) plus its tile object, both
    /// registered in a fresh [`crate::source::InMemoryObjectSource`] under their real,
    /// content-derived keys -- returns `(source, manifest_sha256, tile_bytes, entry)`.
    fn fixture(label_marking: &str) -> (crate::source::InMemoryObjectSource, String, Vec<u8>, TileEntry) {
        let tile_bytes = vec![10u8, 20, 30, 40, 50];
        let tile_sha256 = hex_sha256(&tile_bytes);
        let tile_key = av_store::object_key(KEY_PREFIX, &tile_sha256).unwrap();
        let entry = TileEntry { level: 2, x: 1, y: 1, sha256: tile_sha256.clone(), size_bytes: tile_bytes.len() as u64, uri: String::new(), media_type: "image/png".to_string(), object_key: tile_key.clone() };
        let manifest = TileSetManifest {
            kind: TileSetKind::Imagery as i32,
            scheme: "geographic-plate-carree-2x1".to_string(),
            min_level: 0,
            max_level: 2,
            tile_size: 256,
            bounds: None,
            tiles: vec![entry.clone()],
            source_sha256: vec![],
            parameters: Default::default(),
            root_uri: String::new(),
            job_id: "job-1".to_string(),
            object_key_prefix: KEY_PREFIX.to_string(),
            root_object_key: String::new(),
        };
        let manifest_bytes = manifest.encode_to_vec();
        let manifest_sha256 = hex_sha256(&manifest_bytes);
        let manifest_key = av_store::object_key(KEY_PREFIX, &manifest_sha256).unwrap();

        let source = crate::source::InMemoryObjectSource::new();
        let label = Label { marking: label_marking.to_string(), caveats: vec![] };
        source.insert(manifest_key, manifest_bytes, label.clone());
        source.insert(tile_key, tile_bytes.clone(), label);
        (source, manifest_sha256, tile_bytes, entry)
    }

    fn tile_path(manifest_sha256: &str, entry: &TileEntry) -> String {
        format!("/v1/tilesets/{manifest_sha256}/tiles/{}/{}/{}", entry.level, entry.x, entry.y)
    }

    // -- Acceptance: a refused tile is refused and counted, exactly once ------------------

    #[test]
    fn a_caller_below_the_tile_sets_label_is_refused_403_with_no_tile_bytes_and_counted_exactly_once() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _bytes, entry) = fixture("SECRET");
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), RequestHeaders::auth_only(Some(&format!("Bearer {token}"))), now_tai_ns());

        assert_eq!(response.status, 403);
        assert!(response.body.is_empty(), "a refusal must never carry tile bytes");
        assert_eq!(counters.get("tiles_layer_label_over_clearance"), 1);
    }

    // -- Acceptance: byte equality with the stored object ----------------------------------

    #[test]
    fn a_served_tiles_body_is_byte_identical_to_what_was_put_in_the_store() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, tile_bytes, entry) = fixture("CUI");
        let config = tiles_config(&issuer, &[("operators", "CUI")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), RequestHeaders::auth_only(Some(&format!("Bearer {token}"))), now_tai_ns());

        assert_eq!(response.status, 200);
        assert_eq!(response.body, tile_bytes);
        assert_eq!(response.content_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn a_served_manifests_body_is_byte_identical_to_what_was_put_in_the_store() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _tile_bytes, _entry) = fixture("CUI");
        let config = tiles_config(&issuer, &[("operators", "CUI")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        let stored_manifest_bytes = source.get(&av_store::object_key(KEY_PREFIX, &manifest_sha256).unwrap()).unwrap().bytes;
        let response = handle(&config, &source, &counters, &format!("/v1/tilesets/{manifest_sha256}/manifest"), RequestHeaders::auth_only(Some(&format!("Bearer {token}"))), now_tai_ns());

        assert_eq!(response.status, 200);
        assert_eq!(response.body, stored_manifest_bytes);
    }

    // -- Acceptance: a manifest-hash mismatch is refused, never a 200 ---------------------

    #[test]
    fn a_manifest_hash_mismatch_is_refused_502_and_counted_never_200() {
        let issuer = TestIssuer::new();
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        // A key that claims to be the store's answer for `requested_hash`, but whose actual
        // stored bytes hash to something else entirely -- the store returning "something
        // that is not what was asked for" (this crate's own module doc, crate::source).
        let requested_hash = "a".repeat(64);
        let wrong_bytes = b"these bytes do not hash to the requested value".to_vec();
        let source = crate::source::InMemoryObjectSource::new();
        let key = av_store::object_key(KEY_PREFIX, &requested_hash).unwrap();
        source.insert(key, wrong_bytes, Label { marking: "UNCLASSIFIED".to_string(), caveats: vec![] });

        let response = handle(&config, &source, &counters, &format!("/v1/tilesets/{requested_hash}/manifest"), RequestHeaders::auth_only(Some(&format!("Bearer {token}"))), now_tai_ns());

        assert_eq!(response.status, 502);
        assert!(response.body.is_empty());
        assert_eq!(counters.get("tiles_manifest_hash_mismatch"), 1);
    }

    // -- Acceptance: no token / bad token / expired token, each 401 with their own counters,
    // through the real av_command::oidc::verify with a real signed token and an injected
    // clock (mirrors crates/av-gateway's own tests -- no second way of building one).

    #[test]
    fn no_authorization_header_is_401_and_counted_as_missing_token() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _bytes, entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();

        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), RequestHeaders::auth_only(None), now_tai_ns());

        assert_eq!(response.status, 401);
        assert_eq!(counters.get("tiles_auth_missing_token"), 1);
        assert_eq!(counters.get("tiles_auth_token_invalid"), 0, "a MISSING token is a distinct refusal from an INVALID one");
    }

    #[test]
    fn a_syntactically_present_but_unverifiable_token_is_401_and_counted_as_token_invalid() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _bytes, entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();

        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), RequestHeaders::auth_only(Some("Bearer not.a.real.token")), now_tai_ns());

        assert_eq!(response.status, 401);
        assert_eq!(counters.get("tiles_auth_token_invalid"), 1);
        assert_eq!(counters.get("tiles_auth_missing_token"), 0);
    }

    #[test]
    fn an_expired_token_is_401_and_counted_distinctly_as_token_expired() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _bytes, entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();
        // Minted with exp = NOW_UNIX_S + TTL_S; evaluated at exactly that instant, the
        // documented boundary (`av_command::oidc`'s own module doc): expired.
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);
        let now_tai_ns_at_expiry = av_cdm::time::Tai::from_utc_nanos((NOW_UNIX_S + TTL_S) * 1_000_000_000).as_nanos();

        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), RequestHeaders::auth_only(Some(&format!("Bearer {token}"))), now_tai_ns_at_expiry);

        assert_eq!(response.status, 401);
        assert_eq!(counters.get("tiles_auth_token_invalid"), 1);
        assert_eq!(counters.get("token_expired"), 1, "the underlying TokenError's own code must ALSO be recorded (mirrors av-gateway's identical double-count)");
    }

    // -- Acceptance: a tile address not in the manifest is 404 and counted ----------------

    #[test]
    fn a_tile_address_absent_from_the_manifest_is_404_and_counted_distinctly_from_manifest_not_found() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _bytes, _entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        let path = format!("/v1/tilesets/{manifest_sha256}/tiles/9/9/9");
        let response = handle(&config, &source, &counters, &path, RequestHeaders::auth_only(Some(&format!("Bearer {token}"))), now_tai_ns());

        assert_eq!(response.status, 404);
        assert_eq!(counters.get("tiles_tile_not_found"), 1);
        assert_eq!(counters.get("tiles_manifest_not_found"), 0);
    }

    #[test]
    fn a_manifest_sha256_with_no_stored_object_at_all_is_404_and_counted_as_manifest_not_found() {
        let issuer = TestIssuer::new();
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);
        let source = crate::source::InMemoryObjectSource::new();

        let never_stored = "b".repeat(64);
        let response = handle(&config, &source, &counters, &format!("/v1/tilesets/{never_stored}/manifest"), RequestHeaders::auth_only(Some(&format!("Bearer {token}"))), now_tai_ns());

        assert_eq!(response.status, 404);
        assert_eq!(counters.get("tiles_manifest_not_found"), 1);
    }

    // -- A malformed route is refused before any auth/store work happens ------------------

    #[test]
    fn a_malformed_path_is_400_and_counted_before_any_auth_check() {
        let issuer = TestIssuer::new();
        let config = tiles_config(&issuer, &[]);
        let counters = Counters::new();
        let source = crate::source::InMemoryObjectSource::new();

        // No Authorization header at all -- if route parsing ran AFTER auth, this would be
        // 401, not 400.
        let response = handle(&config, &source, &counters, "/nope", RequestHeaders::auth_only(None), now_tai_ns());

        assert_eq!(response.status, 400);
        assert_eq!(counters.get("tiles_route_malformed"), 1);
        assert_eq!(counters.get("tiles_auth_missing_token"), 0, "route parsing must run before authentication");
    }

    // -- The clearance-derivation refusals (P0's GroupClearanceMap, now exercised through
    // this crate's own real pipeline, not just av-gateway's) -----------------------------

    #[test]
    fn a_token_whose_groups_map_to_no_configured_clearance_is_403_and_counted() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _bytes, entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[]); // no group_clearance entries at all
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), RequestHeaders::auth_only(Some(&format!("Bearer {token}"))), now_tai_ns());

        assert_eq!(response.status, 403);
        assert_eq!(counters.get("tiles_auth_no_clearance_for_subject"), 1);
    }

    #[test]
    fn a_group_clearance_marking_absent_from_the_ladder_is_403_and_counted() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _bytes, entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[("misconfigured", "TOP-SECRET")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["misconfigured"], NOW_UNIX_S, TTL_S);

        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), RequestHeaders::auth_only(Some(&format!("Bearer {token}"))), now_tai_ns());

        assert_eq!(response.status, 403);
        assert_eq!(counters.get("tiles_auth_clearance_marking_not_on_ladder"), 1);
    }

    // -- P2: Range / ETag / If-None-Match, at the handle() level --------------------------

    #[test]
    fn an_unsatisfiable_range_is_416_with_content_range_and_counted() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _bytes, entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        let headers = RequestHeaders { authorization: Some(&format!("Bearer {token}")), range: Some("bytes=100-200"), if_none_match: None };
        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), headers, now_tai_ns());

        assert_eq!(response.status, 416);
        assert!(response.body.is_empty());
        assert_eq!(response.content_range.as_deref(), Some("bytes */5"), "the fixture's tile is 5 bytes");
        assert_eq!(counters.get("tiles_range_unsatisfiable"), 1);
    }

    #[test]
    fn a_malformed_range_header_is_ignored_and_served_as_a_plain_200() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, tile_bytes, entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        let headers = RequestHeaders { authorization: Some(&format!("Bearer {token}")), range: Some("garbage, not a range"), if_none_match: None };
        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), headers, now_tai_ns());

        assert_eq!(response.status, 200, "an unparseable Range must be ignored, never refused (RFC 9110 section 14.2)");
        assert_eq!(response.body, tile_bytes);
        assert_eq!(counters.get("tiles_range_unsatisfiable"), 0);
    }

    #[test]
    fn a_satisfiable_range_returns_206_with_the_exact_slice_and_a_content_range_header() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, tile_bytes, entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        let headers = RequestHeaders { authorization: Some(&format!("Bearer {token}")), range: Some("bytes=1-3"), if_none_match: None };
        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), headers, now_tai_ns());

        assert_eq!(response.status, 206);
        assert_eq!(response.body, tile_bytes[1..=3].to_vec());
        assert_eq!(response.content_range.as_deref(), Some("bytes 1-3/5"));
        assert_eq!(response.etag.as_deref(), Some(format!("\"{}\"", entry.sha256)).as_deref());
    }

    #[test]
    fn a_matching_if_none_match_returns_304_with_no_body_and_the_etag_header() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _tile_bytes, entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        let etag = format!("\"{}\"", entry.sha256);
        let headers = RequestHeaders { authorization: Some(&format!("Bearer {token}")), range: None, if_none_match: Some(&etag) };
        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), headers, now_tai_ns());

        assert_eq!(response.status, 304);
        assert!(response.body.is_empty());
        assert_eq!(response.etag.as_deref(), Some(etag.as_str()));
    }

    #[test]
    fn a_non_matching_if_none_match_still_returns_200_with_the_full_body() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, tile_bytes, entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        let headers = RequestHeaders { authorization: Some(&format!("Bearer {token}")), range: None, if_none_match: Some("\"some-other-etag\"") };
        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), headers, now_tai_ns());

        assert_eq!(response.status, 200);
        assert_eq!(response.body, tile_bytes);
    }

    #[test]
    fn every_200_and_206_response_carries_the_immutable_cache_control_header() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _tile_bytes, entry) = fixture("UNCLASSIFIED");
        let config = tiles_config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"], NOW_UNIX_S, TTL_S);

        let response = handle(&config, &source, &counters, &tile_path(&manifest_sha256, &entry), RequestHeaders::auth_only(Some(&format!("Bearer {token}"))), now_tai_ns());
        assert_eq!(response.cache_control.as_deref(), Some(IMMUTABLE_CACHE_CONTROL));
    }
}
