# SBOM licence exceptions

D2's licence check (`scripts/kit/licences.py`, driven by `tests/test_sbom.py::
test_every_licence_is_allowed_or_declared_an_exception`) evaluates every licence recorded in
every committed SBOM under this directory against `deny.toml`'s `[licenses].allow` list, using
a real SPDX-expression evaluator (`AND`/`OR`/`WITH`, not a string match). Two packages fail
that check today. **This file is the declared exception set the test allows through instead of
either failing on them forever or silently weakening the check: a NEW non-allowed licence
anywhere still fails the test; only the two rows below do not.**

`deny.toml`'s `[licenses].allow` list is the lead's decision, not this task's — this file
records a gap against that policy for the lead to act on; it does not change the policy itself
(`deny.toml` is not edited here).

## Exceptions

| Component kind | Package | Version | Licence | Why acceptable today | What would close it |
|---|---|---|---|---|---|
| python | `numpy` | 2.5.3 | `0BSD` | `numpy` 2.5.3's `License-Expression` is `BSD-3-Clause AND 0BSD AND MIT AND Zlib AND CC0-1.0` — an SPDX `AND` of five licences, so **all five** must be allowed for the whole expression to pass. Four already are (`BSD-3-Clause`, `MIT`, `Zlib`, `CC0-1.0`); only `0BSD` (the zero-clause BSD licence — public-domain-equivalent, even more permissive than `BSD-3-Clause`, used by `numpy` for a handful of vendored/generated files) is missing from `deny.toml`'s allow list. Not a copyleft/BSL concern — `0BSD` is exactly the kind of licence ADR-003's allow-list policy exists to admit, just not yet added. | Add `"0BSD"` to `deny.toml`'s `[licenses].allow` (a one-line, lead-owned change) — the `AND` expression then evaluates fully allowed and this row is deleted. |
| python | `typing_extensions` | 4.16.0 | `PSF-2.0` | `typing_extensions` 4.16.0 declares `License-Expression = "PSF-2.0"` (the Python Software Foundation License 2.0 — a permissive, OSI-approved licence covering CPython's own standard-library-adjacent tooling). `typing_extensions` is a near-universal transitive dependency of this venv's typed Python stack (`fastapi`/`pydantic`/`anyio`/…); it was not previously in the allow list only because no prior licence audit of this venv had been done. | Add `"PSF-2.0"` to `deny.toml`'s `[licenses].allow` (a one-line, lead-owned change) and this row is deleted. |

## How this set was produced

`tests/test_sbom.py::test_every_licence_is_allowed_or_declared_an_exception` recomputes the
found set from the *committed* SBOM files on every run (never from this file) and compares it
against the two rows above; a package/version/licence combination that is not allowed by
`deny.toml` **and** not listed here fails the test by name. Both exceptions above were found by
the same mechanism that guards against a new one: `scripts/kit/sbom.py` records each package's
licence exactly as its own build/package metadata declares it (`cargo metadata`'s `license`
field for Rust, `importlib.metadata`'s `License-Expression`/`License` for Python), and
`scripts/kit/licences.py::evaluate_license_field` evaluates that string — normalised through its
small, explicit `FREE_TEXT_ALIASES` table for the handful of pre-PEP-639 Python packages that
still declare free text (e.g. `protobuf`'s `"3-Clause BSD License"` → `BSD-3-Clause`, `uvloop`'s
`"MIT License"` → `MIT`, both of which *do* pass — they are not exceptions) — against
`deny.toml`'s allow list.

Both exceptions above appear in **both** Python SBOMs (`av-viewer.cdx.json` and
`gmat-service.cdx.json`), because D2's Decision D reads Python packages from
`importlib.metadata.distributions()` over the one shared `.venv` for every Python component —
there is no per-component venv in this repository. This file declares each exception once, by
package/version/licence, rather than once per component that happens to report it.

No Rust package in any of the six Rust SBOMs, and no image component in either image SBOM, has
a non-allowed licence — the search covered all ten committed SBOMs, not only the two Python
ones.
