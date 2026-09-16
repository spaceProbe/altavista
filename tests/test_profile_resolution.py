"""Round 3 (question 217(b), the lead's ruling): unit coverage for the ORDERED SEARCH half of the
packaging change -- `altavista.profile.resolve_profiles_dir` (see that module's own doc for the
five-step order) and `altavista.server`'s equivalent packaged-copy-vs-in-repo-copy default for
`WEB_DIR`. The packaging change's OTHER half -- that a real wheel built from this worktree
actually carries `altavista/web/`/`altavista/profiles/` at all -- is proved by building the wheel
and listing its contents (not a fast unit test: it needs a real `pip wheel` invocation), and by
`tests/test_kit_zero_egress_install.py`'s own real HTTP/profile-load evidence against an installed
wheel inside a container; this file only exercises the RESOLUTION LOGIC, which is fast and needs
no wheel build or container at all.

Question 199: no test here mutates this process's own `os.environ`. The one case that needs a
real `ALTAVISTA_PROFILES_DIR` environment variable runs a real subprocess with `env=`, never
`os.environ[...] = ...`/`monkeypatch.setenv` on this test's own process.
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

import altavista.profile as profile_mod
from altavista import server as server_mod

REPO_ROOT = Path(__file__).resolve().parents[1]


@pytest.fixture(autouse=True)
def _restore_profiles_dir():
    """`altavista.profile.PROFILES_DIR` is process-global, mutable module state (the documented
    monkeypatch escape hatch -- `resolve_profiles_dir`'s own step 3) -- every test here that
    assigns it must restore the module's own prior value afterward, so test order/isolation never
    matters (and so this file never leaks state into any other test module)."""
    original = profile_mod.PROFILES_DIR
    yield
    profile_mod.PROFILES_DIR = original


# =================================================================================================
# altavista.profile.resolve_profiles_dir -- the five-step order
# =================================================================================================

def test_explicit_argument_wins_over_the_module_constant():
    profile_mod.PROFILES_DIR = Path("/should-be-ignored")
    assert profile_mod.resolve_profiles_dir(Path("/explicit/wins")) == Path("/explicit/wins")


def test_module_constant_wins_when_assigned_and_no_explicit_argument():
    """The documented monkeypatch escape hatch (step 3) -- other code (and, before round 3,
    tests/test_kit_zero_egress_install.py) points `load_imagery_config` at an arbitrary directory
    by assigning `altavista.profile.PROFILES_DIR` directly. Assigning a plain module attribute is
    not an `os.environ` mutation -- explicitly allowed by this task's own rule."""
    profile_mod.PROFILES_DIR = Path("/monkeypatched/profiles")
    assert profile_mod.resolve_profiles_dir() == Path("/monkeypatched/profiles")


def test_env_var_wins_over_the_module_constant_but_loses_to_an_explicit_argument():
    """Runs in a SUBPROCESS with `env=` (question 199) -- this test's own process never sets
    `ALTAVISTA_PROFILES_DIR` on itself."""
    env_only_script = "import altavista.profile as p\nprint(p.resolve_profiles_dir())\n"
    result = subprocess.run(
        [sys.executable, "-c", env_only_script], cwd=REPO_ROOT,
        capture_output=True, text=True, timeout=30,
        env={"PATH": "/usr/bin:/bin", "ALTAVISTA_PROFILES_DIR": "/env/wins"},
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "/env/wins"

    explicit_arg_script = (
        "import altavista.profile as p\nprint(p.resolve_profiles_dir('/explicit/still/wins'))\n"
    )
    result2 = subprocess.run(
        [sys.executable, "-c", explicit_arg_script], cwd=REPO_ROOT,
        capture_output=True, text=True, timeout=30,
        env={"PATH": "/usr/bin:/bin", "ALTAVISTA_PROFILES_DIR": "/env/loses/here"},
    )
    assert result2.returncode == 0, result2.stderr
    assert result2.stdout.strip() == "/explicit/still/wins"


def test_falls_through_to_the_in_repo_copy_in_a_source_checkout():
    """No explicit argument, no env var, no monkeypatched module constant -- exactly what a
    developer running straight out of this worktree gets. The packaged copy (step 4) does not
    exist in a source checkout (nothing here ever copies profiles/ into altavista/profiles/ in
    place -- only a real wheel/sdist build does, via setup.py's build_py subclass), so resolution
    must reach step 5, the real in-repo profiles/ directory -- checked by content, not merely by
    path equality, so this would fail if REPO_ROOT ever pointed somewhere without real profile
    files in it."""
    profile_mod.PROFILES_DIR = None
    resolved = profile_mod.resolve_profiles_dir()
    assert resolved == REPO_ROOT / "profiles"
    assert (resolved / "design.yaml").is_file(), "must be the REAL in-repo profiles/, not a guess"


def test_packaged_copy_wins_over_in_repo_copy_when_both_exist(tmp_path, monkeypatch):
    """Simulates being imported from inside an installed wheel: a sibling `profiles/` directory
    next to a stand-in module file, entirely under tmp_path. `monkeypatch.setattr` on
    `profile_mod.__file__` is plain module state, not `os.environ` -- this never touches the real
    source tree, so it cannot make any OTHER test in this suite see a packaged copy that isn't
    really there."""
    fake_pkg_dir = tmp_path / "site-packages" / "altavista"
    fake_pkg_dir.mkdir(parents=True)
    (fake_pkg_dir / "profile.py").write_text("# stand-in\n")
    packaged_profiles = fake_pkg_dir / "profiles"
    packaged_profiles.mkdir()
    (packaged_profiles / "design.yaml").write_text("id: design\nimagery: {}\n")

    monkeypatch.setattr(profile_mod, "__file__", str(fake_pkg_dir / "profile.py"))
    profile_mod.PROFILES_DIR = None
    assert profile_mod.resolve_profiles_dir() == packaged_profiles


def test_load_imagery_config_honours_the_explicit_profiles_dir_argument(tmp_path):
    """`load_imagery_config`'s own new keyword-only parameter, end to end -- not merely
    `resolve_profiles_dir` in isolation."""
    custom_dir = tmp_path / "custom-profiles"
    custom_dir.mkdir()
    (custom_dir / "custom.yaml").write_text(
        "id: custom\nimagery:\n  url_template: 'https://example.test/{z}/{x}/{y}.png'\n"
        "  attribution: 'test fixture'\n  max_level: 3\n"
    )
    imagery = profile_mod.load_imagery_config("custom", profiles_dir=custom_dir)
    assert imagery == {
        "urlTemplate": "https://example.test/{z}/{x}/{y}.png",
        "attribution": "test fixture",
        "maxLevel": 3,
    }


# =================================================================================================
# altavista.server.WEB_DIR / _default_web_dir -- the same packaged-copy-vs-in-repo-copy treatment
# =================================================================================================

def test_web_dir_falls_through_to_the_in_repo_copy_in_a_source_checkout():
    resolved = server_mod._default_web_dir()
    assert resolved == REPO_ROOT / "web"
    assert (resolved / "index.html").is_file(), "must be the REAL in-repo web/, not a guess"


def test_web_dir_prefers_the_packaged_copy_when_present(tmp_path, monkeypatch):
    fake_pkg_dir = tmp_path / "site-packages" / "altavista"
    fake_pkg_dir.mkdir(parents=True)
    (fake_pkg_dir / "server.py").write_text("# stand-in\n")
    packaged_web = fake_pkg_dir / "web"
    packaged_web.mkdir()

    monkeypatch.setattr(server_mod, "__file__", str(fake_pkg_dir / "server.py"))
    assert server_mod._default_web_dir() == packaged_web
