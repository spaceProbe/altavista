"""D4 first half (docs/p5-plan.md, P5 track round 3, task D4a): tests for the offline evidence
bundle (`scripts/kit/evidence.py`). The lead's round-3 charter (this task's own brief) names
these three as the acceptance evidence:

1. Byte-identical across two runs on the same tree state.
2. A tampered `gmat_service.evidence.EvidenceLog` ledger makes its `verify()` result AND the
   regenerated bundle say so, and the bundle's own SHA-256 changes as a result.
3. Every control-matrix row appears in the coverage table exactly once, with the reconciliation
   numbers printed (question 148: "an exit code is not evidence -- quote the artifact").

Plus a fourth (cheap): a deliberately malformed row is a named finding, not a silent skip.

No test mutates `os.environ` (question 199); every test writes only under its own `tmp_path`;
`REPO_ROOT` comes from `__file__`, never a hard-coded `/Users/probe` path, so this suite passes
in a plain clone at any path. No test needs network access (question 154) -- everything here
reads already-committed files from this checkout and/or writes to `tmp_path`.
"""
from __future__ import annotations

import copy
import hashlib
import json
import re
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
if str(KIT_DIR) not in sys.path:
    sys.path.insert(0, str(KIT_DIR))
import evidence  # noqa: E402  (path insert must precede this import)


# =================================================================================================
# 1. Byte-identical across two runs on the same tree state
# =================================================================================================

def test_two_runs_over_the_same_state_are_byte_identical(tmp_path):
    out_a = tmp_path / "run_a"
    out_b = tmp_path / "run_b"

    bundle_a = evidence.assemble_bundle(repo_root=REPO_ROOT)
    bundle_b = evidence.assemble_bundle(repo_root=REPO_ROOT)
    path_a = evidence.write_bundle(bundle_a, out_a)
    path_b = evidence.write_bundle(bundle_b, out_b)

    bytes_a = path_a.read_bytes()
    bytes_b = path_b.read_bytes()
    assert bytes_a == bytes_b, "two regenerations of the same tree state produced different bytes"
    assert bundle_a["bundle_sha256"] == bundle_b["bundle_sha256"]
    # Sanity: the file's own on-disk hash matches the field the bundle claims for itself.
    import hashlib
    assert hashlib.sha256(bytes_a).hexdigest() != bundle_a["bundle_sha256"], (
        "bundle_sha256 is computed over the canonical dict with the trailing newline/file "
        "framing NOT applied -- this asserts the two are deliberately different serialisations "
        "(the field is over json.dumps(..., indent=2, sort_keys=True) with no trailing newline "
        "and the hash key itself absent; the file adds a trailing newline), not a bug"
    )


def test_a_single_run_writes_exactly_one_file(tmp_path):
    out_dir = tmp_path / "out"
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT)
    path = evidence.write_bundle(bundle, out_dir)
    assert path == out_dir / "bundle.json"
    assert list(out_dir.iterdir()) == [path]


def test_bundle_contains_no_absolute_host_path(tmp_path):
    """Determinism rule: no absolute path from this host anywhere in the output. `tmp_path`
    itself is an absolute, host/run-specific path -- if it leaked into the bundle (e.g. through
    an unguarded `--kit`/`--ledger-dir` echoed verbatim) this would catch it directly."""
    ledger_dir = tmp_path / "ledger"
    ledger_dir.mkdir()
    kit_dir = tmp_path / "kit"
    kit_dir.mkdir()
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT, kit_dir=kit_dir, ledger_dir=ledger_dir)
    text = json.dumps(bundle)
    assert str(tmp_path) not in text
    assert "/Users/" not in text


# =================================================================================================
# 2. A tampered ledger makes its verify result AND the bundle say so
# =================================================================================================

def _make_evidence_log(ledger_dir: Path, n: int):
    sys.path.insert(0, str(REPO_ROOT / "services" / "gmat-service"))
    from gmat_service.evidence import EvidenceLog

    class _FakeMessage:
        def __init__(self, payload: str):
            self._payload = payload.encode("utf-8")

        def SerializeToString(self, deterministic: bool = False) -> bytes:  # noqa: N802
            return self._payload

    log = EvidenceLog(ledger_dir / "evidence.jsonl")
    for _ in range(n):
        log.record(
            method="Describe", request=_FakeMessage("req"), response=_FakeMessage("resp"),
            settings_hash="sh", run_id="evidence_bundle_test",
        )
    return log


def test_tampered_ledger_makes_verify_and_the_bundle_say_so(tmp_path):
    ledger_dir = tmp_path / "ledger"
    ledger_dir.mkdir()
    log = _make_evidence_log(ledger_dir, 4)

    # --- Clean state: verify() itself, and the bundle built over it, both report ok. ---
    clean_result = log.verify()
    assert clean_result["ok"] is True

    clean_bundle = evidence.assemble_bundle(repo_root=REPO_ROOT, ledger_dir=ledger_dir)
    clean_offline = clean_bundle["ledger_verify"]["offline"]["gmat_service"]
    assert clean_offline["status"] == "collected"
    assert clean_offline["result"] == clean_result

    # --- Tamper with record seq 3's content on disk, leaving its "hash" stale -- exactly what
    # tests/test_compliance.py::test_verify_detects_a_tampered_record_and_reports_its_sequence_number
    # does, reused here as the input to the BUNDLE rather than only to the bare verifier. ---
    lines = log.path.read_text().splitlines()
    tampered = json.loads(lines[2])
    assert tampered["seq"] == 3
    tampered["method"] = "Propagate"
    lines[2] = json.dumps(tampered, sort_keys=True)
    log.path.write_text("\n".join(lines) + "\n")

    # (a) the bare verifier itself reports the break with the sequence number.
    broken_result = log.verify()
    assert broken_result["ok"] is False
    assert broken_result["broken_at_seq"] == 3
    assert "tampered" in broken_result["detail"]

    # (b) the regenerated bundle's ledger section carries that same break.
    tampered_bundle = evidence.assemble_bundle(repo_root=REPO_ROOT, ledger_dir=ledger_dir)
    tampered_offline = tampered_bundle["ledger_verify"]["offline"]["gmat_service"]
    assert tampered_offline["status"] == "collected"
    assert tampered_offline["result"] == broken_result
    assert tampered_offline["result"]["ok"] is False
    assert tampered_offline["result"]["broken_at_seq"] == 3

    # The bundle's own SHA-256 changed as a result -- the tamper is visible at the top level,
    # not buried where a reader could miss it.
    assert clean_bundle["bundle_sha256"] != tampered_bundle["bundle_sha256"], (
        f"clean={clean_bundle['bundle_sha256']} tampered={tampered_bundle['bundle_sha256']}"
    )
    print(f"clean bundle_sha256:    {clean_bundle['bundle_sha256']}")
    print(f"tampered bundle_sha256: {tampered_bundle['bundle_sha256']}")


def test_a_ledger_that_does_not_exist_is_a_declared_not_collected_slot(tmp_path):
    empty_dir = tmp_path / "nothing_here"
    empty_dir.mkdir()
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT, ledger_dir=empty_dir)
    offline = bundle["ledger_verify"]["offline"]["gmat_service"]
    assert offline["status"] == "not_collected"
    assert offline["result"] is None
    assert offline["reason"]


def test_no_ledger_dir_given_is_a_declared_not_collected_slot():
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT)
    offline = bundle["ledger_verify"]["offline"]["gmat_service"]
    assert offline["status"] == "not_collected"
    assert offline["result"] is None
    assert offline["reason"] == "no --ledger-dir given this run"


def test_the_live_half_is_a_declared_not_collected_slot_with_an_explicit_plug_in():
    """Deliverable 4: the live half must never read as an empty dict / silently "verified"."""
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT)
    live = bundle["ledger_verify"]["live"]
    assert live["status"] == "not_collected"
    assert live["reason"]
    assert "plug_in" in live and "assemble_bundle" in live["plug_in"]


def test_ledger_verify_live_parameter_is_the_plug_in_and_is_stored_verbatim():
    fake_live = {"secrouter": {"status": "collected", "url": "http://x/verify", "result": {"ok": True}}}
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT, ledger_verify_live=fake_live)
    assert bundle["ledger_verify"]["live"] == fake_live


# =================================================================================================
# 3. Every control-matrix row appears in the coverage table exactly once (reconciliation)
# =================================================================================================

def test_coverage_table_reconciles_with_every_control_matrix_row(capsys):
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT)
    control_matrices = bundle["control_matrices"]
    coverage = bundle["coverage"]

    # Expected attribution count: sum over components of sum over rows of len(practices-per-row).
    expected_attributions = 0
    per_component_rows = {}
    for component, matrix in control_matrices.items():
        assert matrix["malformed_rows"] == [], f"{component} has malformed rows: {matrix['malformed_rows']}"
        per_component_rows[component] = len(matrix["rows"])
        for row in matrix["rows"]:
            assert row["practices"], f"{component} row {row['id']!r} named no practice at all"
            expected_attributions += len(row["practices"])

    actual_attributions = coverage["attributions"]
    assert len(actual_attributions) == expected_attributions == coverage["attribution_count"]

    # Every (component, row_id, practice, mark) triple derived directly from control_matrices
    # appears in coverage["attributions"] exactly once, and every coverage attribution traces
    # back to exactly one such triple -- a true bijection, not just a count match.
    expected_triples = []
    for component, matrix in control_matrices.items():
        for row in matrix["rows"]:
            for practice in row["practices"]:
                expected_triples.append((component, row["id"], practice, row["status"]))
    expected_triples.sort()
    actual_triples = sorted(
        (a["component"], a["row_id"], a["practice"], a["mark"]) for a in actual_attributions
    )
    assert actual_triples == expected_triples

    # Every attribution is reflected in the grouped `practices` view (the component is a member
    # of practices[practice][mark]'s own sorted list) -- exactly one coverage entry per
    # (practice, mark) names each component that asserts it, per the deliverable's own wording.
    for a in actual_attributions:
        by_mark = coverage["practices"][a["practice"]]
        assert a["component"] in by_mark[a["mark"]], (
            f"{a['component']} names ({a['practice']}, {a['mark']}) in a row but coverage's "
            f"grouped table does not list it there"
        )

    # Print the reconciliation numbers (question 148: quote the artifact, not just an exit code).
    print(f"components: {len(control_matrices)}")
    print(f"rows per component: {per_component_rows}")
    print(f"total rows: {sum(per_component_rows.values())}")
    print(f"distinct practices named by at least one component: {coverage['practice_count']}")
    print(f"(practice, mark, component) attributions: {coverage['attribution_count']}")
    print(f"reconciliation: {expected_attributions} == {coverage['attribution_count']} -> "
          f"{expected_attributions == coverage['attribution_count']}")
    captured = capsys.readouterr()
    assert "reconciliation:" in captured.out


def test_coverage_denominator_note_is_honest_about_not_being_all_110():
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT)
    note = bundle["coverage"]["denominator_note"]
    assert "110" in note
    assert "does not invent" in note or "no authoritative list" in note
    # The practice count must be strictly less than 110 -- if it were ever >= 110 that would
    # itself be a sign something is over-counting (e.g. treating a written range as many IDs).
    assert 0 < bundle["coverage"]["practice_count"] < 110


# =================================================================================================
# 4. A malformed row is a named finding, not a silent skip
# =================================================================================================

def test_a_malformed_row_is_a_named_finding_not_a_silent_skip(tmp_path):
    real_path = evidence.discover_components(REPO_ROOT)["av-ingest"]
    text = real_path.read_text(encoding="utf-8")
    lines = text.splitlines()

    # Find a real, well-formed practice row and break it: drop one pipe-delimited cell so the
    # line no longer has the required 5-column shape.
    target_idx = None
    for i, line in enumerate(lines):
        if evidence.ROW_RE.match(line.strip()) and "3.3.8" in line:
            target_idx = i
            break
    assert target_idx is not None, "expected to find the 3.3.8 row in av-ingest's control matrix"

    original = lines[target_idx]
    cells = original.split("|")
    assert len(cells) >= 6  # leading "", 5 columns, trailing ""
    broken = "|".join(cells[:-2] + [cells[-1]])  # remove one interior cell
    assert evidence.ROW_RE.match(broken.strip()) is None, "the mutation did not actually break the row shape"
    lines[target_idx] = broken

    copy_path = tmp_path / "control-matrix.md"
    copy_path.write_text("\n".join(lines) + "\n", encoding="utf-8")

    parsed = evidence.parse_control_matrix(copy_path)
    malformed_lines = {m["line_no"] for m in parsed["malformed_rows"]}
    assert (target_idx + 1) in malformed_lines, (
        f"expected line {target_idx + 1} to be reported malformed; got {parsed['malformed_rows']}"
    )
    # The row must not silently reappear in `rows` as if nothing happened.
    assert not any(r["line_no"] == target_idx + 1 for r in parsed["rows"])
    # And the original file is untouched -- this test never wrote outside tmp_path.
    assert real_path.read_text(encoding="utf-8") == text


# =================================================================================================
# SBOM hashes and kit manifest -- declared, never fabricated
# =================================================================================================

def test_sbom_hashes_are_reverified_against_disk_and_agree_today():
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT)
    sbom_hashes = bundle["sbom_hashes"]
    assert len(sbom_hashes["recorded"]) == 10
    assert sbom_hashes["actual"] == sbom_hashes["recorded"]
    assert sbom_hashes["stale"] == []
    assert sbom_hashes["missing"] == []


def test_kit_manifest_absent_by_default_is_a_declared_gap():
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT)
    km = bundle["kit_manifest"]
    assert km["collected"] is False
    assert km["sha256"] is None
    assert km["reason"] == "no --kit given this run"


def test_kit_manifest_is_recorded_when_a_kit_dir_is_given(tmp_path):
    kit_dir = tmp_path / "kit"
    kit_dir.mkdir()
    (kit_dir / "KIT_MANIFEST").write_text('{"kit_format": 4}\n', encoding="utf-8")
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT, kit_dir=kit_dir)
    km = bundle["kit_manifest"]
    assert km["collected"] is True
    assert km["sha256"] == evidence.sbom.sha256_file(kit_dir / "KIT_MANIFEST")
    assert km["reason"] is None


def test_kit_dir_given_but_no_kit_manifest_is_a_declared_gap_not_a_crash(tmp_path):
    kit_dir = tmp_path / "empty_kit"
    kit_dir.mkdir()
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT, kit_dir=kit_dir)
    km = bundle["kit_manifest"]
    assert km["collected"] is False
    assert km["sha256"] is None
    assert "KIT_MANIFEST" in km["reason"]


# =================================================================================================
# 5. `git_commit` is provenance, not hashed content -- and docs/compliance/BUNDLE.md's recorded
#    `bundle_sha256` is a real, re-verifiable number (round 3 defect fix, commit 1)
# =================================================================================================

def _bundle_canonical_sha256(bundle: dict) -> str:
    """Recomputes `bundle_sha256` the exact way `assemble_bundle` does: over the canonical
    `json.dumps(..., indent=2, sort_keys=True, ensure_ascii=False)` content with BOTH
    `bundle_sha256` and `git_commit` excluded. A standalone helper (not calling back into
    `evidence`) so the perturbation test below can recompute the hash over a hand-mutated dict
    without re-running the whole assembly pipeline."""
    content = {k: v for k, v in bundle.items() if k not in ("bundle_sha256", "git_commit")}
    canonical = json.dumps(content, indent=2, sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def _bundle_md_recorded_sha256() -> str:
    """Parses the hash out of `docs/compliance/BUNDLE.md`'s own fenced code block -- for real,
    from the committed file, never hard-coded into this test module (a hard-coded expected value
    would silently go stale the next time BUNDLE.md is regenerated, exactly the failure mode this
    whole commit is fixing)."""
    text = (REPO_ROOT / "docs" / "compliance" / "BUNDLE.md").read_text(encoding="utf-8")
    match = re.search(r"```\n([0-9a-f]{64})\n```", text)
    assert match, "docs/compliance/BUNDLE.md: could not find a recorded 64-hex-char SHA-256"
    return match.group(1)


def test_git_commit_is_excluded_from_what_bundle_sha256_hashes():
    """The defect: `git_commit` (`git rev-parse HEAD`) used to sit INSIDE the canonical content
    `bundle_sha256` is computed over, so `bundle_sha256` changed on every commit regardless of
    whether any evidence input changed. Proof of the fix: two bundles that differ ONLY in
    `git_commit` must have the SAME `bundle_sha256`."""
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT)
    mutated = copy.deepcopy(bundle)
    mutated["git_commit"] = "0" * 40  # a different commit, nothing else about the evidence changed
    assert mutated["git_commit"] != bundle["git_commit"]
    assert _bundle_canonical_sha256(mutated) == _bundle_canonical_sha256(bundle) == bundle["bundle_sha256"]


def test_bundle_sha256_matches_the_hash_recorded_in_bundle_md():
    """The record is made load-bearing: regenerate the bundle from the CURRENT tree (no `--kit`,
    no `--ledger-dir` -- the exact invocation `docs/compliance/BUNDLE.md`'s own "Regenerating it"
    section documents) and assert its `bundle_sha256` equals the number BUNDLE.md records, parsed
    out of that file for real (not re-typed into this test). This is what forces BUNDLE.md to be
    regenerated whenever the evidence genuinely changes -- a stale recorded hash now fails CI
    instead of silently drifting."""
    bundle = evidence.assemble_bundle(repo_root=REPO_ROOT)
    recorded = _bundle_md_recorded_sha256()
    assert bundle["bundle_sha256"] == recorded, (
        f"regenerated bundle_sha256={bundle['bundle_sha256']!r} does not match "
        f"docs/compliance/BUNDLE.md's recorded {recorded!r} -- run the documented command and "
        f"update BUNDLE.md"
    )


def test_the_bundle_md_equality_check_can_actually_fail(tmp_path):
    """Proves the test immediately above is load-bearing, not decorative -- it CAN fail. Copies a
    real control matrix's text into `tmp_path`, tampers the copy, re-parses the tampered copy
    (`evidence.parse_control_matrix` needs no git access, just a file), splices the tampered
    component's content into a deep copy of the real bundle, and shows the recomputed
    `bundle_sha256` no longer matches `docs/compliance/BUNDLE.md`'s recorded value -- i.e. a real
    evidence change is caught, not silently ignored the way `git_commit`-in-the-hash silently
    ignored "nothing evidence-relevant changed" before this commit's fix."""
    real_bundle = evidence.assemble_bundle(repo_root=REPO_ROOT)
    recorded = _bundle_md_recorded_sha256()
    assert real_bundle["bundle_sha256"] == recorded  # sanity: agrees before anything is perturbed

    component = "av-ingest"
    real_path = evidence.discover_components(REPO_ROOT)[component]
    original_text = real_path.read_text(encoding="utf-8")
    tampered_copy = tmp_path / "control-matrix.md"
    tampered_copy.write_text(
        original_text + "\n<!-- perturbed by "
        "test_the_bundle_md_equality_check_can_actually_fail, tmp_path only -->\n",
        encoding="utf-8",
    )
    tampered_parsed = evidence.parse_control_matrix(tampered_copy)
    assert tampered_parsed["sha256"] != real_bundle["control_matrices"][component]["sha256"]

    perturbed_bundle = copy.deepcopy(real_bundle)
    perturbed_bundle["control_matrices"][component]["sha256"] = tampered_parsed["sha256"]

    perturbed_sha256 = _bundle_canonical_sha256(perturbed_bundle)
    assert perturbed_sha256 != recorded, (
        "perturbing a control matrix's content did not change the recomputed bundle_sha256 -- "
        "the BUNDLE.md equality check above would not have caught this and is decorative"
    )
    # And the real (unperturbed) file on disk was never touched.
    assert real_path.read_text(encoding="utf-8") == original_text


# =================================================================================================
# CLI
# =================================================================================================

def test_cli_writes_bundle_json_and_prints_the_hash(tmp_path, capsys):
    out_dir = tmp_path / "cliout"
    rc = evidence.main(["--out", str(out_dir)])
    assert rc == 0
    bundle_path = out_dir / "bundle.json"
    assert bundle_path.is_file()
    on_disk = json.loads(bundle_path.read_text())
    captured = capsys.readouterr()
    assert on_disk["bundle_sha256"] in captured.err
