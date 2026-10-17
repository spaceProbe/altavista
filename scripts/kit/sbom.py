"""scripts/kit/sbom.py -- D2 (docs/compliance/sbom): deterministic CycloneDX SBOMs, one
generator behind one interface, for every suite component plus the two recorded container
images.

Stdlib only. Three generators (Decision D -- where each SBOM's package list comes from):

- `rust_binary_sbom` -- builds the named `[[bin]]` with `cargo auditable build --offline`,
  reads the linked package set back with `rust-audit-info` (ground truth for what the binary
  actually links -- it excludes crates that exist in `Cargo.lock` only for another target, e.g.
  the UEFI-only `r-efi`), and joins licences from `cargo metadata --frozen --offline` by exact
  `(name, version)`. Each package's `kind` (runtime vs build-only) is recorded as a CycloneDX
  `property`. The linked binary's own SHA-256 is deliberately NOT recorded (D2-5: a debug-profile
  macOS binary is not bit-reproducible across independent links of identical source, measured
  directly -- see `rust_binary_sbom`'s own comment); the cargo package/bin/profile and the
  `rust-audit-info` version are recorded as properties instead, all of which ARE reproducible.
- `python_dist_sbom` -- packages from `importlib.metadata.distributions()` over the running
  interpreter's own `.venv` (so this must be run with `.venv/bin/python`), deduplicated by
  `(name, version)` -- this venv reports `altavista 0.1.0` twice (a stale, gitignored
  `altavista.egg-info` left on `sys.path` from an older install, and the real `.dist-info`);
  the candidate carrying real license metadata wins a tie, broken deterministically otherwise.
- `image_sbom` -- from the two components' own committed, hand-maintained records
  (`IMAGE_DIGEST.md`, `IMAGE_CONTEXT_MANIFEST.txt`), never by running docker or reading a live
  daemon. `services/edge-plugin/IMAGE_DIGEST.md` has one clean "most recent build" section this
  reads directly; `services/cfs/IMAGE_DIGEST.md` is a long changelog with no single current-
  state table, so this reads the structured, always-regenerated header lines of
  `IMAGE_CONTEXT_MANIFEST.txt` instead ("Built image ID for this manifest" / "Runtime-content
  hash for this build") rather than parsing changelog prose -- and records what it cannot find
  there (the base image digest) as a declared gap property instead of guessing.

Decision H (the build epoch, never wall-clock): `metadata.timestamp` is the committer date
(`git log -1 --format=%cI`, converted to UTC and printed with a literal `Z`) of the last commit
touching that component's own inputs -- never `datetime.now()`, so the same inputs always
produce the same SBOM. `serialNumber` is likewise derived, not `uuid4()`: a SHA-256 over the
component name and its own epoch, reformatted into a UUID's canonical hex grouping. This is NOT
a real random UUID -- it borrows the `urn:uuid:` syntax CycloneDX's `serialNumber` expects while
staying a pure, reproducible function of (component, epoch), which is what makes two
regenerations of the same inputs byte-identical (`test_two_generations_are_byte_identical`).

CLI: `python scripts/kit/sbom.py --out docs/compliance/sbom [--component NAME]...` writes one
`<out>/<name>.cdx.json` per requested component (default: every component) and rewrites
`<out>/SHA256SUMS`.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tomllib
from collections import defaultdict
from datetime import datetime, timezone
from functools import lru_cache
from importlib import metadata as importlib_metadata
from pathlib import Path
from typing import Iterable, Optional

import licences  # scripts/kit/licences.py -- the one source of truth for FREE_TEXT_ALIASES
                  # (D2-2: the SBOM must never emit a licence string licences.py itself cannot
                  # parse as SPDX, so this module normalises through the SAME alias table
                  # licences.py's own allow-list check uses, never a second copy of it).

REPO_ROOT = Path(__file__).resolve().parents[2]

BOM_FORMAT = "CycloneDX"
SPEC_VERSION = "1.5"
BOM_VERSION = 1

PROP_PREFIX = "altavista:sbom:"
PROP_AUDIT_KIND = PROP_PREFIX + "audit-kind"
PROP_SOURCE = PROP_PREFIX + "source"
PROP_NO_LICENCE = PROP_PREFIX + "no-licence-metadata"
PROP_GAP = PROP_PREFIX + "gap"

# --- Component registries (Decision G / deliverable 6: "Images are an explicit extra set,
# declared in one place"; the eight suite components are never hard-coded -- they come from
# deploy/secdeploy/suite.altavista.toml, see `suite_component_names()` below). ------------------

#: Rust components: package name (`-p`) and `[[bin]]` target name (`--bin`), confirmed against
#: each crate's own `Cargo.toml` (question in this task's own brief: "Confirm each crate/bin
#: name against Cargo.toml before you build; do not guess" -- caught one real mismatch, see
#: "av-dynamics-service" below).
RUST_BINARIES: dict[str, tuple[str, str]] = {
    "av-ingest": ("av-ingest", "av-ingest-server"),
    "av-command": ("av-command", "av-command"),
    "av-gateway": ("av-gateway", "av-gateway"),
    "av-proposer": ("av-proposer", "av-proposer"),
    # The brief named the bin target "server" (matching the source file path,
    # src/bin/server.rs) -- `cargo build --bin server` actually fails ("no bin target named
    # `server`"). crates/av-dynamics-service/Cargo.toml's own [[bin]] table names the target
    # "av-dynamics-service" (only the *path* is src/bin/server.rs); corrected here. See this
    # task's final report for the exact command that surfaced the mismatch.
    "av-dynamics-service": ("av-dynamics-service", "av-dynamics-service"),
    "av-edge-plugin": ("av-ingest-client", "av-edge-plugin"),
}

#: A Rust SBOM's content (`rust_binary_sbom` below) comes from `cargo auditable`'s embedded
#: dependency data plus `cargo metadata`'s licence map -- both are pure functions of the
#: *resolved dependency graph*: `Cargo.lock` and the workspace's and every crate's own
#: `Cargo.toml` manifest (features/dependencies/version). A `.rs` source file is never read by
#: either step, so it cannot change what either produces. Whole-directory `"crates/"` (its `.rs`
#: sources included) was an epoch input until this was measured to be the same class of defect
#: D2-1 already fixed for the Python components below: any commit touching any crate's source
#: -- with no dependency-graph change at all -- made every committed Rust SBOM go stale
#: immediately, a gate failure nobody caused (see docs/compliance/sbom/README.md's
#: "Determinism" section for the measured before/after commit). `"crates/*/Cargo.toml"` is a
#: git pathspec matching every crate's own manifest directly (confirmed against this tree:
#: `git ls-files -- 'crates/*/Cargo.toml'` lists exactly the 18 crate `Cargo.toml` files, no
#: `.rs` file, no nesting deeper than `crates/<name>/Cargo.toml` in this workspace) -- so all
#: six Rust SBOMs still share one epoch, now over only the paths that can actually change one.
#:
#: The other half of the rule (measured for real by the AI-plane round 5 merge, commit
#: `db1e858`, "bump the workspace to `rust-version = "1.87"`", landing on `edge` at `3bcbd63`):
#: because all six Rust components share this ONE path set, a commit that edits the workspace
#: `Cargo.toml` -- or any crate's own `Cargo.toml`, or `Cargo.lock` -- legitimately moves EVERY
#: Rust component's epoch AT ONCE. `db1e858`'s `rust-version` bump touched no `.rs` file and
#: added no dependency, yet correctly moved all six committed SBOMs' epoch from
#: `2026-09-15T12:30:33Z` to `2026-09-15T18:02:25Z`, because a workspace manifest is a real input
#: to the resolved dependency graph and the MSRV `cargo auditable`/`cargo metadata` read -- this
#: is the rule working, not the whole-directory-`crates/` treadmill defect described above (a
#: `.rs`-only commit still must NOT move the epoch; `tests/test_sbom.py::
#: test_rust_epoch_paths_match_no_rust_source_file` guards that). Operationally: any commit that
#: edits `Cargo.lock`, the workspace `Cargo.toml`, or a crate's own `Cargo.toml` must be followed
#: by (or accompanied by, in a separate later commit) an SBOM regeneration
#: (`docs/compliance/sbom/README.md`'s "Regenerating" section) -- `tests/test_sbom.py::
#: test_the_epoch_is_never_wall_clock` is what enforces that; it fails loudly, for exactly that
#: reason, the moment such a commit lands without one.
RUST_EPOCH_PATHS = ["Cargo.lock", "Cargo.toml", "crates/*/Cargo.toml"]

#: Python components. Both read the IDENTICAL declared package set -- question 224's own fix:
#: `scripts/kit/python-lock.json` (`scripts/kit/python_lock.py`'s committed output), never the
#: live worktree `.venv` directly any more (D2-4's original text described the venv itself as
#: the source; that was the defect question 224 found and this fixes -- see `python_dist_sbom`'s
#: own doc below). A Python SBOM's *content* still cannot be changed by editing `altavista/**` or
#: `services/gmat-service/**`; now it is `pyproject.toml` (which declares the roots
#: `python_lock.py` resolves from) AND `scripts/kit/python-lock.json` (the resolved, pinned,
#: licence-bearing result) that can change it. `PYTHON_EPOCH_PATHS` below reflects that directly
#: (D2-1): a component's own source paths are deliberately NOT part of the epoch -- including
#: them made the committed SBOM go stale the instant a later, unrelated commit touched
#: `altavista/server.py` or similar, which is a gate failure nobody caused, since it can never
#: change which distribution is installed.
PYTHON_COMPONENTS: tuple[str, ...] = ("av-viewer", "gmat-service")

#: D2-1: every Python component's epoch is `pyproject.toml` (the declared roots) plus
#: `scripts/kit/python-lock.json` (the resolved, pinned, committed declared set question 224's
#: fix reads instead of the live venv) -- see `PYTHON_COMPONENTS`'s own comment for why
#: per-component source paths were removed from this list, and `scripts/kit/python_lock.py`'s
#: own module doc for why the lock file is a real SBOM input now, not merely a cache of the venv.
PYTHON_EPOCH_PATHS: list[str] = ["pyproject.toml", "scripts/kit/python-lock.json"]

#: The two recorded container images (Decision G/D2 deliverable 6's "declared in one place").
IMAGE_COMPONENTS: tuple[str, ...] = ("edge-plugin-image", "cfs-image")

#: Per-image epoch paths (Decision H: "for an image its IMAGE_DIGEST.md (and
#: IMAGE_CONTEXT_MANIFEST.txt where one exists)") -- named once here so the generator and
#: `tests/test_sbom.py::test_the_epoch_is_never_wall_clock` can never drift apart.
IMAGE_EPOCH_PATHS: dict[str, list[str]] = {
    "edge-plugin-image": ["services/edge-plugin/IMAGE_DIGEST.md"],
    "cfs-image": ["services/cfs/IMAGE_DIGEST.md", "services/cfs/IMAGE_CONTEXT_MANIFEST.txt"],
}


def epoch_paths_for(component: str) -> list[str]:
    """The exact repo-relative paths `git_epoch` is called with for `component` -- exposed so
    tests can recompute the expected epoch without re-typing these lists a second time."""
    if component in RUST_BINARIES:
        return RUST_EPOCH_PATHS
    if component in PYTHON_COMPONENTS:
        return PYTHON_EPOCH_PATHS
    if component in IMAGE_COMPONENTS:
        return IMAGE_EPOCH_PATHS[component]
    raise ValueError(f"unknown component: {component!r}")


def suite_component_names() -> list[str]:
    """The eight suite component names, read from `deploy/secdeploy/suite.altavista.toml`
    itself (never hard-coded) -- so a component added there without a matching SBOM is a test
    failure, not a silent gap."""
    data = tomllib.loads((REPO_ROOT / "deploy" / "secdeploy" / "suite.altavista.toml").read_text())
    return list(data["components"].keys())


def all_component_names() -> list[str]:
    return suite_component_names() + list(IMAGE_COMPONENTS)


# --- Small shared helpers ----------------------------------------------------------------------

def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def git_epoch(paths: list[str]) -> str:
    """Decision H: the committer date of the last commit touching `paths` (repo-relative),
    formatted as UTC ISO-8601 with a literal 'Z' -- never `datetime.now()`, so the SBOM's
    timestamp is a pure function of already-committed history and stays identical across
    regenerations until one of `paths` actually changes."""
    result = subprocess.run(
        ["git", "log", "-1", "--format=%cI", "--", *paths],
        cwd=REPO_ROOT, capture_output=True, text=True, check=True,
    )
    raw = result.stdout.strip()
    if not raw:
        raise RuntimeError(f"`git log` produced no committer date for paths={paths!r}")
    dt = datetime.fromisoformat(raw).astimezone(timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def derive_serial_number(component_name: str, epoch: str) -> str:
    """Decision H: deterministic, not `uuid4()` -- a SHA-256 over the component name and its
    own epoch, reformatted into a UUID's canonical 8-4-4-4-12 hex grouping. This is NOT a real
    random UUID: it borrows only the `urn:uuid:` *syntax* CycloneDX's `serialNumber` field
    expects, while staying a pure, reproducible function of (component, epoch) -- two
    regenerations of the same inputs must produce the same serial number, which a real
    `uuid.uuid4()` could never do."""
    digest = hashlib.sha256(f"{component_name}:{epoch}".encode("utf-8")).hexdigest()
    hex32 = digest[:32]
    grouped = f"{hex32[0:8]}-{hex32[8:12]}-{hex32[12:16]}-{hex32[16:20]}-{hex32[20:32]}"
    return f"urn:uuid:{grouped}"


@lru_cache()
def _workspace_version() -> str:
    data = tomllib.loads((REPO_ROOT / "Cargo.toml").read_text())
    return data["workspace"]["package"]["version"]


@lru_cache()
def _python_project_version() -> str:
    data = tomllib.loads((REPO_ROOT / "pyproject.toml").read_text())
    return data["project"]["version"]


_SIMPLE_LICENSE_ID_RE = re.compile(r"[A-Za-z0-9.+-]+")


def _license_field(raw: Optional[str]) -> tuple[Optional[list[dict]], list[dict]]:
    """Builds a CycloneDX `licenses` array entry from a raw licence string, or -- when there is
    none -- an explicit `no licence metadata` property instead (never a silent hole; every
    component test_every_sbom_is_valid_cyclonedx checks has one or the other). A single simple
    SPDX-looking token (no spaces/parens/slash) becomes a `{"license": {"id": ...}}` entry; a
    compound expression becomes a `{"expression": ...}` entry, both valid CycloneDX
    `LicenseChoice` shapes. This module deliberately does NOT evaluate the licence against
    deny.toml's allow list itself -- that is scripts/kit/licences.py's job, run separately
    against the already-generated, committed SBOM files (tests/test_sbom.py)."""
    if not raw:
        return None, [{"name": PROP_NO_LICENCE, "value": "true"}]
    if _SIMPLE_LICENSE_ID_RE.fullmatch(raw):
        return [{"license": {"id": raw}}], []
    return [{"expression": raw}], []


def _assemble_document(component_name: str, meta_component: dict, components: list[dict], epoch: str) -> dict:
    return {
        "bomFormat": BOM_FORMAT,
        "specVersion": SPEC_VERSION,
        "serialNumber": derive_serial_number(component_name, epoch),
        "version": BOM_VERSION,
        "metadata": {"timestamp": epoch, "component": meta_component},
        "components": sorted(components, key=lambda c: (c["name"], c["version"])),
    }


def write_document(doc: dict, out_path: Path) -> None:
    """`json.dump` with `indent=2, sort_keys=True, ensure_ascii=False`, plus a trailing
    newline -- Decision G's exact, deterministic output format. `sort_keys` only reorders each
    JSON *object's* keys; the `components` array's own order (already sorted by (name,
    version)) is untouched."""
    out_path.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    out_path.write_text(text, encoding="utf-8")


# =================================================================================================
# 1. Rust binaries -- cargo auditable build + rust-audit-info + cargo metadata
# =================================================================================================

@lru_cache()
def _cargo_metadata_license_map() -> dict[tuple[str, str], Optional[str]]:
    result = subprocess.run(
        ["cargo", "metadata", "--frozen", "--offline", "--format-version", "1"],
        cwd=REPO_ROOT, capture_output=True, text=True, check=True,
    )
    data = json.loads(result.stdout)
    return {(p["name"], p["version"]): p.get("license") for p in data["packages"]}


def _cargo_auditable_build(pkg: str, bin_name: str) -> Path:
    """Runs `cargo auditable build --offline -p <pkg> --bin <bin_name>` and returns the
    directory the resulting binary lands in. Respects `CARGO_TARGET_DIR` from the caller's own
    environment (never set by this module) -- pass a scratch directory there to keep the
    repo's own `target/` untouched, exactly as this task's brief instructs for its own
    acceptance-evidence builds."""
    env = os.environ.copy()
    cmd = ["cargo", "auditable", "build", "--offline", "-p", pkg, "--bin", bin_name]
    result = subprocess.run(cmd, cwd=REPO_ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(
            f"`{' '.join(cmd)}` failed (exit {result.returncode}):\n{result.stdout}\n{result.stderr}"
        )
    target_dir = Path(env.get("CARGO_TARGET_DIR", REPO_ROOT / "target"))
    if not target_dir.is_absolute():
        target_dir = REPO_ROOT / target_dir
    return target_dir / "debug"


def _rust_audit_info(bin_path: Path) -> dict:
    result = subprocess.run(
        ["rust-audit-info", str(bin_path)], capture_output=True, text=True, check=True,
    )
    return json.loads(result.stdout)


#: `rust-audit-info` has no `--version`/`-V` flag (it treats every argument as a binary path to
#: read, confirmed: `rust-audit-info --version` prints "Failed to read the binary: No such file
#: or directory") -- the installed version is read once from `cargo install --list` and pinned
#: here as a constant rather than queried at generation time. Recorded as a property (D2-5)
#: alongside the cargo package/bin/profile identifying WHAT was measured, now that the binary's
#: own content hash (which measured a specific, non-reproducible link) is gone.
RUST_AUDIT_INFO_VERSION = "0.5.4"

PROP_CARGO_PACKAGE = PROP_PREFIX + "cargo-package"
PROP_CARGO_BIN = PROP_PREFIX + "cargo-bin"
PROP_CARGO_PROFILE = PROP_PREFIX + "cargo-profile"
PROP_RUST_AUDIT_INFO_VERSION = PROP_PREFIX + "rust-audit-info-version"


def _rust_component(name: str, version: str, source: Optional[str], kind: Optional[str],
                     license_raw: Optional[str]) -> dict:
    props = [{"name": PROP_AUDIT_KIND, "value": kind or "runtime"}]
    if source:
        props.append({"name": PROP_SOURCE, "value": source})
    licenses, extra_props = _license_field(license_raw)
    props.extend(extra_props)
    props.sort(key=lambda p: (p["name"], p["value"]))
    comp = {"type": "library", "name": name, "version": version, "properties": props}
    if licenses is not None:
        comp["licenses"] = licenses
    return comp


def rust_binary_sbom(component: str, pkg: str, bin_name: str) -> dict:
    profile_dir = _cargo_auditable_build(pkg, bin_name)
    bin_path = profile_dir / bin_name
    audit = _rust_audit_info(bin_path)
    license_map = _cargo_metadata_license_map()
    epoch = git_epoch(RUST_EPOCH_PATHS)

    components: list[dict] = []
    seen: set[tuple[str, str]] = set()
    for p in audit["packages"]:
        key = (p["name"], p["version"])
        if key in seen:
            continue
        seen.add(key)
        components.append(_rust_component(p["name"], p["version"], p.get("source"), p.get("kind"),
                                            license_map.get(key)))

    # D2-5 (measured, not assumed): a debug-profile macOS binary is NOT bit-reproducible across
    # independent links of the identical source -- confirmed by building `av-ingest-server`
    # three times into the same target dir with no source change between the first two and
    # getting three different SHA-256 hashes (Mach-O's per-link `LC_UUID` load command, plus
    # other build-environment detail the debug profile embeds). An SBOM's job is the dependency
    # composition, which genuinely IS reproducible (Cargo.lock pins every version); the linked
    # artefact's own content hash is a property of one specific build, not of the source that
    # produced it, and belongs with D3's KIT_MANIFEST (which records every shipped file's real
    # SHA-256 beside the images' own recorded digests) -- never here, where it silently defeats
    # `test_rust_sbom_regenerates_byte_identically` on every relink. What IS reproducible, and is
    # recorded instead: which cargo package/bin/profile this SBOM describes, and the
    # `rust-audit-info` version that read it back.
    meta_component = {
        "type": "application",
        "name": component,
        "version": _workspace_version(),
        "properties": [
            {"name": PROP_CARGO_PACKAGE, "value": pkg},
            {"name": PROP_CARGO_BIN, "value": bin_name},
            {"name": PROP_CARGO_PROFILE, "value": "debug"},
            {"name": PROP_RUST_AUDIT_INFO_VERSION, "value": RUST_AUDIT_INFO_VERSION},
        ],
    }
    return _assemble_document(component, meta_component, components, epoch)


# =================================================================================================
# 2. Python distributions -- the committed declared set (scripts/kit/python-lock.json), the live
#    .venv used only as a cross-check (question 224)
# =================================================================================================
#
# Question 224's finding: a prior revision of this generator called
# `importlib.metadata.distributions()` directly over the running interpreter's own `.venv`, so a
# Python SBOM's package list, versions, and licences were a property of THAT MACHINE's venv --
# the reconciliation worker's worktree venv lacked `setuptools` and carried a stale `altavista`
# dist-info with no licence metadata, so it wrote `setuptools:no-longer-there` and
# `altavista:no-licence-metadata`, while the verification clone's healthy venv regenerated the
# SAME two files differently. A document that changes with the machine it is generated on is not
# the reproducible SBOM D2 promised.
#
# The fix: `python_dist_sbom` below reads `scripts/kit/python-lock.json`
# (`scripts/kit/python_lock.py`'s committed output -- see that module's own doc for what it is
# and how it is refreshed) for the package list, version, and licence every Python SBOM records.
# The live `.venv` is still read (`_venv_representative_versions`), but ONLY to cross-check: if
# a declared package is missing from the venv, or a version disagrees, `python_dist_sbom` raises
# `PythonSbomCrossCheckError` naming exactly what differs, rather than silently writing whichever
# it found (this is the fix for BOTH shapes of the venv the lead found broken: a venv missing
# `setuptools` now fails loudly instead of silently omitting it; a stale extra `altavista`
# dist-info/egg-info -- same name and version either way -- is tolerated, because the SBOM's
# `altavista` entry no longer comes from that dist-info at all).

PROP_LICENCE_RAW = PROP_PREFIX + "licence-raw"
PROP_PYTHON_PACKAGES_SOURCE = PROP_PREFIX + "python-packages-source"

#: D2-4, updated for question 224: every Python component's `metadata.component` carries this
#: verbatim, so a reader of one SBOM file alone (not this module's source) learns that the
#: package list is the committed lock file's declared set, cross-checked against (never sourced
#: from) the live venv -- av-viewer and gmat-service enumerate the identical declared set today,
#: by construction, because there is one `scripts/kit/python-lock.json` for both.
PYTHON_PACKAGES_SOURCE_NOTE = (
    "this component's package list is the committed declared set in "
    "scripts/kit/python-lock.json (pyproject.toml's [project].dependencies and "
    "[project.optional-dependencies].dev, resolved and pinned by scripts/kit/python_lock.py) -- "
    "the live worktree .venv is read only to cross-check the declared set against what is "
    "actually installed (a disagreement raises PythonSbomCrossCheckError rather than silently "
    "writing whichever was found); there is no per-component venv or lock file in this "
    "repository, so every Python component's SBOM enumerates the identical declared set"
)

#: Question 224: packages a venv always carries as installer/bootstrap tooling, never as a
#: consequence of anything `pyproject.toml` declares (confirmed directly: `pip show pip` in this
#: worktree's own healthy `.venv` reports no `Required-by` at all -- nothing in this project's
#: dependency graph, runtime or `dev`, depends on `pip` itself; `python -m venv`/`pip install`
#: put it there regardless). Present in the live venv but absent from the declared set is
#: EXPECTED for exactly this name, not a cross-check disagreement -- everything else absent from
#: the declared set (including every `dev`-only package, which the declared set already includes
#: via `python_lock.declared_roots`'s own `dev` extra roots) still disagrees.
PYTHON_CROSS_CHECK_IGNORED: frozenset[str] = frozenset({"pip"})


class DuplicatePythonDistributionError(RuntimeError):
    """D2-3: raised when `importlib.metadata.distributions()` reports two records for the same
    `(name, version)` whose licence text genuinely disagrees. A prior revision of this module
    broke that tie with a path-string comparison -- deterministic on any one venv layout, but an
    absolute-vs-relative path is not a property anyone should rely on, and if the two disagreeing
    texts ever differ in meaning (not just presence), picking one arbitrarily would be a silent,
    machine-dependent choice. Refusing is the right answer; the caller decides what to do about
    a real conflict, which none has been found to be through this task's own generation runs."""


class PythonSbomCrossCheckError(RuntimeError):
    """Question 224's typed failure: raised by `_cross_check_python_packages` when the committed
    declared set (`scripts/kit/python-lock.json`) and the live worktree `.venv` disagree -- a
    declared package missing from the venv, a version mismatch, or an installed package the
    declared set does not name (`PYTHON_CROSS_CHECK_IGNORED` is the one named exception: venv
    bootstrap tooling no declared dependency ever pulls in). The message lists every
    disagreement found, each naming the exact package, which side, and which version(s) --
    never just "the venv is wrong" -- so it is directly actionable: either the lock file is
    stale (refresh it: `python scripts/kit/python_lock.py --refresh`, on a venv you have
    checked is healthy) or this venv is missing something `pip install -e ".[dev]"` should have
    installed."""


def _dist_license_text(dist: importlib_metadata.Distribution) -> Optional[str]:
    md = dist.metadata
    return md.get("License-Expression") or md.get("License") or None


def _dedupe_python_distributions(
    records: Iterable[tuple[str, str, Optional[str], str]],
) -> list[dict]:
    """The pure, directly-testable core of `python_dist_sbom`'s dedupe (D2-3). `records` is
    `(name, version, license_text, path_str)` -- `path_str` is used only to name a source in
    `DuplicatePythonDistributionError`'s message, never to break a tie: when every record for a
    `(name, version)` that carries licence text agrees, that text is used (this is the real
    `altavista 0.1.0` case -- one record has no licence text at all, the other has
    `Apache-2.0`, so there is nothing to disagree about); when none carries licence text, the
    component gets none (Decision E's "no licence metadata" property, not a hole); when two
    carry DIFFERENT non-empty text, this raises rather than picking one by path string."""
    groups: dict[tuple[str, str], list[tuple[Optional[str], str]]] = defaultdict(list)
    for name, version, license_text, path_str in records:
        groups[(name, version)].append((license_text, path_str))

    components = []
    for (name, version), candidates in sorted(groups.items()):
        licensed = sorted({(lic, path) for lic, path in candidates if lic})
        distinct_licences = sorted({lic for lic, _path in licensed})
        if len(distinct_licences) > 1:
            detail = "; ".join(f"{lic!r} (from {path})" for lic, path in licensed)
            raise DuplicatePythonDistributionError(
                f"{name} {version}: distribution records disagree on licence -- refusing to "
                f"pick one arbitrarily: {detail}"
            )
        chosen_license = licensed[0][0] if licensed else None
        components.append(_python_component(name, version, chosen_license))
    return components


def _normalize_python_dist_name(name: str) -> str:
    """Same normalisation `scripts/kit/python_lock.py::_normalize_dist_name` applies when
    grouping the lock file's roots -- used here only to compare a declared name against an
    installed name for the cross-check (`typing_extensions` vs `typing-extensions`-shaped
    differences must not read as a disagreement); the SBOM itself always emits the name exactly
    as recorded (lock file for the declared package list, live dist metadata for nothing --
    the live venv is never a naming source any more)."""
    return name.lower().replace("_", "-")


@lru_cache()
def _load_python_lock() -> list[dict]:
    """Reads `scripts/kit/python-lock.json` (`scripts/kit/python_lock.py`'s committed output --
    see that module's own doc). This is the ONLY place `python_dist_sbom` gets its package list,
    versions, and licences from now (question 224) -- no network, no live venv scan; a plain,
    already-committed file read, identical on any machine that has checked out this repository."""
    lock_path = REPO_ROOT / "scripts" / "kit" / "python-lock.json"
    if not lock_path.is_file():
        raise RuntimeError(
            f"{lock_path} is missing -- run `python scripts/kit/python_lock.py --refresh` "
            f"first (on a venv installed with `pip install -e \".[dev]\"`), then commit the "
            f"result; scripts/kit/sbom.py no longer reads the live venv for the Python "
            f"components' package list directly (question 224)"
        )
    data = json.loads(lock_path.read_text(encoding="utf-8"))
    return data["packages"]


def _venv_representative_versions() -> dict[str, tuple[str, str]]:
    """Normalised package name -> (name as installed, version), over the live worktree venv,
    deduplicated with the EXACT SAME logic `_dedupe_python_distributions` already applies for
    the SBOM's own package list (D2-3: the known `altavista` dist-info/egg-info duplicate, and
    the same refusal if two installed records for one `(name, version)` genuinely disagree on
    licence text) -- reused, not re-implemented, so this cross-check's notion of "what is
    installed" can never silently drift from the generator's own. Used only by
    `_cross_check_python_packages`; never a source for the SBOM's own components."""
    records = []
    for dist in importlib_metadata.distributions():
        name = dist.metadata.get("Name")
        if not name:
            continue
        path_str = str(getattr(dist, "_path", id(dist)))
        records.append((name, dist.version, _dist_license_text(dist), path_str))
    components = _dedupe_python_distributions(records)
    return {
        _normalize_python_dist_name(c["name"]): (c["name"], c["version"]) for c in components
    }


def _cross_check_python_packages(component: str, declared: list[dict]) -> None:
    """Question 224's typed cross-check: compares the committed declared set (`declared`, from
    `_load_python_lock`) against the live worktree `.venv`
    (`_venv_representative_versions`) and raises `PythonSbomCrossCheckError`, naming every
    disagreement, if they do not match exactly (modulo `PYTHON_CROSS_CHECK_IGNORED`). Two
    directions, both checked: a declared package missing from the venv (or installed at a
    different version) -- this is the `setuptools`-shaped defect question 224 found, now a loud,
    named failure instead of a silently short SBOM; and a venv package the declared set never
    names at all (dev-only packages are NOT this case -- they are part of the declared set via
    `python_lock.declared_roots`'s own `dev` extra roots, so their presence is expected, not a
    disagreement to report)."""
    declared_by_key = {_normalize_python_dist_name(p["name"]): p for p in declared}
    installed = _venv_representative_versions()

    problems: list[str] = []
    for key, pkg in sorted(declared_by_key.items()):
        name, version = pkg["name"], pkg["version"]
        if key not in installed:
            problems.append(
                f"{name} {version}: declared in scripts/kit/python-lock.json but not installed "
                f"in this .venv"
            )
            continue
        installed_name, installed_version = installed[key]
        if installed_version != version:
            problems.append(
                f"{name}: scripts/kit/python-lock.json declares {version}, but this .venv has "
                f"{installed_name} {installed_version} installed"
            )
    for key, (installed_name, installed_version) in sorted(installed.items()):
        if key in PYTHON_CROSS_CHECK_IGNORED:
            continue
        if key not in declared_by_key:
            problems.append(
                f"{installed_name} {installed_version}: installed in this .venv but not "
                f"declared in scripts/kit/python-lock.json"
            )

    if problems:
        raise PythonSbomCrossCheckError(
            f"{component}: the declared Python package set (scripts/kit/python-lock.json) "
            f"disagrees with the live .venv -- refusing to generate an SBOM whose content would "
            f"depend on which machine generated it:\n"
            + "\n".join(f"  - {p}" for p in problems)
        )


def python_dist_sbom(component: str) -> dict:
    """Question 224: the package list, versions, and licences come from the committed
    `scripts/kit/python-lock.json` (`_load_python_lock`), never from scanning the live venv --
    the live venv (`_cross_check_python_packages`) is read only to confirm it agrees with that
    declared set, and raises a typed, named `PythonSbomCrossCheckError` if it does not, rather
    than silently writing whichever the venv happened to have."""
    declared = _load_python_lock()
    _cross_check_python_packages(component, declared)

    records = [
        (pkg["name"], pkg["version"], pkg["license"], "scripts/kit/python-lock.json")
        for pkg in declared
    ]
    components = _dedupe_python_distributions(records)

    epoch = git_epoch(PYTHON_EPOCH_PATHS)
    meta_component = {
        "type": "application",
        "name": component,
        "version": _python_project_version(),
        "properties": [{"name": PROP_PYTHON_PACKAGES_SOURCE, "value": PYTHON_PACKAGES_SOURCE_NOTE}],
    }
    return _assemble_document(component, meta_component, components, epoch)


def _python_component(name: str, version: str, license_raw: Optional[str]) -> dict:
    # D2-2: normalise non-SPDX free text (licences.py's own alias table -- the one source of
    # truth, never duplicated here) BEFORE it goes into CycloneDX's `licenses[].expression`/
    # `.id`, so the SBOM never contains a string licences.py itself cannot parse as SPDX. The
    # raw, as-declared string is kept alongside as a property so nothing is lost.
    normalized = licences.FREE_TEXT_ALIASES.get(license_raw, license_raw) if license_raw else None
    licenses_field, extra_props = _license_field(normalized)
    if license_raw and normalized != license_raw:
        extra_props = extra_props + [{"name": PROP_LICENCE_RAW, "value": license_raw}]
    comp = {"type": "library", "name": name, "version": version}
    if extra_props:
        comp["properties"] = sorted(extra_props, key=lambda p: (p["name"], p["value"]))
    if licenses_field is not None:
        comp["licenses"] = licenses_field
    return comp


# =================================================================================================
# 3. Container images -- from the components' own committed, hand-maintained records
# =================================================================================================

def _fenced_block_after(text: str, anchor: str) -> str:
    idx = text.index(anchor)
    start = text.index("```", idx) + 3
    end = text.index("```", start)
    return text[start:end].strip()


def _image_component(name: str, hash_hex: str, kind: str) -> dict:
    """`kind` is `"container"` (a base image, referenced by its own `sha256:` digest) or
    `"file"` (a prebuilt binary, referenced by its own recorded SHA-256). Neither
    `IMAGE_DIGEST.md` nor `IMAGE_CONTEXT_MANIFEST.txt` records a licence for these -- an
    explicit "no licence metadata" property, never a silent hole."""
    return {
        "type": kind,
        "name": name,
        "version": _workspace_version(),
        "hashes": [{"alg": "SHA-256", "content": hash_hex}],
        "properties": [{"name": PROP_NO_LICENCE, "value": "true"}],
    }


def _edge_plugin_image_sbom() -> dict:
    digest_path = REPO_ROOT / "services" / "edge-plugin" / "IMAGE_DIGEST.md"
    text = digest_path.read_text()

    image_tag = re.search(r"Image tag:\s*`([^`]+)`", text).group(1)
    image_id = _fenced_block_after(text, "Image ID (docker image inspect")
    runtime_base = _fenced_block_after(text, "Runtime base image, pinned by digest")
    prebuild_base = _fenced_block_after(text, "Prebuild base image, pinned by digest")
    binary_sha = _fenced_block_after(text, "Prebuilt `av-edge-plugin` binary SHA-256")

    def split_pinned(ref: str) -> tuple[str, str]:
        name, _, digest = ref.partition("@sha256:")
        return name, digest

    runtime_name, runtime_hash = split_pinned(runtime_base)
    prebuild_name, prebuild_hash = split_pinned(prebuild_base)
    binary_hash = binary_sha.removeprefix("sha256:")
    image_id_hash = image_id.removeprefix("sha256:")

    components = [
        _image_component(runtime_name, runtime_hash, "container"),
        _image_component(prebuild_name, prebuild_hash, "container"),
        _image_component("av-edge-plugin", binary_hash, "file"),
    ]
    epoch = git_epoch(IMAGE_EPOCH_PATHS["edge-plugin-image"])
    meta_component = {
        "type": "container",
        "name": "edge-plugin-image",
        "version": image_tag,
        "hashes": [{"alg": "SHA-256", "content": image_id_hash}],
    }
    return _assemble_document("edge-plugin-image", meta_component, components, epoch)


def _cfs_image_sbom() -> dict:
    digest_path = REPO_ROOT / "services" / "cfs" / "IMAGE_DIGEST.md"
    manifest_path = REPO_ROOT / "services" / "cfs" / "IMAGE_CONTEXT_MANIFEST.txt"
    digest_text = digest_path.read_text()
    manifest_text = manifest_path.read_text()

    # services/cfs/IMAGE_DIGEST.md is a long, hand-written changelog with no single "current
    # state" table (unlike edge-plugin's clean top section) -- the image id / runtime-content
    # hash are instead read from IMAGE_CONTEXT_MANIFEST.txt's own machine-generated header
    # ("do not hand-edit", regenerated by services/cfs/build-image.sh every run), which is
    # always in sync with the actual last build, rather than parsed out of changelog prose.
    image_id = re.search(r"# Built image ID for this manifest:\s*(sha256:[0-9a-f]+)", manifest_text).group(1)
    runtime_content_hash = re.search(
        r"# Runtime-content hash for this build[^:\n]*:\s*(sha256:[0-9a-f]+)", manifest_text
    ).group(1)
    tag_match = re.search(r"docker build -f services/cfs/Dockerfile -t (\S+)\s+\.", digest_text)
    image_tag = tag_match.group(1) if tag_match else None

    components = []
    for line in manifest_text.splitlines():
        m = re.match(r"^([0-9a-f]{64})\s+(\S+)\s+BUILD_ARTIFACT\s*$", line.strip())
        if not m:
            continue
        file_hash, file_path = m.groups()
        components.append(_image_component(Path(file_path).name, file_hash, "file"))

    props = [
        {"name": PROP_PREFIX + "runtime-content-hash", "value": runtime_content_hash},
        {
            "name": PROP_GAP,
            "value": (
                "base image digest not recorded: services/cfs/IMAGE_DIGEST.md has no single "
                "current-state field for it (only narrative changelog prose, e.g. the "
                "'Digest-pinned base, both stages' section) and "
                "IMAGE_CONTEXT_MANIFEST.txt does not record base-image provenance -- omitted "
                "rather than parsed out of prose that could be stale"
            ),
        },
    ]
    epoch = git_epoch(IMAGE_EPOCH_PATHS["cfs-image"])
    meta_component = {
        "type": "container",
        "name": "cfs-image",
        "version": image_tag or "unknown",
        "hashes": [{"alg": "SHA-256", "content": image_id.removeprefix("sha256:")}],
        "properties": sorted(props, key=lambda p: (p["name"], p["value"])),
    }
    return _assemble_document("cfs-image", meta_component, components, epoch)


def image_sbom(component: str) -> dict:
    if component == "edge-plugin-image":
        return _edge_plugin_image_sbom()
    if component == "cfs-image":
        return _cfs_image_sbom()
    raise ValueError(f"unknown image component: {component!r}")


# =================================================================================================
# Dispatch + CLI
# =================================================================================================

def generate_one(component: str) -> dict:
    if component in RUST_BINARIES:
        pkg, bin_name = RUST_BINARIES[component]
        return rust_binary_sbom(component, pkg, bin_name)
    if component in PYTHON_COMPONENTS:
        return python_dist_sbom(component)
    if component in IMAGE_COMPONENTS:
        return image_sbom(component)
    raise ValueError(f"unknown component: {component!r}")


def rewrite_sha256sums(out_dir: Path) -> None:
    """Decision G: `SHA256SUMS` records each `*.cdx.json` file's SHA-256, `sha256sum`-format,
    sorted by path -- rewritten from whatever `*.cdx.json` files are actually present, not
    merely the ones this run happened to (re)generate."""
    files = sorted(out_dir.glob("*.cdx.json"), key=lambda p: p.name)

    def rel(p: Path) -> str:
        # Repo-relative when writing to the real docs/compliance/sbom/ location (the normal
        # case); falls back to out_dir-relative for a test/scratch `out_dir` outside the repo
        # (test_two_generations_are_byte_identical regenerates into tmp_path) -- never an
        # absolute path either way.
        try:
            return p.relative_to(REPO_ROOT).as_posix()
        except ValueError:
            return p.relative_to(out_dir).as_posix()

    lines = [f"{sha256_file(p)}  {rel(p)}" for p in files]
    text = "\n".join(lines) + ("\n" if lines else "")
    (out_dir / "SHA256SUMS").write_text(text, encoding="utf-8")


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, help="output directory for *.cdx.json + SHA256SUMS")
    parser.add_argument("--component", action="append", default=None,
                        help="generate only this component (repeatable); default: every component")
    args = parser.parse_args(argv)

    out_dir = Path(args.out)
    if not out_dir.is_absolute():
        out_dir = REPO_ROOT / out_dir

    wanted = args.component or all_component_names()
    known = set(all_component_names())
    unknown = [c for c in wanted if c not in known]
    if unknown:
        parser.error(f"unknown component(s): {unknown!r}; known components: {sorted(known)}")

    for component in wanted:
        try:
            doc = generate_one(component)
        except PythonSbomCrossCheckError as exc:
            # Question 224: a clean, one-shot CLI failure naming exactly what disagreed --
            # never a bare traceback -- so this is directly actionable from the command line,
            # not just from a caught-in-a-test exception message.
            print(f"error: {exc}", file=sys.stderr)
            return 1
        write_document(doc, out_dir / f"{component}.cdx.json")
        print(f"wrote {out_dir / f'{component}.cdx.json'}", file=sys.stderr)

    rewrite_sha256sums(out_dir)
    print(f"rewrote {out_dir / 'SHA256SUMS'}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
