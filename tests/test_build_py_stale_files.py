"""tests/test_build_py_stale_files.py -- round 3 (question 217(b) follow-on, a review defect):
`setup.py`'s `build_py` subclass copies `web/`/`profiles/` into the build tree with
`self.copy_tree`, which is purely ADDITIVE -- it never deletes a file at `dest` that is no longer
present at `src`. `build/lib/...` is a PERSISTENT tree (`pip wheel .` never cleans it between
invocations), so a file removed from `web/`/`profiles/` between two builds stayed packaged into
EVERY wheel built afterward, with nothing anywhere saying so. Proved here directly: build a wheel
with an extra file present, remove the file from source, build again, and assert the file is gone
from the second wheel.

This drives the REAL `build_py` subclass -- copied verbatim (`shutil.copy2`) from this repo's own
`setup.py`, never reimplemented -- but against a SYNTHETIC source tree assembled fresh under
`tmp_path` for each test, never against this worktree's real `web/`/`profiles/`. That is a
deliberate, more conservative choice than mutating the real `web/` transiently (which the manager
who found this defect did once, by hand, to prove it): a synthetic tree means a failure partway
through this test can never leave the shared `AltaVista-edge` worktree dirty, and `tmp_path` is
cleaned up by pytest regardless of outcome. The one file this test still touches directly (the
probe file, itself under the synthetic tree) is still removed in a `finally`, on the same
belt-and-suspenders principle the task asked for.

Not a fast unit test -- each `pip wheel .` invocation is a real subprocess build (`--no-deps
--no-build-isolation`, the same flags `scripts/kit/build_kit.py`'s own `collect_wheels` uses for
this project's own wheel), so this takes on the order of a second or two, not milliseconds.

Question 199: no test mutates this process's own `os.environ` -- both `pip wheel` subprocesses
are given an explicit `env=` (a copy of this process's environment; PYTHONPATH is not narrowed
here the way `test_profile_resolution.py`'s subprocess tests do, because this synthetic build
needs whatever `sys.executable` already has installed -- setuptools, pip -- not this repo's own
`altavista` package).
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]


def _make_synthetic_repo(tmp_path: Path) -> Path:
    """A minimal source tree exercising the same shape `setup.py`'s own top doc describes: a real
    `altavista` package (for `packages.find` to discover) plus a `web/` sibling directory that is
    not itself a package. `setup.py` is copied byte-for-byte from the real repo -- this test proves
    the ACTUAL `build_py` subclass, not a description of it."""
    repo = tmp_path / "synthetic-repo"
    (repo / "altavista").mkdir(parents=True)
    (repo / "altavista" / "__init__.py").write_text("")
    (repo / "web").mkdir()
    (repo / "web" / "index.html").write_text("<html></html>\n")

    shutil.copy2(REPO_ROOT / "setup.py", repo / "setup.py")

    (repo / "pyproject.toml").write_text(
        "[build-system]\n"
        'requires = ["setuptools>=68"]\n'
        'build-backend = "setuptools.build_meta"\n'
        "\n"
        "[project]\n"
        'name = "altavista"\n'
        'version = "0.1.0"\n'
        "\n"
        "[tool.setuptools.packages.find]\n"
        'include = ["altavista*"]\n'
        "\n"
        "[tool.setuptools.package-data]\n"
        'altavista = ["web/**/*"]\n'
    )
    return repo


def _wheel_member_names(wheel_path: Path) -> set[str]:
    with zipfile.ZipFile(wheel_path) as zf:
        return set(zf.namelist())


def _pip_wheel(repo: Path, out_dir: Path) -> subprocess.CompletedProcess:
    out_dir.mkdir(parents=True)
    return subprocess.run(
        [sys.executable, "-m", "pip", "wheel", ".", "--no-deps", "--no-build-isolation",
         "-w", str(out_dir)],
        cwd=repo, capture_output=True, text=True, timeout=120,
        env=dict(os.environ),
    )


def test_a_file_removed_from_web_does_not_survive_into_the_next_wheel(tmp_path):
    repo = _make_synthetic_repo(tmp_path)
    probe = repo / "web" / "ZZ_STALE_PROBE.txt"

    try:
        probe.write_text("stale\n")

        first = _pip_wheel(repo, tmp_path / "wheels-1")
        assert first.returncode == 0, first.stdout + first.stderr
        first_wheels = sorted((tmp_path / "wheels-1").glob("altavista-*.whl"))
        assert len(first_wheels) == 1, first_wheels
        first_names = _wheel_member_names(first_wheels[0])
        assert "altavista/web/ZZ_STALE_PROBE.txt" in first_names, sorted(first_names)
        assert "altavista/web/index.html" in first_names, sorted(first_names)

        probe.unlink()  # removed from SOURCE, but `build/lib/altavista/web/...` still has it

        second = _pip_wheel(repo, tmp_path / "wheels-2")
        assert second.returncode == 0, second.stdout + second.stderr
        second_wheels = sorted((tmp_path / "wheels-2").glob("altavista-*.whl"))
        assert len(second_wheels) == 1, second_wheels
        second_names = _wheel_member_names(second_wheels[0])

        # The defect this test guards against: without removing `dest` first, the second wheel --
        # built from the same persistent `build/lib/` tree the first wheel used -- would still
        # carry the probe file, even though it no longer exists anywhere in the source tree.
        assert "altavista/web/ZZ_STALE_PROBE.txt" not in second_names, sorted(second_names)
        # And the fix is not simply "web/ is dropped entirely" -- a real, still-present file must
        # still make it into the second wheel.
        assert "altavista/web/index.html" in second_names, sorted(second_names)
    finally:
        if probe.exists():
            probe.unlink()


def test_a_missing_source_directory_is_skipped_not_fatal(tmp_path):
    """The behaviour `setup.py`'s own comment promises is untouched by this fix: a source checkout
    that genuinely lacks `web/`/`profiles/` still builds a wheel, just without that piece, rather
    than failing the whole build."""
    repo = _make_synthetic_repo(tmp_path)
    shutil.rmtree(repo / "web")

    result = _pip_wheel(repo, tmp_path / "wheels")
    assert result.returncode == 0, result.stdout + result.stderr
    wheels = sorted((tmp_path / "wheels").glob("altavista-*.whl"))
    assert len(wheels) == 1, wheels
    names = _wheel_member_names(wheels[0])
    assert not any(n.startswith("altavista/web/") for n in names), sorted(names)
