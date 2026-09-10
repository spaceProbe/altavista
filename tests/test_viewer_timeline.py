"""M25.3d (docs/sil-plan.md's M25 milestone: "telemetry into the viewer"): the timeline
shows contact windows and command-state transitions distinctly, and the run products
panel shows command acknowledgements as far as the real data lets it.

Same "run the real code, don't port it" discipline as tests/test_viewer_panels.py /
tests/test_viewer_viewport.py: this file builds a real ``altavista.v1.RunProducts``
directly from the generated Python bindings (never a hand-rolled byte layout --
question 121's own "no AVRUN1 framing" rule, and ``tests/test_cdm_run.py``'s own
``_build_run_products``/synthetic-FAULT-bundle precedent for exactly this situation: no
existing DRM fixture produces ``EVENT_KIND_CONTACT_START``/``_END``/
``EVENT_KIND_COMMAND_TRANSITION`` events together in one run, and building one for real
would need ``crates/av-kernel`` built -- a Rust crate another worker is actively editing
concurrently, so this task deliberately never invokes ``cargo`` at all, per its own
brief), POSTs it through the real ``POST /api/cdm/run`` route
(``altavista.server.create_app`` + ``fastapi.testclient.TestClient``), reads the
published scenario back exactly as a browser would, and hands it to
``web/js/timeline_check.mjs``, which drives the real, shipped
``web/js/timeline_events.js`` and reports one JSON object of named checks.

**Every event's `detail`/`name`/`entity_id` below is written to match, verbatim, the
real `format!` calls in the Rust source this mirrors** (read, not modified -- this
worker owns ``web/``/``tests/``/``altavista/`` only, never ``crates/``):

* ``crates/av-kernel/src/drm/events.rs::contact_event``:
  ``detail = format!("ground instance {:?}: {} (target {:?})", cmd.instance, name, sender)``,
  ``entity_id = cmd.instance`` (the ground/receiving side), ``name`` is
  ``"contact_start"``/``"contact_end"``.
* ``crates/av-kernel/src/drm/command.rs::transition_event``:
  ``detail = format!("command {:?} ({}): {} -> {}", cmd.id, cmd.command_class, state.as_str_name(), reason)``,
  ``entity_id = cmd.instance`` (the command's target), ``name = state.as_str_name()``
  (the real ``CommandState`` name, e.g. ``"COMMAND_STATE_ACKED"``), and -- on the
  ``ACKED`` transition only -- ``provenance.attributes["ack_level"] =
  ack_level.as_str_name()`` (``"ACK_LEVEL_ASSET_EXECUTED"``, per that module's own
  ``acked_event`` doc comment: never merely ``AssetReceived``).

Station/instance/command-id names (``ground_alpha``/``ground_beta``/``demo_flt``/
``demo_other``/``cmd1``/``cmd2``/``cmd3``) are chosen to match the style of
``drms/demo_ground_segment.*`` (station ``ground``, spacecraft ``flight``) and
``drms/demo_ground_command.drm.yaml`` (command ``cmd1``, instance ``demo_flt``, field
``Cd``, class ``drag_sail``, dispatcher ``demo_ground``) -- not copied byte for byte
(this fixture needs THREE stations/commands to build the pairing/grouping traps below,
those DRMs declare one each), but drawn from the same real DRM vocabulary rather than
placeholder names invented from nothing.

## The load-bearing traps (this task's own "name the wrong implementation it would fail
against" requirement)

* **Contact pairing must key on (station, counterpart), not station alone.** The fixture
  has a matched window at ``ground_alpha``/``demo_flt`` AND a concurrent, unmatched
  ``contact_end`` at the SAME station ``ground_alpha`` but a DIFFERENT counterpart
  (``demo_other``). An implementation that pairs by station alone would wrongly close
  the real window with the wrong end -- ``web/js/timeline_check.mjs``'s own "TRAP --
  window end is the demo_flt end, not cross-paired" check fails against exactly that bug
  (see this task's own worked demonstration in ``web/js/REPORT_M25_3d.md``: a deliberate
  wrong edit, shown failing, then restored).
* **Command grouping must key on the command id (recovered from `detail`), not the
  target instance.** ``cmd1`` and ``cmd2`` both target instance ``demo_flt``; an
  implementation that groups transitions by ``spacecraft``/``entity_id`` instead of the
  parsed command id would wrongly merge cmd2's one ``PROPOSED`` transition into cmd1's
  five-transition sequence -- ``web/js/timeline_check.mjs``'s own "TRAP -- cmd2 ... not
  merged into cmd1" check fails against exactly that bug (also demonstrated in
  ``web/js/REPORT_M25_3d.md``).

## Work item 2's finding, proven here (not just asserted in prose)

``ack_level`` genuinely never reaches the viewer: it is written to
``Event.provenance.attributes["ack_level"]`` on the real CDM event below (mirroring
``command.rs::acked_event``/``transition_event`` exactly), but
``altavista.cdm.cdm_event_to_viewer_event`` never reads ``ev.provenance`` at all (see
that function's own source) -- so the published scenario's ACKED event carries no trace
of the ack level anywhere, not in a structured field and not in ``detail``'s free text
either (``transition_event``'s own `format!` for `detail` never mentions ``ack_level``).
``test_acked_transition_reaches_the_viewer_with_no_trace_of_ack_level`` below confirms
this by inspecting the REAL published JSON directly -- not by assumption.
"""
from __future__ import annotations

import json
import shutil
import subprocess
import tempfile
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from altavista import cdm as cdm_adapter
from altavista.pb import core_pb2, trajectory_pb2
from altavista.pb.altavista.v1 import run_pb2
from altavista.server import create_app

REPO_ROOT = Path(__file__).resolve().parent.parent
TIMELINE_CHECK = REPO_ROOT / "web" / "js" / "timeline_check.mjs"
NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip("node is not installed in this environment; web/js/timeline_check.mjs "
                     "drives real ES modules and is intentionally not ported to Python.")
    return NODE


# TAI epoch base: the same real start_tai_ns drms/demo_ground_segment.drm.yaml /
# drms/demo_ground_command.drm.yaml declare (question 123's own "reuse the real demo
# epoch, don't invent one" convention), so every epoch below is a plain, documented
# offset from a real DRM's own start rather than an arbitrary number.
START_TAI_NS = 1767225637000000000
NS_PER_S = 1_000_000_000


def _provenance(**attributes: str) -> core_pb2.Provenance:
    return core_pb2.Provenance(tool="tests/test_viewer_timeline.py (synthetic, mirrors "
                                     "crates/av-kernel/src/drm/{events,command}.rs verbatim)",
                                config_hash="timeline-fixture-hash", run_id="timeline-fixture-run",
                                attributes=attributes)


def _contact_event(*, station: str, counterpart: str, is_start: bool, offset_s: float) -> trajectory_pb2.Event:
    """Mirrors crates/av-kernel/src/drm/events.rs::contact_event's own `format!` verbatim."""
    name = "contact_start" if is_start else "contact_end"
    kind = trajectory_pb2.EVENT_KIND_CONTACT_START if is_start else trajectory_pb2.EVENT_KIND_CONTACT_END
    tai_ns = START_TAI_NS + int(offset_s * NS_PER_S)
    return trajectory_pb2.Event(
        id=f"contact:{station}:{tai_ns}:{name}", entity_id=station, tai_ns=tai_ns, kind=kind, name=name,
        detail=f'ground instance "{station}": {name} (target "{counterpart}")',
        provenance=_provenance(instance=station, sender=counterpart),
    )


def _decode_error_event(*, instance: str, port: str, is_start: bool, offset_s: float,
                        first_offset_s: float | None = None, first_error: str = "",
                        frames_affected: int = 0, resumed: bool = True) -> trajectory_pb2.Event:
    """Mirrors crates/av-kernel/src/drm/events.rs::decode_error_start_event/
    decode_error_end_event's own `format!` calls verbatim (question 193, R6.2). `id`/
    `reference_id`/`name`/`kind` follow those builders exactly: `kind` is
    EVENT_KIND_FAULT for BOTH (a decode-error event is distinguished from every other
    fault-shaped event by `name`, never `kind` -- see events.rs's own module doc
    comment's "Naming" section), `reference_id` is the receiving port,
    `id = f"{name}:{instance}:{port}:{first_tai_ns}"` (keyed by the EPISODE's own
    first_tai_ns on both the start and its own end, so start/end share the pairing
    key but never the same id -- the `name` prefix differs)."""
    name = "decode_error_start" if is_start else "decode_error_end"
    tai_ns = START_TAI_NS + int(offset_s * NS_PER_S)
    first_tai_ns = START_TAI_NS + int((first_offset_s if first_offset_s is not None else offset_s) * NS_PER_S)
    if is_start:
        detail = f'instance "{instance}" port "{port}": undecodable frame -- {first_error}'
        attrs = {"instance": instance, "port": port, "codec_error": first_error}
    elif resumed:
        detail = f'instance "{instance}" port "{port}": decoding resumed after {frames_affected} undecodable frame(s)'
        attrs = {"instance": instance, "port": port}
    else:
        detail = (f'instance "{instance}" port "{port}": the run ended with {frames_affected} '
                  f'undecodable frame(s) since tai_ns={first_tai_ns} still unresolved -- decoding never resumed')
        attrs = {"instance": instance, "port": port}
    return trajectory_pb2.Event(
        id=f"{name}:{instance}:{port}:{first_tai_ns}", entity_id=instance, tai_ns=tai_ns,
        kind=trajectory_pb2.EVENT_KIND_FAULT, name=name, detail=detail,
        reference_id=port, provenance=_provenance(**attrs),
    )


def _command_transition(*, command_id: str, instance: str, command_class: str, state_name: str,
                        principal: str, reason: str, offset_s: float, ack_level: str = "") -> trajectory_pb2.Event:
    """Mirrors crates/av-kernel/src/drm/command.rs::transition_event's own `format!`
    verbatim, including (only on ACKED) `provenance.attributes["ack_level"]` -- the one
    real field the viewer never receives (Work item 2's own finding)."""
    tai_ns = START_TAI_NS + int(offset_s * NS_PER_S)
    attrs = {"principal": principal, "reason": reason, "command_class": command_class, "field": "value"}
    if ack_level:
        attrs["ack_level"] = ack_level
    return trajectory_pb2.Event(
        id=f"command_transition:{command_id}:{state_name}", entity_id=instance, tai_ns=tai_ns,
        kind=trajectory_pb2.EVENT_KIND_COMMAND_TRANSITION, name=state_name,
        detail=f'command "{command_id}" ({command_class}): {state_name} -> {reason}',
        values={"value": 220.0}, reference_id=command_id,
        provenance=_provenance(**attrs),
    )


@pytest.fixture(scope="module")
def fixture_events() -> list[trajectory_pb2.Event]:
    events = []

    # ---- contact windows: one matched (ground_alpha<->demo_flt), two deliberately not.
    events.append(_contact_event(station="ground_alpha", counterpart="demo_flt", is_start=True, offset_s=50))
    events.append(_contact_event(station="ground_beta", counterpart="demo_flt", is_start=True, offset_s=100))  # never closed
    events.append(_contact_event(station="ground_alpha", counterpart="demo_other", is_start=False, offset_s=200))  # no start
    events.append(_contact_event(station="ground_alpha", counterpart="demo_flt", is_start=False, offset_s=795))

    # ---- decode-error episode windows (question 193, R6.2): one matched
    # (controller/startracker_in), two deliberately not -- the SAME shape as the
    # contact-window trap above, applied to the decode-error pairing key
    # (spacecraft/entity_id + referenceId/port, both real fields -- no detail parsing
    # needed here, unlike contact's counterpart). An unclosed start on a DIFFERENT
    # PORT of the SAME instance (controller/imu_in) proves pairing keys on port, not
    # spacecraft alone; an orphan end on a THIRD port of the SAME instance
    # (controller/cmd_in), landing chronologically BETWEEN the real window's own start
    # and end, is the load-bearing trap: an implementation that paired by spacecraft
    # alone would wrongly close the real startracker_in episode with THIS end instead
    # of its own real, later one.
    events.append(_decode_error_event(instance="controller", port="startracker_in", is_start=True, offset_s=50,
                                      first_error="apid 511 names no declared PacketCodec"))
    events.append(_decode_error_event(instance="controller", port="imu_in", is_start=True, offset_s=100,
                                      first_error="packet is 3 byte(s), shorter than the 6-byte primary header"))  # never closed
    events.append(_decode_error_event(instance="controller", port="cmd_in", is_start=False, offset_s=200,
                                      first_offset_s=190, frames_affected=4))  # no start
    events.append(_decode_error_event(instance="controller", port="startracker_in", is_start=False, offset_s=795,
                                      first_offset_s=50, frames_affected=300, resumed=True))

    # ---- cmd1: full real state machine on demo_flt (drms/demo_ground_command.drm.yaml's
    # own vocabulary: field Cd, class drag_sail, dispatcher demo_ground).
    events.append(_command_transition(command_id="cmd1", instance="demo_flt", command_class="drag_sail",
                                      state_name="COMMAND_STATE_PROPOSED", principal="mission-planning",
                                      reason="declared in Scenario.events", offset_s=0))
    events.append(_command_transition(command_id="cmd1", instance="demo_flt", command_class="drag_sail",
                                      state_name="COMMAND_STATE_CHECKED", principal="sil-validator",
                                      reason="structurally valid: target instance and writable field both resolve",
                                      offset_s=0.000000001))
    events.append(_command_transition(command_id="cmd1", instance="demo_flt", command_class="drag_sail",
                                      state_name="COMMAND_STATE_AUTHORIZED", principal="sil-auto-authority",
                                      reason="SIL auto-authorization: no external authority configured for this run",
                                      offset_s=0.000000002))
    events.append(_command_transition(command_id="cmd1", instance="demo_flt", command_class="drag_sail",
                                      state_name="COMMAND_STATE_DISPATCHED", principal="ground-segment",
                                      reason='CCSDS telecommand framed and handed to the router from "demo_ground"',
                                      offset_s=6207.4))
    events.append(_command_transition(command_id="cmd1", instance="demo_flt", command_class="drag_sail",
                                      state_name="COMMAND_STATE_ACKED", principal="demo_flt",
                                      reason="flight software's own telemetry acknowledged execution",
                                      offset_s=6207.5, ack_level="ACK_LEVEL_ASSET_EXECUTED"))

    # ---- cmd2: SAME instance (demo_flt) as cmd1, but a DIFFERENT command -- the
    # "grouped by command id, not by instance" trap. Only ever reaches PROPOSED.
    events.append(_command_transition(command_id="cmd2", instance="demo_flt", command_class="generic",
                                      state_name="COMMAND_STATE_PROPOSED", principal="mission-planning",
                                      reason="declared in Scenario.events", offset_s=10))

    # ---- cmd3: a different instance, REJECTED instead of ACKED (a different real
    # terminal CommandState, never paraphrased).
    events.append(_command_transition(command_id="cmd3", instance="demo_ground2", command_class="generic",
                                      state_name="COMMAND_STATE_PROPOSED", principal="mission-planning",
                                      reason="declared in Scenario.events", offset_s=0))
    events.append(_command_transition(command_id="cmd3", instance="demo_ground2", command_class="generic",
                                      state_name="COMMAND_STATE_CHECKED", principal="sil-validator",
                                      reason="structurally valid: target instance and writable field both resolve",
                                      offset_s=0.000000001))
    events.append(_command_transition(command_id="cmd3", instance="demo_ground2", command_class="generic",
                                      state_name="COMMAND_STATE_AUTHORIZED", principal="sil-auto-authority",
                                      reason="SIL auto-authorization: no external authority configured for this run",
                                      offset_s=0.000000002))
    events.append(_command_transition(command_id="cmd3", instance="demo_ground2", command_class="generic",
                                      state_name="COMMAND_STATE_DISPATCHED", principal="ground-segment",
                                      reason='CCSDS telecommand framed and handed to the router from "demo_ground"',
                                      offset_s=300))
    events.append(_command_transition(command_id="cmd3", instance="demo_ground2", command_class="generic",
                                      state_name="COMMAND_STATE_REJECTED", principal="demo_ground2",
                                      reason="target field validation failed", offset_s=301))

    # ---- one unrelated kind (FAULT), to prove it is unaffected by this task's changes
    # (crates/av-kernel/src/drm/events.rs::fault_event's own `format!`, mirrored).
    fault_tai_ns = START_TAI_NS + 400 * NS_PER_S
    events.append(trajectory_pb2.Event(
        id="fault:fault1", entity_id="demo_flt", tai_ns=fault_tai_ns, kind=trajectory_pb2.EVENT_KIND_FAULT,
        name="fault1", detail='FAULT_TARGET_KIND_DYNAMICS fault "fault1" applied: target="force_model.gravity_order", kind=Parameter',
        values={"value": 0.0}, reference_id="fault1", provenance=_provenance(),
    ))
    return events


@pytest.fixture(scope="module")
def published_scenario(fixture_events: list[trajectory_pb2.Event]) -> dict:
    """POSTs the synthetic-but-protocol-honest RunProducts through the real
    POST /api/cdm/run route and reads the published scenario back -- the exact JSON a
    browser's loadScenario() (web/js/app.js) receives. One trajectory (zero samples,
    the same minimal shape tests/test_cdm_run.py's own JSON-transcoding test already
    proves the server accepts) is enough: the server converts trajectories/events
    independently (altavista/server.py's publish_cdm_run reads run_products.events on
    its own, never cross-validated against which entities have a trajectory)."""
    traj = trajectory_pb2.Trajectory(id="demo_flt-traj", entity_id="demo_flt", frame_id="EarthMJ2000Eq")
    provenance = core_pb2.Provenance(config_hash="timeline-fixture-hash", run_id="timeline-fixture-run")
    rp = run_pb2.RunProducts(run_id="timeline-fixture-run", events=fixture_events, provenance=provenance)
    rp.trajectories["demo_flt"].CopyFrom(traj)

    with tempfile.TemporaryDirectory() as d:
        app = create_app(texture_dir=Path(d), web_dir=Path(d))
        client = TestClient(app)
        resp = client.post("/api/cdm/run", content=rp.SerializeToString(),
                           headers={"content-type": "application/x-protobuf"})
        assert resp.status_code == 200, resp.text
        name = resp.json()["name"]
        resp = client.get(f"/api/scenario/{name}")
        assert resp.status_code == 200, resp.text
        return resp.json()


# ============================================== M25.3e (question 177): the gap, closed
# M25.3d's own `test_acked_transition_reaches_the_viewer_with_no_trace_of_ack_level`
# proved (by inspecting the real published JSON) that `ack_level` genuinely did not
# reach the viewer: `Event.to_dict()`'s wire shape was exactly `{name, t, type,
# spacecraft, detail}`, and that test's own docstring said finding a 6th/7th key there
# -- or an ACK_LEVEL_* substring in `detail` -- "is exactly the signal to come back and
# wire up the client-side display for real." That signal fired (`altavista/model.py`'s
# `Event` gained `reference_id`/`attributes`, question 177's own decision) and this test
# is its replacement: it proves the ACKED transition's real `referenceId` (the command
# id, `cmd1`) and real `ack_level` (inside `attributes`) now reach the exact same
# published JSON, and that a NON-ACKED transition on the same command still carries no
# `ack_level` at all (never fabricated for a state that has none).
def test_acked_transition_now_carries_the_real_reference_id_and_ack_level(published_scenario):
    events = published_scenario["events"]
    transitions = [e for e in events if e["type"] == "command_transition"]
    acked = [e for e in transitions if e["name"] == "COMMAND_STATE_ACKED"]
    assert len(acked) == 1, f"expected exactly one ACKED transition, got {acked}"
    ev = acked[0]
    assert set(ev.keys()) == {"name", "t", "type", "spacecraft", "detail", "referenceId", "attributes"}, \
        f"Event.to_dict's own wire shape changed -- re-check this test's own premise: {sorted(ev.keys())}"
    assert ev["referenceId"] == "cmd1", \
        "the ACKED transition's referenceId must be the real Command.id (cmd1), not empty/None/a different id"
    assert ev["attributes"].get("ack_level") == "ACK_LEVEL_ASSET_EXECUTED", \
        f"the ACKED transition's attributes must carry the real ack_level: {ev['attributes']!r}"
    # Still real, still honest: detail's own free text never mentioned ack_level either
    # way (command.rs::transition_event's own format! never includes it) -- this half of
    # the old test's premise is unchanged.
    assert "ACK_LEVEL" not in ev["detail"]
    assert "COMMAND_STATE_ACKED" in ev["detail"]
    assert "flight software" in ev["detail"]

    proposed_cmd1 = next(e for e in transitions if e["referenceId"] == "cmd1" and e["name"] == "COMMAND_STATE_PROPOSED")
    assert "ack_level" not in proposed_cmd1["attributes"], \
        "a non-ACKED transition must not carry a fabricated ack_level"

    cmd3_rejected = next(e for e in transitions if e["referenceId"] == "cmd3" and e["name"] == "COMMAND_STATE_REJECTED")
    assert "ack_level" not in cmd3_rejected["attributes"], \
        "a command that never reaches ACKED (cmd3, REJECTED) must not carry a fabricated ack_level either"


def test_every_command_transition_carries_its_real_reference_id(published_scenario):
    """The core of question 177's own "grouped by referenceId instead of parsing text"
    requirement, checked at the wire level directly: every command_transition event's
    referenceId is the real Command.id this fixture built it with (cmd1/cmd2/cmd3), and
    a fault event's own referenceId (a different real field, `reference_id="fault1"` in
    this fixture) is unaffected -- referenceId is a generic Event field, not something
    special-cased per event type.

    Question 193 (R6.2): `type == "fault"` is no longer unique to `fault1` alone --
    decode_error_start/_end events (added by this same round's fixture) also carry
    `type == "fault"` (EVENT_KIND_FAULT, `events.rs`'s own "Naming" doc section: a
    decode-error event is distinguished from a plain fault by `name`, never `kind`),
    so this test now selects `fault1` by `name` too, rather than assuming it is the
    only `type == "fault"` event in the scenario.
    """
    events = published_scenario["events"]
    for ev in events:
        if ev["type"] == "command_transition":
            assert ev["referenceId"] in ("cmd1", "cmd2", "cmd3"), ev
    fault = next(e for e in events if e["type"] == "fault" and e["name"] == "fault1")
    assert fault["referenceId"] == "fault1"


# ======================================================================== headless harness
@pytest.fixture(scope="module")
def timeline_check_input_path(tmp_path_factory, published_scenario, fixture_events):
    """Builds the one JSON file web/js/timeline_check.mjs reads: the real published
    scenario, plus the matched window's expected start/end A1MJD epochs -- converted
    with the SAME altavista.cdm.tai_ns_to_a1mjd conversion the whole publish path uses
    (mirrors tests/test_cdm_run.py's own convention of comparing published epochs this
    way), computed here directly from this file's own declared offset_s=50/795, never
    read back off the JS side first.
    """
    payload = {
        "scenario": published_scenario,
        "expectedWindowStartT": cdm_adapter.tai_ns_to_a1mjd(START_TAI_NS + 50 * NS_PER_S),
        "expectedWindowEndT": cdm_adapter.tai_ns_to_a1mjd(START_TAI_NS + 795 * NS_PER_S),
        # Question 193 (R6.2): the decode-error episode's own matched window -- SAME
        # offset_s=50/795 as the contact window above (deliberately reused, not a
        # coincidence: both fixtures' matched windows share the same start/end so this
        # file states one pair of epochs, not two, while still proving the two pairing
        # functions never cross-pair each other's events -- section 2/2b of
        # timeline_check.mjs each run against the full, combined scenario.events).
        "expectedDecodeErrorWindowStartT": cdm_adapter.tai_ns_to_a1mjd(START_TAI_NS + 50 * NS_PER_S),
        "expectedDecodeErrorWindowEndT": cdm_adapter.tai_ns_to_a1mjd(START_TAI_NS + 795 * NS_PER_S),
    }
    d = tmp_path_factory.mktemp("timeline_check")
    path = d / "timeline_input.json"
    path.write_text(json.dumps(payload))
    return path


@pytest.fixture(scope="module")
def timeline_data(timeline_check_input_path) -> dict:
    node = _require_node()
    proc = subprocess.run([node, str(TIMELINE_CHECK), str(timeline_check_input_path)],
                          cwd=str(TIMELINE_CHECK.parent), capture_output=True, text=True, timeout=30)
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"timeline_check.mjs did not print valid JSON (exit {proc.returncode})\n"
                             f"stdout: {proc.stdout!r}\nstderr: {proc.stderr}")
    return data


def _failed(data: dict, substring: str) -> list[str]:
    return [c["name"] for c in data["checks"] if substring in c["name"] and not c["pass"]]


def _matched(data: dict, substring: str) -> list[dict]:
    matches = [c for c in data["checks"] if substring in c["name"]]
    assert matches, f"no checks matched substring {substring!r} -- timeline_check.mjs's check names changed?"
    return matches


def test_real_published_scenario_carries_all_three_new_event_kinds(timeline_data):
    """Background section's own required confirmation: contact_start/contact_end/
    command_transition all reach sc.events on a REAL published scenario, by inspection.
    """
    failed = _failed(timeline_data, 'real published scenario:')
    assert not failed, f"real-published-event-kind checks failed: {failed}"


def test_reference_id_and_attributes_reach_the_viewer(timeline_data):
    """Question 177's own required test at the headless-harness level: every
    command_transition event on the real published scenario carries a real, non-empty
    referenceId and an attributes object, the ACKED transition's attributes carries the
    real ack_level, and a non-ACKED transition's attributes never fabricates one.
    """
    failed = _failed(timeline_data, 'referenceId/attributes:')
    assert not failed, f"referenceId/attributes checks failed: {failed}"


def test_contact_windows_paired_correctly_never_cross_paired(timeline_data):
    """Work item 1's own required test: a contact_start/contact_end pair for the SAME
    station/counterpart renders as one spanned window with its duration; an unmatched
    start or end is reported unmatched, never dropped, never paired with the wrong
    partner. Fails against an implementation that pairs by station alone (the TRAP
    check) or that drops/silently-merges an unmatched event.
    """
    failed = _failed(timeline_data, 'pairContactWindows:')
    assert not failed, f"contact-window pairing checks failed: {failed}"


def test_decode_error_windows_paired_correctly_never_cross_paired(timeline_data):
    """Question 193's (R6.2) own required test, mirroring `test_contact_windows_paired_
    correctly_never_cross_paired` exactly: a decode_error_start/decode_error_end pair
    for the SAME (instance, port) renders as one spanned window with its duration; an
    unmatched start or end is reported unmatched, never dropped, never paired with the
    wrong partner. Fails against an implementation that pairs by instance/spacecraft
    alone, ignoring the port (the TRAP check), or that drops/silently-merges an
    unmatched event.
    """
    failed = _failed(timeline_data, 'pairDecodeErrorWindows:')
    assert not failed, f"decode-error-window pairing checks failed: {failed}"


def test_command_transitions_grouped_by_command_with_real_state_names(timeline_data):
    """Work item 1/2's own required test: command transitions are grouped by command
    (never by target instance -- the TRAP check) and every transition's state is the
    real CommandState name, including a non-ACKED terminal state (REJECTED). ackLevel is
    proven null (Work item 2's own honest-gap contract -- fails against a fabricated
    value).
    """
    failed = _failed(timeline_data, 'groupCommandTransitions:')
    assert not failed, f"command-transition grouping checks failed: {failed}"


def test_timeline_tick_plan_renders_contacts_and_commands_distinctly_without_regressing_other_kinds(timeline_data):
    """Work item 1's own required test: a command_transition tick is visually
    (className) and textually (real state name) distinct from a contact window and
    from every other event kind; contact events are never double-rendered as a leftover
    point tick; an unrelated kind (fault) renders exactly as before.
    """
    failed = _failed(timeline_data, 'timelineTickPlan:')
    assert not failed, f"timeline tick plan checks failed: {failed}"


def test_detail_parsing_edge_cases(timeline_data):
    """parseContactCounterpart/parseCommandId never guess -- null for text that does not
    match the real Rust format!, and correctly unescape a Rust {:?}-escaped id."""
    failed = _failed(timeline_data, 'parseContactCounterpart:') + _failed(timeline_data, 'parseCommandId:')
    assert not failed, f"detail-parsing edge case checks failed: {failed}"


def test_timeline_check_report(timeline_data, capsys):
    """Prints the full named-check table (per this task's incremental-reporting
    requirement) -- not a correctness assertion on its own, the individual test
    functions above are."""
    with capsys.disabled():
        print(f"\ntimeline_check.mjs: {len(timeline_data['checks'])} checks, allPass={timeline_data['allPass']}")
        for c in timeline_data["checks"]:
            mark = "PASS" if c["pass"] else "FAIL"
            print(f"  [{mark}] {c['name']}")
    assert timeline_data["allPass"] is True, \
        "timeline_check.mjs reported at least one failing check -- see the printed table above (-s)"
