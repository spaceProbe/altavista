"""AltaVista P5 track, round 1 task D1: proves the secdeploy merge machinery
(`deploy/secdeploy/merge.py`, `deploy/secdeploy/ports.py`) and the fragment it renders
(`deploy/secdeploy/suite.altavista.toml`) actually do what `docs/secdeploy-upstream.md` and the
fragment's own header claim.

Three things this file has to prove, independently:

1. The declared ports in `suite.altavista.toml` really match their claimed sources (`ports.py`,
   checked both the real way and via two deliberate-failure proofs — a wrong port, and a "gap"
   row whose source has since grown a real default).
2. The merge into the user's own `secdeploy` (`/Users/probe/code/secdeploy`) is byte-identical
   and reversible, and the merged manifest/site really validate and plan against the REAL
   secdeploy checkout (never vendored, never edited — question 213(a)).
3. The gap this whole `[tier_compat]` mapping exists to paper over is real: secdeploy rejects
   ADR-003's own tier names outright when the mapping is skipped. That's what makes
   `docs/secdeploy-upstream.md` evidence, not opinion.

No test writes anywhere but `tmp_path`, and no test mutates the process environment (question
199) — the `uv`/secdeploy subprocess calls below inherit the ambient environment (needed for
PATH to find `uv` at all) but never set/unset anything on it.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SECDEPLOY_DIR = Path("/Users/probe/code/secdeploy")
BASE_MANIFEST = SECDEPLOY_DIR / "suite.toml"

FRAGMENT_PATH = REPO_ROOT / "deploy/secdeploy/suite.altavista.toml"
EVAL_SITE = REPO_ROOT / "deploy/secdeploy/secsite.altavista-eval.toml"
SITE_3 = REPO_ROOT / "deploy/secdeploy/secsite.altavista-3.toml"

ALTAVISTA_COMPONENTS = [
    "av-ingest", "av-command", "av-gateway", "av-proposer",
    "av-dynamics-service", "gmat-service", "av-edge-plugin", "av-viewer",
]

sys.path.insert(0, str(REPO_ROOT / "deploy" / "secdeploy"))
import merge  # noqa: E402
import ports  # noqa: E402


# ── Gating (question 194): a typed reason, computed once, asserted-by-name via
# pytest.mark.skipif -- same pattern as tests/test_edge_plugin_container.py's docker gate. ──
def _secdeploy_skip_reason() -> str | None:
    missing = []
    if shutil.which("uv") is None:
        missing.append("`uv` is not on PATH")
    if not BASE_MANIFEST.exists():
        missing.append(f"{BASE_MANIFEST} does not exist (no secdeploy checkout)")
    if missing:
        return "secdeploy round-trip tests need " + " and ".join(missing)
    return None


_SECDEPLOY_SKIP_REASON = _secdeploy_skip_reason()
requires_secdeploy = pytest.mark.skipif(
    _SECDEPLOY_SKIP_REASON is not None, reason=_SECDEPLOY_SKIP_REASON or ""
)


def _uv_run(*args: str) -> subprocess.CompletedProcess:
    """Run the real `secdeploy` CLI exactly as documented: `uv run --offline --project
    /Users/probe/code/secdeploy secdeploy <args>`, from any cwd (every path handed to *args*
    must therefore be absolute). No env= override -- this inherits the ambient environment (so
    PATH resolves `uv`) and sets nothing on it."""
    return subprocess.run(
        ["uv", "run", "--offline", "--project", str(SECDEPLOY_DIR), "secdeploy", *args],
        capture_output=True, text=True, timeout=180,
    )


# ── 1. Port provenance ──────────────────────────────────────────────────────────────────────

def test_fragment_ports_match_their_sources():
    fragment = merge.load_fragment(FRAGMENT_PATH)
    findings = ports.check_ports(fragment, REPO_ROOT)
    assert findings == [], "\n".join(
        f"{f.component}: fragment declares {f.expected!r}, source says {f.found!r} ({f.detail})"
        for f in findings
    )
    # Question 217(g): the owned port map (docs/architecture.md "### Default ports") is now the
    # single source every kind = "const" row is cross-checked against too -- assert how many
    # fragment rows were checked against how many map entries, so this isn't just "zero
    # findings" but a stated, non-trivial coverage claim.
    port_map = ports.load_owned_port_map(REPO_ROOT / "docs" / "architecture.md")
    const_rows = {n for n, spec in fragment["ports"].items() if spec.get("kind") == "const"}
    exempt_rows = {n for n in const_rows if fragment["ports"][n].get("owned_map_exempt")}
    print(f"checked {len(fragment['ports'])} fragment [ports.*] rows "
          f"({len(const_rows)} kind=\"const\") against {len(port_map)} owned port map entries "
          f"({len(exempt_rows)} const row(s) explicitly exempted: {sorted(exempt_rows)})")
    assert len(port_map) == 9, sorted(port_map)
    assert const_rows - exempt_rows <= set(port_map), (
        "every non-exempt const row must have owned-map provenance", const_rows, exempt_rows, port_map
    )
    assert exempt_rows == {"av-viewer"}, exempt_rows


def test_a_wrong_port_is_caught_for_av_ingest(tmp_path):
    """Same deliberate-failure proof as test_a_wrong_port_is_caught, for av-ingest's own new
    kind = "const" row (question 219(a)): the negative half of the three-way agreement must
    cover av-ingest too, not just the rows that already existed before this round."""
    text = FRAGMENT_PATH.read_text()
    assert 'port = 50060' in text, "fixture assumption: av-ingest's declared port"
    mutated = text.replace("port = 50060", "port = 12345", 1)
    assert mutated != text

    frag_copy = tmp_path / "suite.altavista.toml"
    frag_copy.write_text(mutated)

    fragment = merge.load_fragment(frag_copy)
    findings = ports.check_ports(fragment, REPO_ROOT)  # real repo -- only the DECLARED port moved

    matches = [f for f in findings if f.component == "av-ingest"]
    assert len(matches) == 1, findings
    finding = matches[0]
    assert finding.kind == "const"
    assert finding.expected == 12345, finding
    assert finding.found == 50060, finding
    other = [f for f in findings if f.component != "av-ingest"]
    assert other == [], other


def test_a_wrong_port_is_caught(tmp_path):
    """Deliberate-failure proof: mutate one declared port in a COPY of the fragment (never the
    real file, never the repo) and confirm the checker names exactly that component with both
    numbers."""
    text = FRAGMENT_PATH.read_text()
    assert 'port = 50070' in text, "fixture assumption: av-command's declared port"
    mutated = text.replace("port = 50070", "port = 12345", 1)
    assert mutated != text

    frag_copy = tmp_path / "suite.altavista.toml"
    frag_copy.write_text(mutated)

    fragment = merge.load_fragment(frag_copy)
    findings = ports.check_ports(fragment, REPO_ROOT)  # real repo -- only the DECLARED port moved

    matches = [f for f in findings if f.component == "av-command"]
    assert len(matches) == 1, findings
    finding = matches[0]
    assert finding.expected == 12345, finding
    assert finding.found == 50070, finding


def test_a_wrong_owned_port_map_entry_is_caught(tmp_path):
    """New capability (question 217(g)): a const row's declared port and its source constant
    can both be correct while the OWNED PORT MAP itself disagrees -- mutate a COPY of
    docs/architecture.md's "### Default ports" table under tmp_path (never the real file) and
    confirm check_ports flags it, naming the map."""
    arch_text = (REPO_ROOT / "docs" / "architecture.md").read_text()
    line = (
        "| `av-command` | `127.0.0.1:50070` | `127.0.0.1:50170` | "
        "`crates/av-command/src/bin/av-command.rs::DEFAULT_BIND` / `::DEFAULT_ADMIN_BIND` |\n"
    )
    assert line in arch_text, "fixture assumption: av-command's exact owned-port-map row"
    mutated = arch_text.replace(line, line.replace("50070", "50099", 1), 1)
    assert mutated != arch_text

    arch_copy = tmp_path / "architecture.md"
    arch_copy.write_text(mutated)

    fragment = merge.load_fragment(FRAGMENT_PATH)
    findings = ports.check_ports(fragment, REPO_ROOT, architecture_md=arch_copy)  # real repo_root -- only the MAP moved

    matches = [f for f in findings if f.component == "av-command"]
    assert len(matches) == 1, findings
    finding = matches[0]
    assert finding.kind == "const"
    assert "owned port map" in finding.detail
    assert "50099" in finding.detail
    other = [f for f in findings if f.component != "av-command"]
    assert other == [], other


def test_a_const_row_missing_from_the_owned_port_map_is_caught(tmp_path):
    """A kind = "const" row whose component has NO row in the owned port map at all -- and no
    explicit `owned_map_exempt` -- must be its own Finding: it has no owned provenance,
    independent of whether fragment and source agree with each other. Remove av-viewer's
    documented exemption from a COPY of the fragment (never the real file) and confirm
    check_ports catches the missing provenance against the REAL (unmutated) architecture.md,
    which genuinely carries no av-viewer row."""
    text = FRAGMENT_PATH.read_text()
    line = "owned_map_exempt = true\n"
    assert line in text, "fixture assumption: av-viewer's owned_map_exempt exemption line"
    mutated = text.replace(line, "", 1)
    assert mutated != text

    frag_copy = tmp_path / "suite.altavista.toml"
    frag_copy.write_text(mutated)

    fragment = merge.load_fragment(frag_copy)
    assert "owned_map_exempt" not in fragment["ports"]["av-viewer"]

    findings = ports.check_ports(fragment, REPO_ROOT)  # real architecture.md -- genuinely no av-viewer row
    matches = [f for f in findings if f.component == "av-viewer"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "const"
    assert "no row in the owned port map" in matches[0].detail
    other = [f for f in findings if f.component != "av-viewer"]
    assert other == [], other


def test_a_new_owned_port_map_row_closes_the_gap(tmp_path):
    """New capability (question 217(g)): a `kind = "gap"` row must fail the day the OWNED PORT
    MAP itself grows a row for it -- independent of whether the source file has grown a default
    bind (that's the existing, separate check). This used to be pinned to av-proposer, but
    av-proposer's own gap closed for real this round (question 219(a): the heavy track's crate
    landed a default bind, matching av-ingest's kind = "const" shape) and the fragment now has
    no real "gap" row left at all -- so this uses a synthetic in-memory fragment (never a fake
    row added to suite.altavista.toml) with the identical shape a real gap row would have.
    Mutate a COPY of docs/architecture.md to insert a fabricated row for the synthetic
    component and confirm check_ports reports the gap as closed, with a Finding whose detail
    names the map."""
    fragment = {
        "components": {"synthetic-gap-svc": {"port": 0}},
        "ports": {
            "synthetic-gap-svc": {
                "kind": "gap",
                "file": "synthetic/gap_svc.rs",
                "flag": "--serve-model-service",
            },
        },
    }

    fabricated_source = tmp_path / "synthetic" / "gap_svc.rs"
    fabricated_source.parent.mkdir(parents=True, exist_ok=True)
    fabricated_source.write_text("// --serve-model-service (no default bind declared)\n")

    arch_text = (REPO_ROOT / "docs" / "architecture.md").read_text()
    separator = "| --- | --- | --- | --- |\n"
    assert separator in arch_text, "fixture assumption: the owned port map's header separator row"
    fabricated_row = (
        '| `synthetic-gap-svc` | `127.0.0.1:59999` | *(none)* | '
        '`synthetic/gap_svc.rs::DEFAULT_BIND` |\n'
    )
    mutated = arch_text.replace(separator, separator + fabricated_row, 1)
    assert mutated != arch_text
    assert "synthetic-gap-svc" in mutated

    arch_copy = tmp_path / "architecture.md"
    arch_copy.write_text(mutated)

    findings = ports.check_ports(fragment, repo_root=tmp_path, architecture_md=arch_copy)

    matches = [f for f in findings if f.component == "synthetic-gap-svc"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "gap"
    assert "owned port map" in matches[0].detail.lower()


def test_a_new_default_bind_closes_the_gap(tmp_path):
    """Inverse deliberate-failure proof, for a `kind = "gap"` row: point the checker at a
    FABRICATED source file (under tmp_path, never the real repo) that now declares a real
    default bind, and confirm the checker reports that gap row as stale. This used to be pinned
    to av-proposer, but av-proposer's own gap closed for real this round (question 219(a): the
    heavy track's crate landed a default bind, matching av-ingest's kind = "const" shape since
    the round before it) and the fragment now has no real "gap" row left at all -- so this uses
    a synthetic in-memory fragment (never a fake row added to suite.altavista.toml) with the
    identical shape a real gap row would have."""
    fragment = {
        "components": {"synthetic-gap-svc": {"port": 0}},
        "ports": {
            "synthetic-gap-svc": {
                "kind": "gap",
                "file": "synthetic/gap_svc.rs",
                "flag": "--serve-model-service",
            },
        },
    }

    fabricated = tmp_path / "synthetic" / "gap_svc.rs"
    fabricated.parent.mkdir(parents=True, exist_ok=True)
    fabricated.write_text(
        'const DEFAULT_BIND: &str = "127.0.0.1:9999";\n'
        '// --serve-model-service (kept so the flag-provenance half of the check still passes)\n'
    )

    findings = ports.check_ports(fragment, repo_root=tmp_path)  # repo_root swapped to tmp_path
    stale = [f for f in findings if f.component == "synthetic-gap-svc"]
    assert len(stale) == 1, findings
    assert stale[0].kind == "gap"
    assert "DEFAULT_BIND" in str(stale[0].found)
    assert "closed" in stale[0].detail.lower()


def test_a_component_with_no_ports_row_is_caught(tmp_path):
    """A [components.*] table with no matching [ports.<name>] row must not be silently
    unchecked -- it must surface as its own ("unpaired") Finding. Same tmp_path-copy technique
    as test_a_wrong_port_is_caught: delete one [ports.*] block from a COPY of the fragment."""
    text = FRAGMENT_PATH.read_text()
    block = (
        '[ports.av-command]\n'
        'kind = "const"\n'
        'file = "crates/av-command/src/bin/av-command.rs"\n'
        'pattern = \'DEFAULT_BIND:\\s*&str\\s*=\\s*"[\\d.]+:(\\d+)"\'\n'
        '\n'
    )
    assert block in text, "fixture assumption: av-command's exact [ports.*] block"
    mutated = text.replace(block, "", 1)
    assert mutated != text

    frag_copy = tmp_path / "suite.altavista.toml"
    frag_copy.write_text(mutated)

    fragment = merge.load_fragment(frag_copy)
    assert "av-command" in fragment["components"]
    assert "av-command" not in fragment["ports"]

    findings = ports.check_ports(fragment, REPO_ROOT)
    matches = [f for f in findings if f.component == "av-command"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "unpaired"
    # Every OTHER component must still check out clean -- this is a targeted gap, not a
    # cascade failure.
    other = [f for f in findings if f.component != "av-command"]
    assert other == [], other


def test_a_real_listener_in_the_no_listener_file_is_caught(tmp_path):
    """Inverse deliberate-failure proof for a `kind = "no-listener"` row: point the checker at
    a FABRICATED source file (under tmp_path, never the real repo) that now looks like it binds
    a real listener, and confirm the checker reports the "no-listener" claim as wrong."""
    fragment = merge.load_fragment(FRAGMENT_PATH)
    plugin_file = fragment["ports"]["av-edge-plugin"]["file"]
    assert fragment["ports"]["av-edge-plugin"]["kind"] == "no-listener"

    fabricated = tmp_path / plugin_file
    fabricated.parent.mkdir(parents=True, exist_ok=True)
    fabricated.write_text(
        '// av-edge-plugin, but now it also serves something:\n'
        'let listener = tokio::net::TcpListener::bind("127.0.0.1:9999").await?;\n'
    )

    findings = ports.check_ports(fragment, repo_root=tmp_path)
    wrong = [f for f in findings if f.component == "av-edge-plugin"]
    assert len(wrong) == 1, findings
    assert wrong[0].kind == "no-listener"
    assert "TcpListener" in str(wrong[0].found)


# ── FragmentRenderError: a fragment that cannot be safely rendered ─────────────────────────

def test_an_unmapped_tier_raises_fragmentrendererror(tmp_path):
    """[tier_compat] must map EVERY AltaVista tier explicitly -- a tier with no entry must
    raise, not silently fall through as an identity mapping. Delete one [tier_compat] mapping
    from a COPY of the fragment and confirm render_components refuses."""
    text = FRAGMENT_PATH.read_text()
    line = 'engine = "inference"\n'
    assert line in text, "fixture assumption: the engine -> inference tier_compat mapping"
    mutated = text.replace(line, "", 1)
    assert mutated != text

    frag_copy = tmp_path / "suite.altavista.toml"
    frag_copy.write_text(mutated)
    fragment = merge.load_fragment(frag_copy)
    assert "engine" not in fragment["tier_compat"]

    with pytest.raises(merge.FragmentRenderError, match=r"av-ingest.*engine|engine.*av-ingest"):
        merge.render_components(fragment, map_tiers=True)


def test_an_unsafe_field_value_raises_fragmentrendererror():
    """A component field containing a character that would break unescaped TOML string
    interpolation (here: a literal double-quote in `role`) must raise, never render a broken
    manifest. Built as an in-memory fragment dict -- no file, no repo, no tmp_path needed."""
    fragment = {
        "tier_compat": {"engine": "inference"},
        "components": {
            "av-ingest": {
                "repo": "spaceProbe/altavista",
                "ref": "develop",
                "kind": "service",
                "tier": "engine",
                "role": 'has a stray " quote in it',
            },
        },
    }
    with pytest.raises(merge.FragmentRenderError, match=r"role"):
        merge.render_components(fragment, map_tiers=True)


# ── 2. The merge itself ─────────────────────────────────────────────────────────────────────

@requires_secdeploy
def test_merge_is_byte_identical_and_reversible(tmp_path):
    out1, out2 = tmp_path / "out1", tmp_path / "out2"
    p1 = merge.merge(base=BASE_MANIFEST, fragment=FRAGMENT_PATH, site=EVAL_SITE, out=out1)
    p2 = merge.merge(base=BASE_MANIFEST, fragment=FRAGMENT_PATH, site=EVAL_SITE, out=out2)

    bytes1, bytes2 = p1.read_bytes(), p2.read_bytes()
    assert bytes1 == bytes2, "merging twice into two different dirs must be byte-identical"

    base_text, altavista_text = merge.split_merged(bytes1.decode("utf-8"))
    assert base_text.encode("utf-8") == BASE_MANIFEST.read_bytes(), (
        "split_merged must recover the base manifest's exact bytes"
    )
    for name in ALTAVISTA_COMPONENTS:
        assert name in altavista_text, f"{name} missing from the rendered AltaVista section"


@requires_secdeploy
def test_secdeploy_verify_accepts_the_merged_manifest(tmp_path):
    out = tmp_path / "out"
    merged = merge.merge(base=BASE_MANIFEST, fragment=FRAGMENT_PATH, site=EVAL_SITE, out=out)
    site_copy = out / "secsite.merged.toml"

    result = _uv_run("--manifest", str(merged), "verify", "--site", str(site_copy))
    assert result.returncode == 0, result.stdout + result.stderr

    assert "✓ manifest valid" in result.stdout
    assert "✓ target assets present" in result.stdout
    assert "✓ topology valid" in result.stdout
    for name in ALTAVISTA_COMPONENTS:
        assert name in result.stdout, f"{name} missing from verify output"


@requires_secdeploy
def test_secdeploy_plan_macos_lists_every_altavista_component(tmp_path):
    out = tmp_path / "out"
    merged = merge.merge(base=BASE_MANIFEST, fragment=FRAGMENT_PATH, site=EVAL_SITE, out=out)
    site_copy = out / "secsite.merged.toml"

    result = _uv_run("--manifest", str(merged), "plan", "macos", "--site", str(site_copy))
    assert result.returncode == 0, result.stdout + result.stderr

    fragment = merge.load_fragment(FRAGMENT_PATH)
    for name in ALTAVISTA_COMPONENTS:
        ref = fragment["components"][name]["ref"]
        assert name in result.stdout, f"{name} missing from plan output"
        assert f"@ {ref}" in result.stdout, f"{name}'s ref {ref!r} missing from plan output"


@requires_secdeploy
def test_secdeploy_verify_accepts_the_three_resource_site(tmp_path):
    out = tmp_path / "out"
    merged = merge.merge(base=BASE_MANIFEST, fragment=FRAGMENT_PATH, site=SITE_3, out=out)
    site_copy = out / "secsite.merged.toml"

    result = _uv_run("--manifest", str(merged), "verify", "--site", str(site_copy))
    assert result.returncode == 0, result.stdout + result.stderr
    for resource_name in ("edge-a", "edge-b", "edge-c"):
        assert resource_name in result.stdout


# ── 3. The gap docs/secdeploy-upstream.md is evidence for ──────────────────────────────────

@requires_secdeploy
def test_adr003_tiers_are_still_rejected_upstream(tmp_path):
    """Merge WITHOUT the [tier_compat] rewrite (map_tiers=False) and confirm secdeploy rejects
    ADR-003's own tier names outright. The day this test's `verify` call starts returning 0
    instead of 1, secdeploy has grown manifest-declared tiers (docs/secdeploy-upstream.md's
    first proposal) and [tier_compat] -- plus merge.py's whole map_tiers machinery -- can be
    deleted."""
    out = tmp_path / "out"
    merged = merge.merge(
        base=BASE_MANIFEST, fragment=FRAGMENT_PATH, site=EVAL_SITE, out=out, map_tiers=False,
    )
    site_copy = out / "secsite.merged.toml"

    result = _uv_run("--manifest", str(merged), "verify", "--site", str(site_copy))
    assert result.returncode == 1, result.stdout + result.stderr
    combined = result.stdout + result.stderr
    assert "tier must be one of" in combined
    assert any(adr003_tier in combined for adr003_tier in ("engine", "design")), combined


# ── 4. fedora-fips dry-run render (P5 round 3, D6) ──────────────────────────────────────────
#
# `secdeploy deploy fedora-fips --dry-run` run for real, offline, over OUR merged manifest and
# OUR evaluation site — see docs/compliance/fedora-fips.md, which this test backs. Proves the
# predicted shape (also true of `targets/macos.py`'s `deploy()`, see docs/secdeploy-upstream.md
# Proposal 3): `targets/fedora_fips.py`'s `deploy()` walks its own hard-coded `SERVICES` tuple
# (`secdns`/`seccert`/`secllm`/`secrouter`/`secagent`/`secrecorder`/`secproxy`), never the
# manifest, so none of our eight components get installed code, a config file, or a systemd
# unit — with exactly one partial exception: `av-viewer` (the only AltaVista component with
# `fronted = true`) is named as a certbot `-d` flag for secproxy's SAN cert, because
# `wiring.fronted_instances` reads the WHOLE merged manifest, independent of `SERVICES`.

FEDORA_FIPS_NATIVE_SERVICES = "secdns, seccert, secrouter, secrecorder, secproxy"


@requires_secdeploy
def test_secdeploy_deploy_fedora_fips_dry_run_renders_nothing_for_our_components(tmp_path):
    out = tmp_path / "out"
    merged = merge.merge(base=BASE_MANIFEST, fragment=FRAGMENT_PATH, site=EVAL_SITE, out=out)
    site_copy = out / "secsite.merged.toml"

    result = _uv_run(
        "--manifest", str(merged),
        "--work", str(tmp_path / "work"), "--out", str(tmp_path / "secdeploy-out"),
        "deploy", "fedora-fips", "--dry-run", "--site", str(site_copy),
    )
    assert result.returncode == 0, result.stdout + result.stderr
    stdout = result.stdout

    # secdeploy's own native services placed here — no --with-inference/--with-agent passed, so
    # secllm/secagent are excluded too. This is the real rendered header line, not an assumption.
    assert f"native services here: {FEDORA_FIPS_NATIVE_SERVICES}" in stdout, stdout

    # None of our eight components got a code install or a systemd unit — the positive assertion
    # docs/p5-plan.md D6 asks for: "render nothing", stated and checked, not just absence of a
    # crash.
    for name in ALTAVISTA_COMPONENTS:
        assert f"install {name} code" not in stdout, (name, "unexpectedly got a code install", stdout)
        assert f"install {name}.service" not in stdout, (name, "unexpectedly got a systemd unit", stdout)

    # The one partial exception: av-viewer's FQDN is named in the certbot SAN-cert -d flags for
    # secproxy (it's the only AltaVista component with `fronted = true` in suite.altavista.toml)
    # -- the description line names it in the -d COUNT/list, and the actual certbot command line
    # right below it carries the real `-d av-viewer.altavista.internal` flag.
    desc_lines = [l for l in stdout.splitlines() if "issue secproxy SAN cert" in l]
    assert len(desc_lines) == 1, stdout
    assert "av-viewer.altavista.internal" in desc_lines[0], desc_lines[0]
    cmd_lines = [l for l in stdout.splitlines() if "certbot certonly --standalone" in l]
    assert len(cmd_lines) == 1, stdout
    assert "-d av-viewer.altavista.internal" in cmd_lines[0], cmd_lines[0]

    # Every OTHER AltaVista FQDN is entirely absent from the render — av-viewer is the only
    # component that appears anywhere, and even it gets no code/config/unit (asserted above).
    for name in ALTAVISTA_COMPONENTS:
        if name == "av-viewer":
            continue
        assert f"{name}.altavista.internal" not in stdout, (name, stdout)
        assert f"install {name}" not in stdout, (name, stdout)
