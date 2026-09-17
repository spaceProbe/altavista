"""scripts/kit/python_lock.py -- question 224 (docs/open-questions.md): the committed Python
dependency lock file (`scripts/kit/python-lock.json`) that `scripts/kit/sbom.py`'s Python SBOM
generator (`python_dist_sbom`) now reads as its declared-set input, instead of enumerating
whatever happens to be installed in the shared worktree `.venv` at generation time.

**The problem this fixes.** `python_dist_sbom` used to call `importlib.metadata.distributions()`
directly over the running interpreter's own `.venv` -- so the SBOM's package list, versions, and
licences were a property of THAT MACHINE'S venv, not of anything committed. Question 224's own
finding: the reconciliation worker's worktree venv lacked `setuptools` and carried a stale
`altavista` dist-info with no licence metadata, so its generated SBOMs recorded
`setuptools:no-longer-there` and `altavista:no-licence-metadata`; the verification clone's
healthy venv regenerated the SAME two files DIFFERENTLY. A document that changes with the
machine it is generated on is not the reproducible SBOM D2 promised.

**The fix (question 224's decision, route "a committed lock file the kit builder produces").**
This module is that kit builder step. `write_lock()` walks the CURRENT interpreter's installed
distributions (so it must be run with a healthy `.venv/bin/python -m scripts.kit.python_lock
--refresh`, i.e. after `pip install -e ".[dev]"` -- the task's own standing instruction) breadth-
first from `declared_roots()` (`pyproject.toml`'s own `[project].dependencies` AND
`[project.optional-dependencies].dev` -- see that function's own doc for why `dev` is included),
capturing each reachable package's exact version and raw licence text (`License-Expression` or
`License`, whichever `importlib.metadata` reports -- the identical two fields
`scripts/kit/sbom.py::_dist_license_text` already reads) into `scripts/kit/python-lock.json`, a
small, sorted, committed JSON file. `scripts/kit/sbom.py::python_dist_sbom` then reads THAT FILE
-- not the live venv -- for the package list, version, and licence every committed Python SBOM
records. The result: the same committed bytes on any machine that checks out this repository,
because the file is not regenerated as a side effect of running the test suite or the SBOM CLI;
it only changes when someone runs `--refresh` again, against a venv they attest is healthy, and
commits the result -- exactly the same discipline `docs/compliance/sbom/*.cdx.json` themselves
already follow (generated, but deliberately committed and only ever refreshed by hand).

**Why `dev` is in the declared set, not runtime dependencies alone.** The two Python SBOM
components (`av-viewer`, `gmat-service`) have always recorded the shared worktree venv's FULL
installed set, `dev` extra included (`docs/compliance/sbom/README.md`: "there is one `.venv` for
this whole worktree ... both read the identical installed-distribution set"), and question 224's
own text repeats that standing instruction verbatim: "a committed Python SBOM is regenerated
only from a venv installed with `pip install -e '.[dev]'`". So the declared set this lock
captures is deliberately `[project].dependencies` UNION the `dev` extra's closure, not
runtime-only -- narrowing to runtime-only here would make every dev-only package (`pytest`,
`httpx`, `grpcio-tools`, and their own transitive closure, e.g. `setuptools` via
`grpcio-tools`'s own `Requires-Dist: setuptools>=77.0.1`) look like an unexplained cross-check
disagreement on every single generation, which is not what this task is fixing.

**The live venv's role now: a cross-check only, never a source.** `scripts/kit/sbom.py` still
reads the live venv (it must -- otherwise a broken venv could regenerate a Python SBOM at all,
which is exactly the failure mode question 224 found), but only to compare against this lock
file's declared set and raise a typed error (`sbom.PythonSbomCrossCheckError`) naming exactly
which package, which side, and which version disagree -- never to silently write whichever it
found. See `sbom.py`'s own module doc, generator 2, for the cross-check itself.

CLI: `python scripts/kit/python_lock.py --refresh` overwrites `scripts/kit/python-lock.json`
from the running interpreter's own installed distributions. Never run automatically by
`sbom.py`, by the test suite, or by any CI gate -- a human (or the lead, at a merge, mirroring
question 220's own SBOM-regeneration discipline) runs it deliberately, on a venv they have
checked is healthy, and commits the result.
"""
from __future__ import annotations

import argparse
import json
import sys
import tomllib
from importlib import metadata as importlib_metadata
from pathlib import Path
from typing import Optional

from packaging.markers import default_environment
from packaging.requirements import Requirement

REPO_ROOT = Path(__file__).resolve().parents[2]
LOCK_PATH = REPO_ROOT / "scripts" / "kit" / "python-lock.json"

#: The self package is always installed editable (`pip install -e .`) but is never its own
#: `[project].dependencies` entry, so it is added as an explicit root here -- exactly the same
#: gap `scripts/kit/build_kit.py::VIEWER_RUNTIME_ROOTS` does NOT have to work around (that root
#: set never needs `altavista` itself, since it only walks the viewer's RUNTIME closure, and the
#: viewer's own server code is what does the importing, not a name in its own dependency list).
_SELF_PACKAGE = "altavista"


def _normalize_dist_name(name: str) -> str:
    return name.lower().replace("_", "-")


def declared_roots() -> dict[str, tuple[str, ...]]:
    """Root package name -> extras, read from `pyproject.toml` itself (never hard-coded a
    second time): every `[project].dependencies` entry, every `[project.optional-dependencies]
    .dev` entry, and `_SELF_PACKAGE`. A root named in both places (there is none today, but
    `grpc`'s two packages are also listed inside `dev`) gets the UNION of its extras."""
    data = tomllib.loads((REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    project = data["project"]
    roots: dict[str, tuple[str, ...]] = {_SELF_PACKAGE: ()}
    for raw in project["dependencies"]:
        req = Requirement(raw)
        roots[req.name] = tuple(sorted(set(roots.get(req.name, ())) | req.extras))
    for raw in project["optional-dependencies"]["dev"]:
        req = Requirement(raw)
        roots[req.name] = tuple(sorted(set(roots.get(req.name, ())) | req.extras))
    return roots


def _dist_license_text(dist: importlib_metadata.Distribution) -> Optional[str]:
    md = dist.metadata
    return md.get("License-Expression") or md.get("License") or None


def _group_installed() -> dict[str, list[importlib_metadata.Distribution]]:
    groups: dict[str, list[importlib_metadata.Distribution]] = {}
    for dist in importlib_metadata.distributions():
        name = dist.metadata.get("Name")
        if not name:
            continue
        groups.setdefault(_normalize_dist_name(name), []).append(dist)
    return groups


def _pick_representative(
    key: str, candidates: list[importlib_metadata.Distribution],
) -> importlib_metadata.Distribution:
    """When the same normalized name resolves to more than one installed record (the known
    `altavista` dist-info/egg-info duplicate -- D2-3's own case, `scripts/kit/sbom.py
    ::DuplicatePythonDistributionError`'s doc has the full story), every candidate must agree on
    VERSION (never silently picked between two different versions of the "same" package -- that
    would be a real ambiguity, not a leftover), and a candidate WITH licence text wins over one
    without; two candidates that disagree on non-empty licence text is refused outright, exactly
    as the SBOM generator's own dedupe already refuses it, so this lock file is captured with the
    same discipline it hands the generator."""
    versions = {d.version for d in candidates}
    if len(versions) > 1:
        raise RuntimeError(
            f"python_lock: {key!r} resolves to {len(candidates)} installed records with "
            f"DIFFERENT versions ({sorted(versions)!r}) -- refusing to pick one arbitrarily; "
            f"clean up this venv before refreshing the lock file"
        )
    licensed = sorted(
        {(_dist_license_text(d), str(getattr(d, "_path", id(d)))) for d in candidates if _dist_license_text(d)}
    )
    distinct = sorted({lic for lic, _path in licensed})
    if len(distinct) > 1:
        detail = "; ".join(f"{lic!r} (from {path})" for lic, path in licensed)
        raise RuntimeError(
            f"python_lock: {key!r} {candidates[0].version}: installed records disagree on "
            f"licence text -- refusing to pick one arbitrarily: {detail}"
        )
    for d in candidates:
        if _dist_license_text(d):
            return d
    return candidates[0]


def resolve_closure() -> list[dict]:
    """Breadth-first walk of the CURRENT interpreter's installed distributions, rooted at
    `declared_roots()` -- the same technique `scripts/kit/build_kit.py::viewer_runtime_closure`
    already uses for the viewer's runtime-only wheel closure, extended here to the `dev` extra
    too and capturing each package's raw licence text alongside its version (never emitted by
    `viewer_runtime_closure`, which only needs versions). Marker evaluation uses THIS process's
    own environment, exactly as `viewer_runtime_closure` documents -- every marker in this
    dependency set only discriminates non-Windows platforms, which macOS and Linux agree on."""
    env = default_environment()
    groups = _group_installed()
    resolved: dict[str, importlib_metadata.Distribution] = {}
    seen_extras: dict[str, set[str]] = {}
    stack: list[tuple[str, tuple[str, ...]]] = list(declared_roots().items())
    missing: list[str] = []
    while stack:
        pkg, extras = stack.pop()
        key = _normalize_dist_name(pkg)
        candidates = groups.get(key)
        if not candidates:
            missing.append(pkg)
            continue
        already = seen_extras.get(key)
        if already is not None and set(extras) <= already:
            continue
        seen_extras[key] = (already or set()) | set(extras)
        dist = _pick_representative(key, candidates)
        resolved[key] = dist
        for r in dist.requires or []:
            req = Requirement(r)
            if req.marker is not None:
                want_extras = extras or ("",)
                if not any(req.marker.evaluate({**env, "extra": e}) for e in want_extras):
                    continue
            stack.append((req.name, tuple(req.extras)))
    if missing:
        raise RuntimeError(
            "python_lock.resolve_closure: declared package(s) not installed in this "
            "interpreter's venv -- run `pip install -e \".[dev]\"` first, then retry --refresh: "
            f"{sorted(set(missing))}"
        )
    packages = []
    for key in sorted(resolved):
        dist = resolved[key]
        packages.append({
            "name": dist.metadata["Name"],
            "version": dist.version,
            "license": _dist_license_text(dist),
        })
    return packages


def write_lock(out_path: Path = LOCK_PATH) -> list[dict]:
    packages = resolve_closure()
    doc = {
        "//": (
            "scripts/kit/python-lock.json -- committed Python declared-dependency lock "
            "(question 224). Generated by `python scripts/kit/python_lock.py --refresh` over a "
            "venv installed with `pip install -e \".[dev]\"`; read by "
            "`scripts/kit/sbom.py::python_dist_sbom` as the Python SBOMs' package/version/"
            "licence source, cross-checked (never sourced) against the live venv at generation "
            "time. Never regenerated automatically -- refresh by hand and commit the result."
        ),
        "packages": packages,
    }
    text = json.dumps(doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    out_path.write_text(text, encoding="utf-8")
    return packages


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--refresh", action="store_true", required=True,
        help="overwrite scripts/kit/python-lock.json from this interpreter's installed "
             "distributions (run with a venv installed via `pip install -e \".[dev]\"`)",
    )
    parser.add_argument("--out", default=str(LOCK_PATH), help="lock file path (default: %(default)s)")
    args = parser.parse_args(argv)

    out_path = Path(args.out)
    if not out_path.is_absolute():
        out_path = REPO_ROOT / out_path
    packages = write_lock(out_path)
    print(f"wrote {out_path} ({len(packages)} packages)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
