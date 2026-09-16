"""Question 208(c): the owned port map (`docs/architecture.md` section 4, "Default ports")
must agree with the real constants every default-bind call site cites it from. This test
parses BOTH sides for real -- the actual committed markdown table, and the actual committed
source files -- so an edit to either side alone (a constant bumped without the table, or the
table hand-edited without touching the code) fails here instead of silently drifting apart the
way `container.control_port`/`av-command`'s `DEFAULT_BIND` and `tests/test_gmat_service.py`'s
hard-coded admin port already had, before this task fixed both.

Deliberately does NOT import any of these crates as Rust code (most of them share no
dependency edge with each other, so there is no single Rust test binary that could link
against all of them at once) -- it reads the same committed source text `cargo build` would,
with a narrow, targeted regex per constant, and fails loudly (not silently) if a source file or
expected symbol goes missing.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
ARCHITECTURE_MD = REPO_ROOT / "docs" / "architecture.md"


def _read(rel_path: str) -> str:
    path = REPO_ROOT / rel_path
    if not path.is_file():
        pytest.fail(f"question 208(c) port-map test: expected file not found: {path} "
                    f"(repo root resolved as {REPO_ROOT} from {__file__}) -- this test cannot "
                    f"silently pass with a missing file, so it fails loudly instead.")
    return path.read_text()


def _extract(text: str, pattern: str, *, source: str, flags: int = 0) -> str:
    m = re.search(pattern, text, flags)
    if m is None:
        pytest.fail(f"question 208(c) port-map test: pattern {pattern!r} not found in {source} "
                     f"-- either the constant was renamed/removed, or this test's own pattern "
                     f"is stale. Update whichever one is wrong, not both blindly.")
    return m.group(1)


def _parse_architecture_table() -> dict[str, dict[str, str]]:
    """Real parsing of the real `docs/architecture.md` "Default ports" table: every data row
    (a header-separator `| --- | ... |` row is skipped by requiring a backtick right after the
    first `|`) becomes {service: {"grpc": ..., "admin": ..., "constant": ...}}."""
    text = _read("docs/architecture.md")
    marker = "### Default ports"
    start = text.find(marker)
    if start == -1:
        pytest.fail(f"question 208(c) port-map test: {ARCHITECTURE_MD} has no {marker!r} "
                     f"section -- the owned port map moved or was renamed; update this test's "
                     f"own marker to match, or restore the section.")
    # The table runs until the next "## " (level-2) heading, or end of file.
    rest = text[start:]
    next_heading = re.search(r"\n## ", rest)
    section = rest[:next_heading.start()] if next_heading else rest

    rows: dict[str, dict[str, str]] = {}
    for line in section.splitlines():
        if not line.startswith("| `"):
            continue  # skips the header row and the `| --- | --- | ... |` separator row
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) != 4:
            pytest.fail(f"question 208(c) port-map test: table row does not have exactly 4 "
                         f"cells (service, gRPC default, admin default, constant): {line!r}")
        service_cell, grpc_cell, admin_cell, constant_cell = cells
        # The service cell is sometimes just a backticked name (`` `av-command` ``) and
        # sometimes a backticked name followed by plain-text description (`` `av-kernel`
        # binding registry, ... ``) -- only the FIRST backtick span is the key this test
        # matches against `_real_constants()`'s own dict; the rest is for a human reader.
        service_match = re.match(r"`([^`]+)`", service_cell)
        if service_match is None:
            pytest.fail(f"question 208(c) port-map test: table row's service cell has no "
                         f"backtick-wrapped name to key on: {service_cell!r}")
        service = service_match.group(1)
        # Strip a leading "127.0.0.1:" so this side compares like-for-like with the numeric
        # ports the Rust/Python constants below are parsed as -- the table intentionally
        # spells out the full loopback address (documentation for a human), the source
        # constants sometimes carry only the bare port number (av-dynamics-service,
        # gmat-service, av-kernel).
        def _port_only(cell: str) -> "str | None":
            cell = cell.strip("`")
            if cell in ("*(none)*", "", "-"):
                return None
            if ":" in cell:
                return cell.rsplit(":", 1)[1]
            return cell
        rows[service] = {
            "grpc": _port_only(grpc_cell),
            "admin": _port_only(admin_cell),
            "constant": constant_cell,
        }
    if not rows:
        pytest.fail(f"question 208(c) port-map test: found the {marker!r} section but parsed "
                     f"zero table rows out of it -- the table's own markdown shape changed "
                     f"(this test expects `| \\`service\\` | ... |` rows) and this test's own "
                     f"parser needs updating to match.")
    return rows


def _real_constants() -> dict[str, dict[str, "str | None"]]:
    """The actual committed default bind/port for each service, read from its own real source
    file -- never a value copied by hand into this test."""
    real: dict[str, dict[str, "str | None"]] = {}

    av_command = _read("crates/av-command/src/bin/av-command.rs")
    real["av-command"] = {
        "grpc": _extract(av_command, r'const DEFAULT_BIND: &str = "127\.0\.0\.1:(\d+)"', source="crates/av-command/src/bin/av-command.rs"),
        "admin": _extract(av_command, r'const DEFAULT_ADMIN_BIND: &str = "127\.0\.0\.1:(\d+)"', source="crates/av-command/src/bin/av-command.rs"),
    }

    av_gateway = _read("crates/av-gateway/src/bin/av-gateway.rs")
    real["av-gateway"] = {
        "grpc": _extract(av_gateway, r'const DEFAULT_BIND: &str = "127\.0\.0\.1:(\d+)"', source="crates/av-gateway/src/bin/av-gateway.rs"),
        "admin": _extract(av_gateway, r'const DEFAULT_ADMIN_BIND: &str = "127\.0\.0\.1:(\d+)"', source="crates/av-gateway/src/bin/av-gateway.rs"),
    }

    dyn_config = _read("crates/av-dynamics-service/src/config.rs")
    dyn_grpc = _extract(dyn_config, r"pub const DEFAULT_PORT: u16 = (\d+);", source="crates/av-dynamics-service/src/config.rs")
    dyn_server = _read("crates/av-dynamics-service/src/bin/server.rs")
    # Confirms the +100 convention is still spelled out in code, not merely asserted here.
    _extract(dyn_server, r"const DEFAULT_ADMIN_PORT: u16 = av_dynamics_service::config::DEFAULT_PORT \+ (\d+);", source="crates/av-dynamics-service/src/bin/server.rs")
    real["av-dynamics-service"] = {"grpc": dyn_grpc, "admin": str(int(dyn_grpc) + 100)}

    gmat_config = _read("services/gmat-service/gmat_service/config.py")
    real["gmat-service"] = {
        "grpc": _extract(gmat_config, r"DEFAULT_PORT = (\d+)", source="services/gmat-service/gmat_service/config.py"),
        "admin": _extract(gmat_config, r"DEFAULT_ADMIN_PORT = (\d+)", source="services/gmat-service/gmat_service/config.py"),
    }

    shim = _read("crates/av-lockstep-shim/src/bin/av-lockstep-shim.rs")
    real["av-lockstep-shim"] = {
        "grpc": _extract(shim, r'const DEFAULT_GRPC_ADDR: &str = "127\.0\.0\.1:(\d+)"', source="crates/av-lockstep-shim/src/bin/av-lockstep-shim.rs"),
        "admin": None,
    }

    proposer = _read("crates/av-proposer/src/bin/av-proposer.rs")
    real["av-proposer"] = {
        "grpc": _extract(proposer, r'const DEFAULT_MODEL_SERVICE_BIND: &str = "127\.0\.0\.1:(\d+)"', source="crates/av-proposer/src/bin/av-proposer.rs"),
        "admin": None,
    }

    binding = _read("crates/av-kernel/src/drm/binding.rs")
    default_impl = _extract(binding, r"impl Default for ContainerSpec \{(.*?)\n\}", source="crates/av-kernel/src/drm/binding.rs", flags=re.DOTALL)
    real["av-kernel"] = {
        "grpc": _extract(default_impl, r"control_port:\s*(\d+),", source="crates/av-kernel/src/drm/binding.rs (ContainerSpec::default)"),
        "admin": None,
    }

    return real


def test_architecture_port_map_matches_the_real_constants():
    table = _parse_architecture_table()
    real = _real_constants()

    assert set(table.keys()) == set(real.keys()), (
        f"question 208(c): docs/architecture.md's port table and this test's own known "
        f"services disagree on WHICH services are listed.\n"
        f"table only: {sorted(set(table) - set(real))}\n"
        f"code only: {sorted(set(real) - set(table))}"
    )

    for service in sorted(real):
        for kind in ("grpc", "admin"):
            table_value = table[service][kind]
            real_value = real[service][kind]
            assert table_value == real_value, (
                f"question 208(c): docs/architecture.md's port table says {service}'s "
                f"{kind} default is {table_value!r}, but the real constant "
                f"({table[service]['constant']}) says {real_value!r}. Re-measure and fix "
                f"whichever side is stale -- never edit the test to match either one blindly."
            )
