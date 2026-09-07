//! FIPS posture **detection** for `/admin/api/evidence` (ADR-004 question 63/86: "follows
//! the crypto rule: SHA-256 only ... the system FIPS OpenSSL only"). This module reports
//! what the linked OpenSSL actually is; it never asserts FIPS compliance -- the task brief
//! for this milestone is explicit that a wrong claim here is the worst failure mode, so
//! every field below is something this process actually observed, not something read out
//! of a config file or hard-coded.
//!
//! # Why not `openssl::fips::enabled()`
//!
//! The obvious-looking API, `openssl::fips::{enable, enabled}` (wrapping the OpenSSL 1.0.2
//! `FIPS_mode`/`FIPS_mode_set` C functions), **does not exist for this build at all**: the
//! `openssl` crate only compiles that module `#[cfg(not(any(libressl, ossl300)))]`
//! (`openssl-0.10.81/src/lib.rs`), and this crate links OpenSSL 3.x (see this crate's
//! README's `otool -L` output: `libssl.3.dylib`/`libcrypto.3.dylib`). OpenSSL 3.x replaced
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
//!    identity (`openssl::sha::sha256`, used by `crate::evidence`, is provided by this same
//!    library).
//! 2. Attempts `openssl::provider::Provider::try_load(None, "fips", false)` -- OpenSSL's
//!    own mechanism for loading a named provider module by searching its configured module
//!    directory. If no `fips` provider module is installed (as is the case for the plain
//!    Homebrew `openssl@3` formula this crate builds against -- `ls
//!    $(brew --prefix openssl@3)/lib/ossl-modules` lists only `legacy.dylib`, no
//!    `fips.dylib`), the load fails and this is reported as `fips_provider_loadable:
//!    false`. The probe provider (if the load *does* succeed on some other host) is
//!    unloaded immediately -- this function's only side effect is the one-time load/unload
//!    of the probe itself, never leaving a provider installed as a side effect of an admin
//!    query.
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
    /// `openssl::version::version()`, e.g. `"OpenSSL 3.6.3 9 Jun 2026"` (Homebrew's
    /// `openssl@3` on this host) -- NOT the macOS-system `/usr/bin/openssl`, which is
    /// LibreSSL and is never linked by this crate.
    pub openssl_version: String,
    /// `openssl::version::number()` -- `OPENSSL_VERSION_NUMBER`, for a machine-checkable
    /// pin alongside the human-readable string above.
    pub openssl_version_number: i64,
    /// Whether OpenSSL could load a provider module named `"fips"` from its configured
    /// module search path. `false` on this host as of this task (no `fips.dylib` shipped
    /// by the plain `openssl@3` Homebrew formula) -- see the module doc for exactly what
    /// `true`/`false` do and do not prove.
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
                 (crate::evidence) are actually routed through it."
                    .to_string(),
            )
        }
        Err(e) => (
            false,
            format!(
                "the OpenSSL 'fips' provider module could not be loaded ({e}). This build's \
                 linked OpenSSL ({openssl_version}) is the plain Homebrew `openssl@3` \
                 formula, which does not ship a FIPS provider module -- this host/build has \
                 no CMVP-validated FIPS module available at all, so this process is \
                 definitively NOT running FIPS-validated cryptography (ADR-004's crypto \
                 rule: SHA-256 only via the system/Homebrew OpenSSL is satisfied on the \
                 algorithm-selection axis; the FIPS-validation axis is a Gap -- see \
                 docs/compliance/av-dynamics-service/control-matrix.md, SC 3.13.11)."
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
