"""Round 4 regression guard for the defect in commit c7c04dd (question 219(c)): that commit
relativized the root `Cargo.toml`'s own `spoore-cdm` path dependency (`../spoore/crates/
spoore-cdm`, question 219(c)'s stated goal -- "a clone anywhere with spoore beside it builds")
but left `crates/av-track/Cargo.toml`'s four spoore path dependencies
(spoore-engine/models/assoc/tree) as absolute `/Users/probe/code/spoore/...` paths. Because
each of those crates depends on `spoore-cdm` through spoore's own workspace, cargo resolved
`spoore-cdm` via *two* different manifest paths (the root's relative one and the absolute one
reached through av-track's four) and refused to write the lockfile:
"error: package collision in the lockfile: packages spoore-cdm v0.0.0 (.../spoore/crates/
spoore-cdm) and spoore-cdm v0.0.0 (.../spoore/crates/spoore-cdm) are different, but only one
can be written to lockfile unambiguously" -- reproduced in a clone elsewhere, invisible on this
host only because `../spoore` from `AltaVista-edge/` happens to already equal
`/Users/probe/code/spoore`.

This test walks every Cargo.toml git actually tracks and asserts no absolute
`/Users/probe/...` path dependency exists in any of them, with exactly one declared, commented,
POSITIVE exception list: `crates/av-proposer/Cargo.toml`'s own two remaining absolute spoore
path deps (`spoore-models`, `spoore-ml`) -- `crates/av-proposer` is off-limits to this track
(round 4 brief) and is the heavy track's own remaining half of question 219(b)/(c). "Positive"
means the exception list is asserted to still be accurate, not just used to silence a check: if
the heavy track relativizes av-proposer's two lines, `test_every_declared_exception_is_still_
present` below starts failing, forcing this file's exception list (and this docstring) to be
edited rather than silently continuing to permit something already fixed.
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]

# Matches an actual TOML `path = "..."` value assigned to an absolute /Users/probe/... path --
# not a `#`-comment merely mentioning the string in prose (several files, including this
# repo's own root Cargo.toml and crates/av-track/Cargo.toml, explain the fix in comments that
# quote the old absolute path; those are not dependency declarations and must not trip this).
_ABS_PATH_DEP_RE = re.compile(r'path\s*=\s*"(/Users/probe/[^"]*)"')

# Positive exception list, keyed by repo-relative Cargo.toml path -> the exact dependency
# lines (stripped) that are still allowed to declare an absolute /Users/probe/... path.
# crates/av-proposer is off-limits to this track (round 4 brief); its two spoore path deps
# are the heavy track's own remaining half of question 219(b)/(c), not this track's to fix.
_ALLOWED_ABSOLUTE_PATH_LINES: dict[str, tuple[str, ...]] = {
    "crates/av-proposer/Cargo.toml": (
        'spoore-models = { path = "/Users/probe/code/spoore/crates/spoore-models" }',
        'spoore-ml = { path = "/Users/probe/code/spoore/crates/spoore-ml" }',
    ),
}


def _committed_cargo_tomls() -> list[Path]:
    """Every Cargo.toml git actually tracks -- a `git ls-files` listing, not a filesystem walk,
    so nothing untracked (e.g. a scratch clone, a build directory) can trip or hide from this
    test, and nothing committed can be missed by relying on a hardcoded list going stale."""
    result = subprocess.run(
        ["git", "ls-files", "*Cargo.toml"],
        cwd=REPO_ROOT, capture_output=True, text=True, check=True,
    )
    return sorted(REPO_ROOT / line for line in result.stdout.splitlines() if line)


CARGO_TOMLS = _committed_cargo_tomls()


def test_fixture_assumption_cargo_tomls_were_actually_found():
    """If this is empty, `git ls-files` ran from the wrong place or found nothing -- every
    other test in this file would then vacuously pass, which is worse than failing loudly."""
    assert len(CARGO_TOMLS) >= 20, CARGO_TOMLS


@pytest.mark.parametrize("cargo_toml", CARGO_TOMLS, ids=lambda p: str(p.relative_to(REPO_ROOT)))
def test_no_unexpected_absolute_spoore_or_home_path_dependency(cargo_toml: Path):
    rel = str(cargo_toml.relative_to(REPO_ROOT))
    allowed = _ALLOWED_ABSOLUTE_PATH_LINES.get(rel, ())
    text = cargo_toml.read_text()
    for lineno, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if stripped.startswith("#"):
            continue  # prose explaining the fix, not a dependency declaration
        if not _ABS_PATH_DEP_RE.search(line):
            continue
        assert stripped in allowed, (
            f"{rel}:{lineno}: absolute /Users/probe/... path dependency is not allowed "
            f"(question 219(c) -- see this file's module docstring for the failure mode this "
            f"guards against). Found: {line!r}. A path dependency must resolve relative to a "
            f"sibling checkout, e.g. crates/av-track/Cargo.toml's "
            f'`{{ path = "../../../spoore/crates/<name>" }}`. If this really is a deliberate, '
            f"reviewed new exception, it must be added to _ALLOWED_ABSOLUTE_PATH_LINES here as "
            f"a positive, commented entry -- never silently passed over."
        )


def test_every_declared_exception_is_still_present():
    """The other half of the guard, and the reason the exception list is safe to keep at all:
    each allowed line must actually still be there, verbatim. The day the heavy track
    relativizes av-proposer's remaining two lines (its own half of question 219(b)/(c)), this
    test starts failing -- forcing this file's exception entry to be deleted rather than
    silently continuing to permit something that no longer exists. A stale allowance that
    silently permits something already fixed is the failure mode this test exists to prevent."""
    assert _ALLOWED_ABSOLUTE_PATH_LINES, "exception list should not be empty while av-proposer exists"
    for rel, lines in _ALLOWED_ABSOLUTE_PATH_LINES.items():
        path = REPO_ROOT / rel
        assert path.exists(), (
            f"{rel} no longer exists -- delete its now-meaningless exception entry from "
            f"_ALLOWED_ABSOLUTE_PATH_LINES"
        )
        file_lines = {l.strip() for l in path.read_text().splitlines()}
        for expected in lines:
            assert expected in file_lines, (
                f"{rel}: expected exception line no longer present verbatim: {expected!r}. "
                f"This means av-proposer's absolute spoore path dependency has been fixed "
                f"(question 219(b)/(c)'s remaining, heavy-track half). Delete this line from "
                f"_ALLOWED_ABSOLUTE_PATH_LINES -- do not leave a stale allowance in place for "
                f"something already fixed."
            )


def test_av_track_four_spoore_deps_are_the_measured_relative_path():
    """The actual round 4 regression, pinned directly: av-track's four spoore path deps must
    be relative with exactly the measured `../../../spoore/crates/<name>` depth (one more `..`
    than the root Cargo.toml's own `../spoore/crates/spoore-cdm`, because a path dependency
    resolves relative to its OWN manifest's directory, crates/av-track/, not the workspace
    root) -- not the absolute host paths c7c04dd's fix left behind here."""
    text = (REPO_ROOT / "crates/av-track/Cargo.toml").read_text()
    for name in ("spoore-engine", "spoore-models", "spoore-assoc", "spoore-tree"):
        expected = f'{name} = {{ path = "../../../spoore/crates/{name}" }}'
        assert expected in text, f"crates/av-track/Cargo.toml: expected {expected!r}"
    for lineno, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if stripped.startswith("#"):
            continue
        assert "/Users/probe" not in line, (
            f"crates/av-track/Cargo.toml:{lineno}: still has an absolute path outside a "
            f"comment: {line!r}"
        )


def test_build_rs_spoore_proto_root_absolute_reference_is_the_declared_exception():
    """`crates/av-proposer/build.rs`'s `SPOORE_PROTO_ROOT` constant is the same off-limits
    crate's remaining absolute path, just in a build script rather than a Cargo.toml, so it
    does not fit `_ALLOWED_ABSOLUTE_PATH_LINES` (that dict is keyed by Cargo.toml path/line;
    forcing a `.rs` file into the same shape would obscure more than it would share) -- covered
    here instead with the same positive-exception shape: this test both pins that the absolute
    constant is still there today and, like the Cargo.toml exceptions above, is expected to
    start failing (and be deleted) the day the heavy track relativizes it."""
    build_rs = REPO_ROOT / "crates/av-proposer/build.rs"
    expected = 'const SPOORE_PROTO_ROOT: &str = "/Users/probe/code/spoore/proto";'
    text = build_rs.read_text()
    assert expected in text, (
        f"crates/av-proposer/build.rs: expected SPOORE_PROTO_ROOT's absolute path constant "
        f"still present verbatim: {expected!r}. If this has been relativized, delete this "
        f"test -- it is the heavy track's own remaining half of question 219(b)/(c), and a "
        f"stale allowance must not outlive the thing it was excusing."
    )
