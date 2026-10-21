"""Test-only profile directory (question 264): the flat-colour fixture imagery.

The shipped ``profiles/*.yaml`` point at the Blue Marble pyramid served at ``/imagery/bluemarble/``;
that pyramid is a large artefact fetched once and built on the host, so a test that needs *an*
imagery source must never get it from the shipped profiles (it would pass on a host with the pyramid
and fail, or silently draw nothing, on one without). ``tests/fixtures/profiles/{design,analysis,
execution,feasibility}.yaml`` carry today's fixture instead (``./fixtures/tiles/{z}/{x}/{y}.png``,
``max_level`` 2, served from ``web/``):

* in-process:  ``create_app(..., profiles_dir=FIXTURE_PROFILES_DIR)``
* subprocess:  ``[sys.executable, "-m", "altavista", "serve", *FIXTURE_PROFILE_ARGS, ...]``

Configuration by argument, never the process environment (question 199).
"""
from pathlib import Path

FIXTURE_PROFILES_DIR = Path(__file__).resolve().parent / "fixtures" / "profiles"
FIXTURE_PROFILE_ARGS = ["--profiles-dir", str(FIXTURE_PROFILES_DIR)]
