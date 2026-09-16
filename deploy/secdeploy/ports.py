"""Port-provenance checker for the AltaVista secdeploy fragment.

``deploy/secdeploy/suite.altavista.toml`` declares, per component, both a placement port and
the real source-of-truth that port is supposed to match (see that file's own
``[ports.<name>]`` tables). This module is the thing that actually checks the declaration
against the source, so a fragment edit that drifts from the real default — or a real default
that lands where today there's a documented gap — is caught by a test
(``tests/test_suite_declarations.py``) instead of trusted on faith.

Library code only: this module never prints and never calls ``sys.exit``. Callers (tests, or a
future CLI) decide what to do with the findings list.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import NamedTuple

# A component's declared port must be a positive, non-zero value for a "const" row (0 doesn't
# mean "unassigned" in this schema, see suite.altavista.toml's header — it always means either
# a documented gap or a component that never listens).
_VALID_KINDS = {"const", "gap", "no-listener"}

# Generic "this file now looks like it declares a default bind" detector, used to catch a
# "gap" row going stale (question 208(c) landing a real default). Deliberately loose — it only
# has to fire on the shapes AltaVista's own binaries actually use (a DEFAULT_BIND/DEFAULT_PORT
# constant, or a clap-style `default = N` / `default=N`), not defend against every conceivable
# way a default could someday be spelled.
_DEFAULT_BIND_RE = re.compile(r"DEFAULT_BIND|DEFAULT_PORT|default\s*=\s*\d+")

# Generic "this file now looks like it declares a listener" detector, used by
# `_check_no_listener` to keep a "no-listener" row honest. Picked to match the exact shapes
# AltaVista's own binaries use for a real inbound listener: a raw `TcpListener`/`.bind(` call
# (av-ingest-server.rs, av-proposer.rs, av-gateway.rs all use one of these), a tonic
# `Server::builder`, or one of the required-flag names (`--grpc-bind`/`--admin-bind`) that the
# two "gap" rows above already use as their own positive marker. If av-edge-plugin ever grows a
# listener, its own `--endpoint` (dial-out) flag would gain a sibling with one of these shapes.
_LISTENER_RE = re.compile(r"TcpListener|\.bind\(|--grpc-bind|--admin-bind|Server::builder")


class Finding(NamedTuple):
    """One thing ``check_ports`` found wrong. ``expected`` is the fragment's declared port
    (``None`` when the row itself is malformed before a port comparison is even possible);
    ``found`` is whatever the source actually said (a port number, a regex-match count, or a
    short marker string) — its shape depends on ``kind`` and ``detail`` always says which."""

    component: str
    expected: int | None
    found: object
    kind: str
    detail: str


# The "### Default ports" table's own section marker in docs/architecture.md (question 208(c),
# owned by 217(g)). `load_owned_port_map` looks for this exact string.
_ARCH_TABLE_MARKER = "### Default ports"


def load_owned_port_map(architecture_md: str | Path) -> dict[str, dict[str, object]]:
    """Parse ``docs/architecture.md``'s "### Default ports" table (question 208(c), made the
    single source for the fragment's own port checks by question 217(g)) into
    ``{service: {"grpc": int | None, "admin": int | None, "constant": str}}``.

    This is a real parse of the real committed markdown, mirroring
    ``tests/test_port_map.py``'s own independent parser (which proves this same table matches
    the real source constants) — kept as a separate implementation deliberately, so this
    library module never imports from the test suite. Raises ``ValueError`` if the section or
    its rows cannot be found; never silently returns an empty/partial map.
    """
    path = Path(architecture_md)
    text = path.read_text()
    start = text.find(_ARCH_TABLE_MARKER)
    if start == -1:
        raise ValueError(f"{path}: no {_ARCH_TABLE_MARKER!r} section found")
    rest = text[start:]
    next_heading = re.search(r"\n## ", rest)
    section = rest[:next_heading.start()] if next_heading else rest

    def _port_only(cell: str) -> "str | None":
        cell = cell.strip("`")
        if cell in ("*(none)*", "", "-"):
            return None
        if ":" in cell:
            return cell.rsplit(":", 1)[1]
        return cell

    rows: dict[str, dict[str, object]] = {}
    for line in section.splitlines():
        if not line.startswith("| `"):
            continue  # skips the header row and the `| --- | --- | ... |` separator row
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) != 4:
            raise ValueError(f"{path}: table row does not have exactly 4 cells: {line!r}")
        service_cell, grpc_cell, admin_cell, constant_cell = cells
        service_match = re.match(r"`([^`]+)`", service_cell)
        if service_match is None:
            raise ValueError(f"{path}: table row's service cell has no backtick name: {service_cell!r}")
        service = service_match.group(1)
        grpc_raw = _port_only(grpc_cell)
        admin_raw = _port_only(admin_cell)
        rows[service] = {
            "grpc": int(grpc_raw) if grpc_raw is not None else None,
            "admin": int(admin_raw) if admin_raw is not None else None,
            "constant": constant_cell,
        }
    if not rows:
        raise ValueError(f"{path}: found {_ARCH_TABLE_MARKER!r} but parsed zero table rows")
    return rows


def check_ports(
    fragment: dict,
    repo_root: str | Path,
    architecture_md: str | Path | None = None,
) -> list[Finding]:
    """Check every ``[ports.<name>]`` row in a parsed fragment (``load_fragment`` /
    ``tomllib.loads`` output — a dict with top-level ``components`` and ``ports`` tables)
    against the real source tree rooted at ``repo_root``, AND against the owned port map in
    ``docs/architecture.md`` (question 217(g)). ``architecture_md`` defaults to
    ``repo_root / "docs" / "architecture.md"`` — pass a different path (e.g. a mutated copy
    under ``tmp_path``) to test the owned-port-map checks in isolation from the real file.
    Returns an empty list when every row checks out.
    """
    repo_root = Path(repo_root)
    if architecture_md is None:
        architecture_md = repo_root / "docs" / "architecture.md"
    components: dict = fragment.get("components") or {}
    ports: dict = fragment.get("ports") or {}
    findings: list[Finding] = []

    try:
        port_map = load_owned_port_map(architecture_md)
    except (FileNotFoundError, ValueError) as exc:
        port_map = {}
        findings.append(Finding(
            "<owned-port-map>", None, None, "port-map",
            f"failed to parse the owned port map at {architecture_md}: {exc}",
        ))

    # Every declared component must have a matching [ports.<name>] row and vice versa — a
    # component with no row gets NO port provenance checked at all, silently, which is worse
    # than any individual row being wrong (nothing here would ever say so). Checked as a set
    # comparison up front, before any per-row check, so a name present in only one table is its
    # own Finding with its own kind ("unpaired") rather than being quietly skipped.
    component_names = set(components)
    port_names = set(ports)
    for name in sorted(component_names - port_names):
        findings.append(Finding(name, None, None, "unpaired",
                                 f"components.{name} has no matching [ports.{name}] row — "
                                 f"its port has no checked provenance at all"))
    for name in sorted(port_names - component_names):
        findings.append(Finding(name, None, None, "unpaired",
                                 f"[ports.{name}] does not name any [components.{name}] table"))

    for name in ports:
        if name not in components:
            continue  # already reported above as "unpaired" — nothing more to check
        spec = ports[name]
        kind = spec.get("kind", "")
        file_rel = spec.get("file", "")
        declared = components.get(name, {}).get("port", 0)
        try:
            declared = int(declared)
        except (TypeError, ValueError):
            findings.append(Finding(name, None, declared, kind,
                                     f"components.{name}.port is not an integer: {declared!r}"))
            continue

        if kind not in _VALID_KINDS:
            findings.append(Finding(name, declared, None, kind,
                                     f"ports.{name}.kind must be one of {sorted(_VALID_KINDS)}, got {kind!r}"))
            continue

        path = repo_root / file_rel if file_rel else None

        if kind == "const":
            findings.extend(_check_const(name, declared, spec, path, file_rel, port_map))
        elif kind == "gap":
            findings.extend(_check_gap(name, declared, spec, path, file_rel, port_map))
        elif kind == "no-listener":
            findings.extend(_check_no_listener(name, declared, path, file_rel))

    return findings


def _check_const(
    name: str, declared: int, spec: dict, path: Path | None, file_rel: str,
    port_map: dict[str, dict[str, object]],
) -> list[Finding]:
    findings: list[Finding] = []
    if declared == 0:
        findings.append(Finding(name, declared, None, "const",
                                 f"kind = \"const\" requires a non-zero declared port, got 0"))
    pattern = spec.get("pattern")
    if not pattern:
        findings.append(Finding(name, declared, None, "const",
                                 f"ports.{name}: kind = \"const\" requires a 'pattern' key"))
        return findings
    if path is None or not path.exists():
        findings.append(Finding(name, declared, None, "const",
                                 f"source file not found: {file_rel}"))
        return findings
    text = path.read_text()
    matches = re.findall(pattern, text, re.MULTILINE)
    if len(matches) != 1:
        findings.append(Finding(name, declared, matches, "const",
                                 f"pattern {pattern!r} matched {len(matches)} time(s) in {file_rel} "
                                 f"(expected exactly 1)"))
        return findings
    try:
        found_port = int(matches[0])
    except ValueError:
        findings.append(Finding(name, declared, matches[0], "const",
                                 f"pattern {pattern!r} captured a non-integer {matches[0]!r} in {file_rel}"))
        return findings

    # Question 217(g): a `kind = "const"` row is checked THREE ways -- the fragment's own
    # declared port, the source constant this pattern just extracted, and the owned port map
    # in docs/architecture.md's "### Default ports" table. All three must agree.
    exempt = bool(spec.get("owned_map_exempt", False))
    map_entry = port_map.get(name)
    if map_entry is None:
        if not exempt:
            findings.append(Finding(
                name, declared, found_port, "const",
                f"components.{name} (kind = \"const\") has no row in the owned port map "
                f"(docs/architecture.md '### Default ports', question 217(g)) -- no owned "
                f"provenance for its port at all (fragment declares {declared}, source "
                f"constant {file_rel} says {found_port})",
            ))
        # Exempt (or not) -- the fragment-vs-source agreement still has to hold on its own.
        if found_port != declared:
            findings.append(Finding(name, declared, found_port, "const",
                                     f"declared port {declared} != source port {found_port} in {file_rel}"))
        return findings

    map_grpc = map_entry.get("grpc")
    try:
        map_port = int(map_grpc) if map_grpc is not None else None
    except (TypeError, ValueError):
        map_port = None

    if map_port is None or not (declared == found_port == map_port):
        findings.append(Finding(
            name, declared, found_port, "const",
            f"three-way port mismatch for {name}: fragment declares {declared}, "
            f"owned port map (docs/architecture.md '### Default ports') says {map_port!r}, "
            f"source constant {file_rel} says {found_port}",
        ))
    return findings


def _check_gap(
    name: str, declared: int, spec: dict, path: Path | None, file_rel: str,
    port_map: dict[str, dict[str, object]],
) -> list[Finding]:
    findings: list[Finding] = []
    if declared != 0:
        findings.append(Finding(name, declared, None, "gap",
                                 f"kind = \"gap\" requires declared port 0, got {declared}"))
    if path is None or not path.exists():
        findings.append(Finding(name, declared, None, "gap",
                                 f"source file not found: {file_rel}"))
        return findings
    text = path.read_text()
    flag = spec.get("flag", "")
    if flag and flag not in text:
        findings.append(Finding(name, declared, None, "gap",
                                 f"expected marker {flag!r} (the required-flag proof this is a real gap, "
                                 f"not just an unset field) not found in {file_rel} — gap provenance stale"))
    # The positive check: the day this file starts declaring a real default bind, the "gap" row
    # is out of date and must become a "const" row with a real port (question 208(c)). This is
    # deliberately a POSITIVE assertion (the pattern must NOT match), not just "port is 0" —
    # see tests/test_suite_declarations.py::test_a_new_default_bind_closes_the_gap.
    default_match = _DEFAULT_BIND_RE.search(text)
    if default_match:
        findings.append(Finding(name, declared, default_match.group(0), "gap",
                                 f"{file_rel} now appears to declare a default bind "
                                 f"({default_match.group(0)!r}) — the documented gap (question 208(c)) "
                                 f"looks closed; update suite.altavista.toml's port and change kind to "
                                 f"\"const\" with a real pattern"))
    # The OTHER positive check (question 217(g)): the day the OWNED PORT MAP itself gains a row
    # for this component -- independent of whether the source file has grown a default bind --
    # the gap is closed too and must become a "const" row that the three-way check above covers.
    if name in port_map:
        findings.append(Finding(
            name, declared, port_map[name], "gap",
            f"{name} now has a row in the owned port map (docs/architecture.md '### Default "
            f"ports', question 217(g)) -- the documented gap looks closed; update "
            f"suite.altavista.toml's port and change kind to \"const\" with a real pattern",
        ))
    return findings


def _check_no_listener(name: str, declared: int, path: Path | None, file_rel: str) -> list[Finding]:
    findings: list[Finding] = []
    if declared != 0:
        findings.append(Finding(name, declared, None, "no-listener",
                                 f"kind = \"no-listener\" requires declared port 0, got {declared}"))
    if path is None or not path.exists():
        findings.append(Finding(name, declared, None, "no-listener",
                                 f"source file not found: {file_rel}"))
        return findings
    text = path.read_text()
    listener_match = _LISTENER_RE.search(text)
    if listener_match:
        findings.append(Finding(name, declared, listener_match.group(0), "no-listener",
                                 f"{file_rel} now appears to declare a listener "
                                 f"({listener_match.group(0)!r}) — the \"no-listener\" claim "
                                 f"looks wrong; update suite.altavista.toml's kind/port for {name}"))
    return findings
