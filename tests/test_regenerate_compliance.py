"""tests/test_regenerate_compliance.py -- round 3, question 227's fourth deliverable: proves
`scripts/kit/regenerate_compliance.py` is idempotent on a clean tree (running it again makes no
change and creates no commit).

**Why its own file, not folded into `tests/test_sbom.py` or `tests/test_evidence_bundle.py`:**
those two test each generator's own content directly (`sbom.py`'s SBOM shape, `evidence.py`'s
bundle shape). This tests the CYCLE script that drives both of them together plus its own
git-commit bookkeeping -- a different unit under test, with its own real, `cargo
auditable`-triggering cost, so it gets its own opt-in gate rather than piggy-backing on either
neighbour's `AV_SBOM_REBUILD` guard for an unrelated reason.

**Exercises the real script via subprocess** (`sys.executable scripts/kit/regenerate_compliance.py
--check`), never a mock of it, per this task's own instruction. `--check` is the honest choice
for "prove idempotent without mutating the shared worktree's git history": by construction it
never runs `git add`/`git commit` and never writes any real repository path -- it regenerates
into a throwaway temp directory and compares against `git show HEAD:...` (see that script's own
doc comment, "The flag set"). That makes it safe to run inside a worktree another track is
concurrently editing (this repository's own situation while this task ran: `crates/av-orbital/**`
was being edited by another worker throughout) without needing `--dry-run`'s real working-tree
file writes, or a throwaway git clone, either.

**Gated on `AV_SBOM_REBUILD=1`**, the SAME opt-in variable `tests/test_sbom.py`'s own expensive
Rust round-trip test already uses (`test_rust_sbom_regenerates_byte_identically`): `--check`
regenerates ALL TEN components, including the six Rust ones (`cargo auditable build --offline`,
a real compile per component, tens of seconds to a few minutes each, plus this host's own
`syspolicyd` scan of every freshly-linked binary) -- the identical cost, so it stays out of the
default fast suite for the identical reason, and skips VISIBLY (question 154's own pattern:
named, never silent) rather than being silently absent.

No test here mutates `os.environ` (question 199) -- `AV_SBOM_REBUILD` is only ever READ, via
`os.environ.get`, exactly matching `tests/test_sbom.py`'s own `_opted_in()`. No network
(question 154): `--check`'s only subprocesses are `git`, `cargo --offline`, and this
repository's own `scripts/kit/sbom.py` / `scripts/kit/evidence.py`.

**The `update_bundle_md`/`_recorded_hash_and_span` tests below are NOT gated.** Manager review
(round 3 follow-up) found this gap: both tests above only ever run `--check`, which is entirely
about the SBOMs; nothing in the default suite exercised the "record it in BUNDLE.md" step at all
-- the one most likely to break silently, since it hand-edits text rather than regenerating a
whole document from scratch. `update_bundle_md` and `_recorded_hash_and_span` are pure functions
(no subprocess, no cargo, no git-mutating call -- `text -> text`), so testing them costs
milliseconds and has no reason to wait for `AV_SBOM_REBUILD=1`.
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
SCRIPT = KIT_DIR / "regenerate_compliance.py"
if str(KIT_DIR) not in sys.path:
    sys.path.insert(0, str(KIT_DIR))
import regenerate_compliance as rc  # noqa: E402  (path insert must precede this import)

OPT_IN_VAR = "AV_SBOM_REBUILD"


def _opted_in() -> bool:
    return os.environ.get(OPT_IN_VAR, "").strip().lower() in ("1", "true", "yes")


def _compute_skip_reason() -> str | None:
    if not _opted_in():
        return (
            f"{OPT_IN_VAR} is not set -- scripts/kit/regenerate_compliance.py --check "
            "regenerates all ten components, including the six Rust ones (`cargo auditable "
            "build --offline`, a real compile per component). The default suite skips it, "
            "matching tests/test_sbom.py's own test_rust_sbom_regenerates_byte_identically; "
            "set AV_SBOM_REBUILD=1 to opt in and actually run it."
        )
    return None


_SKIP_REASON = _compute_skip_reason()


def _git_head_and_status() -> tuple[str, str]:
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True, check=True,
    ).stdout.strip()
    status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=REPO_ROOT, capture_output=True, text=True, check=True,
    ).stdout
    return head, status


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_check_is_idempotent_on_a_clean_tree():
    """The acceptance criterion, verbatim: running the script on a tree whose compliance
    documents are already current makes no change and creates no commit. `--check` never writes
    or commits anything BY CONSTRUCTION -- so proving idempotence here means proving it reports
    "nothing to do" (exit 0, "up to date" on stderr) against this checkout's own real, current,
    committed state, AND that `HEAD`/the working tree are byte-for-byte unmoved afterward."""
    before_head, before_status = _git_head_and_status()

    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--check"], cwd=REPO_ROOT, capture_output=True, text=True,
    )
    print(f"\nrc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}")
    assert result.returncode == 0, (
        "--check reported the tree is NOT up to date -- either this checkout's compliance "
        "documents really are stale (run the script for real and commit the result) or the "
        "script has a bug"
    )
    assert "up to date" in result.stderr

    after_head, after_status = _git_head_and_status()
    assert after_head == before_head, "--check must never create a commit"
    assert after_status == before_status, "--check must never change the working tree"


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_check_run_twice_in_a_row_agrees_with_itself():
    """A direct restatement of "idempotent": two --check runs back to back over the same
    (unchanged) tree state must report the IDENTICAL verdict -- not just "both exit 0", but the
    identical stdout/stderr text, since --check's own report names exactly what (if anything)
    would change and that list must not itself be nondeterministic."""
    results = [
        subprocess.run(
            [sys.executable, str(SCRIPT), "--check"], cwd=REPO_ROOT, capture_output=True, text=True,
        )
        for _ in range(2)
    ]
    for i, r in enumerate(results):
        print(f"\nrun {i}: rc={r.returncode}\nstdout:\n{r.stdout}\nstderr:\n{r.stderr}")
    assert results[0].returncode == results[1].returncode
    assert results[0].stdout == results[1].stdout
    assert results[0].stderr == results[1].stderr


# =================================================================================================
# `update_bundle_md` / `_recorded_hash_and_span` -- pure functions, no cargo cost, UNGATED.
# =================================================================================================

def _real_bundle_md_text() -> str:
    return (REPO_ROOT / "docs" / "compliance" / "BUNDLE.md").read_text(encoding="utf-8")


def test_update_bundle_md_keeps_exactly_one_hash_block_and_adds_one_row():
    """The "record it in BUNDLE.md" step's own contract, exercised directly against the real,
    currently-committed-or-working-tree `docs/compliance/BUNDLE.md` text (not a synthetic
    fixture -- a hand-built fixture could accidentally not match this file's real shape). Feeds
    `update_bundle_md` a new hash and asserts: still exactly one fenced 64-hex-char block
    (`tests/test_evidence_bundle.py`'s own parsed shape) and it IS the new hash; exactly one new
    row was prepended directly under the table header; and the previous top row is still present,
    directly below the new one -- nothing else in the table was disturbed."""
    text = _real_bundle_md_text()
    old_hash, _, _ = rc._recorded_hash_and_span(text)

    # The previous top row, verbatim, so we can look for it unperturbed in the output.
    header_idx = text.index(rc.TABLE_HEADER)
    after_header = text[header_idx + len(rc.TABLE_HEADER):]
    previous_top_row = after_header.splitlines()[0]
    assert old_hash in previous_top_row, (
        f"test's own assumption broken: the table's current top row does not contain the "
        f"recorded hash -- previous_top_row={previous_top_row!r}"
    )

    new_hash = "2" * 64
    new_text = rc.update_bundle_md(text, new_hash, "2026-01-01", "abc1234", "synthetic test cause")

    matches = rc.HASH_BLOCK_RE.findall(new_text)
    assert matches == [new_hash], (
        f"expected exactly one fenced hash block containing the NEW hash; got {matches!r}"
    )

    new_header_idx = new_text.index(rc.TABLE_HEADER)
    new_after_header = new_text[new_header_idx + len(rc.TABLE_HEADER):]
    lines = new_after_header.splitlines()
    assert lines[0] == (
        f"| 2026-01-01 | `{new_hash}` | `abc1234` | synthetic test cause |"
    ), f"new row not directly under the table header: {lines[0]!r}"
    assert lines[1] == previous_top_row, (
        f"previous top row must still be present, directly below the new one; got {lines[1]!r}"
    )

    # Nothing else changed: strip out the one row we added and the one hash we replaced, and the
    # remainder must be byte-identical to the original.
    without_new_row = new_text.replace(lines[0] + "\n", "", 1)
    without_new_hash = without_new_row.replace(new_hash, old_hash, 1)
    assert without_new_hash == text, "update_bundle_md changed something other than the hash block and the new row"


def test_recorded_hash_and_span_refuses_on_two_hash_blocks():
    """The refusal that stops the script from ever guessing which fenced block to update. Built
    from the real file's own text (so the surrounding shape is real) with a second, synthetic
    fenced 64-hex-char block spliced in."""
    text = _real_bundle_md_text()
    extra_block = "\n```\n" + ("3" * 64) + "\n```\n"
    perturbed = text + extra_block
    assert len(rc.HASH_BLOCK_RE.findall(perturbed)) == 2  # sanity: the perturbation worked

    with pytest.raises(rc.RegenerateRefusal, match="expected exactly one"):
        rc._recorded_hash_and_span(perturbed)
