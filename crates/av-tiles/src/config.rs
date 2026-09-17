//! This deployment's own configuration: everything [`crate::core::handle`] needs beyond a
//! single request's own path/headers. Built once at process startup (or once per test),
//! never mutated per request.

use std::sync::Arc;

use av_command::oidc::IssuerConfig;
use av_label::{ClearanceLadder, GroupClearanceMap};

/// The media type a `TileSetManifest`'s own encoded (protobuf) bytes are served under --
/// the identical value `crates/av-jobs::tiler::MANIFEST_MEDIA_TYPE` stores objects under
/// (this crate does not depend on `av-jobs` -- see `Cargo.toml`'s own "Deliberately NOT a
/// dependency" block -- so the literal is restated here, the same "two independently-owned
/// crates share a wire convention, not a Rust dependency edge" shape `crates/av-jobs::scheme`
/// already uses for `web/js/globe_lod.js`'s own tiling arithmetic).
pub const MANIFEST_MEDIA_TYPE: &str = "application/vnd.altavista.tileset-manifest+pb";

/// Everything [`crate::core::handle`] needs: the key prefix every manifest and tile object
/// was stored under (`av_store::object_key`'s own `prefix` argument -- must match whatever
/// prefix the tiler job that produced this tile set was configured with), the OIDC issuer
/// configuration [`av_command::oidc::verify`] checks tokens against, this deployment's
/// clearance ladder, and its group-to-clearance mapping (P0 of this round: [`GroupClearanceMap`],
/// moved into `av-label` so this crate reuses the identical mapping `av-gateway` already had,
/// never a second copy).
#[derive(Clone)]
pub struct TilesConfig {
    key_prefix: String,
    pub issuer_config: Arc<IssuerConfig>,
    pub ladder: Arc<ClearanceLadder>,
    pub group_clearance: Arc<GroupClearanceMap>,
}

impl TilesConfig {
    pub fn new(key_prefix: impl Into<String>, issuer_config: Arc<IssuerConfig>, ladder: Arc<ClearanceLadder>, group_clearance: Arc<GroupClearanceMap>) -> Self {
        Self { key_prefix: key_prefix.into(), issuer_config, ladder, group_clearance }
    }

    pub fn key_prefix(&self) -> &str {
        &self.key_prefix
    }
}
