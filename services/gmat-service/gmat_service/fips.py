"""FIPS posture **detection** for ``/admin/api/evidence`` (ADR-004 question 63/86: "follows
the crypto rule: SHA-256 only ... the system FIPS OpenSSL only"). This module reports what
the OpenSSL Python's own interpreter is linked against actually is; it never asserts FIPS
compliance -- the task brief for this milestone is explicit that a wrong claim here is the
worst failure mode, so every field below is something this process actually observed, not
something read out of a config file or hard-coded.

Mirrors `crates/av-dynamics-service/src/fips.rs` in spirit (detect, don't assert) but uses
Python's own available introspection rather than that module's OpenSSL-provider-loading
probe, since Python's ``ssl``/``_hashlib`` modules already expose what is needed directly:

- ``ssl.OPENSSL_VERSION`` / ``ssl.OPENSSL_VERSION_NUMBER`` -- the linked OpenSSL's own
  self-reported identity (the same library ``hashlib.sha256``, used by
  :mod:`gmat_service.evidence`, is backed by on this interpreter build).
- ``_hashlib.get_fips_mode()`` -- a private-but-stable CPython API (its own docstring:
  "good enough for unittests") that, for OpenSSL 3.0+, "returns the state of the default
  provider in the default OSSL context"; non-zero means FIPS mode. This is the Python-side
  analogue of the Rust side's attempt to load the OpenSSL ``"fips"`` provider by name: both
  report whether *this specific linked OpenSSL* is actually in a FIPS posture, not a
  version-string guess.

**What ``fips_provider_active: true`` would NOT mean**: that the module has passed its
power-up self-tests in a CMVP sense beyond what ``get_fips_mode()`` itself already checks,
or that every code path in this process (not just ``_hashlib``) is FIPS-gated. A non-zero
result is Python's own honest signal that FIPS mode is active for its default OpenSSL
context; nothing here inflates or reinterprets it.

**Field-name note**: this reports ``fips_provider_active`` (is FIPS mode ON right now),
while the Rust side reports ``fips_provider_loadable`` (CAN a "fips" provider module be
found at all) -- genuinely different questions, asked with each runtime's own best
available introspection. Do not read the two field names as interchangeable; the "Every
approximation, deviation or shortcut" section of this task's report says so explicitly.
"""
from __future__ import annotations

import ssl


def detect() -> dict:
    """Detects (never asserts) this process's FIPS posture."""
    openssl_version = ssl.OPENSSL_VERSION
    openssl_version_number = ssl.OPENSSL_VERSION_NUMBER

    try:
        import _hashlib  # CPython-internal; not guaranteed on every implementation.
        fips_mode = _hashlib.get_fips_mode()
        fips_provider_active = fips_mode != 0
        detail = (
            f"_hashlib.get_fips_mode() returned {fips_mode} for the default OpenSSL "
            f"provider/context this interpreter's hashlib is linked against ({openssl_version}); "
            "non-zero means FIPS mode is active. A `false` result here does NOT mean OpenSSL "
            "has no FIPS module installed anywhere on the host, only that this process's "
            "default context is not running in FIPS mode -- see "
            "docs/compliance/gmat-service/control-matrix.md, SC 3.13.11."
        )
    except (ImportError, AttributeError) as e:
        fips_provider_active = False
        detail = f"could not query _hashlib.get_fips_mode() on this interpreter ({e}); treating as not-FIPS."

    return {
        "openssl_version": openssl_version,
        "openssl_version_number": openssl_version_number,
        "fips_provider_active": fips_provider_active,
        "detail": detail,
    }
