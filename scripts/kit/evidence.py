"""scripts/kit/evidence.py -- D4 (docs/p5-plan.md), the OFFLINE half of the evidence bundle.

D4's own text: "`secdeploy evidence` run over a deployed evaluation placement, plus our own
`scripts/kit/evidence.py` that collects what secdeploy cannot know: every component's control
matrix, the cross-component NIST 800-171 coverage table (per practice: Met, Partial, Inherited,
Gap, and which component says so), the deficiency list, the ledger `verify` results, the SBOM
hashes and the kit manifest hash, into one hashed bundle." The lead's round-3 charter moves the
bundle's own location to `out/evidence/` (not `docs/compliance/bundle/`, which the plan text
says -- the lead's later instruction wins), keeps the bundle itself uncommitted, and records only
its SHA-256 in the committed `docs/compliance/BUNDLE.md`.

This module builds the bundle -- library only, in the same shape as `scripts/kit/sbom.py`/
`scripts/kit/manifest.py`: functions that never print or `sys.exit`; `main` (the CLI) decides.
`python scripts/kit/evidence.py --out out/evidence [--kit <dir>] [--ledger-dir <dir>]` writes
one file, `<out>/bundle.json`.

# Why one JSON file, not a directory of copies

`KIT_MANIFEST` (`manifest.py`) is the precedent this follows: a kit's manifest does not COPY
every file it describes, it records each one's path/size/SHA-256 and lets the already-committed
(or already-built) bytes stay exactly where they are. This bundle does the same for the six
control matrices (deliverable 1's "plus the file's own SHA-256 so the bundle pins exactly which
text it summarised" -- a hash pin, not a duplicate copy) and for the ten SBOMs (already committed
under `docs/compliance/sbom/`, already carrying their own `SHA256SUMS`). A single JSON file
keeps the "byte-identical across two runs" proof to one `sha256_file` call instead of a directory
walk, and keeps the bundle's own top-level `bundle_sha256` meaningful as a single number a reader
can quote (rule 148: "an exit code is not evidence -- quote the artifact").

# Bundle shape (top-level keys)

- `schema` / `schema_version` -- so a reader (or a future format change) can tell what shape this
  is without guessing from content.
- `epoch` / `epoch_paths` -- Decision H's own discipline (`sbom.git_epoch`), reused rather than
  reinvented: the committer date of the last commit touching the paths this bundle's
  control-matrix/SBOM content is actually sensitive to (the six `control-matrix.md` files plus
  `docs/compliance/sbom/SHA256SUMS`, itself rewritten whenever any of the ten SBOMs changes --
  `sbom.rewrite_sha256sums`'s own doc). Never `datetime.now()`. `epoch_paths` is recorded
  alongside so a reader never has to re-derive which commits legitimately move this field --
  exactly the transparency `docs/compliance/sbom/README.md`'s own "Determinism" section models.
- `git_commit` -- `git rev-parse HEAD`, informational only (which commit this bundle was built
  against), never used to compute anything else in this module.
- `control_matrices` -- keyed by component name (`av-command`, `av-dynamics-service`,
  `av-edge-plugin`, `av-gateway`, `av-ingest`, `gmat-service`, discovered from
  `docs/compliance/*/control-matrix.md` rather than hard-coded -- see `discover_components`, the
  same "never hard-code a component list that already exists on disk" instinct
  `sbom.suite_component_names` follows for the SBOM side). Each entry: `path` (repo-relative),
  `sha256` (the file's own hash -- deliverable 1's "pins exactly which text it summarised"),
  `rows` (every parsed practice row: `id` as written, `practices` -- the ID column split on `/`,
  one token per practice-or-range exactly as written, never expanded or invented -- `requirement`,
  `status`, `implementation`, `evidence`, `line_no`), and `malformed_rows` (a table line inside a
  `## 3.x ... (FAM)` family section that does not match the expected 5-column shape -- a named
  finding, never a silent skip; empty for all six real files today).
- `coverage` -- the cross-component NIST 800-171 table. See `build_coverage`'s own doc for the
  denominator honesty rule (deliverable 2's explicit instruction: do not invent the 110-practice
  universe).
- `deficiencies` -- keyed by component, each entry `{"number", "title", "text"}` parsed from that
  component's own `## Deficiencies` section (deliverable 3).
- `ledger_verify` -- `{"offline": {...}, "live": {...}}`. **This is where the second worker's live
  half plugs in** -- see `assemble_bundle`'s `ledger_verify_live` parameter, immediately below,
  and its own doc comment for the exact shape to pass.
- `sbom_hashes` -- `docs/compliance/sbom/SHA256SUMS`'s ten recorded hashes, RE-VERIFIED against
  the files on disk right now (deliverable 5: "a stale SHA256SUMS must be a recorded finding, not
  a silent copy") -- `recorded`, `actual`, `stale` (paths where they disagree), `missing` (a
  recorded path with no file on disk at all).
- `kit_manifest` -- deliverable 6: the named kit's own `KIT_MANIFEST` hash when `--kit <dir>` is
  given, or a declared, named absence (never fabricated) when it is not.
- `bundle_sha256` -- deliverable 7, added LAST: SHA-256 over `json.dumps(bundle, indent=2,
  sort_keys=True, ensure_ascii=False)` computed with this key itself absent, exactly the
  `secdeploy` deploy-audit chain's own convention (`/Users/probe/code/secdeploy/src/secdeploy/
  audit.py`'s own comment: "the SHA-256 of this record's own canonical content, `prevHash`
  included, `hash` itself excluded -- you cannot hash yourself") -- cited here rather than
  reinvented, per this task's own instruction to follow that precedent.

# Determinism (the point of this module, same rule `sbom.py` already proves)

No wall-clock timestamp anywhere (`epoch`/`git_commit` are both pure functions of already-
committed git history, never `datetime.now()`); no absolute host path baked into the output
(`_display_path` below resolves a caller-given `--kit`/`--ledger-dir` to a repo-relative path
when it is inside this repo, or to just its own basename -- never a full `/Users/<name>/...`
string -- when it is not, e.g. a test's own `tmp_path`); every dict is written with
`sort_keys=True`; every list this module builds is already sorted by construction (components,
practice IDs, `stale`/`missing` paths, `attributions`) rather than left in whatever order a
directory walk or dict iteration happened to produce. Two runs over the same tree state, the same
`--kit`, and the same `--ledger-dir` content produce byte-identical output
(`tests/test_evidence_bundle.py::test_two_runs_over_the_same_state_are_byte_identical`).

# What this module does NOT do

It never runs `secdeploy evidence` itself (that is the live half, a separate worker's job, run
over an actually-reachable deployment -- see `/Users/probe/code/secdeploy/docs/compliance.md`'s
own "this needs a live, reachable deployment" rule, and `docs/secdeploy-upstream.md`'s Proposal 2
in this repo for why `secdeploy evidence` could never enumerate an AltaVista component even if
one were reachable: `COMPONENTS` there is a five-name module constant, not manifest-driven). It
never re-implements `tests/test_compliance.py`'s own structural checks (every `Met` row names a
real `file:function` that exists) -- this module only PARSES what is already there; the
enforcement that it is honest lives in that test file, run separately, every time.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Optional

REPO_ROOT = Path(__file__).resolve().parents[2]

# scripts/kit/sbom.py lives in this same directory (like manifest.py's own `import sbom`) --
# `sha256_file`/`git_epoch` are reused rather than re-implemented a third time. Caller is
# responsible for putting scripts/kit on sys.path (true for `python scripts/kit/evidence.py`,
# and done explicitly by tests/test_evidence_bundle.py, matching tests/test_kit_manifest.py's own
# convention for `manifest.py`).
import sbom  # noqa: E402

# gmat_service.evidence.EvidenceLog is a REAL, importable, file-backed hash-chained ledger with a
# real verifier (services/gmat-service/gmat_service/evidence.py) -- the natural offline subject
# for deliverable 4. tests/test_compliance.py already imports it the identical way: neither
# evidence.py nor fips.py nor admin.py import anything GMAT-specific at module scope, so this
# stays cheap (no grpcio/GMAT process needed just to call `.verify()`).
_GMAT_SERVICE_DIR = REPO_ROOT / "services" / "gmat-service"
if str(_GMAT_SERVICE_DIR) not in sys.path:
    sys.path.insert(0, str(_GMAT_SERVICE_DIR))
from gmat_service.evidence import EvidenceLog as _EvidenceLog  # noqa: E402


# =================================================================================================
# 1. Control-matrix discovery and parsing
# =================================================================================================

#: A single-line markdown table row: `| ID | Requirement | Status | Implementation | Evidence |`.
#: Deliberately the same shape `tests/test_compliance.py::_ROW_RE` already uses (reusing the
#: parsing idea per this task's own brief, not duplicating that file's checks): the ID column
#: must start with a digit, so the differently-shaped "Inherited wholesale" table (whose first
#: column is a family name like "AT (Awareness & Training)") and the two-column "Legend" table
#: never match this regex at all -- no family/table-boundary bookkeeping needed to exclude them.
#: The ID column is usually bare (`3.1.1`, `3.1.1 / 3.1.2`, `3.1.4/3.1.6–3.1.11`), but several
#: rows disambiguate two rows sharing one practice ID with a trailing parenthetical, e.g.
#: `3.3.1 (SIEM export)` (av-command, av-gateway) or `3.14.6 (log integrity)` (av-ingest,
#: av-edge-plugin) -- the char class below allows letters/parens/spaces so those still match;
#: `_clean_practice_token` (below) strips the parenthetical back off before the token is used as
#: a coverage-table practice ID, so both rows still attribute to the SAME practice.
ROW_RE = re.compile(r"^\|\s*([0-9][0-9./ ,–\-()A-Za-z]*)\s*\|(.+)\|(.+)\|(.+)\|(.+)\|\s*$")

#: Strips a trailing ` (free text)` annotation off one practice token, e.g. `3.3.1 (SIEM export)`
#: -> `3.3.1` -- so a row's own `id` field stays the literal, as-written text (for traceability)
#: while its `practices` list (used for coverage grouping) names the bare practice/range token.
_PRACTICE_PAREN_RE = re.compile(r"\s*\([^()]*\)\s*$")


def _clean_practice_token(token: str) -> str:
    return _PRACTICE_PAREN_RE.sub("", token).strip()

#: A practice-family heading, e.g. "## 3.1 Access Control (AC)". Table lines are only scanned
#: inside a section bounded by one of these (see `_iter_family_sections`) -- deliberately
#: narrower than "every line starting with `|` in the file", so the "Legend"/"Scope"/"Inherited
#: wholesale" tables, and the free-standing tables some documents carry (e.g. av-edge-plugin's
#: "## Numbers this document's evidence rests on"), can never be mistaken for a malformed
#: practice row just because a line inside them happens to start with `|`.
FAMILY_HEADING_RE = re.compile(r"^## 3\.\d+ .*\([A-Z]{2,4}\)\s*$")

#: Any second-level heading -- used only to find where a family (or Deficiencies) section ENDS.
HEADING_RE = re.compile(r"^##\s+")

DEFICIENCIES_HEADING_RE = re.compile(r"^##\s+Deficiencies\s*$")
DEFICIENCY_ITEM_RE = re.compile(r"^(\d+)\.\s+(.*)$")
DEFICIENCY_TITLE_RE = re.compile(r"^\*\*(.+?)\*\*")


def discover_components(repo_root: Path) -> dict[str, Path]:
    """Every `docs/compliance/<name>/control-matrix.md` that actually exists, keyed by `<name>`,
    sorted -- discovered from disk (like `sbom.suite_component_names` reads
    `deploy/secdeploy/suite.altavista.toml` rather than hard-coding the eight suite components),
    so a seventh component added later needs no edit here. `docs/compliance/sbom/` (no
    `control-matrix.md` of its own) is excluded by construction, not by name-listing it out."""
    compliance_dir = repo_root / "docs" / "compliance"
    out: dict[str, Path] = {}
    if not compliance_dir.is_dir():
        return out
    for child in sorted(compliance_dir.iterdir()):
        candidate = child / "control-matrix.md"
        if candidate.is_file():
            out[child.name] = candidate
    return out


def _iter_family_sections(lines: list[str]) -> list[tuple[int, int]]:
    """`(start, end)` line-index ranges (0-based, `end` exclusive) for every `## 3.x ... (FAM)`
    section -- `end` is the next `## ` heading of ANY kind, or the end of the file."""
    all_heading_idxs = [i for i, l in enumerate(lines) if HEADING_RE.match(l)]
    starts = [i for i in all_heading_idxs if FAMILY_HEADING_RE.match(lines[i].rstrip())]
    ranges = []
    for s in starts:
        end = next((h for h in all_heading_idxs if h > s), len(lines))
        ranges.append((s, end))
    return ranges


def _parse_rows(lines: list[str]) -> tuple[list[dict], list[dict]]:
    """Returns `(rows, malformed_rows)` for every `## 3.x` family section in `lines`. Within each
    section, the first two `|`-prefixed lines are the header (`| ID | Requirement | ... |`) and
    the `|---|...|` separator -- skipped; every `|`-prefixed line after that is either a real row
    (matches `ROW_RE`) or a malformed one (deliverable's optional 4th test: "a deliberately
    malformed row is a named finding, not a silent skip")."""
    rows: list[dict] = []
    malformed: list[dict] = []
    for start, end in _iter_family_sections(lines):
        table_idxs = [i for i in range(start, end) if lines[i].strip().startswith("|")]
        for i in table_idxs[2:]:  # skip header + separator
            raw = lines[i]
            line_no = i + 1  # 1-based
            m = ROW_RE.match(raw.strip())
            if not m:
                malformed.append({
                    "line_no": line_no,
                    "line": raw,
                    "reason": (
                        "table line inside a practice-family section does not match the "
                        "5-column `| id | requirement | status | implementation | evidence |` "
                        "shape"
                    ),
                })
                continue
            id_text, requirement, status, implementation, evidence = (
                g.strip() for g in m.groups()
            )
            practices = [
                _clean_practice_token(p) for p in re.split(r"\s*/\s*", id_text) if p.strip()
            ]
            rows.append({
                "id": id_text,
                "practices": practices,
                "requirement": requirement,
                "status": status,
                "implementation": implementation,
                "evidence": evidence,
                "line_no": line_no,
            })
    return rows, malformed


def _parse_deficiencies(lines: list[str]) -> list[dict]:
    """Every numbered entry (`N. **Title.** rest of text...`, continuation lines un-indented and
    joined by a single space) under the file's `## Deficiencies` heading, up to the next `## `
    heading or end of file. `title` is the leading `**bold**` span when a row opens with one
    (every deficiency observed across all six documents does), else `None` -- never assumed."""
    heading_idxs = [i for i, l in enumerate(lines) if DEFICIENCIES_HEADING_RE.match(l)]
    if not heading_idxs:
        return []
    start = heading_idxs[0] + 1
    all_heading_idxs = [i for i, l in enumerate(lines) if HEADING_RE.match(l)]
    end = next((h for h in all_heading_idxs if h > heading_idxs[0]), len(lines))

    entries: list[dict] = []
    current: Optional[dict] = None
    for line in lines[start:end]:
        m = DEFICIENCY_ITEM_RE.match(line)
        if m:
            if current is not None:
                entries.append(current)
            current = {"number": int(m.group(1)), "parts": [m.group(2).strip()]}
        elif current is not None and line.strip():
            current["parts"].append(line.strip())
    if current is not None:
        entries.append(current)

    out = []
    for e in entries:
        text = " ".join(e["parts"]).strip()
        title_m = DEFICIENCY_TITLE_RE.match(text)
        out.append({
            "number": e["number"],
            "title": title_m.group(1) if title_m else None,
            "text": text,
        })
    return out


def parse_control_matrix(path: Path) -> dict:
    """Parses one `control-matrix.md` file at `path` (any path -- a real component's file, or a
    scratch copy under `tmp_path`; this function has no notion of "component name"). Returns
    `{"sha256", "rows", "malformed_rows", "deficiencies"}`. `path`'s own repo-relative location
    is the CALLER's business (`build_control_matrices`, below), not this function's -- keeping
    this parser path-agnostic is what lets `tests/test_evidence_bundle.py`'s malformed-row test
    point it at a mutated tmp_path copy with no special-casing."""
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    rows, malformed_rows = _parse_rows(lines)
    deficiencies = _parse_deficiencies(lines)
    return {
        "sha256": sbom.sha256_file(path),
        "rows": rows,
        "malformed_rows": malformed_rows,
        "deficiencies": deficiencies,
    }


def build_control_matrices(repo_root: Path) -> dict[str, dict]:
    """`{component: {"path", "sha256", "rows", "malformed_rows"}}` for every discovered
    component, sorted by component name (`discover_components` already returns them sorted, and
    a `dict` literal built by iterating it preserves that order; `json.dump`'s own `sort_keys`
    makes the final on-disk order canonical regardless)."""
    out: dict[str, dict] = {}
    for component, path in discover_components(repo_root).items():
        parsed = parse_control_matrix(path)
        out[component] = {
            "path": path.relative_to(repo_root).as_posix(),
            "sha256": parsed["sha256"],
            "rows": parsed["rows"],
            "malformed_rows": parsed["malformed_rows"],
        }
    return out


def build_deficiencies(repo_root: Path) -> dict[str, list[dict]]:
    """Deliverable 3: every component's `## Deficiencies` section, parsed and attributed to its
    component -- re-parsed independently from `build_control_matrices` (both call
    `parse_control_matrix`, cheap: a handful of small markdown files) rather than smuggled inside
    that function's own return shape, so a reader of the bundle finds deficiencies at their own
    top-level key exactly where deliverable 3 names them."""
    out: dict[str, list[dict]] = {}
    for component, path in discover_components(repo_root).items():
        out[component] = parse_control_matrix(path)["deficiencies"]
    return out


# =================================================================================================
# 2. Cross-component NIST SP 800-171 coverage table
# =================================================================================================

#: Deliverable 2's own honesty instruction, quoted into the bundle itself (not just this task's
#: report) so a reader of `bundle.json` alone -- not this module's source -- sees the caveat:
#: NIST SP 800-171 Rev 2 has 110 practices across 14 families, but this repository carries no
#: authoritative list of all 110 identifiers to check against, and inventing one was explicitly
#: ruled out ("If you do not have an authoritative list of all 110 practice identifiers in this
#: repository, DO NOT invent one"). The denominator below is therefore "practices (or, for a row
#: whose ID column names a written range like `3.1.6–3.1.11`, that exact range token) that at
#: least one component's control matrix mentions by ID" -- never all 110, and never an expanded
#: enumeration of a written range into individual practice numbers (that would itself be
#: inventing IDs the matrix text does not literally contain).
COVERAGE_DENOMINATOR_NOTE = (
    "NIST SP 800-171 Rev 2 defines 110 practices across 14 families. This repository has no "
    "authoritative list of all 110 practice identifiers to check coverage against, and this "
    "bundle does not invent one (per this task's own instruction: honesty beats completeness). "
    "The 'practices' table below and 'practice_count' therefore cover exactly the practice "
    "identifiers (or, where a control-matrix row's ID column names a written range such as "
    "'3.1.6–3.1.11' rather than a single ID, that exact range token as written -- never "
    "expanded into individual practice numbers, which would itself be inventing IDs the source "
    "text does not literally contain) that AT LEAST ONE component's control matrix mentions by "
    "ID. A practice this repository's six matrices never mention at all cannot appear here, "
    "because this module has no independently-sourced list of what those unmentioned practices "
    "even are -- the denominator is 'practices any component speaks to', not all 110."
)


def build_coverage(control_matrices: dict[str, dict]) -> dict:
    """The cross-component coverage table. `attributions` is the literal, un-deduplicated list of
    every (component, row, practice-token) triple -- one entry per practice named by one row,
    sorted by `(component, row_id, practice)` -- which is exactly what
    `tests/test_evidence_bundle.py`'s reconciliation test (deliverable/question 148: "print the
    reconciliation numbers") counts against `sum(len(row['practices']) for every row)`.
    `practices` groups those same attributions by practice -> mark -> sorted component list (a
    SET per (practice, mark) pair, matching the deliverable's own wording: "exactly one coverage
    entry attributes that (practice, mark) to that component" -- a component that named the same
    practice at the same mark from two different rows would still appear once in this grouped
    view, while still counting twice in `attributions`, which is the number that must reconcile).
    """
    attributions: list[dict] = []
    for component in sorted(control_matrices):
        for row in control_matrices[component]["rows"]:
            for practice in row["practices"]:
                attributions.append({
                    "component": component,
                    "row_id": row["id"],
                    "practice": practice,
                    "mark": row["status"],
                })
    attributions.sort(key=lambda a: (a["component"], a["row_id"], a["practice"], a["mark"]))

    grouped: dict[str, dict[str, set]] = {}
    for a in attributions:
        by_mark = grouped.setdefault(a["practice"], {})
        by_mark.setdefault(a["mark"], set()).add(a["component"])

    practices = {
        practice: {mark: sorted(components) for mark, components in sorted(by_mark.items())}
        for practice, by_mark in sorted(grouped.items())
    }

    return {
        "denominator_note": COVERAGE_DENOMINATOR_NOTE,
        "practice_count": len(practices),
        "attribution_count": len(attributions),
        "practices": practices,
        "attributions": attributions,
    }


# =================================================================================================
# 3. Ledger verify -- the offline half driven for real; the live half's declared plug-in point
# =================================================================================================

def _display_path(path: Path, repo_root: Path) -> str:
    """A repo-relative POSIX path when `path` is inside `repo_root`; otherwise just `path`'s own
    final component (never the full absolute path) -- the determinism rule's "no absolute path
    from this host baked into the output" applied to a caller-supplied `--kit`/`--ledger-dir`
    that might live outside this repository entirely (a test's own `tmp_path`, for instance)."""
    path = Path(path)
    try:
        return path.resolve().relative_to(repo_root.resolve()).as_posix()
    except ValueError:
        return path.name


#: Deliverable 4's declared, honest placeholder for the SECOND worker's live half -- the exact
#: value `build_ledger_verify` returns for `"live"` when `assemble_bundle`'s `ledger_verify_live`
#: parameter is not supplied. Read `assemble_bundle`'s own doc, immediately below, for the plug-in
#: instructions this dict's own `"plug_in"` field also carries (so the instructions travel with
#: every bundle that has not yet been given a live half, not only with this module's source).
LIVE_LEDGER_VERIFY_NOT_COLLECTED = {
    "status": "not_collected",
    "reason": (
        "the live half -- `secdeploy evidence` run over a deployed evaluation placement, plus "
        "real ledger `verify` results fetched from running components' own "
        "/admin/api/evidence/verify HTTP endpoints -- needs an actually-reachable deployment "
        "(see the secdeploy checkout's own docs/compliance.md, 'this needs a live, "
        "reachable deployment; there is no offline/dry-run mode') and is the second worker's "
        "job, not this offline half's."
    ),
    "plug_in": (
        "scripts/kit/evidence.py:assemble_bundle's `ledger_verify_live` parameter is exactly "
        "where this plugs in. Build a dict shaped like `build_ledger_verify`'s own 'offline' "
        "entries -- one key per live-checked component (av-command, av-dynamics-service, "
        "av-edge-plugin, av-gateway, gmat-service), each value "
        "`{'status': 'collected'|'not_collected'|'error', 'url': <the /admin/api/evidence/"
        "verify URL actually dialed>, 'result': <that endpoint's JSON body, already exactly "
        "{'ok','checked','broken_at_seq','detail'} -- gmat_service.admin's own handler and "
        "crates/av-dynamics-service/src/admin.rs's Rust twin both already return this shape> "
        "or None, 'reason': <why not_collected/error, or None>}` -- then call "
        "`assemble_bundle(repo_root=..., ledger_verify_live={'offline_replaced_by_nothing': "
        "..., **your_dict})` (or simply pass your dict as `ledger_verify_live` directly; "
        "`assemble_bundle` stores it verbatim at `ledger_verify['live']`, replacing this "
        "placeholder one key at a time is NOT required -- the whole dict is substituted)."
    ),
}


def build_ledger_verify(
    repo_root: Path, ledger_dir: Optional[Path], ledger_verify_live: Optional[dict],
) -> dict:
    """`{"offline": {...}, "live": {...}}`. `offline["gmat_service"]` drives
    `gmat_service.evidence.EvidenceLog.verify()` for real against
    `<ledger_dir>/evidence.jsonl` when `ledger_dir` is given and that file exists; otherwise a
    declared, named `"not_collected"` entry -- never an empty dict (deliverable 4: "leave a
    declared, documented slot with an explicit 'not_collected' reason rather than an empty dict
    that reads as 'verified'"). `live` is `ledger_verify_live` verbatim when the caller supplies
    one (the second worker's own collection), else `LIVE_LEDGER_VERIFY_NOT_COLLECTED`."""
    offline: dict[str, dict] = {}
    if ledger_dir is None:
        offline["gmat_service"] = {
            "status": "not_collected", "path": None, "result": None,
            "reason": "no --ledger-dir given this run",
        }
    else:
        ledger_dir = Path(ledger_dir)
        ledger_path = ledger_dir / "evidence.jsonl"
        if not ledger_path.is_file():
            offline["gmat_service"] = {
                "status": "not_collected",
                "path": _display_path(ledger_path, repo_root),
                "result": None,
                "reason": f"{_display_path(ledger_dir, repo_root)} has no evidence.jsonl",
            }
        else:
            log = _EvidenceLog(ledger_path)
            offline["gmat_service"] = {
                "status": "collected",
                "path": _display_path(ledger_path, repo_root),
                "result": log.verify(),
                "reason": None,
            }

    live = ledger_verify_live if ledger_verify_live is not None else LIVE_LEDGER_VERIFY_NOT_COLLECTED
    return {"offline": offline, "live": live}


# =================================================================================================
# 4. SBOM hashes (deliverable 5) and kit manifest hash (deliverable 6)
# =================================================================================================

def build_sbom_hashes(repo_root: Path) -> dict:
    """Re-verifies `docs/compliance/sbom/SHA256SUMS`'s ten recorded hashes against the files on
    disk right now -- `stale` (recorded and actual disagree) and `missing` (recorded but the file
    is gone) are both named findings, never a silent copy of the recorded values (deliverable 5).
    """
    sums_path = repo_root / "docs" / "compliance" / "sbom" / "SHA256SUMS"
    recorded: dict[str, str] = {}
    if sums_path.is_file():
        for line in sums_path.read_text(encoding="utf-8").splitlines():
            line = line.rstrip("\n")
            if not line.strip():
                continue
            digest, _, relpath = line.partition("  ")
            recorded[relpath.strip()] = digest.strip()

    actual: dict[str, str] = {}
    stale: list[str] = []
    missing: list[str] = []
    for relpath in sorted(recorded):
        full = repo_root / relpath
        if not full.is_file():
            missing.append(relpath)
            continue
        digest = sbom.sha256_file(full)
        actual[relpath] = digest
        if digest != recorded[relpath]:
            stale.append(relpath)

    return {
        "sha256sums_path": (
            sums_path.relative_to(repo_root).as_posix() if sums_path.is_file() else None
        ),
        "recorded": recorded,
        "actual": actual,
        "stale": sorted(stale),
        "missing": sorted(missing),
    }


def build_kit_manifest_section(kit_dir: Optional[Path], repo_root: Path) -> dict:
    """Deliverable 6: the named kit's own `KIT_MANIFEST` hash, or a declared, named absence --
    never fabricated. `kit_dir` is the caller's `--kit <dir>` (a directory `manifest.write_manifest`
    already wrote a `KIT_MANIFEST` into); this function does not build or verify a kit itself."""
    if kit_dir is None:
        return {
            "collected": False, "kit_dir": None, "path": None, "sha256": None,
            "reason": "no --kit given this run",
        }
    kit_dir = Path(kit_dir)
    manifest_path = kit_dir / "KIT_MANIFEST"
    if not manifest_path.is_file():
        return {
            "collected": False,
            "kit_dir": _display_path(kit_dir, repo_root),
            "path": None, "sha256": None,
            "reason": f"{_display_path(kit_dir, repo_root)} has no KIT_MANIFEST",
        }
    return {
        "collected": True,
        "kit_dir": _display_path(kit_dir, repo_root),
        "path": _display_path(manifest_path, repo_root),
        "sha256": sbom.sha256_file(manifest_path),
        "reason": None,
    }


# =================================================================================================
# 5. Assembly
# =================================================================================================

def _git_head_commit(repo_root: Path) -> str:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo_root, capture_output=True, text=True, check=True,
    )
    return result.stdout.strip()


def assemble_bundle(
    *,
    repo_root: Path,
    kit_dir: Optional[Path] = None,
    ledger_dir: Optional[Path] = None,
    ledger_verify_live: Optional[dict] = None,
) -> dict:
    """Builds the whole bundle dict (unwritten -- see `write_bundle`). Two calls with the same
    `repo_root` tree state, the same `kit_dir` content (or both `None`), and the same
    `ledger_dir` content (or both `None`) produce byte-identical `json.dumps` output (the
    determinism this module's own top doc proves is the point).

    `ledger_verify_live` IS THE SECOND WORKER'S PLUG-IN POINT for the live half (`secdeploy
    evidence` over a reachable deployment, plus real `/admin/api/evidence/verify` results) --
    pass a dict shaped like `LIVE_LEDGER_VERIFY_NOT_COLLECTED`'s own `"plug_in"` field describes;
    leaving it `None` (this offline half's own default) records the honest, declared
    `"not_collected"` placeholder instead of an empty dict that would read as "verified".
    """
    control_matrices = build_control_matrices(repo_root)
    coverage = build_coverage(control_matrices)
    deficiencies = build_deficiencies(repo_root)
    ledger_verify = build_ledger_verify(repo_root, ledger_dir, ledger_verify_live)
    sbom_hashes = build_sbom_hashes(repo_root)
    kit_manifest = build_kit_manifest_section(kit_dir, repo_root)

    epoch_paths = sorted(
        (Path("docs") / "compliance" / c / "control-matrix.md").as_posix()
        for c in control_matrices
    ) + ["docs/compliance/sbom/SHA256SUMS"]
    epoch = sbom.git_epoch(epoch_paths)

    bundle = {
        "schema": "altavista-evidence-bundle",
        "schema_version": 1,
        "epoch": epoch,
        "epoch_paths": epoch_paths,
        "git_commit": _git_head_commit(repo_root),
        "control_matrices": control_matrices,
        "coverage": coverage,
        "deficiencies": deficiencies,
        "ledger_verify": ledger_verify,
        "sbom_hashes": sbom_hashes,
        "kit_manifest": kit_manifest,
    }
    # Deliverable 7: the bundle's own SHA-256, computed over its canonical content with the hash
    # field itself excluded (secdeploy's own deploy-audit chain convention, cited in this
    # module's top doc) -- so this key is added LAST, after every other key already has its
    # final value, never included in what it hashes.
    canonical = json.dumps(bundle, indent=2, sort_keys=True, ensure_ascii=False)
    bundle["bundle_sha256"] = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
    return bundle


def write_bundle(bundle: dict, out_dir: Path) -> Path:
    """Writes `<out_dir>/bundle.json`: `json.dump(bundle, indent=2, sort_keys=True,
    ensure_ascii=False)` plus one trailing newline -- the same deterministic format
    `sbom.write_document`/`manifest.write_manifest` already use. Returns the path written."""
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / "bundle.json"
    text = json.dumps(bundle, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    out_path.write_text(text, encoding="utf-8")
    return out_path


# =================================================================================================
# CLI
# =================================================================================================

def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, help="output directory; writes <out>/bundle.json")
    parser.add_argument(
        "--kit", default=None,
        help="a kit directory (scripts/kit/build_kit.py's own --out) whose KIT_MANIFEST hash "
             "should be recorded; omitted means a declared, named absence, never fabricated",
    )
    parser.add_argument(
        "--ledger-dir", default=None,
        help="a directory containing evidence.jsonl (gmat_service.evidence.EvidenceLog's own "
             "file) to verify for real; omitted means a declared 'not_collected' offline ledger "
             "entry",
    )
    args = parser.parse_args(argv)

    out_dir = Path(args.out)
    if not out_dir.is_absolute():
        out_dir = REPO_ROOT / out_dir
    kit_dir = Path(args.kit) if args.kit else None
    ledger_dir = Path(args.ledger_dir) if args.ledger_dir else None

    bundle = assemble_bundle(repo_root=REPO_ROOT, kit_dir=kit_dir, ledger_dir=ledger_dir)
    path = write_bundle(bundle, out_dir)
    print(f"wrote {path}", file=sys.stderr)
    print(f"bundle_sha256: {bundle['bundle_sha256']}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
