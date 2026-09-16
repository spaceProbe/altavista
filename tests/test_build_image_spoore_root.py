"""Question 219(b): `services/edge-plugin/build-image.sh` and `services/proposer/build-image.sh`
both used to hardcode `SPOORE_HOST_PATH="/Users/probe/code/spoore"`. Both now take a
`SPOORE_ROOT` environment override, defaulting to the sibling checkout `../spoore` next to this
repository's own root (question 12's convention, matching `Cargo.toml`'s own now-relative
`spoore-cdm` path dependency, question 219(c)).

This file proves the resolution logic WITHOUT running docker (a real cross-build is 20+ minutes
and the host is shared -- see the round 4 report): it greps the two scripts for the shape of
their own `SPOORE_ROOT` handling, and re-runs the exact default-assignment line each script
itself uses (extracted from the committed source, never retyped by hand) in a real `bash`
subprocess against a synthetic directory tree under `tmp_path`, so the assertion is about the
scripts' own committed text, not a parallel reimplementation of it that could quietly drift.
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = [
    REPO_ROOT / "services/edge-plugin/build-image.sh",
    REPO_ROOT / "services/proposer/build-image.sh",
]

# The exact default-assignment shape both scripts use (question 219(b)): SPOORE_ROOT, unset,
# defaults to REPO_ROOT/../spoore -- captured as a regex so this test reads the real committed
# line rather than assuming its exact spelling.
_DEFAULT_ASSIGNMENT_RE = re.compile(
    r'^SPOORE_ROOT="\$\{SPOORE_ROOT:-\$\{REPO_ROOT\}/\.\./spoore\}"\s*$', re.MULTILINE
)

# The retired variable -- question 219(b) removes every hardcoded default host path, this
# name included, from both scripts. (A fixed CONTAINER-side mount destination is a different
# thing -- see test_proposers_remaining_absolute_reference_is_only_a_mount_destination below.)
_OLD_VARIABLE_RE = re.compile(r"SPOORE_HOST_PATH")


@pytest.mark.parametrize("script", SCRIPTS, ids=lambda p: p.name)
def test_bash_syntax_is_valid(script):
    """`bash -n` on each script -- a real syntax check, not a guess that the edits parse."""
    result = subprocess.run(["bash", "-n", str(script)], capture_output=True, text=True)
    assert result.returncode == 0, (
        f"bash -n {script} failed:\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )


@pytest.mark.parametrize("script", SCRIPTS, ids=lambda p: p.name)
def test_neither_script_uses_the_old_retired_variable(script):
    text = script.read_text()
    assert not _OLD_VARIABLE_RE.search(text), (
        f"{script} still references the retired SPOORE_HOST_PATH variable -- question 219(b) "
        f"replaces it with SPOORE_ROOT everywhere"
    )


@pytest.mark.parametrize("script", SCRIPTS, ids=lambda p: p.name)
def test_spoore_root_default_assignment_is_the_sibling_checkout_not_a_literal_path(script):
    """The default-assignment line itself must be an expression (REPO_ROOT/../spoore), never a
    literal absolute path baked in as the default -- this is the actual question 219(b) fix,
    checked against the script's own committed text."""
    text = script.read_text()
    match = _DEFAULT_ASSIGNMENT_RE.search(text)
    assert match is not None, (
        f"{script}: expected exactly the shape "
        f'SPOORE_ROOT="${{SPOORE_ROOT:-${{REPO_ROOT}}/../spoore}}" -- not found (script text '
        f"may have changed shape; update this test's own regex to match, or restore the line)"
    )
    # A hardcoded absolute default must never sit on this same line.
    assert "/Users/probe/code/spoore" not in match.group(0)


@pytest.mark.parametrize("script", SCRIPTS, ids=lambda p: p.name)
def test_spoore_root_resolves_to_the_sibling_checkout_by_default(script, tmp_path):
    """Runs the script's OWN default-assignment line (extracted above, never retyped) in a real
    bash subprocess against a synthetic REPO_ROOT/../spoore tree under tmp_path -- proves the
    resolution, not just the shape of the text."""
    text = script.read_text()
    match = _DEFAULT_ASSIGNMENT_RE.search(text)
    assert match is not None, "fixture assumption: see the previous test"
    default_assignment_line = match.group(0)

    synthetic_repo = tmp_path / "clone" / "AltaVista-edge"
    synthetic_repo.mkdir(parents=True)
    synthetic_spoore = tmp_path / "clone" / "spoore"
    (synthetic_spoore / "crates" / "spoore-cdm").mkdir(parents=True)

    snippet = f'REPO_ROOT="{synthetic_repo}"\n{default_assignment_line}\nprintf "%s" "${{SPOORE_ROOT}}"\n'
    result = subprocess.run(
        ["bash", "-c", snippet], capture_output=True, text=True,
        env={"PATH": "/usr/bin:/bin"},  # no ambient SPOORE_ROOT -- this is the default path
    )
    assert result.returncode == 0, result.stderr
    resolved = Path(result.stdout)
    # Not resolved via `cd`/`pwd` at this point (the scripts themselves do that separately,
    # against a real directory, right after this assignment) -- string-equal against the
    # un-normalized expression is enough to prove the DEFAULT is the sibling, not a literal
    # absolute host path.
    assert str(resolved) == f"{synthetic_repo}/../spoore", result.stdout
    # And it really does point at the synthetic spoore checkout once normalized.
    assert resolved.resolve() == synthetic_spoore.resolve()


@pytest.mark.parametrize("script", SCRIPTS, ids=lambda p: p.name)
def test_an_explicit_spoore_root_wins_over_the_default(script, tmp_path):
    text = script.read_text()
    match = _DEFAULT_ASSIGNMENT_RE.search(text)
    assert match is not None, "fixture assumption: see the previous test"
    default_assignment_line = match.group(0)

    synthetic_repo = tmp_path / "clone" / "AltaVista-edge"
    synthetic_repo.mkdir(parents=True)
    explicit_spoore = tmp_path / "elsewhere" / "my-spoore"
    explicit_spoore.mkdir(parents=True)

    snippet = f'REPO_ROOT="{synthetic_repo}"\n{default_assignment_line}\nprintf "%s" "${{SPOORE_ROOT}}"\n'
    result = subprocess.run(
        ["bash", "-c", snippet], capture_output=True, text=True,
        env={"PATH": "/usr/bin:/bin", "SPOORE_ROOT": str(explicit_spoore)},
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == str(explicit_spoore), result.stdout


def test_proposers_remaining_absolute_reference_is_only_a_mount_destination():
    """`services/proposer/build-image.sh` is the one script that still contains the literal
    string `/Users/probe/code/spoore` -- deliberately, as `SPOORE_FIXED_ABS_PATH`, a
    CONTAINER-side bind-mount DESTINATION (never a host-side default, and never read from the
    host filesystem): `crates/av-proposer/Cargo.toml`'s own `spoore-models` path dependency and
    its `build.rs`'s direct proto read are still absolute host paths (av-proposer is off-limits
    to this track -- the heavy team's remaining half of question 219(b)/(c)), so SPOORE_ROOT
    (wherever it actually is) is bind-mounted a second time at that exact fixed container path.
    This test pins that the ONLY surviving occurrences are the `SPOORE_FIXED_ABS_PATH`
    assignment, the `-v` mount line using it, and explanatory comments -- never a `[ -d ...]`/
    `[ -f ...]` precondition check or a `SPOORE_ROOT=` default, which would mean a host path
    silently crept back in."""
    proposer = REPO_ROOT / "services/proposer/build-image.sh"
    text = proposer.read_text()
    lines_with_literal = [
        line for line in text.splitlines() if "/Users/probe/code/spoore" in line
    ]
    assert lines_with_literal, "fixture assumption: the literal still appears somewhere"
    for line in lines_with_literal:
        stripped = line.strip()
        is_comment = stripped.startswith("#")
        is_fixed_path_assignment = stripped.startswith("SPOORE_FIXED_ABS_PATH=")
        is_the_mount_line = "-v \"${SPOORE_ROOT}:${SPOORE_FIXED_ABS_PATH}:ro\"" in stripped
        assert is_comment or is_fixed_path_assignment or is_the_mount_line, (
            f"unexpected use of the literal absolute spoore path outside a comment, the "
            f"SPOORE_FIXED_ABS_PATH assignment, or its own mount line: {line!r}"
        )
        # Never a host-path precondition check against the literal (that would mean this
        # script started reading the literal off the host filesystem again).
        assert "[ -d \"/Users/probe/code/spoore" not in stripped
        assert "[ -f \"/Users/probe/code/spoore" not in stripped


def test_edge_plugin_has_no_surviving_literal_absolute_spoore_path():
    """Unlike the proposer script, `services/edge-plugin/build-image.sh` has no crate off-limits
    complication -- `av-edge-plugin`'s own dependency graph never reaches an absolute spoore
    path, so this script's SPOORE_ROOT sibling mount is the only one it needs, and the literal
    string should not appear anywhere in its executable body (comments describing history are
    fine, but this script's own repo has none left after question 219(b))."""
    edge_plugin = REPO_ROOT / "services/edge-plugin/build-image.sh"
    text = edge_plugin.read_text()
    assert "/Users/probe/code/spoore" not in text, (
        "services/edge-plugin/build-image.sh should carry no literal absolute spoore path at "
        "all after question 219(b) -- av-edge-plugin's dependency graph has no off-limits "
        "crate forcing one, unlike services/proposer/build-image.sh"
    )
