"""Round 4 manager review, defect 1: `scripts/kit/d5_three_placements.py`'s three placements
used to bind `edge-b`/`edge-c` onto `127.0.0.1:50061`/`50161` and `127.0.0.1:50062`/`50162` --
which are not free ports at all, but `gmat-service`'s and `av-dynamics-service`'s own OWNED
gRPC/admin defaults (`docs/architecture.md`'s "### Default ports" table, question 208(c)/
217(g)). It only ever worked because neither service happened to be running at the same time
this script's own containers were -- exactly the kind of silent drift the three-way owned-port-
map check (question 217(g), `deploy/secdeploy/ports.py::check_ports`) exists to catch for the
secdeploy fragment, but this script sits OUTSIDE that fragment (it is not a `[ports.<name>]`
row -- D5 is a demonstration runner, not a shipped component with its own owned default) so
nothing there was ever going to catch it.

This file is that same guard, applied here: it parses the REAL owned port map with
`deploy/secdeploy/ports.py::load_owned_port_map` (never a reimplementation of that parser) and
asserts, against `scripts/kit/d5_three_placements.py`'s own real, committed `PLACEMENTS` list:

1. `edge-a`'s ports are exactly `av-ingest`'s owned row -- positively, so this placement can
   never silently drift off the very default it exists to demonstrate.
2. Every OTHER placement port (`edge-b`/`edge-c`'s gRPC and admin ports) is absent from every
   row of the owned map -- so the day the map grows a `50065`/`50066` row (or is edited to
   reuse any of D5's chosen ports for any reason), this test fails and forces D5 to move,
   instead of D5 quietly re-squatting on a real service's default the way it did before this
   fix.

Round 4's fix moved `edge-b`/`edge-c` to `50063`/`50163` and `50064`/`50164`. The aiplane
merge (this round) landed the heavy track's real owned default for `av-proposer`,
`127.0.0.1:50063` (`docs/architecture.md` "### Default ports", question 219(a)), which this
test then caught as a genuine collision with `edge-b`'s gRPC port -- exactly the regression
class this file exists to catch, this time from a row the owned map *grew* rather than from a
row D5 never checked. `edge-b`/`edge-c` moved again, to `50065`/`50165` and `50066`/`50166`.
"""
from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
ARCHITECTURE_MD = REPO_ROOT / "docs" / "architecture.md"

sys.path.insert(0, str(REPO_ROOT / "deploy" / "secdeploy"))
import ports  # noqa: E402  (path insert must precede this import)

sys.path.insert(0, str(REPO_ROOT / "scripts" / "kit"))
import d5_three_placements  # noqa: E402  (path insert must precede this import)


def _owned_map() -> dict[str, dict[str, object]]:
    return ports.load_owned_port_map(ARCHITECTURE_MD)


def _placement(label: str) -> dict:
    for p in d5_three_placements.PLACEMENTS:
        if p["label"] == label:
            return p
    raise AssertionError(
        f"fixture assumption: scripts/kit/d5_three_placements.py's own PLACEMENTS has no "
        f"{label!r} entry -- PLACEMENTS: {d5_three_placements.PLACEMENTS}"
    )


def test_fixture_assumption_three_placements_exist():
    """If this is wrong, every other test below would either vacuously pass or fail for the
    wrong reason -- checked loudly, up front."""
    labels = [p["label"] for p in d5_three_placements.PLACEMENTS]
    assert labels == ["edge-a", "edge-b", "edge-c"], d5_three_placements.PLACEMENTS


def test_edge_a_ports_are_av_ingests_own_owned_default():
    """Positive assertion (question 217(g)'s own shape): `edge-a` must be bound to EXACTLY
    `av-ingest`'s owned row in `docs/architecture.md`'s "### Default ports" table -- not just
    "not colliding with anything," since `edge-a` exists specifically to demonstrate that real
    default. A future edit that quietly moved `edge-a` off it would otherwise pass every other
    check in this file and still be wrong."""
    owned = _owned_map()
    assert "av-ingest" in owned, (
        f"fixture assumption: the owned port map has no av-ingest row at all: {sorted(owned)}"
    )
    edge_a = _placement("edge-a")
    assert edge_a["grpc_port"] == owned["av-ingest"]["grpc"], (
        f"edge-a's grpc_port ({edge_a['grpc_port']}) no longer matches av-ingest's owned "
        f"default ({owned['av-ingest']['grpc']!r}) in {ARCHITECTURE_MD} -- edge-a exists to "
        f"demonstrate that exact default; if av-ingest's default really moved, edge-a must "
        f"move with it, deliberately, not silently."
    )
    assert edge_a["admin_port"] == owned["av-ingest"]["admin"], (
        f"edge-a's admin_port ({edge_a['admin_port']}) no longer matches av-ingest's owned "
        f"admin default ({owned['av-ingest']['admin']!r}) in {ARCHITECTURE_MD}"
    )


def test_edge_b_and_edge_c_ports_collide_with_no_owned_map_row():
    """The actual round 4 regression, pinned directly: `edge-b`/`edge-c` must sit on ports that
    belong to NO row of the owned port map at all -- gRPC or admin, any service -- not merely
    "the two rows this script used to squat on" (`gmat-service`'s 50061/50161 and
    `av-dynamics-service`'s 50062/50162). A docker container simply not running right now is
    not the same as a port being free; this is checked against the real, parsed map, not
    against which services this test happens to have running."""
    owned = _owned_map()
    owned_ports: set[int] = set()
    for service, row in owned.items():
        for kind in ("grpc", "admin"):
            value = row.get(kind)
            if value is not None:
                owned_ports.add(int(value))
    assert owned_ports, f"fixture assumption: the owned port map parsed with zero real ports: {owned}"

    for label in ("edge-b", "edge-c"):
        placement = _placement(label)
        for kind in ("grpc_port", "admin_port"):
            port = placement[kind]
            assert port not in owned_ports, (
                f"{label}'s {kind} ({port}) collides with a row of the owned port map "
                f"(docs/architecture.md '### Default ports', question 208(c)/217(g)) -- "
                f"owned ports: {sorted(owned_ports)}. scripts/kit/d5_three_placements.py must "
                f"move this placement to a port that is verified free against the real map, "
                f"never merely assumed free because nothing happens to be listening on it "
                f"right now (round 4 defect 1's own root cause)."
            )


def test_edge_a_is_the_only_placement_reusing_an_owned_port():
    """The other half of the same guard, stated as a single coverage claim: exactly one of the
    three placements (`edge-a`) is allowed to sit on an owned-map port at all, and it must be
    av-ingest's own row (proven by the two tests above) -- never `edge-b`/`edge-c`, and never a
    fourth placement added later without updating this test too."""
    owned = _owned_map()
    owned_ports: set[int] = set()
    for row in owned.values():
        for kind in ("grpc", "admin"):
            value = row.get(kind)
            if value is not None:
                owned_ports.add(int(value))

    on_owned_map = [
        p["label"] for p in d5_three_placements.PLACEMENTS
        if p["grpc_port"] in owned_ports or p["admin_port"] in owned_ports
    ]
    assert on_owned_map == ["edge-a"], (
        f"expected exactly ['edge-a'] to sit on an owned-map port, got {on_owned_map} -- "
        f"scripts/kit/d5_three_placements.py's own PLACEMENTS: {d5_three_placements.PLACEMENTS}"
    )
