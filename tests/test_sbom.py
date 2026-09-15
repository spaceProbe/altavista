"""D2 (docs/p5-plan.md, P5 track round 1): tests for the CycloneDX SBOMs
(`docs/compliance/sbom/*.cdx.json`), the SPDX licence evaluator/allow-list check
(`scripts/kit/licences.py`), and the generator that produces both (`scripts/kit/sbom.py`).

Every test here reads already-committed files or (for the licence-allow-list and Cargo.lock
coverage checks) runs `cargo metadata --frozen --offline` -- cheap dependency-graph resolution,
never a compile. The one test that actually rebuilds a Rust binary
(`test_rust_sbom_regenerates_byte_identically`) is gated on `AV_SBOM_REBUILD=1` and skips
visibly, by name, otherwise (Decision G) -- see that test's own docstring, modelled directly on
`services/cfs/tests/test_image_reproducibility.py`'s `AV_CFS_RUN_REPRO_BUILD` gate.

No test here writes anywhere but `tmp_path`, and no test sets an environment variable (question
199) -- `os.environ` is only ever read.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tomllib
from collections import defaultdict, deque
from pathlib import Path
from typing import Optional

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SBOM_DIR = REPO_ROOT / "docs" / "compliance" / "sbom"
KIT_DIR = REPO_ROOT / "scripts" / "kit"
sys.path.insert(0, str(KIT_DIR))
import sbom  # noqa: E402  (path insert must precede this import)
import licences as L  # noqa: E402


# =================================================================================================
# 1. Every suite component (+ the two declared images) has a committed SBOM
# =================================================================================================

def test_every_suite_component_has_an_sbom():
    """The component list comes from `deploy/secdeploy/suite.altavista.toml` itself
    (`sbom.suite_component_names()`), not a hard-coded list here -- a component added to the
    suite without a matching SBOM fails this test. Images are the one explicit extra set,
    declared once (`sbom.IMAGE_COMPONENTS`)."""
    suite_components = sbom.suite_component_names()
    assert len(suite_components) == 8, f"expected 8 suite components, found {suite_components!r}"
    for name in suite_components:
        path = SBOM_DIR / f"{name}.cdx.json"
        assert path.is_file(), f"suite component {name!r} has no committed SBOM at {path}"
    for name in sbom.IMAGE_COMPONENTS:
        path = SBOM_DIR / f"{name}.cdx.json"
        assert path.is_file(), f"declared image {name!r} has no committed SBOM at {path}"


# =================================================================================================
# 2. Every committed SBOM is structurally valid CycloneDX
# =================================================================================================

@pytest.mark.parametrize("component", sbom.all_component_names())
def test_every_sbom_is_valid_cyclonedx(component):
    doc = json.loads((SBOM_DIR / f"{component}.cdx.json").read_text())
    assert doc["bomFormat"] == "CycloneDX"
    assert doc["specVersion"]
    assert doc["serialNumber"].startswith("urn:uuid:")
    assert "timestamp" in doc["metadata"]
    assert doc["metadata"]["component"]["name"] == component
    components = doc["components"]
    assert components, f"{component}: SBOM has no components at all"
    for c in components:
        assert c.get("name"), f"{component}: a component is missing 'name': {c!r}"
        assert c.get("version"), f"{component}: a component is missing 'version': {c!r}"
        assert c.get("type"), f"{component}: a component is missing 'type': {c!r}"
        has_licenses = bool(c.get("licenses"))
        has_no_licence_property = any(
            p.get("name") == sbom.PROP_NO_LICENCE for p in c.get("properties", [])
        )
        assert has_licenses or has_no_licence_property, (
            f"{component}: component {c['name']} {c['version']} has neither a 'licenses' entry "
            f"nor an explicit '{sbom.PROP_NO_LICENCE}' property"
        )


# =================================================================================================
# 3. SHA256SUMS matches the committed files exactly
# =================================================================================================

def test_sha256sums_matches_the_committed_files():
    sums_path = SBOM_DIR / "SHA256SUMS"
    lines = [l for l in sums_path.read_text().splitlines() if l.strip()]
    recorded = {}
    for line in lines:
        digest, _, rel = line.partition("  ")
        assert rel, f"malformed SHA256SUMS line (expected two-space separator): {line!r}"
        recorded[rel] = digest

    on_disk = {
        p.relative_to(REPO_ROOT).as_posix(): sbom.sha256_file(p)
        for p in SBOM_DIR.glob("*.cdx.json")
    }
    assert set(recorded) == set(on_disk), (
        f"SHA256SUMS entries do not match the *.cdx.json files present -- "
        f"missing from SHA256SUMS: {sorted(set(on_disk) - set(recorded))}, "
        f"stale entries (no longer on disk): {sorted(set(recorded) - set(on_disk))}"
    )
    for rel, expected_digest in recorded.items():
        actual_digest = on_disk[rel]
        assert actual_digest == expected_digest, (
            f"SHA256SUMS is stale for {rel}: recorded {expected_digest}, actual {actual_digest}"
        )


# =================================================================================================
# 4. Every licence in every committed SBOM is allowed outright (Decision E; question 214(a))
# =================================================================================================
# The prior carve-out file naming specific non-allowed packages is gone: question 214(a) (lead
# ruling, docs/open-questions.md) put `0BSD` and `PSF-2.0` into `deny.toml`'s `[licenses].allow`
# list itself, so the SBOM licence check now reads that list as its single source and nothing
# found in any committed SBOM needs anything declared separately any more.


def _all_licence_findings() -> list[L.LicenseFinding]:
    allow = L.load_allow_list(REPO_ROOT / "deny.toml")
    findings: list[L.LicenseFinding] = []
    for path in sorted(SBOM_DIR.glob("*.cdx.json")):
        component = path.stem.removesuffix(".cdx")
        doc = json.loads(path.read_text())
        for c in doc["components"]:
            for entry in c.get("licenses") or []:
                raw = entry.get("expression") or (entry.get("license") or {}).get("id") \
                    or (entry.get("license") or {}).get("name")
                findings.extend(
                    L.evaluate_license_field(component, c["name"], c["version"], raw, allow)
                )
    return findings


def _all_licence_combinations() -> set[tuple[str, str, str, str]]:
    """(component, package, version, licence) combinations actually evaluated across every
    committed SBOM -- counted separately from `_all_licence_findings` so the test below can
    print what it checked, not only that it came back clean (question 148)."""
    combos: set[tuple[str, str, str, str]] = set()
    for path in sorted(SBOM_DIR.glob("*.cdx.json")):
        component = path.stem.removesuffix(".cdx")
        doc = json.loads(path.read_text())
        for c in doc["components"]:
            for entry in c.get("licenses") or []:
                raw = entry.get("expression") or (entry.get("license") or {}).get("id") \
                    or (entry.get("license") or {}).get("name")
                if raw:
                    combos.add((component, c["name"], c["version"], raw))
    return combos


def test_every_licence_in_every_committed_sbom_is_allowed_outright():
    """Question 214(a): with `0BSD` and `PSF-2.0` now in `deny.toml`'s `[licenses].allow` list,
    every licence recorded in every committed SBOM is allowed by that list alone --
    `_all_licence_findings()` must be empty, with no declared-exception carve-out left anywhere.
    Prints the number of (component, package, version, licence) combinations actually evaluated,
    so this test shows what it checked rather than only that it was green (question 148)."""
    combos = _all_licence_combinations()
    findings = _all_licence_findings()
    sbom_count = len(list(SBOM_DIR.glob("*.cdx.json")))
    print(
        f"\nlicence check: {len(combos)} (component, package, version, licence) combinations "
        f"evaluated across {sbom_count} committed SBOMs, {len(findings)} finding(s)"
    )
    assert findings == [], "non-allowed licence(s) found in a committed SBOM:\n" + "\n".join(
        f"  component={f.component} package={f.package} version={f.version} "
        f"licence={f.licence!r} reason={f.reason}"
        for f in findings
    )


def test_a_licence_not_in_the_allow_list_is_still_reported_as_a_finding():
    """The gate stays meaningful in the failing direction too: a licence genuinely absent from
    `deny.toml`'s allow list must still come back as a `LicenseFinding`, not be silently
    accepted, now that the declared-exception carve-out is gone."""
    allow = L.load_allow_list(REPO_ROOT / "deny.toml")
    assert "GPL-3.0-only" not in allow
    findings = L.evaluate_license_field(
        "fake-component", "fake-package", "9.9.9", "GPL-3.0-only", allow
    )
    assert len(findings) == 1
    assert findings[0].licence == "GPL-3.0-only"
    assert findings[0].reason == "not-allowed"


def test_the_previously_excepted_licences_are_allowed_because_deny_toml_says_so():
    """Question 214(a): `0BSD` and `PSF-2.0` are allowed now because they are IN `deny.toml`'s
    allow list, not because the check itself was loosened. Proven by evaluating the real
    licence expressions -- `numpy` 2.5.3's real five-way `AND` and `typing_extensions` 4.16.0's
    real `PSF-2.0`, both as actually recorded in the committed SBOMs -- against a deliberately
    REDUCED COPY of the allow list, built in memory from `load_allow_list(...)` minus those two
    entries (`deny.toml` itself is never touched), and asserting they come back as findings
    there. This is the test that would catch someone silently deleting the two entries again."""
    allow = L.load_allow_list(REPO_ROOT / "deny.toml")
    assert "0BSD" in allow and "PSF-2.0" in allow  # the actual policy, sanity-checked first

    reduced = allow - {"0BSD", "PSF-2.0"}

    numpy_expr = "BSD-3-Clause AND 0BSD AND MIT AND Zlib AND CC0-1.0"
    numpy_findings_reduced = L.evaluate_license_field(
        "av-viewer", "numpy", "2.5.3", numpy_expr, reduced
    )
    assert {f.licence for f in numpy_findings_reduced} == {"0BSD"}

    typing_ext_findings_reduced = L.evaluate_license_field(
        "av-viewer", "typing_extensions", "4.16.0", "PSF-2.0", reduced
    )
    assert {f.licence for f in typing_ext_findings_reduced} == {"PSF-2.0"}

    # Against the REAL, committed deny.toml allow list, both are fully allowed -- no finding at
    # all -- because deny.toml says so, not because of any other loosening.
    assert L.evaluate_license_field("av-viewer", "numpy", "2.5.3", numpy_expr, allow) == []
    assert L.evaluate_license_field(
        "av-viewer", "typing_extensions", "4.16.0", "PSF-2.0", allow
    ) == []


def test_no_committed_sbom_contains_an_unparseable_licence_string():
    """D2-2: a prior revision put the raw, un-normalised metadata string straight into
    CycloneDX's `licenses[].expression` -- `docs/compliance/sbom/av-viewer.cdx.json` contained
    `"expression": "3-Clause BSD License"` for `protobuf`, which is not valid SPDX at all, even
    though `licences.FREE_TEXT_ALIASES` already knows it means `BSD-3-Clause`. Every licence
    string actually emitted into a committed SBOM must parse as SPDX (`licences.parse_spdx`) --
    a consumer of our own SBOM must never receive something it cannot parse."""
    bad = []
    for path in sorted(SBOM_DIR.glob("*.cdx.json")):
        doc = json.loads(path.read_text())
        for c in doc["components"]:
            for entry in c.get("licenses") or []:
                raw = entry.get("expression") or (entry.get("license") or {}).get("id") \
                    or (entry.get("license") or {}).get("name")
                try:
                    L.parse_spdx(raw)
                except L.SpdxParseError as exc:
                    bad.append(f"{path.name}: {c['name']} {c['version']} licence={raw!r} ({exc})")
    assert not bad, "unparseable licence string(s) emitted into a committed SBOM:\n" + "\n".join(bad)


# =================================================================================================
# 5. The SPDX evaluator, directly (Decision F)
# =================================================================================================

@pytest.fixture(scope="module")
def allow_list() -> frozenset[str]:
    return L.load_allow_list(REPO_ROOT / "deny.toml")


@pytest.mark.parametrize("expr", [
    "MIT OR Apache-2.0",
    "Apache-2.0 / MIT",
    "MIT/Apache-2.0",
    "(MIT OR Apache-2.0) AND Unicode-3.0",
    "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT",
])
def test_the_spdx_evaluator_allows_the_real_positive_expressions(expr, allow_list):
    result = L.evaluate_expression(expr, allow_list)
    assert result.satisfied, f"{expr!r} should be satisfied; unmet={result.unmet_leaves}"
    assert result.unmet_leaves == frozenset()


def test_the_spdx_evaluator_and_of_a_non_allowed_leaf_is_not_satisfied(allow_list):
    """A real-shaped five-way `AND` (numpy 2.5.3's own expression, with its one previously-
    non-allowed leaf swapped for a licence that is never in `deny.toml`'s allow list, now that
    question 214(a) put `0BSD` itself into that list -- see
    `test_the_previously_excepted_licences_are_allowed_because_deny_toml_says_so` for the real,
    now-fully-allowed `0BSD` expression)."""
    result = L.evaluate_expression(
        "BSD-3-Clause AND GPL-3.0-only AND MIT AND Zlib AND CC0-1.0", allow_list
    )
    assert result.satisfied is False
    assert result.unmet_leaves == frozenset({"GPL-3.0-only"})


@pytest.mark.parametrize("expr, expected_unmet", [
    ("GPL-3.0-only", frozenset({"GPL-3.0-only"})),
    ("MIT AND GPL-3.0-only", frozenset({"GPL-3.0-only"})),
])
def test_the_spdx_evaluator_negatives(expr, expected_unmet, allow_list):
    result = L.evaluate_expression(expr, allow_list)
    assert result.satisfied is False
    assert result.unmet_leaves == expected_unmet


def test_the_spdx_evaluator_with_exception_leaf_is_a_single_unit(allow_list):
    """`Apache-2.0 WITH LLVM-exception` must be evaluated as ONE leaf (its exact text is in the
    allow list verbatim) -- not as `Apache-2.0` with a dangling `WITH LLVM-exception`."""
    node = L.parse_spdx("Apache-2.0 WITH LLVM-exception")
    assert node == L.Leaf("Apache-2.0 WITH LLVM-exception")
    result = L.evaluate_expression("Apache-2.0 WITH LLVM-exception", allow_list)
    assert result.satisfied is True

    # A DIFFERENT exception on the same base licence is a different leaf and is NOT allowed
    # merely because "Apache-2.0" alone would be.
    result2 = L.evaluate_expression("Apache-2.0 WITH Some-Other-Exception", allow_list)
    assert result2.satisfied is False
    assert result2.unmet_leaves == frozenset({"Apache-2.0 WITH Some-Other-Exception"})


def test_the_spdx_evaluator_rejects_malformed_expressions(allow_list):
    for bad in ["", "(MIT", "MIT)", "AND MIT", "MIT AND", "MIT WITH"]:
        with pytest.raises(L.SpdxParseError):
            L.parse_spdx(bad)


# =================================================================================================
# 6. Rust SBOM <-> Cargo.lock coverage (the cheap drift detector)
# =================================================================================================

@pytest.fixture(scope="module")
def cargo_lock_packages() -> set[tuple[str, str]]:
    data = tomllib.loads((REPO_ROOT / "Cargo.lock").read_text())
    return {(p["name"], p["version"]) for p in data["package"]}


@pytest.fixture(scope="module")
def cargo_metadata_full() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--frozen", "--offline", "--format-version", "1"],
        cwd=REPO_ROOT, capture_output=True, text=True, check=True,
    )
    return json.loads(result.stdout)


def _rust_sbom_package_union() -> tuple[dict[str, set], set[tuple[str, str]]]:
    per_component = {}
    union = set()
    for component in sbom.RUST_BINARIES:
        doc = json.loads((SBOM_DIR / f"{component}.cdx.json").read_text())
        pkgs = {(c["name"], c["version"]) for c in doc["components"]}
        per_component[component] = pkgs
        union |= pkgs
    return per_component, union


def test_rust_sbom_versions_match_cargo_lock(cargo_lock_packages):
    _per_component, union = _rust_sbom_package_union()
    missing = union - cargo_lock_packages
    assert not missing, (
        f"Rust SBOM package(s) not present in Cargo.lock at the recorded version: {sorted(missing)}"
    )


def _classify_unaccounted_lock_package(
    nv: tuple[str, str], metadata: dict, host_reachable: set, any_target_reachable: set,
    dev_reachable: set, id_to_nv: dict, node_by_id: dict, workspace_members: set,
) -> str:
    """Explains why a Cargo.lock package is not linked by any of our six Rust binaries
    (Decision D: rust-audit-info is ground truth for what's actually linked), using
    `cargo metadata`'s own resolve graph -- never guessed."""
    ids = [pid for pid, v in id_to_nv.items() if v == nv]
    assert ids, f"{nv} not found in `cargo metadata`'s own package list at all"
    pid = ids[0]
    if pid in workspace_members:
        return "a separate workspace member/binary in its own right (not one of our six)"
    if pid in host_reachable:
        # Reachable via a normal/build edge with no target restriction from SOME workspace
        # member -- just not one of our six binaries. Name a direct parent for a useful message.
        parents = sorted(
            id_to_nv[n["id"]][0]
            for n in node_by_id.values()
            if n["id"] in host_reachable
            for dep in n.get("deps", [])
            if dep["pkg"] == pid
            and any(dk["kind"] in (None, "build") and dk["target"] is None for dk in dep["dep_kinds"])
        )
        parent = parents[0] if parents else "(no direct host-reachable parent found)"
        return f"not linked by any of our six binaries (pulled in by workspace crate {parent!r})"
    if pid in any_target_reachable:
        return "other-target (reachable only via a target-gated dependency edge)"
    if pid in dev_reachable:
        return "dev-only (reachable only via a dev-dependency edge)"
    return "UNEXPLAINED"  # deliberately not swallowed -- see the assertion below


def test_cargo_lock_packages_not_in_any_rust_sbom_are_all_accounted_for(
    cargo_lock_packages, cargo_metadata_full,
):
    """The converse direction of the coverage check: every `Cargo.lock` package absent from
    every Rust SBOM is accounted for by name -- dev-only, other-target, or pulled in only by a
    workspace crate outside our six binaries. If one cannot be explained, this test reports it
    rather than loosening."""
    _per_component, union = _rust_sbom_package_union()
    unaccounted = cargo_lock_packages - union

    packages = cargo_metadata_full["packages"]
    id_to_nv = {p["id"]: (p["name"], p["version"]) for p in packages}
    nodes = cargo_metadata_full["resolve"]["nodes"]
    node_by_id = {n["id"]: n for n in nodes}
    workspace_members = set(cargo_metadata_full["workspace_members"])

    def bfs(start_ids, predicate) -> set:
        seen = set(start_ids)
        q = deque(start_ids)
        while q:
            cur = q.popleft()
            node = node_by_id.get(cur)
            if not node:
                continue
            for dep in node.get("deps", []):
                if predicate(dep):
                    nid = dep["pkg"]
                    if nid not in seen:
                        seen.add(nid)
                        q.append(nid)
        return seen

    any_target_reachable = bfs(
        workspace_members,
        lambda dep: any(dk["kind"] in (None, "build") for dk in dep["dep_kinds"]),
    )
    host_reachable = bfs(
        workspace_members,
        lambda dep: any(dk["kind"] in (None, "build") and dk["target"] is None for dk in dep["dep_kinds"]),
    )
    dev_reachable = bfs(
        workspace_members,
        lambda dep: any(dk["kind"] in (None, "build", "dev") for dk in dep["dep_kinds"]),
    )

    reasons = {
        nv: _classify_unaccounted_lock_package(
            nv, cargo_metadata_full, host_reachable, any_target_reachable, dev_reachable,
            id_to_nv, node_by_id, workspace_members,
        )
        for nv in unaccounted
    }

    unexplained = {nv: r for nv, r in reasons.items() if r == "UNEXPLAINED"}
    assert not unexplained, (
        f"Cargo.lock package(s) not in any Rust SBOM and not explainable by the reachability "
        f"graph at all (dev-only / other-target / pulled in by a non-six workspace crate): "
        f"{sorted(unexplained)}"
    )

    # Print the full accounting (visible with `pytest -s`, and always in this task's own
    # acceptance evidence) -- not asserted further, since the counts legitimately shift as the
    # dependency tree changes; the load-bearing assertion is `not unexplained` above.
    by_reason = defaultdict(list)
    for nv, r in reasons.items():
        by_reason[r].append(nv)
    print(f"\nCargo.lock coverage: {len(cargo_lock_packages)} total, "
          f"{len(union)} in some Rust SBOM, {len(unaccounted)} unaccounted-for:")
    for reason, nvs in sorted(by_reason.items()):
        print(f"  {len(nvs)} x {reason}")
        for nv in sorted(nvs):
            print(f"      {nv}")


# =================================================================================================
# 7. The `altavista 0.1.0` duplicate proof
# =================================================================================================

@pytest.mark.parametrize("component", sbom.PYTHON_COMPONENTS)
def test_python_sbom_has_no_duplicate_packages(component):
    doc = json.loads((SBOM_DIR / f"{component}.cdx.json").read_text())
    keys = [(c["name"], c["version"]) for c in doc["components"]]
    assert len(keys) == len(set(keys)), f"{component}: duplicate (name, version) in components"

    altavista_entries = [k for k in keys if k[0] == "altavista"]
    assert altavista_entries == [("altavista", "0.1.0")], (
        f"{component}: expected exactly one 'altavista' 0.1.0 component (the venv reports it "
        f"twice -- egg-info and dist-info -- and the generator must dedupe it), found "
        f"{altavista_entries!r}"
    )


def test_dedupe_agrees_when_only_one_candidate_has_licence_text():
    """The real `altavista 0.1.0` shape: one distribution record has no licence text at all,
    the other has `Apache-2.0` -- nothing to disagree about, so the licensed one wins and no
    error is raised."""
    components = sbom._dedupe_python_distributions([
        ("altavista", "0.1.0", None, "/venv/.../altavista-0.1.0.dist-info"),
        ("altavista", "0.1.0", "Apache-2.0", "altavista.egg-info"),
    ])
    assert len(components) == 1
    assert components[0]["name"] == "altavista"
    assert components[0]["licenses"] == [{"license": {"id": "Apache-2.0"}}]


def test_dedupe_refuses_to_arbitrarily_pick_between_disagreeing_licence_texts():
    """D2-3: a fabricated pair for one `(name, version)` where BOTH candidates carry licence
    text and the texts genuinely disagree -- a prior revision broke this tie with a raw
    `dist._path` string comparison (absolute for one, relative for the other), which happened
    to give the right answer only because the real case has one candidate with no licence text
    at all. This must refuse, naming the package and both disagreeing values, rather than
    silently preferring whichever path sorts first."""
    with pytest.raises(sbom.DuplicatePythonDistributionError) as excinfo:
        sbom._dedupe_python_distributions([
            ("fabricated-pkg", "1.2.3", "MIT", "/abs/path/fabricated_pkg-1.2.3.dist-info"),
            ("fabricated-pkg", "1.2.3", "Apache-2.0", "fabricated_pkg.egg-info"),
        ])
    message = str(excinfo.value)
    assert "fabricated-pkg" in message and "1.2.3" in message
    assert "MIT" in message and "Apache-2.0" in message


# =================================================================================================
# 8. Determinism: two regenerations are byte-identical (the cheap generators)
# =================================================================================================

_CHEAP_COMPONENTS = list(sbom.PYTHON_COMPONENTS) + list(sbom.IMAGE_COMPONENTS)


@pytest.mark.parametrize("component", _CHEAP_COMPONENTS)
def test_two_generations_are_byte_identical(component, tmp_path):
    out_a = tmp_path / "a"
    out_b = tmp_path / "b"
    sbom.write_document(sbom.generate_one(component), out_a / f"{component}.cdx.json")
    sbom.write_document(sbom.generate_one(component), out_b / f"{component}.cdx.json")

    bytes_a = (out_a / f"{component}.cdx.json").read_bytes()
    bytes_b = (out_b / f"{component}.cdx.json").read_bytes()
    assert bytes_a == bytes_b, f"{component}: two regenerations differ"

    committed = (SBOM_DIR / f"{component}.cdx.json").read_bytes()
    assert bytes_a == committed, (
        f"{component}: regenerated SBOM differs from the committed file -- "
        f"docs/compliance/sbom/{component}.cdx.json is stale, run "
        f"`.venv/bin/python scripts/kit/sbom.py --out docs/compliance/sbom --component {component}`"
    )


@pytest.mark.parametrize("component", sbom.RUST_BINARIES)
def test_rust_sbom_never_records_the_binarys_own_hash(component):
    """D2-5 (measured, not assumed): a debug-profile macOS binary is NOT bit-reproducible
    across independent links of identical source -- three `cargo auditable build`s of the same
    binary with no source change between the first two produced three different SHA-256 hashes
    (Mach-O's per-link `LC_UUID`, plus other build-environment detail the debug profile embeds).
    A `metadata.component.hashes` entry naming the binary's own content hash therefore made
    `test_rust_sbom_regenerates_byte_identically` fail on every relink -- the artefact hash is a
    property of one specific build, not of the source that produced it, and its home is D3's
    KIT_MANIFEST, never this file. This guards against that field coming back silently. The two
    IMAGE components are NOT covered here and legitimately keep `hashes` -- a recorded image id
    or a recorded file hash there is read from a committed file (IMAGE_DIGEST.md /
    IMAGE_CONTEXT_MANIFEST.txt), not produced by a link on this host, so it IS reproducible."""
    doc = json.loads((SBOM_DIR / f"{component}.cdx.json").read_text())
    assert "hashes" not in doc["metadata"]["component"], (
        f"{component}: metadata.component still carries a 'hashes' entry -- a Rust binary's own "
        f"link is not bit-reproducible on this platform (see this test's own docstring), so this "
        f"must never come back"
    )


# =================================================================================================
# 9. The expensive Rust round-trip -- gated on AV_SBOM_REBUILD=1, skips visibly otherwise
# =================================================================================================

OPT_IN_VAR = "AV_SBOM_REBUILD"


def _opted_in() -> bool:
    return os.environ.get(OPT_IN_VAR, "").strip().lower() in ("1", "true", "yes")


def _compute_skip_reason() -> Optional[str]:
    if not _opted_in():
        return (
            f"{OPT_IN_VAR} is not set -- this test runs `cargo auditable build --offline` for "
            "all six Rust components (a real compile, tens of seconds to a few minutes) and "
            "confirms each regenerated SBOM is byte-identical to the committed one. The default "
            "suite skips it; set AV_SBOM_REBUILD=1 to opt in and actually run it."
        )
    return None


_SKIP_REASON = _compute_skip_reason()


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
@pytest.mark.parametrize("component", sbom.RUST_BINARIES)
def test_rust_sbom_regenerates_byte_identically(component, tmp_path):
    pkg, bin_name = sbom.RUST_BINARIES[component]
    doc = sbom.rust_binary_sbom(component, pkg, bin_name)
    out_path = tmp_path / f"{component}.cdx.json"
    sbom.write_document(doc, out_path)

    committed = (SBOM_DIR / f"{component}.cdx.json").read_bytes()
    assert out_path.read_bytes() == committed, (
        f"{component}: a fresh `cargo auditable build --offline` regeneration differs from the "
        f"committed SBOM -- docs/compliance/sbom/{component}.cdx.json is stale"
    )


# =================================================================================================
# 10. The epoch is never wall-clock
# =================================================================================================

@pytest.mark.parametrize("component", sbom.all_component_names())
def test_the_epoch_is_never_wall_clock(component):
    """Recomputes the git-derived epoch the generator itself would use and asserts the
    committed SBOM's `metadata.timestamp` equals it exactly -- a `datetime.now()` regression
    would make this fail on every run after the first."""
    doc = json.loads((SBOM_DIR / f"{component}.cdx.json").read_text())
    expected = sbom.git_epoch(sbom.epoch_paths_for(component))
    assert doc["metadata"]["timestamp"] == expected
    assert doc["serialNumber"] == sbom.derive_serial_number(component, expected)
