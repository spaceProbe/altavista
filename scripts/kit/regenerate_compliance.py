"""scripts/kit/regenerate_compliance.py -- round 3, question 227's decision: one committed
script that performs the WHOLE cross-track compliance-document regeneration cycle, so a
cross-track merge that touches the generated documents (six Rust SBOMs, `SHA256SUMS`,
`docs/compliance/BUNDLE.md`'s recorded hash) never again costs the lead a manual
regenerate-commit-record-commit cycle. Three such cycles in two days (this task's own charter)
is the defect this closes. `native-dynamics` (the P5 successors) owns `scripts/kit/` as of this
round and takes this file next round.

# The five steps, and why they are two commits, not one

1. Regenerate the SBOMs: `scripts/kit/sbom.py --out docs/compliance/sbom` (its own CLI, reused
   verbatim via subprocess -- never re-implemented here), all ten components.
2. Commit them (commit 1) -- only if that regeneration actually changed a byte; an unchanged
   regeneration commits nothing (this is what makes the whole script idempotent on a clean tree,
   deliverable 4's own requirement).
3. Compute the bundle hash: `scripts/kit/evidence.py`'s own `assemble_bundle` (imported, not
   subprocessed -- reading `bundle_sha256` straight off the returned dict is simpler and exactly
   what `docs/compliance/BUNDLE.md`'s own "Regenerating it" section already documents doing by
   hand), with no `--kit`/`--ledger-dir` (the same offline invocation that section names).
4. Record it in `docs/compliance/BUNDLE.md` -- both the fenced hash block
   `tests/test_evidence_bundle.py::_bundle_md_recorded_sha256` parses (`` ```\n([0-9a-f]{64})\n```
   ``, and there must stay exactly one such block in the whole file) and a new row prepended to
   the "Regeneration history" table, dated, with the commit and a one-line cause.
5. Commit that (commit 2) -- again, only if the hash actually moved.

**Why two commits and not one, and why THIS order specifically -- the load-bearing part.**
`scripts/kit/evidence.py::assemble_bundle`'s own top doc: the bundle's `epoch` is
`sbom.git_epoch(epoch_paths)`, and `epoch_paths` names `docs/compliance/sbom/SHA256SUMS` among
its inputs (`git_epoch` is Decision H: "the committer date of the last commit touching these
paths", read straight off already-committed git history, never `datetime.now()` -- see
`scripts/kit/sbom.py::git_epoch`'s own doc). If step 1's SBOM regeneration changes
`SHA256SUMS` and steps 3-4 run BEFORE that change is committed, `epoch` is computed against the
OLD `SHA256SUMS`-touching commit -- wrong the moment the SBOM commit actually lands, because
`epoch` (and therefore `bundle_sha256`) would then immediately recompute to something different
from what got recorded. This is question 214's platform lesson, restated in
`scripts/kit/evidence.py`'s own doc for `git_commit` ("a committed artifact whose input set
includes its own commit is stale the moment it lands") and, concretely, THIS REPOSITORY'S OWN
git history already demonstrates the failure mode directly: commit `d3f18b5` ("Regenerate the
two Python SBOMs from a complete venv and re-record the bundle hash") did both the SBOM
regeneration AND the bundle-hash record in one commit, and recorded a hash
(`b5b99cac4338633f3ea960deecf7d4896b37bb438323632f1b1b07d4263fad1e`) that was already stale the
instant it landed -- because the epoch that commit's own `SHA256SUMS` change would produce did
not exist yet at the moment the hash was computed. The very next commit, `9cf8921` ("Record the
evidence bundle hash at the SBOM regeneration commit"), fixed it by re-running the SAME command
against the now-existing `d3f18b5` commit and recording what it ACTUALLY printed
(`06a1838aed06080ed56a8d9d46a5b131b02c3bbc74f5c3232fb24205e019db3e`). `docs/compliance/BUNDLE.md`'s
own "What the hash depends on" section documents the identical lesson for `git_commit` with a
measured before/after pair at commit `18d923a`. This script's whole reason to exist is to make
that ordering automatic rather than a manual habit the lead has to remember three times in two
days: step 3 (`compute_bundle_hash`) always runs AFTER step 2 (the SBOM commit) has either
landed or been confirmed unnecessary -- never before, never in the same commit.

# What "a complete venv" means, and why this definition (not a new one)

`scripts/kit/sbom.py::python_dist_sbom` already refuses to generate a Python SBOM from an
incomplete venv: `_cross_check_python_packages` compares the committed, declared package set
(`scripts/kit/python-lock.json`, `_load_python_lock`) against what `importlib.metadata` reports
is actually installed in the venv currently running (`_venv_representative_versions`), and
raises `PythonSbomCrossCheckError`, naming every disagreement by package and version, if they do
not match exactly modulo `PYTHON_CROSS_CHECK_IGNORED` (`{"pip"}` -- venv-bootstrap tooling no
declared dependency ever pulls in). Two of the lead's three regenerate cycles (this task's own
charter) were exactly this: a venv missing `setuptools`, or carrying a stale `altavista`
dist-info, silently producing SBOMs that later turned out wrong. `sbom.py` already has the right,
tested, named failure for this -- question 224's own deliverable, proven by
`tests/test_sbom.py::test_a_venv_missing_a_declared_package_fails_the_cross_check_loudly` and
`test_two_different_venvs_produce_one_byte_identical_python_sbom`. Re-implementing a second
notion of "complete" here (a different package list, a different comparison) would be exactly
the kind of duplicated, driftable logic this task's own brief warns against ("reuse scripts/kit's
own existing machinery for the check -- do not re-implement a package resolver"). So: **"complete"
operationally means `check_venv_complete()` below, which calls `sbom._cross_check_python_packages`
for EVERY `sbom.PYTHON_COMPONENTS` entry (both currently use the identical declared set, but this
does not assume that stays true) and raises nothing.** This is checked FIRST, before any file is
touched or any git command that could change history is run -- `scripts/kit/sbom.py`'s own CLI
only discovers this mid-run (after it may have already written some earlier-ordered component
files), which is not good enough for a script whose whole point is "never let an incomplete venv
reach a commit".

# The dirty-tree refusal, and what it does about paths it owns

Before anything is touched, `check_clean_tree()` requires `git status --porcelain` to report
nothing except this worktree's own permanently-pre-existing entries -- the symlinks
`scripts/kit/README.md`'s own `KIT_MANIFEST` doc already names ("symlinks/checkouts reaching
outside the worktree, present before P5 started", e.g. `third_party/mirrors ->
/Users/probe/code/AltaVista/third_party/mirrors`). These are detected structurally
(`_is_known_preexisting_symlink`: a `??` entry that IS a symlink resolving OUTSIDE the repo root),
never as a hard-coded path list, so a new one appearing later does not need an edit here.
**Anything else dirty is a refusal -- INCLUDING the paths this script owns**
(`docs/compliance/sbom/*`, `docs/compliance/BUNDLE.md`). This is deliberate, not an oversight:
another track's uncommitted work sitting anywhere in the tree must never be swept into a
compliance commit this script makes, and a pre-existing uncommitted edit to a generated document
(a stale hand-edit, a half-finished previous run) is itself a problem this script should surface
by refusing, not silently fold into its own commit and launder. Concretely: this repository's own
`crates/av-orbital/**` is, per this task's own dispatch, being edited concurrently by another
worker in this SAME worktree -- if that worker's changes are ever staged/committed mid-flight
(never expected, but not this script's to assume), `check_clean_tree` refuses rather than
guessing whose work is whose. Once this precondition passes, the ONLY paths this script itself
ever `git add`s are the exact ones it just wrote (`docs/compliance/sbom/`, then separately
`docs/compliance/BUNDLE.md`) -- never `git add -A`, never a wildcard broader than that.

# The flag set: default / --dry-run / --check, and why these three

- **(no flag)** -- the real cycle: steps 1-5 above, for real, up to two commits, never pushes.
- **--dry-run** -- performs step 1 for real (writes the regenerated SBOMs to their real,
  committed location, so `git diff`/`git status` show exactly what a real run would stage) but
  never runs `git add`/`git commit`. It also prints a PROVISIONAL `bundle_sha256`, computed
  against the CURRENT HEAD (since dry-run makes no commit, there is no new epoch-affecting commit
  to compute it after) -- explicitly labelled provisional, because `epoch` is git-history-derived
  and a real run's own SBOM commit (if any) can move it; this is an honest limitation of
  previewing a value whose own definition depends on a commit existing, not a bug. For the same
  reason, `--dry-run` never writes `docs/compliance/BUNDLE.md` -- writing a "final-looking" hash
  into it that a real run might not actually produce would be actively misleading.
- **--check** -- a read-only staleness check with NO side effects anywhere: it regenerates the
  SBOMs into a throwaway temporary directory (never touching `docs/compliance/sbom/`), compares
  the fresh bytes against what is actually committed at `HEAD` (`git show HEAD:<path>`, not the
  possibly-dirty working tree -- so `--check` is safe to run inside a worktree that has unrelated
  uncommitted changes elsewhere, exactly this shared worktree's own situation with the concurrent
  Rust track), and separately recomputes the bundle hash and compares it against what
  `docs/compliance/BUNDLE.md` currently records. Exit 0 ("up to date") if everything agrees, exit
  2 (distinct from the venv/dirty-tree refusal's exit 1) naming exactly what would change
  otherwise. This is what `tests/test_regenerate_compliance.py`'s idempotence test drives -- it
  needed a mode that proves nothing WOULD change without ever mutating the shared worktree's git
  history, and `--check` is that mode. (`compute_bundle_hash` still reads the evidence content --
  control matrices, `SHA256SUMS` -- from the WORKING TREE, matching `evidence.py`'s own design;
  `--check`'s SBOM comparison is HEAD-based specifically to stay correct under a dirty tree, but
  this one piece inherits `evidence.py`'s own scope, stated here rather than glossed over.)

These three cover the three real needs: do it, preview a real pending change before trusting it,
and answer "is anything stale" with no side effects at all (safe for CI, and for a test).

# Other standing rules this script follows

- **No push, anywhere.** Never calls `git push`.
- **No Co-Authored-By, no Claude/Anthropic attribution, in any commit this script writes** --
  the user's own standing rule. Every commit message is written to a temp file first and passed
  with `git commit -F <file>`, never `-m`/a heredoc.
- **No new third-party dependency.** Stdlib only, plus this repository's own `sbom`/`evidence`
  modules (ADR-004's crypto rule: hashing goes through `hashlib`, exactly as `sbom.sha256_file`
  already does -- reused here transitively through `evidence.assemble_bundle`, never
  re-implemented).
- **No network** (standing rule 154) -- every subprocess this script runs is `git` or this
  repository's own `scripts/kit/sbom.py` (which itself runs `cargo auditable build --offline`,
  never online, for the Rust components).

CLI: `.venv/bin/python scripts/kit/regenerate_compliance.py [--dry-run | --check]`
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Optional

REPO_ROOT = Path(__file__).resolve().parents[2]
KIT_DIR = Path(__file__).resolve().parent
if str(KIT_DIR) not in sys.path:
    sys.path.insert(0, str(KIT_DIR))
import sbom  # noqa: E402  (path insert must precede this import)
# `evidence` is deliberately NOT imported here: `evidence.py` imports `gmat_service.evidence` at
# module scope, which imports real `google.protobuf` code -- an actual package install, not just
# `importlib.metadata` -- so importing it eagerly would mean this script could crash with a raw
# traceback on a broken-enough environment BEFORE `check_venv_complete` (below) ever runs, which
# defeats the whole point of checking "before it changes anything". `sbom` alone is enough for
# that check (stdlib + `importlib.metadata` + this repository's own `licences` module, all
# metadata-only). `evidence` is imported lazily, inside `compute_bundle_hash`, the one place that
# actually needs it -- always called AFTER `check_venv_complete` has already passed.

SBOM_OUT_DIR = REPO_ROOT / "docs" / "compliance" / "sbom"
BUNDLE_MD_PATH = REPO_ROOT / "docs" / "compliance" / "BUNDLE.md"

#: The exact shape `tests/test_evidence_bundle.py::_bundle_md_recorded_sha256` parses -- there
#: must stay exactly one match in the whole file.
HASH_BLOCK_RE = re.compile(r"```\n([0-9a-f]{64})\n```")

#: Must match, verbatim, the table header actually committed in `docs/compliance/BUNDLE.md`'s
#: own "Regeneration history" section -- `update_bundle_md` (below) inserts each new row right
#: after this exact text.
TABLE_HEADER = "| Date | Hash | Commit | Cause |\n|---|---|---|---|\n"
TABLE_HEADER_RE = re.compile(re.escape(TABLE_HEADER))


class RegenerateRefusal(RuntimeError):
    """Any pre-flight or structural refusal (incomplete venv, dirty tree, a `sbom.py`/BUNDLE.md
    shape this script does not understand) -- caught once, in `main`, and turned into a clean,
    named, one-shot CLI failure on stderr with a non-zero exit, never a raw traceback."""


def _print(*args: object) -> None:
    print(*args, file=sys.stderr)


# =================================================================================================
# 1. venv completeness (checked FIRST, before any file is touched)
# =================================================================================================

def check_venv_complete() -> None:
    """See this module's own top doc, "What 'a complete venv' means". Reuses
    `sbom._load_python_lock`/`sbom._cross_check_python_packages` verbatim -- never a second
    package resolver."""
    try:
        declared = sbom._load_python_lock()
    except RuntimeError as exc:
        raise RegenerateRefusal(str(exc)) from exc

    problems: list[str] = []
    for component in sbom.PYTHON_COMPONENTS:
        try:
            sbom._cross_check_python_packages(component, declared)
        except sbom.PythonSbomCrossCheckError as exc:
            problems.append(str(exc))
    if problems:
        raise RegenerateRefusal(
            "this venv is not complete -- the declared Python package set "
            "(scripts/kit/python-lock.json) disagrees with what is actually installed here:\n"
            + "\n".join(f"  - {p}" for p in problems)
            + "\n\nFix: .venv/bin/pip install -e \".[dev]\", then re-run this script."
        )


# =================================================================================================
# 2. Dirty-tree refusal
# =================================================================================================

def _is_known_preexisting_symlink(relpath: str) -> bool:
    """True for a `??` `git status` entry that is a symlink resolving OUTSIDE this repository --
    `scripts/kit/README.md`'s own documented permanently-pre-existing category. Structural, not a
    fixed path list: a new one appearing later needs no edit here."""
    p = REPO_ROOT / relpath
    if not p.is_symlink():
        return False
    try:
        target = p.resolve()
    except OSError:
        return False
    try:
        target.relative_to(REPO_ROOT.resolve())
        return False  # resolves INSIDE the repo -- not this category
    except ValueError:
        return True


def check_clean_tree() -> None:
    """Refuses on anything in `git status --porcelain` except the known pre-existing symlinks
    above -- deliberately including this script's own owned paths (`docs/compliance/sbom/*`,
    `docs/compliance/BUNDLE.md`). See this module's own top doc, "The dirty-tree refusal, and
    what it does about paths it owns", for why owned paths are not exempted."""
    status = _git("status", "--porcelain").stdout
    blocking = []
    for line in status.splitlines():
        if not line.strip():
            continue
        relpath = line[3:].split(" -> ")[-1].strip()
        if line[:2] == "??" and _is_known_preexisting_symlink(relpath):
            continue
        blocking.append(line)
    if blocking:
        raise RegenerateRefusal(
            "the working tree is not clean -- refusing to run (a compliance regeneration commit "
            "must never sweep in another track's uncommitted work, or a stale, hand-edited "
            "compliance document, alongside its own changes). Offending `git status --porcelain` "
            "line(s):\n"
            + "\n".join(f"  {line}" for line in blocking)
            + "\n\nCommit or stash that work first, in its own commit -- including any "
              "uncommitted edit under a path this script owns: an uncommitted change there was "
              "not made by this run, so it is not this run's to fold in either."
        )


# =================================================================================================
# 3. git helpers
# =================================================================================================

def _git(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", *args], cwd=REPO_ROOT, capture_output=True, text=True, check=check,
    )


def _git_show_bytes(relpath: str) -> Optional[bytes]:
    """`git show HEAD:<relpath>` -- the COMMITTED bytes, regardless of what the working tree
    currently holds at that path. `None` if `HEAD` has no such path."""
    result = subprocess.run(
        ["git", "show", f"HEAD:{relpath}"], cwd=REPO_ROOT, capture_output=True, check=False,
    )
    if result.returncode != 0:
        return None
    return result.stdout


def _stage(paths: list[str]) -> tuple[list[str], str]:
    """Stages exactly `paths` (never `-A`) and returns `(changed_paths, diff --stat text)` --
    leaves the paths STAGED either way; caller decides whether to commit or `_unstage`."""
    _git("add", "--", *paths)
    names = [l for l in _git("diff", "--cached", "--name-only").stdout.splitlines() if l.strip()]
    stat = _git("diff", "--cached", "--stat").stdout
    return names, stat


def _unstage(paths: list[str]) -> None:
    _git("reset", "--", *paths, check=False)


def _commit_staged(message: str) -> str:
    """`git commit -F <tempfile>` -- never `-m`, never a heredoc (this task's own instruction).
    No Co-Authored-By / Claude attribution is ever added to `message`. Returns the new commit's
    short hash. Never pushes."""
    with tempfile.NamedTemporaryFile(
        "w", suffix=".txt", delete=False, dir=str(REPO_ROOT / ".git"),
    ) as f:
        f.write(message.strip() + "\n")
        msg_path = f.name
    try:
        _git("commit", "-F", msg_path)
    finally:
        Path(msg_path).unlink(missing_ok=True)
    return _git("rev-parse", "--short", "HEAD").stdout.strip()


# =================================================================================================
# 4. SBOM regeneration (reuses scripts/kit/sbom.py's own CLI, via subprocess, verbatim)
# =================================================================================================

def regenerate_sboms(out_dir: Path) -> subprocess.CompletedProcess:
    """Runs `scripts/kit/sbom.py --out <out_dir>` under `sys.executable` -- the SAME interpreter
    (and therefore venv) this script itself is running under, so `sbom.py`'s own internal
    cross-check (redundant with `check_venv_complete` above, by design -- defence in depth, not
    duplication of logic, since both call the identical `sbom._cross_check_python_packages`) is
    over the identical venv already verified complete."""
    return subprocess.run(
        [sys.executable, str(KIT_DIR / "sbom.py"), "--out", str(out_dir)],
        cwd=REPO_ROOT, capture_output=True, text=True,
    )


# =================================================================================================
# 5. Bundle hash + docs/compliance/BUNDLE.md update
# =================================================================================================

def compute_bundle_hash() -> str:
    """`evidence.assemble_bundle(repo_root=REPO_ROOT)` with no `--kit`/`--ledger-dir` -- the
    exact offline invocation `docs/compliance/BUNDLE.md`'s own "Regenerating it" section
    documents running by hand. Reads the CURRENT WORKING TREE (evidence.py's own design, not
    something this script changes). Imports `evidence` lazily, here -- see the module-level
    comment where `sbom` is imported for why."""
    import evidence  # noqa: E402  (lazy: see the module-level comment on the sbom import)
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT)
    return bundle["bundle_sha256"]


def _recorded_hash_and_span(text: str) -> tuple[str, int, int]:
    """`(hash, start, end)` for the sole fenced 64-hex-char block -- `start`/`end` bound just the
    hex digits, so `text[:start] + new_hash + text[end:]` replaces only them. Refuses (rather
    than guessing) if there is not EXACTLY one match, matching
    `tests/test_evidence_bundle.py::_bundle_md_recorded_sha256`'s own single-match assumption."""
    matches = list(HASH_BLOCK_RE.finditer(text))
    if len(matches) != 1:
        raise RegenerateRefusal(
            f"docs/compliance/BUNDLE.md: expected exactly one fenced 64-hex-char hash block, "
            f"found {len(matches)} -- refusing to guess which one to update"
        )
    m = matches[0]
    return m.group(1), m.start(1), m.end(1)


def _describe_cause(sbom_changed_paths: list[str]) -> str:
    non_sums = sorted(
        Path(p).name for p in sbom_changed_paths if not p.endswith("SHA256SUMS")
    )
    if non_sums:
        names = ", ".join(f"`{n}`" for n in non_sums)
        return f"scripts/kit/regenerate_compliance.py: SBOM regeneration changed {names}."
    return (
        "scripts/kit/regenerate_compliance.py: `bundle_sha256` moved with no SBOM byte changed "
        "-- some other evidence-dependent input (e.g. a control matrix) changed; see this "
        "commit's own diff."
    )


def update_bundle_md(text: str, new_hash: str, date: str, commit: str, cause: str) -> str:
    """Updates BOTH the fenced hash block and prepends one new row to the "Regeneration history"
    table -- the two things `docs/compliance/BUNDLE.md`'s own "record it" step must keep in sync
    (this task's own deliverable 2's explicit instruction)."""
    old_hash, start, end = _recorded_hash_and_span(text)
    text = text[:start] + new_hash + text[end:]

    header_match = TABLE_HEADER_RE.search(text)
    if not header_match:
        raise RegenerateRefusal(
            "docs/compliance/BUNDLE.md: could not find the regeneration-history table header "
            f"({TABLE_HEADER!r}) to insert a new row into"
        )
    row = f"| {date} | `{new_hash}` | `{commit}` | {cause} |\n"
    insert_at = header_match.end()
    return text[:insert_at] + row + text[insert_at:]


# =================================================================================================
# 6. The three modes
# =================================================================================================

def run_real(dry_run: bool) -> int:
    check_venv_complete()
    check_clean_tree()

    result = regenerate_sboms(SBOM_OUT_DIR)
    if result.returncode != 0:
        _print(result.stdout)
        _print(result.stderr)
        raise RegenerateRefusal("scripts/kit/sbom.py failed -- see its own stderr above")

    sbom_changed, sbom_stat = _stage([str(SBOM_OUT_DIR)])

    if dry_run:
        _unstage([str(SBOM_OUT_DIR)])
        _print("[dry-run] step 1 (SBOM regeneration) -- files on disk were updated for real; "
               "nothing staged or committed:")
        _print(sbom_stat.strip() or "  (no change)")
        provisional = compute_bundle_hash()
        recorded, _, _ = _recorded_hash_and_span(BUNDLE_MD_PATH.read_text(encoding="utf-8"))
        _print(
            f"[dry-run] provisional bundle_sha256, computed against the CURRENT HEAD (no commit "
            f"was made, so there is no new epoch-affecting commit to compute it after -- a real "
            f"run's own commit 1, if any, may move it): {provisional}"
        )
        if not sbom_stat.strip() and provisional == recorded:
            _print("[dry-run] tree is already current -- a real run would commit nothing.")
        else:
            _print(
                f"[dry-run] docs/compliance/BUNDLE.md currently records {recorded}; it was NOT "
                f"written (writing a value that may not match what a real run actually commits "
                f"would be misleading)."
            )
        return 0

    if sbom_changed:
        sbom_commit = _commit_staged(
            "compliance: regenerate the SBOMs\n\n"
            "scripts/kit/regenerate_compliance.py, step 1/2 of the compliance regeneration "
            "cycle (question 227)."
        )
        _print(f"[commit 1] {sbom_commit}")
        _print(sbom_stat.strip())
    else:
        _unstage([str(SBOM_OUT_DIR)])
        _print("[commit 1] SBOMs already current -- no commit")

    new_hash = compute_bundle_hash()
    text = BUNDLE_MD_PATH.read_text(encoding="utf-8")
    recorded_hash, _, _ = _recorded_hash_and_span(text)
    if new_hash == recorded_hash:
        _print(f"[commit 2] bundle_sha256 unchanged ({new_hash}) -- no BUNDLE.md update, no commit")
        return 0

    head_short = _git("rev-parse", "--short", "HEAD").stdout.strip()
    date = _git("log", "-1", "--format=%cs", "HEAD").stdout.strip()
    cause = _describe_cause(sbom_changed)
    new_text = update_bundle_md(text, new_hash, date, head_short, cause)
    BUNDLE_MD_PATH.write_text(new_text, encoding="utf-8")

    bundle_changed, bundle_stat = _stage([str(BUNDLE_MD_PATH)])
    if bundle_changed:
        bundle_commit = _commit_staged(
            "compliance: record the regenerated bundle hash\n\n"
            "scripts/kit/regenerate_compliance.py, step 2/2 -- computed after commit 1 (or "
            "confirmed unnecessary), per the epoch-ordering rule docs/compliance/BUNDLE.md's "
            "own \"What the hash depends on\" section requires (question 227)."
        )
        _print(f"[commit 2] {bundle_commit}: recorded bundle_sha256={new_hash}")
    else:  # pragma: no cover -- new_hash != recorded_hash already implies a real diff
        _unstage([str(BUNDLE_MD_PATH)])
        _print("[commit 2] BUNDLE.md already reflected this hash -- no commit")
    return 0


def run_check() -> int:
    check_venv_complete()

    stale: list[str] = []
    with tempfile.TemporaryDirectory() as td:
        tmp_out = Path(td)
        result = regenerate_sboms(tmp_out)
        if result.returncode != 0:
            _print(result.stdout)
            _print(result.stderr)
            raise RegenerateRefusal("scripts/kit/sbom.py failed during --check -- see stderr above")

        fresh_hashes: dict[str, str] = {}
        for name in sbom.all_component_names():
            fname = f"{name}.cdx.json"
            fresh = (tmp_out / fname).read_bytes()
            fresh_hashes[fname] = sbom.sha256_file(tmp_out / fname)
            committed = _git_show_bytes(f"docs/compliance/sbom/{fname}")
            if committed != fresh:
                stale.append(f"docs/compliance/sbom/{fname}")

        # `sbom.rewrite_sha256sums` records each path RELATIVE TO REPO_ROOT when writing to the
        # real docs/compliance/sbom/ location, but falls back to OUT-DIR-relative paths when
        # `out_dir` (here, `tmp_out`) is outside the repo (its own doc: "a test/scratch out_dir
        # outside the repo"). A raw byte-comparison of `tmp_out/SHA256SUMS` against the committed
        # file would therefore report every entry "stale" purely because of WHERE this check
        # happened to regenerate into, never mind whether any SBOM's content actually changed --
        # measured directly against this checkout (a real run of `--check` during this task hit
        # exactly this false positive before this fix). So the expected text is built here from
        # the fresh hashes with the REAL repo-relative names, byte-for-byte matching what
        # `rewrite_sha256sums` itself produces when it runs against the real location.
        expected_sums_lines = [
            f"{fresh_hashes[fname]}  docs/compliance/sbom/{fname}" for fname in sorted(fresh_hashes)
        ]
        expected_sums_text = ("\n".join(expected_sums_lines) + "\n") if expected_sums_lines else ""
        committed_sums = _git_show_bytes("docs/compliance/sbom/SHA256SUMS")
        if committed_sums != expected_sums_text.encode("utf-8"):
            stale.append("docs/compliance/sbom/SHA256SUMS")

    new_hash = compute_bundle_hash()
    head_bundle_md = _git_show_bytes("docs/compliance/BUNDLE.md")
    if head_bundle_md is None:
        raise RegenerateRefusal("docs/compliance/BUNDLE.md is not committed at HEAD")
    recorded_hash, _, _ = _recorded_hash_and_span(head_bundle_md.decode("utf-8"))
    if new_hash != recorded_hash:
        stale.append(
            f"docs/compliance/BUNDLE.md (records {recorded_hash}, regenerates to {new_hash})"
        )

    if stale:
        _print("STALE -- a real run would change:")
        for s in stale:
            _print(f"  {s}")
        return 2
    _print("up to date -- a real run would change nothing")
    return 0


# =================================================================================================
# CLI
# =================================================================================================

def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--dry-run", action="store_true",
        help="regenerate the SBOMs for real (written to their real, committed path) and print "
             "the diff plus a provisional bundle_sha256; commit nothing; leave "
             "docs/compliance/BUNDLE.md untouched",
    )
    mode.add_argument(
        "--check", action="store_true",
        help="read-only: regenerate into a scratch directory and compare against what HEAD "
             "actually has committed; exit 2 (not 1) if anything is stale; never writes or "
             "commits anything",
    )
    args = parser.parse_args(argv)

    try:
        if args.check:
            return run_check()
        return run_real(dry_run=args.dry_run)
    except RegenerateRefusal as exc:
        _print(f"error: {exc}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
