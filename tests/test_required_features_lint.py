"""Question 231 (heavy round 6, task 7): `cargo clippy --workspace --all-targets` never
builds a `[[bin]]`, `[[example]]`, `[[test]]` or `[[bench]]` that declares
`required-features` unless those features are enabled, so such a target is invisible to the
workspace's own named clippy gate -- found when `av-jobs`'s `av-tile-fixture` binary
(`required-features = ["store-fixture"]`) went unlinted by a plain `cargo clippy --workspace
--all-targets`. `scripts/lint/required_features_clippy.sh` (and its companion
`scripts/lint/required_features_clippy.py`, which does the actual discovery) closes that
hole by discovering every such target from `cargo metadata` and linting it explicitly.

This test has three jobs, each with teeth:

1. Parse `cargo metadata --no-deps` itself, independently of the discovery script, and
   assert the two enumerations agree -- so the script cannot silently drift into
   discovering nothing (or the wrong thing) while still looking like it works.
2. Assert the gate recipe in `README.md`'s "Building the Rust workspace" code block
   actually invokes the script -- so deleting that one line from the gate fails this suite,
   not just a human's memory.
3. Assert today's enumeration is non-empty and contains `av-jobs`'s `av-tile-fixture` under
   `store-fixture` -- so the mechanism is proven against something real, not vacuously true.

`cargo metadata --no-deps` reads `Cargo.toml`/`Cargo.lock` and resolves the workspace graph
that is already on disk; it does not compile anything and (per the Cargo book) does not need
the network when `--no-deps` is given and the lockfile already covers every workspace
member's own path dependencies, which is the only kind of dependency a `--no-deps` metadata
call reports on. This is asserted directly below (not just claimed) by running it with
`CARGO_NET_OFFLINE=true` in the child's own environment -- passed through the `env=` keyword
to `subprocess.run`, so the current *test* process's environment is never mutated (question
199) -- and requiring it still exits 0.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
DISCOVERY_SCRIPT = REPO_ROOT / "scripts" / "lint" / "required_features_clippy.py"
GATE_SCRIPT = REPO_ROOT / "scripts" / "lint" / "required_features_clippy.sh"
README_MD = REPO_ROOT / "README.md"


def _cargo_metadata_no_deps() -> tuple[dict, float]:
    """Run `cargo metadata --no-deps --format-version=1` for real, with `CARGO_NET_OFFLINE=1`
    forced in the CHILD's environment only (never mutating this process's `os.environ`), and
    return (parsed JSON, wall-clock seconds). A network-dependent metadata resolution would
    fail outright under `CARGO_NET_OFFLINE=1`, so a successful, fast return here is itself the
    proof that this call needs no network -- not merely a claim in a docstring."""
    if not DISCOVERY_SCRIPT.is_file():
        pytest.fail(
            f"question 231 required-features lint test: expected discovery script not "
            f"found at {DISCOVERY_SCRIPT} -- it was renamed or removed; update this test's "
            f"path or restore the script."
        )

    # Question 194 (skips are visible, with a reason): `cargo` is not guaranteed to be on
    # PATH for every invocation of this suite -- the Rust toolchain lives under
    # /opt/homebrew/opt/rustup/bin on this host and a Python-only run may not have it.
    # Without this guard the three metadata-backed tests ERROR with a bare
    # `FileNotFoundError: [Errno 2] No such file or directory: 'cargo'` out of
    # `subprocess.run`, which is neither a visible skip nor a legible failure. Found by the
    # manager while re-running this file's own proof from a shell without the rustup bin
    # directory exported.
    if shutil.which("cargo") is None:
        pytest.skip(
            "cargo is not on PATH, so 'cargo metadata --no-deps' cannot be run to discover "
            "this workspace's required-features targets (question 231's gate). This suite "
            "checks a cargo-based gate and deliberately runs cargo for real rather than "
            "parsing Cargo.toml by hand; export the toolchain (on this host: "
            "PATH=\"/opt/homebrew/opt/rustup/bin:$PATH\") and re-run."
        )

    env = dict(os.environ)
    env["CARGO_NET_OFFLINE"] = "true"

    start = time.monotonic()
    proc = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version=1"],
        cwd=REPO_ROOT,
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
    )
    elapsed = time.monotonic() - start

    if proc.returncode != 0:
        pytest.fail(
            "question 231 required-features lint test: 'cargo metadata --no-deps "
            "--format-version=1' failed with CARGO_NET_OFFLINE=true (exit "
            f"{proc.returncode}) -- either it needs the network after all (this test's own "
            "offline claim is wrong and must be fixed), or cargo/the workspace is broken. "
            f"stderr:\n{proc.stderr}"
        )

    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        pytest.fail(
            f"question 231 required-features lint test: 'cargo metadata --no-deps' did not "
            f"print valid JSON on stdout ({exc}); first 500 chars:\n{proc.stdout[:500]}"
        )

    return data, elapsed


def _independent_enumeration(metadata: dict) -> dict[tuple[str, tuple[str, ...]], list[str]]:
    """This test's OWN parse of the metadata JSON -- deliberately a separate implementation
    from `required_features_clippy.py`'s `discover_groups`, reading the same
    `workspace_members` / `packages[].targets[].required-features` fields directly, so that
    the two can be compared for agreement rather than one simply calling the other."""
    ws_members = set(metadata["workspace_members"])
    groups: dict[tuple[str, tuple[str, ...]], list[str]] = {}
    for pkg in metadata["packages"]:
        if pkg["id"] not in ws_members:
            continue
        for target in pkg["targets"]:
            features = target.get("required-features") or []
            if not features:
                continue
            key = (pkg["name"], tuple(sorted(features)))
            groups.setdefault(key, []).append(target["name"])
    return groups


def _script_enumeration(metadata_file: Path) -> dict[tuple[str, tuple[str, ...]], list[str]]:
    """Run the REAL, committed `required_features_clippy.py` as a subprocess (not imported,
    so this exercises exactly what the gate script itself invokes) against a metadata file
    already on disk, and parse its tab-separated stdout back into the same shape
    `_independent_enumeration` produces, for a direct equality check."""
    proc = subprocess.run(
        [sys.executable, str(DISCOVERY_SCRIPT), str(metadata_file)],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=30,
    )
    if proc.returncode != 0:
        pytest.fail(
            f"question 231 required-features lint test: {DISCOVERY_SCRIPT} exited "
            f"{proc.returncode} instead of discovering targets. stderr:\n{proc.stderr}"
        )

    groups: dict[tuple[str, tuple[str, ...]], list[str]] = {}
    for line in proc.stdout.splitlines():
        if not line.strip():
            continue
        pkg_name, features_field, targets_field = line.split("\t")
        features = tuple(sorted(features_field.split(","))) if features_field else ()
        target_names = [entry.split(":", 1)[0] for entry in targets_field.split(";") if entry]
        groups[(pkg_name, features)] = target_names
    return groups


@pytest.fixture(scope="module")
def metadata_and_timing() -> tuple[dict, float]:
    return _cargo_metadata_no_deps()


def test_cargo_metadata_no_deps_is_fast_and_needs_no_network(metadata_and_timing):
    """The proof that `cargo metadata --no-deps` is cheap and offline, not just a claim:
    it already ran (in the `metadata_and_timing` fixture) with `CARGO_NET_OFFLINE=true` in
    its own environment and exited 0 -- if it had needed the registry, cargo would have
    refused under that flag and `_cargo_metadata_no_deps` would already have failed this
    test via `pytest.fail`. This test additionally asserts it was fast."""
    _, elapsed = metadata_and_timing
    assert elapsed < 30.0, (
        f"question 231 required-features lint test: 'cargo metadata --no-deps' took "
        f"{elapsed:.2f}s, which is no longer 'cheap' -- investigate before trusting this "
        f"gate to run on every invocation of the Rust workspace recipe."
    )


def test_discovery_script_enumeration_matches_independent_parse(metadata_and_timing, tmp_path):
    """The drift guard: parse `cargo metadata --no-deps` ourselves, run the real
    `required_features_clippy.py` against the SAME metadata, and assert the two
    enumerations are byte-for-byte the same set of (package, feature-set) -> targets. If the
    script's own logic diverges (a bug, a refactor that silently narrows what it looks at),
    this fails even though the script would still exit 0 and print *something*."""
    metadata, _ = metadata_and_timing
    metadata_file = tmp_path / "metadata.json"
    metadata_file.write_text(json.dumps(metadata))

    expected = _independent_enumeration(metadata)
    actual = _script_enumeration(metadata_file)

    assert actual == expected, (
        "question 231 required-features lint test: required_features_clippy.py's own "
        "enumeration does not match this test's independent parse of the same "
        f"'cargo metadata --no-deps' output.\n  independent parse: {expected}\n"
        f"  script output:     {actual}"
    )


def test_todays_enumeration_is_nonempty_and_contains_av_tile_fixture(metadata_and_timing):
    """The mechanism must be looking at something real today, not vacuously discovering
    nothing (which would make every other assertion in this file pass for the wrong
    reason). `av-jobs`'s `av-tile-fixture` binary, gated behind `store-fixture`
    (crates/av-jobs/Cargo.toml), is question 231's own example."""
    metadata, _ = metadata_and_timing
    groups = _independent_enumeration(metadata)

    assert groups, (
        "question 231 required-features lint test: no workspace target declares "
        "required-features at all -- either av-jobs/Cargo.toml's [[bin]] entry for "
        "av-tile-fixture was changed, or 'cargo metadata --no-deps' stopped reporting "
        "required-features (a cargo behaviour change); either way this proof is now "
        "vacuous and must be investigated before it can be trusted."
    )

    key = ("av-jobs", ("store-fixture",))
    assert key in groups, (
        f"question 231 required-features lint test: expected ('av-jobs', "
        f"('store-fixture',)) among the discovered groups, got {sorted(groups)}"
    )
    assert "av-tile-fixture" in groups[key], (
        f"question 231 required-features lint test: expected 'av-tile-fixture' among the "
        f"targets discovered for av-jobs/store-fixture, got {groups[key]}"
    )


def test_readme_gate_recipe_invokes_the_script():
    """Question 231's own ruling: required-features binaries are linted EXPLICITLY in the
    gate. `README.md`'s "Building the Rust workspace" section is this repo's canonical gate
    recipe (it is the block that states `cargo clippy --workspace --all-targets --
    -D warnings`). This asserts the new step is actually IN that fenced code block, right
    next to the standing clippy line -- not merely mentioned in prose somewhere else in the
    file, which a human could satisfy without the gate itself gaining the step. Removing the
    line from the code block (even while leaving prose that talks about it) fails this."""
    if not README_MD.is_file():
        pytest.fail(f"question 231 required-features lint test: {README_MD} not found.")
    text = README_MD.read_text()

    heading = "## Building the Rust workspace"
    heading_pos = text.find(heading)
    assert heading_pos != -1, (
        f"question 231 required-features lint test: {README_MD} has no {heading!r} "
        "section -- the canonical Rust-workspace gate recipe moved or was renamed; update "
        "this test's own marker to match, or restore the section."
    )

    # The specific fenced ```bash ... ``` block that contains the standing clippy gate line,
    # searched for starting at the heading so a same-named block elsewhere can't satisfy this.
    rest = text[heading_pos:]
    code_blocks = re.findall(r"```bash\n(.*?)\n```", rest, flags=re.DOTALL)
    gate_block = next(
        (block for block in code_blocks if "cargo clippy --workspace --all-targets" in block),
        None,
    )
    assert gate_block is not None, (
        "question 231 required-features lint test: no ```bash code block under "
        f"{heading!r} in {README_MD} contains the standing "
        "'cargo clippy --workspace --all-targets' gate line -- the recipe moved; update "
        "this test's own search, or restore the line."
    )

    assert "scripts/lint/required_features_clippy.sh" in gate_block, (
        "question 231 required-features lint test: the Rust-workspace gate recipe in "
        f"{README_MD} no longer invokes scripts/lint/required_features_clippy.sh -- this is "
        "exactly the hole question 231 closed (required-features targets going unlinted by "
        "the plain workspace clippy gate); restore the step in the code block beside the "
        "standing clippy line."
    )

    assert GATE_SCRIPT.is_file(), (
        f"question 231 required-features lint test: README.md's gate recipe names "
        f"{GATE_SCRIPT}, but that file does not exist."
    )
    assert os.access(GATE_SCRIPT, os.X_OK), (
        f"question 231 required-features lint test: {GATE_SCRIPT} is not executable "
        "(README.md's recipe invokes it directly, not via 'bash <path>')."
    )
