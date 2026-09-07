"""profiles/*.yaml: the first real profile files (M7.2, docs/architecture.md section 5).

The required check here: the execution profile lists no Python-hosted component (question 85
/ docs/adr/003-substrate-and-deployment.md's amendment -- the Python services/gmat-service is
design-time only and must never appear in a deployed profile).

Structured so the check is genuinely capable of failing, not a tautology over a hardcoded
list: `_iter_components` walks the parsed YAML generically (any dict with both a `name` and a
`path` key is treated as a component, wherever it is nested), and `assert_no_python_hosted_
component` flags one two independent ways -- its own self-declared `kind` field, and an
actual filesystem check of whether the `path` it names resolves to a real Python package on
disk -- so a future component that is merely mislabeled (`kind` left as `rust_service` while
its `path` points at real Python-hosted code) is still caught by the second signal.
`test_the_check_itself_can_fail` proves this directly by feeding the checker hand-built "bad"
profiles and asserting it raises, rather than only ever exercising it against a profile file
that is already known to pass -- the difference between "this test is green" and "this test
would actually turn red if someone added a Python service to execution.yaml."
"""
from __future__ import annotations

import pathlib
import re

import pytest
import yaml

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
PROFILES_DIR = REPO_ROOT / "profiles"
GLOBE_JS = REPO_ROOT / "web" / "js" / "globe.js"

_PLANES = ("ingestion", "hot_track", "heavy_track", "viewer", "ai", "command")
_SKIP_DIRS = {"target", "node_modules", ".venv", "__pycache__", ".git"}


def _iter_components(node):
    """Yield every dict in `node` that looks like a profile component entry -- anything with
    both a `name` and a `path` key, found anywhere in the parsed structure (the
    `dynamics_backend` list, each plane's own `components` list, ...). Generic over
    profiles/README.md's own nesting rather than a per-plane hardcoded walk, so a new plane or
    a differently-nested component is still visited.
    """
    if isinstance(node, dict):
        if "name" in node and "path" in node:
            yield node
        for value in node.values():
            yield from _iter_components(value)
    elif isinstance(node, list):
        for item in node:
            yield from _iter_components(item)


def _is_python_package(path: pathlib.Path) -> bool:
    """True if `path` is a directory containing a real Python package (an `__init__.py`
    anywhere under it, outside build/venv/cache noise) -- what services/gmat-service/
    gmat_service is, and what a Rust crate or the plain web/ directory is not. A filesystem
    check, independent of any component's own self-declared `kind`, so a mislabeled component
    is still caught.
    """
    if not path.is_dir():
        return False
    for candidate in path.rglob("__init__.py"):
        if not any(part in _SKIP_DIRS for part in candidate.parts):
            return True
    return False


def assert_no_python_hosted_component(doc: dict) -> None:
    """The actual check, factored out of the test functions so
    `test_the_check_itself_can_fail` can exercise it directly against a synthetic "bad"
    profile, not only ever against the real, already-passing file."""
    for component in _iter_components(doc):
        kind = str(component.get("kind", "")).lower()
        assert "python" not in kind, (
            f"{component.get('name')!r} is declared with kind={component.get('kind')!r}: a "
            "Python-hosted component may not appear in the execution profile (question 85)"
        )
        target = REPO_ROOT / component["path"]
        assert not _is_python_package(target), (
            f"{component.get('name')!r} points at {component['path']!r}, which resolves to a "
            "Python package on disk: a Python-hosted component may not appear in the "
            "execution profile (question 85), regardless of its declared kind"
        )


def _load(name: str) -> dict:
    return yaml.safe_load((PROFILES_DIR / name).read_text())


@pytest.mark.parametrize("name", ["design.yaml", "feasibility.yaml", "analysis.yaml", "execution.yaml"])
def test_profile_files_parse_and_have_the_shared_shape(name):
    doc = _load(name)
    assert doc["id"] == name.removesuffix(".yaml")
    assert "architecture_ref" in doc
    assert "planes" in doc
    for plane in _PLANES:
        assert plane in doc["planes"], f"{name}: missing plane {plane!r} (docs/architecture.md section 5's six rows)"
        assert "per_architecture" in doc["planes"][plane]
        assert "components" in doc["planes"][plane]
    assert "dynamics_backend" in doc
    backend_names = {c["name"] for c in doc["dynamics_backend"]}
    assert "av-dynamics-service" in backend_names, f"{name}: must list the Rust DynamicsService (question 85 -- present in every profile)"


def test_design_and_feasibility_profiles_list_the_python_gmat_service():
    """The mirror image of the execution check below: gmat-service must actually be declared
    where architecture.md and question 85 put it (design and feasibility heavy tracks), so
    this directory is not simply omitting it everywhere to make the execution check trivially
    pass."""
    for name in ("design.yaml", "feasibility.yaml"):
        doc = _load(name)
        names = {c["name"] for c in _iter_components(doc)}
        assert "gmat-service" in names, f"{name}: must list services/gmat-service (question 85)"


def test_analysis_profile_does_not_list_the_python_gmat_service():
    doc = _load("analysis.yaml")
    names = {c["name"] for c in _iter_components(doc)}
    assert "gmat-service" not in names


def test_execution_profile_contains_no_python_hosted_service():
    doc = _load("execution.yaml")
    components = list(_iter_components(doc))
    assert components, "execution.yaml must declare at least one component for this test to be meaningful"
    assert_no_python_hosted_component(doc)


def test_the_check_itself_can_fail():
    """Proves `assert_no_python_hosted_component` is a real check, not a tautology: fed
    hand-built profiles shaped like a Python-hosted component slipped into the execution
    profile, it must raise -- directly demonstrating what would happen if someone added one
    for real, without waiting for that to actually occur.
    """
    bad_by_kind = {
        "planes": {
            "command": {"components": [{"name": "rogue-planner", "kind": "python_service", "path": "services/gmat-service"}]},
        },
    }
    with pytest.raises(AssertionError):
        assert_no_python_hosted_component(bad_by_kind)

    # Mislabeled kind, but the path really does resolve to a Python package on disk -- the
    # filesystem-based signal must catch this even when the declared kind lies.
    bad_by_path = {
        "planes": {
            "command": {"components": [{"name": "rogue-planner", "kind": "rust_service", "path": "services/gmat-service"}]},
        },
    }
    with pytest.raises(AssertionError):
        assert_no_python_hosted_component(bad_by_path)

    # A genuinely clean profile (Rust-only components) must not raise.
    good = {
        "planes": {
            "hot_track": {"components": [{"name": "av-kernel", "kind": "rust_crate", "path": "crates/av-kernel"}]},
        },
        "dynamics_backend": [{"name": "av-dynamics-service", "kind": "rust_service", "path": "crates/av-dynamics-service"}],
    }
    assert_no_python_hosted_component(good)  # must not raise


# ==================================================================================
# M19.5: the globe's imagery source is a profile setting (question 132, decided by the
# lead, applying question 11's rule -- "profiles select components, never behaviour" --
# to the globe's tile source: an XYZ/WMTS URL template, an attribution string, and the
# source's own declared max zoom level).
# ==================================================================================

def _default_imagery_url_from_globe_js() -> str:
    """The real, shipped default straight out of web/js/globe.js's own source text
    (never a hand-copied literal that could silently drift from it -- the same
    "parse the real fixture/source, don't shadow it" convention
    tests/test_cdm_run.py's own `_declared_drm_hash()` already uses)."""
    text = GLOBE_JS.read_text()
    m = re.search(r"const DEFAULT_IMAGERY_URL = '([^']+)'", text)
    assert m, f"{GLOBE_JS} has no `const DEFAULT_IMAGERY_URL = '...'` line to read"
    return m.group(1)


@pytest.mark.parametrize("name", ["design.yaml", "feasibility.yaml", "analysis.yaml", "execution.yaml"])
def test_every_profile_declares_an_imagery_section(name):
    """Every profile must declare `imagery: {url_template, attribution, max_level}` --
    the exact shape `altavista/profile.py`'s `load_imagery_config` reads structurally
    (not by pattern-matching this file's text). Fails against a profile file that omits
    the section entirely, or declares it with the wrong key names/types -- both of
    which would make `altavista.profile.load_imagery_config` raise `ProfileError` for a
    server actually trying to start against that profile.
    """
    doc = _load(name)
    assert "imagery" in doc, f"{name}: missing 'imagery' section (question 132)"
    imagery = doc["imagery"]
    assert isinstance(imagery, dict), f"{name}: 'imagery' must be a mapping, got {type(imagery)}"
    assert isinstance(imagery.get("url_template"), str) and imagery["url_template"], (
        f"{name}: imagery.url_template must be a non-empty string"
    )
    for placeholder in ("{z}", "{x}", "{y}"):
        assert placeholder in imagery["url_template"], (
            f"{name}: imagery.url_template {imagery['url_template']!r} is missing {placeholder!r}"
        )
    assert isinstance(imagery.get("attribution"), str) and imagery["attribution"].strip(), (
        f"{name}: imagery.attribution must be a non-empty string (it must actually be "
        f"displayable in the viewer, not blank)"
    )
    assert isinstance(imagery.get("max_level"), int) and imagery["max_level"] >= 0, (
        f"{name}: imagery.max_level must be a non-negative integer"
    )


@pytest.mark.parametrize("name", ["design.yaml", "feasibility.yaml", "analysis.yaml", "execution.yaml"])
def test_every_profiles_default_imagery_matches_globe_js(name):
    """M19.5's own required check: "a test that the default profile yields the offline
    fixture source." Every profile's declared `url_template` must equal web/js/globe.js's
    own `DEFAULT_IMAGERY_URL` constant -- fails if a profile's default is pointed
    somewhere else (a real basemap URL, a typo'd fixture path, an empty string) while
    globe.js's own fallback still expects the fixture, or vice versa.
    """
    doc = _load(name)
    assert doc["imagery"]["url_template"] == _default_imagery_url_from_globe_js()


def test_imagery_section_is_never_swept_into_the_component_checker():
    """Guards the schema choice itself: `imagery` is a data value (URL template,
    attribution, max level), not a `dynamics_backend`/plane-style component list, so it
    must never carry both a `name` and a `path` key together -- if it ever did,
    `_iter_components` (this file's generic, structural walker) would sweep it into
    `assert_no_python_hosted_component`'s execution-profile check for reasons that have
    nothing to do with question 85. Fails if a future edit adds `name`+`path` fields to
    any profile's `imagery` section.
    """
    for name in ("design.yaml", "feasibility.yaml", "analysis.yaml", "execution.yaml"):
        doc = _load(name)
        imagery = doc["imagery"]
        assert not ("name" in imagery and "path" in imagery), (
            f"{name}: imagery section must not declare both 'name' and 'path' -- it is "
            f"a data value (question 11), not a component"
        )
        assert imagery not in list(_iter_components(doc)), (
            f"{name}: imagery section was swept into _iter_components -- it is not a component"
        )
