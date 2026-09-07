"""CDM v1 (proto/altavista/v1) compiles, round-trips, and keeps spoore.v0 as a compatible subset.

Generates Python bindings with protoc into build/pb (skipped when protoc is not installed).
The spoore.v0 checks run only when ~/code/spoore (or $SPOORE_ROOT) is present.
"""
import hashlib
import importlib
import os
import re
import shutil
import subprocess
import sys
import types
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[1]
PROTO = REPO / "proto"
OUT = REPO / "build" / "pb"
SPOORE = Path(os.environ.get("SPOORE_ROOT", Path.home() / "code" / "spoore"))


def _include_dir() -> list:
    for cand in ("/opt/homebrew/include", "/usr/local/include", "/usr/include"):
        if (Path(cand) / "google" / "protobuf" / "any.proto").exists():
            return ["-I", cand]
    return []


# M26.1 (question 160): protoc's Python codegen emits absolute cross-imports rooted at the
# proto package path ("from altavista.v1 import core_pb2 as ..."). Since the platform's own
# top-level Python package is also named "altavista" (and is already imported into
# sys.modules by the time this fixture runs, pulled in transitively by other test modules),
# a bare "altavista.v1" resolves against the *real* package's own __path__, not against this
# fixture's scratch `build/pb` on sys.path -- ModuleNotFoundError, exactly the collision
# `altavista/pb/generate.py` documents and fixes for the committed bindings. This fixture
# deliberately compiles proto/ from scratch on every run (to catch drift the committed
# altavista/pb/ output would not), so it needs its own two-part fix rather than reusing the
# committed output: (1) rewrite the generated absolute cross-imports to relative ("from .
# import X as Y"), same as generate.py's `_rewrite_cross_imports_relative`, so the modules no
# longer hardcode the name "altavista"; (2) load the seven modules under a private alias
# package name instead of "altavista.v1", via hand-built namespace modules registered in
# sys.modules with the right `__path__`, so nothing here ever imports a bare top-level
# "altavista" at all -- no collision is possible regardless of what the real package is named.
_CROSS_IMPORT_RE = re.compile(r"^from altavista\.v1 import (\w+) as (\w+)$", re.MULTILINE)
_ALIAS = "_test_cdm_v1_pb"


def _rewrite_cross_imports_relative(out_dir: Path) -> None:
    for g in (out_dir / "altavista" / "v1").glob("*_pb2*.py"):
        text = g.read_text()
        new_text = _CROSS_IMPORT_RE.sub(r"from . import \1 as \2", text)
        if new_text != text:
            g.write_text(new_text)


def _load_pb2_modules(out_dir: Path, names: tuple) -> dict:
    """Import ``{name}_pb2`` for each of ``names`` from ``out_dir/altavista/v1/`` under the
    private alias `_ALIAS`.v1, never under the real top-level "altavista" name."""
    v1_dir = out_dir / "altavista" / "v1"
    pkg_root = types.ModuleType(_ALIAS)
    pkg_root.__path__ = [str(out_dir / "altavista")]
    pkg_root.__package__ = _ALIAS
    sys.modules[_ALIAS] = pkg_root
    pkg_v1 = types.ModuleType(f"{_ALIAS}.v1")
    pkg_v1.__path__ = [str(v1_dir)]
    pkg_v1.__package__ = f"{_ALIAS}.v1"
    sys.modules[f"{_ALIAS}.v1"] = pkg_v1
    return {n: importlib.import_module(f"{_ALIAS}.v1.{n}_pb2") for n in names}


@pytest.fixture(scope="module")
def pb():
    protoc = shutil.which("protoc") or "/opt/homebrew/bin/protoc"
    if not Path(protoc).exists():
        pytest.skip("protoc not installed")
    pytest.importorskip("google.protobuf")
    OUT.mkdir(parents=True, exist_ok=True)
    files = sorted(str(p) for p in (PROTO / "altavista" / "v1").glob("*.proto"))
    subprocess.run([protoc, "-I", str(PROTO), *_include_dir(), f"--python_out={OUT}", *files], check=True)
    _rewrite_cross_imports_relative(OUT)
    if (SPOORE / "proto" / "spoore" / "v0" / "cdm.proto").exists():
        subprocess.run([protoc, "-I", str(SPOORE / "proto"), *_include_dir(), f"--python_out={OUT}",
                        str(SPOORE / "proto" / "spoore" / "v0" / "cdm.proto")], check=True)
    if str(OUT) not in sys.path:
        sys.path.insert(0, str(OUT))
    mods = _load_pb2_modules(
        OUT, ("core", "envelope", "entity", "trajectory", "system", "command", "dynamics_service")
    )
    try:
        mods["spoore"] = importlib.import_module("spoore.v0.cdm_pb2")
    except ModuleNotFoundError:
        mods["spoore"] = None
    return mods


def test_spoore_v0_is_a_compatible_subset(pb):
    spoore = pb["spoore"]
    if spoore is None:
        pytest.skip("spoore checkout not found")
    core = pb["core"]
    sg = spoore.GaussianState(mean=[7.0e6, 0, 0, 0, 7500.0, 0], cov=[1.0] * 36, state_space_id="cv_3d", epoch_ns=1_700_000_000_000_000_000)
    ag = core.GaussianState.FromString(sg.SerializeToString())
    assert list(ag.mean) == list(sg.mean) and ag.state_space_id == "cv_3d" and ag.epoch_ns == sg.epoch_ns
    sb = spoore.Belief(components=[spoore.MixtureComponent(hypothesis_label="root", weight=1.0, state=sg)])
    ab = core.Belief.FromString(sb.SerializeToString())
    assert ab.components[0].hypothesis_label == "root" and ab.components[0].state.epoch_ns == sg.epoch_ns
    sm = spoore.Measurement(measurement_id="m1", z=[1, 2, 3], r=[9, 0, 0, 0, 9, 0, 0, 0, 9], epoch_ns=5,
                            sensor_id="radar_0", shard_key="cell_0_0", frame=spoore.FRAME_ECEF, meta={"src": "x"})
    am = core.Measurement.FromString(sm.SerializeToString())
    assert am.measurement_id == "m1" and am.sensor_id == "radar_0" and dict(am.meta) == {"src": "x"}
    assert am.frame_id == ""  # spoore's enum field 7 is reserved in v1; it never aliases frame_id
    si = spoore.Innovation(nu=[0.1], s=[2.0], log_likelihood=-1.5, nis=0.005)
    ai = core.Innovation.FromString(si.SerializeToString())
    assert ai.log_likelihood == -1.5 and ai.nis == 0.005
    # and the other direction: a v1 state without new fields is byte-identical to spoore's
    assert core.GaussianState(mean=[1, 2], cov=[1, 0, 0, 1], state_space_id="s", epoch_ns=3).SerializeToString() == \
        spoore.GaussianState(mean=[1, 2], cov=[1, 0, 0, 1], state_space_id="s", epoch_ns=3).SerializeToString()


def test_drm_with_ric_frame_and_mixed_bindings_round_trips(pb):
    core, system = pb["core"], pb["system"]
    ric = core.FrameDefinition(id="target_ric", entity_id="sat_target", axes=core.AXES_KIND_RIC,
                               reference_entity_id="sat_target", reference_body="Earth")
    sos = system.SosConfiguration(id="rpo_demo", version="1", instances=[
        system.SystemInstance(name="gnc", system_id="gnc", entity_id="sat_chaser",
                              binding=system.Binding(kind=system.BINDING_KIND_BOARD,
                                                     board=system.BoardBinding(edge_node_id="edge-lab-1", port_devices={"imu": "/dev/ttyUSB0@115200"}))),
        system.SystemInstance(name="adcs", system_id="adcs", step_rate_hz=50,
                              binding=system.Binding(kind=system.BINDING_KIND_RENODE,
                                                     renode=system.RenodeBinding(platform="stm32f4.repl", binary_sha256="ab" * 32))),
    ], connections=[system.Connection(from_instance="adcs", from_port="bus", to_instance="gnc", to_port="imu")])
    drm = system.DesignReferenceMission(
        id="drm_rpo_1", version="1", name="RPO approach", sos_configuration_id=sos.id,
        scenario=system.Scenario(start_tai_ns=1, end_tai_ns=86_400_000_000_000, frames=[ric], seeds={"mc": 42},
                                 faults=[system.Fault(id="f1", tai_ns=3_600_000_000_000, target_kind=system.FAULT_TARGET_KIND_PORT,
                                                      instance="adcs", target="bus", kind="delay", params={"ms": 50})]),
        objectives=[system.Objective(name="range_at_hold", expression="range(sat_chaser, sat_target) @ end",
                                     target=100, tolerance=5, unit=core.UNIT_METER)],
        options=system.DrmOptions(covariance=True, default_step_rate_hz=10, sample_interval_s=1))
    drm.hash = hashlib.sha256(drm.SerializeToString(deterministic=True)).hexdigest()
    again = system.DesignReferenceMission.FromString(drm.SerializeToString())
    assert again == drm
    assert again.scenario.frames[0].axes == core.AXES_KIND_RIC
    assert sos.instances[0].binding.WhichOneof("config") == "board"
    assert sos.instances[1].binding.WhichOneof("config") == "renode"
    # the hash is stable across serializations
    copy = system.DesignReferenceMission.FromString(drm.SerializeToString())
    copy.hash = ""
    assert hashlib.sha256(copy.SerializeToString(deterministic=True)).hexdigest() == drm.hash


def test_trajectory_signed_batch_and_command_round_trip(pb):
    core, env, traj, cmd, dyn = pb["core"], pb["envelope"], pb["trajectory"], pb["command"], pb["dynamics_service"]
    tr = traj.Trajectory(id="t1", entity_id="sat_chaser", state_space_id="rel_ric_6", frame_id="target_ric",
                         interpolation=traj.INTERPOLATION_HERMITE_VELOCITY)
    for k in range(3):
        tr.samples.add(tai_ns=k * 1_000_000_000, mean=[100.0 - k, 0, 0, -1.0, 0, 0], cov=[0.01] * 36)
    tr.segments.add(name="approach", dynamics_model="native.relative.cw", dynamics_depth="native")
    assert traj.Trajectory.FromString(tr.SerializeToString()) == tr

    meas = core.Measurement(measurement_id="m1", z=[1.0], r=[1.0], epoch_ns=5, sensor_id="s", frame_id="itrf")
    batch = env.Batch(batch_id="b1", producer_id="plugin.adsb.1", label=env.Label(marking="CUI"), sequence=7)
    bm = batch.messages.add(envelope=env.Envelope(message_id="m1", producer_id="plugin.adsb.1", event_tai_ns=5,
                                                  payload_type="altavista.v1.Measurement"))
    bm.payload.Pack(meas)
    raw = batch.SerializeToString(deterministic=True)
    prev = b"GENESIS"
    signed = env.SignedBatch(batch=raw, prev_hash=prev, hash=hashlib.sha256(prev + raw).digest(), signer_cert_sha256="00" * 32)
    back = env.SignedBatch.FromString(signed.SerializeToString())
    assert hashlib.sha256(back.prev_hash + back.batch).digest() == back.hash
    inner = env.Batch.FromString(back.batch)
    m2 = core.Measurement()
    assert inner.messages[0].payload.Unpack(m2) and m2.frame_id == "itrf"

    c = cmd.Command(id="c1", idempotency_key="k1", entity_id="sat_chaser", command_class="burn", hazardous=True,
                    deadline_tai_ns=10, state=cmd.COMMAND_STATE_PROPOSED)
    c.transitions.add(state=cmd.COMMAND_STATE_PROPOSED, tai_ns=1, principal="agent:design-assistant", reason="hold at 100 m")
    prop = cmd.CommandProposal(command=c, rationale="range rate exceeds corridor", evidence_ids=["e1"])
    assert cmd.CommandProposal.FromString(prop.SerializeToString()).command.transitions[0].principal.startswith("agent:")

    seed = core.GaussianState(mean=[7.0e6, 0, 0, 0, 7500.0, 0], cov=[1.0] * 36, state_space_id="cart_6", epoch_ns=1)
    req = dyn.PropagateRequest(model_id="gmat.earth.jgm2_8x8", seed=seed, horizon_tai_ns=86_400_000_000_000,
                               sample_interval_s=60, covariance=True)
    assert dyn.PropagateRequest.FromString(req.SerializeToString()).seed.epoch_ns == 1
