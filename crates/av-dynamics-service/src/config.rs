//! Fixed identity and settings for the one model this server hosts -- the Rust-side twin
//! of `services/gmat-service/gmat_service/config.py`, pinned against the **same golden**
//! (`goldens/leo_1day_jgm2_8x8_sunmoon.json`: Earth JGM2 8x8 gravity, Luna + Sun point
//! masses, no drag, no SRP) but at **ADR-002 depth 2** ("gmat-ffi": `gmat-sys`'s
//! `GetDerivatives` FFI driven by `av_dynamics::integrate::Dopri5`, our own integrator) --
//! not depth 1 (`gmat_service`, GMAT's own Python-API `Propagator`/`PrinceDormand78`).
//!
//! **`settings_hash()` is deliberately different from `gmat_service.config.settings_hash()`.**
//! The two services genuinely run different integrators over the same force model --
//! `Dopri5` here, GMAT's native `PrinceDormand78` propagator there -- so a settings hash
//! that captures "what determines the physics" (ADR-002's own description of the field)
//! must differ between them; identical hashes would be the misleading result, not this
//! one. Both are pinned against the same golden and are expected to reproduce it within
//! its declared tolerance independently (see this crate's README).
use std::collections::BTreeMap;

/// Localhost-only default port (ADR-004/ADR-003 amendment: plaintext on loopback, mTLS
/// terminated by a service-owned nginx front for any cross-host hop). Distinct from
/// `gmat_service.config.DEFAULT_PORT` (50061) so both services can run side by side on one
/// host during the transition described in `services/gmat-service/README.md`.
pub const DEFAULT_PORT: u16 = 50062;

/// Matches `gmat_service.config.MODEL_ID` and the golden's own force model exactly: the
/// two services host the *same* physical configuration at two different ADR-002 depths.
pub const MODEL_ID: &str = "gmat.earth.jgm2_8x8.sun_moon";
pub const GMAT_VERSION: &str = "R2026a";
/// Matches `altavista.cdm.DEFAULT_STATE_SPACE_ID` verbatim (the same `[x,y,z,vx,vy,vz]` SI
/// state space `gmat_service` reports) -- there is no separate Rust-side state-space
/// registry to mint a second id from.
pub const STATE_SPACE_ID: &str = "altavista.cartesian_pos_vel_6";
pub const FRAME_ID: &str = "EarthMJ2000Eq";
pub const GOLDEN_NAME: &str = "leo_1day_jgm2_8x8_sunmoon";

/// Reference epoch (GMAT A.1 Modified Julian Date) the two bound `GmatModel`s
/// (`worker::Worker::build`) are constructed at. Reuses `gmat_service.config
/// .DERIVATIVES_BASE_EPOCH_A1MJD`'s exact value for parity/documentation, but -- unlike
/// that Python constant -- the value here is not merely a hygiene detail for one
/// specialized "Derivatives engine": `gmat_sys::model::GmatModel::derivatives`/`step`
/// compute `dt_s` relative to *this* epoch for every call (`Describe`, `Derivatives`,
/// `Step`, `Propagate`, at any caller-supplied `tai_ns`), so the exact value is likewise
/// immaterial to correctness (`GetDerivatives(state, dt=D)` at epoch `T0` is bit-identical
/// to `GetDerivatives(state, dt=0)` at `T0+D` -- `crates/gmat-sys/src/model.rs`'s own doc
/// comment) -- only that it is fixed and known.
pub const REFERENCE_EPOCH_A1MJD: f64 = 21545.0;

/// Every entry that determines `derivatives`'/`step`'s physical output, for
/// [`av_dynamics::settings_hash`]. See the module doc for why this intentionally differs
/// from `gmat_service.config.SETTINGS`'s own hash.
pub fn settings() -> BTreeMap<String, String> {
    let integrator = av_dynamics::integrate::Dopri5::default();
    let mut m = BTreeMap::new();
    m.insert("model_id".to_string(), MODEL_ID.to_string());
    m.insert("gmat_version".to_string(), GMAT_VERSION.to_string());
    m.insert("depth".to_string(), "gmat-ffi".to_string());
    m.insert("central_body".to_string(), "Earth".to_string());
    m.insert("gravity_file".to_string(), "JGM2.cof".to_string());
    m.insert("gravity_degree".to_string(), "8".to_string());
    m.insert("gravity_order".to_string(), "8".to_string());
    m.insert("point_masses".to_string(), "Luna,Sun".to_string());
    m.insert("drag".to_string(), "none".to_string());
    m.insert("srp".to_string(), "false".to_string());
    // Our own integrator (ADR-002: "the integrator is ours"), not GMAT's native
    // PrinceDormand78 propagator -- see the module doc for why this makes the hash
    // deliberately different from gmat_service's.
    m.insert("integrator".to_string(), "av_dynamics::integrate::Dopri5".to_string());
    m.insert("rtol".to_string(), format!("{:e}", integrator.rtol));
    m.insert("atol".to_string(), format!("{:e}", integrator.atol));
    m.insert("max_step_s".to_string(), format!("{}", integrator.max_step));
    m
}

pub fn settings_hash() -> String {
    av_dynamics::settings_hash(&settings())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_hash_is_stable_and_hex() {
        let h = settings_hash();
        assert_eq!(h.len(), 64);
        assert_eq!(h, settings_hash(), "must be a pure function of `settings()`");
    }
}
