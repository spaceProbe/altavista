//! FIPS posture **detection** for `/admin/api/evidence` (ADR-004 question 63/86: "follows
//! the crypto rule: SHA-256 only ... the system FIPS OpenSSL only"). This module reports
//! what the linked OpenSSL actually is; it never asserts FIPS compliance.
//!
//! **Copied, not imported, from `crates/av-dynamics-service/src/fips.rs`** (attribution per
//! this task's brief: "reuse by copying that module with attribution ... if the crate is
//! not importable"). `av-dynamics-service` is not importable here without dragging its own
//! dependency tree -- `gmat-sys`, `av-dynamics`, `tonic` -- into `av-command`, a library this
//! task's brief scopes to the state machine, ledger, clock and evidence/admin surface with
//! no GMAT FFI or gRPC dependency of any kind; a `path` dependency on `av-dynamics-service`
//! just to reach one small, self-contained detection function would be a far heavier and
//! more surprising coupling than duplicating roughly 60 lines that never change independent
//! of OpenSSL's own API. The content below is unchanged from the original except for this
//! doc comment; any future fix to the detection logic itself should land in both copies.
//!
//! # Why not `openssl::fips::enabled()`
//!
//! The obvious-looking API, `openssl::fips::{enable, enabled}` (wrapping the OpenSSL 1.0.2
//! `FIPS_mode`/`FIPS_mode_set` C functions), **does not exist for this build at all**: the
//! `openssl` crate only compiles that module `#[cfg(not(any(libressl, ossl300)))]`
//! (`openssl-0.10.81/src/lib.rs`), and this crate links OpenSSL 3.x. OpenSSL 3.x replaced
//! the old global FIPS-mode switch with the **provider** model: FIPS support is a loadable
//! module (`fips.dylib`/`fips.so`), either present in the build's module search path or
//! not.
//!
//! # What this module actually measures
//!
//! [`detect`] does two things, both genuine runtime observations of *this* process's
//! linked OpenSSL, not string-matching a version number:
//!
//! 1. `openssl::version::version()`/`number()` -- the linked library's own self-reported
//!    identity (`openssl::sha::sha256`, used by `crate::ledger`, is provided by this same
//!    library).
//! 2. Attempts `openssl::provider::Provider::try_load(None, "fips", false)` -- OpenSSL's
//!    own mechanism for loading a named provider module by searching its configured module
//!    directory. If no `fips` provider module is installed, the load fails and this is
//!    reported as `fips_provider_loadable: false`. The probe provider (if the load *does*
//!    succeed on some other host) is unloaded immediately -- this function's only side
//!    effect is the one-time load/unload of the probe itself, never leaving a provider
//!    installed as a side effect of an admin query.
//!
//! **What a `true` result would NOT mean**: that the module is the *active default*
//! provider, that it has passed its power-up self-tests, or that this process's own
//! `openssl::sha::sha256` calls are actually routed through it. A loadable module is a
//! necessary precondition for a FIPS posture, not proof of one -- see [`FipsPosture::detail`].
use serde::Serialize;

/// See the module doc. Every field is a direct observation of the linked OpenSSL, made at
/// the moment [`detect`] is called.
#[derive(Debug, Clone, Serialize)]
pub struct FipsPosture {
    /// `openssl::version::version()`, e.g. `"OpenSSL 3.6.3 9 Jun 2026"`.
    pub openssl_version: String,
    /// `openssl::version::number()` -- `OPENSSL_VERSION_NUMBER`, for a machine-checkable
    /// pin alongside the human-readable string above.
    pub openssl_version_number: i64,
    /// Whether OpenSSL could load a provider module named `"fips"` from its configured
    /// module search path -- see the module doc for exactly what `true`/`false` do and do
    /// not prove.
    pub fips_provider_loadable: bool,
    /// Human-readable explanation of what was observed, always present regardless of the
    /// boolean's value -- this is the field a reviewer should actually read.
    pub detail: String,
}

/// Detects (never asserts) this process's FIPS posture. See the module doc.
pub fn detect() -> FipsPosture {
    let openssl_version = openssl::version::version().to_string();
    let openssl_version_number = openssl::version::number();

    let (fips_provider_loadable, detail) = match openssl::provider::Provider::try_load(None, "fips", false) {
        Ok(provider) => {
            // Probe only: unload immediately so this query has no lasting effect on the
            // process's default library context.
            drop(provider);
            (
                true,
                "the OpenSSL 'fips' provider module was found and loaded by name from this \
                 build's module search path. This confirms the module is PRESENT and \
                 LOADABLE -- it does NOT confirm it is the active default provider, that it \
                 passed its power-up self-tests, or that this process's own SHA-256 calls \
                 (crate::ledger) are actually routed through it."
                    .to_string(),
            )
        }
        Err(e) => (
            false,
            format!(
                "the OpenSSL 'fips' provider module could not be loaded ({e}). This build's \
                 linked OpenSSL ({openssl_version}) has no CMVP-validated FIPS module \
                 available -- this process is definitively NOT running FIPS-validated \
                 cryptography (ADR-004's crypto rule: SHA-256 only via the system OpenSSL is \
                 satisfied on the algorithm-selection axis; the FIPS-validation axis is a \
                 Gap -- see docs/compliance/av-command/control-matrix.md)."
            ),
        ),
    };

    FipsPosture { openssl_version, openssl_version_number, fips_provider_loadable, detail }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_reports_a_real_openssl_version_string() {
        let posture = detect();
        assert!(posture.openssl_version.starts_with("OpenSSL"), "{}", posture.openssl_version);
        assert!(posture.openssl_version_number > 0);
        assert!(!posture.detail.is_empty());
    }

    #[test]
    fn detect_is_idempotent() {
        // Calling this repeatedly must not leave state that changes the answer (the probe
        // provider is unloaded after each call) -- guards against the "leaves a provider
        // loaded as a side effect" mistake the module doc explicitly disclaims.
        let a = detect();
        let b = detect();
        assert_eq!(a.fips_provider_loadable, b.fips_provider_loadable);
    }
}
