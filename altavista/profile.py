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

# Where ``profiles/`` actually is (round 3, question 217(b))

Before round 3, ``PROFILES_DIR`` was a single fixed constant (``REPO_ROOT / "profiles"``) -- the
IN-REPO copy, always, unconditionally. That is exactly why a wheel-only install of ``altavista``
(no worktree anywhere on the machine) could never find a profiles directory at all: ``REPO_ROOT``
still computed to *some* path (``Path(__file__).resolve().parent.parent``), but nothing existed
there once ``altavista`` was installed as a package on its own (see ``pyproject.toml``'s own
comment on ``[tool.setuptools.packages.find]``, and ``setup.py``'s doc for how the wheel now
carries a packaged copy at ``altavista/profiles/`` instead).

``resolve_profiles_dir`` (below) replaces that single fixed lookup with an ordered search, highest
priority first:

1. an explicit ``profiles_dir`` argument, passed by the caller (``load_imagery_config``'s own new
   keyword-only parameter) -- the most specific instruction available, always honoured first.
2. the ``ALTAVISTA_PROFILES_DIR`` environment variable, if set -- read fresh on every call (never
   cached, never written by this module), so a deployment can point the viewer at a profiles
   directory outside both the wheel and any worktree without a code change.
3. the module-level ``PROFILES_DIR`` below, if some caller has assigned it a real value -- kept
   for exactly one reason: other code (and, before round 3, ``tests/
   test_kit_zero_egress_install.py``) could point ``load_imagery_config`` at an arbitrary
   directory by monkeypatching ``altavista.profile.PROFILES_DIR`` directly, and that escape hatch
   keeps working unchanged. It starts unassigned (``None``) -- an ordinary import, with nothing
   overriding it at all, must fall through to steps 4-5 rather than silently re-hard-coding the
   worktree path the old constant did (which is precisely the defect this round closes).
4. the PACKAGED copy, ``Path(__file__).resolve().parent / "profiles"`` -- ``altavista/profiles/``
   beside this very file -- if it exists. Only a real wheel/sdist build (``setup.py``'s
   ``build_py`` subclass) ever materialises this; an ordinary source checkout does not.
5. the IN-REPO copy, ``Path(__file__).resolve().parent.parent / "profiles"`` -- the worktree's own
   ``profiles/``, sibling of ``altavista/``. This is the fallback of last resort, and it is what a
   developer running straight out of this worktree (no wheel install, no env var, no monkeypatch)
   actually gets: step 4 never finds a packaged copy in a source checkout (nothing here ever
   copies ``profiles/`` to ``altavista/profiles/`` in place), so resolution always reaches step 5
   for that case. **This is the entry that wins for an in-repo developer run** -- not because it
   is preferred over the packaged copy in the abstract, but because the packaged copy provably
   does not exist there; the ordering itself (4 before 5) is what lets a real wheel install with
   no worktree in sight still find ITS OWN profiles automatically, with no argument, env var, or
   monkeypatch required.
"""
from __future__ import annotations

import os
import pathlib
from typing import Dict, Optional

import yaml

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

#: Round-3 override slot (question 217(b)) -- see this module's own top doc, step 3. Starts
#: unassigned; other code may still do ``altavista.profile.PROFILES_DIR = Path(...)`` to force a
#: specific directory, exactly as before round 3, but no longer supplies the DEFAULT (steps 4-5,
#: below, do that now).
PROFILES_DIR: "Optional[pathlib.Path]" = None

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


def resolve_profiles_dir(profiles_dir: "Optional[os.PathLike]" = None) -> pathlib.Path:
    """The ordered search this module's own top doc documents (round 3, question 217(b)),
    highest priority first: (1) ``profiles_dir`` itself, if given; (2) the
    ``ALTAVISTA_PROFILES_DIR`` environment variable, read fresh on every call (never mutated,
    never cached -- question 199); (3) the module-level ``PROFILES_DIR``, if some caller has
    assigned it a real value; (4) the packaged copy beside this file
    (``altavista/profiles/``), if it exists; (5) the in-repo copy at the worktree root. Never
    raises itself -- a nonexistent result is exactly what ``load_imagery_config`` already turns
    into a ``ProfileError`` naming the path it tried.
    """
    if profiles_dir is not None:
        return pathlib.Path(profiles_dir)
    env_value = os.environ.get("ALTAVISTA_PROFILES_DIR")
    if env_value:
        return pathlib.Path(env_value)
    if PROFILES_DIR is not None:
        return pathlib.Path(PROFILES_DIR)
    packaged = pathlib.Path(__file__).resolve().parent / "profiles"
    if packaged.is_dir():
        return packaged
    return REPO_ROOT / "profiles"


def load_imagery_config(
    profile_id: str = DEFAULT_PROFILE_ID, *, profiles_dir: "Optional[os.PathLike]" = None,
) -> Dict[str, object]:
    """Read ``<profiles_dir>/<profile_id>.yaml``'s ``imagery:`` section and return it in the
    viewer's own camelCase wire shape (matching every other additive field the scene
    JSON carries -- ``configHash``, ``runId``, ``t0Iso``, etc., see ``altavista/model.py``):
    ``{"urlTemplate": str, "attribution": str, "maxLevel": int}``.

    ``profiles_dir`` (round 3, question 217(b)): an explicit override, threaded straight to
    ``resolve_profiles_dir`` (see that function's own doc for the full, ordered search) -- left at
    its default (``None``) this behaves exactly as before, resolving through the environment
    variable / module constant / packaged-or-in-repo fallback chain instead of the old single
    fixed ``PROFILES_DIR`` constant.

    Raises ``ProfileError`` (never returns a fabricated default) if the profile file
    does not exist, does not parse, or its ``imagery:`` section is missing or missing a
    required key -- the same "no unrecorded approximation" posture
    ``tests/test_profiles.py`` already holds every profile file to.
    """
    path = resolve_profiles_dir(profiles_dir) / f"{profile_id}.yaml"
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
