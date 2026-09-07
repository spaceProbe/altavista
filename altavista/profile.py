"""Profile loading (M19.5, docs/open-questions.md question 132): the one thing a running
``altavista`` server reads out of ``profiles/*.yaml`` today is the globe's imagery source --
an XYZ/WMTS URL template, an attribution string, and the source's own declared max zoom
level. Nothing else in a profile file is consumed by any runtime code (``profiles/`` is
still "files first" -- see ``profiles/README.md``); this module does not become a general
profile loader, only the narrow seam question 132 asked for.

This is a *selection*, not a behaviour change (question 11's rule, restated in every
profile file's own ``rule:`` field): the profile says which imagery source the viewer is
pointed at, never how the globe behaves once it has one (LOD thresholds, tile budget and
resident-cache size all stay hardcoded in ``web/js/globe.js``).
"""
from __future__ import annotations

import pathlib
from typing import Dict

import yaml

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
PROFILES_DIR = REPO_ROOT / "profiles"

# altavista has no running "current profile" concept yet (no console -- profiles/README.md's
# own "files first" note) -- "design" is the profile altavista itself (the design-time GMAT
# Python API host, profiles/design.yaml's own viewer entry) is documented against, so it is
# the honest default for a server started with no other instruction.
DEFAULT_PROFILE_ID = "design"


class ProfileError(Exception):
    """Raised when a named profile file is missing, malformed, or declares no imagery
    section -- never silently substituted for (this task's honesty rule: say plainly
    what is missing rather than fabricate a default that isn't actually declared
    anywhere)."""


def load_imagery_config(profile_id: str = DEFAULT_PROFILE_ID) -> Dict[str, object]:
    """Read ``profiles/<profile_id>.yaml``'s ``imagery:`` section and return it in the
    viewer's own camelCase wire shape (matching every other additive field the scene
    JSON carries -- ``configHash``, ``runId``, ``t0Iso``, etc., see ``altavista/model.py``):
    ``{"urlTemplate": str, "attribution": str, "maxLevel": int}``.

    Raises ``ProfileError`` (never returns a fabricated default) if the profile file
    does not exist, does not parse, or its ``imagery:`` section is missing or missing a
    required key -- the same "no unrecorded approximation" posture
    ``tests/test_profiles.py`` already holds every profile file to.
    """
    path = PROFILES_DIR / f"{profile_id}.yaml"
    if not path.is_file():
        raise ProfileError(f"no such profile {profile_id!r}: {path} does not exist")
    try:
        doc = yaml.safe_load(path.read_text())
    except yaml.YAMLError as exc:
        raise ProfileError(f"profile {profile_id!r} ({path}) is not valid YAML: {exc}") from exc
    if not isinstance(doc, dict):
        raise ProfileError(f"profile {profile_id!r} ({path}) did not parse to a mapping")
    imagery = doc.get("imagery")
    if not isinstance(imagery, dict):
        raise ProfileError(f"profile {profile_id!r} ({path}) declares no 'imagery' section")
    try:
        return {
            "urlTemplate": imagery["url_template"],
            "attribution": imagery["attribution"],
            "maxLevel": imagery["max_level"],
        }
    except KeyError as exc:
        raise ProfileError(f"profile {profile_id!r} ({path}) imagery section missing key {exc}") from exc
