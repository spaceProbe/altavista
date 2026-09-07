"""GMAT's ``Propagator.Step(dt)`` can fail silently unless its return value is checked.

Found in lead review of team 1 (2026-09-02). ``RungeKutta::Step(Real dt)`` loops substeps
until ``dt`` is consumed but returns ``False`` once more than ``MaxStepAttempts`` (default 50,
counting rejected attempts) have been taken, with the state only partly advanced. A caller
that ignores the boolean gets a plausible-looking wrong trajectory: 3600 s chunks at
``MaxStep = 300`` diverged by 11,000 km over one day. Raising ``MaxStepAttempts`` or keeping
``dt`` to a few ``MaxStep`` makes the same chunks exact. Every stepping loop in this repo must
check the return value; this test pins the behaviour so the trap stays documented.
"""
import sys
from pathlib import Path

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from altavista.gmat_env import load_gmat  # noqa: E402

T = 86400.0
_counter = [0]


def _propagator(g, max_step, attempts):
    _counter[0] += 1
    n = _counter[0]
    sat = g.Construct("Spacecraft", f"StepRet{n}")
    sat.SetField("DateFormat", "UTCGregorian"); sat.SetField("Epoch", "01 Jan 2026 00:00:00.000")
    sat.SetField("CoordinateSystem", "EarthMJ2000Eq"); sat.SetField("DisplayStateType", "Keplerian")
    sat.SetField("SMA", 6878.0); sat.SetField("ECC", 0.001); sat.SetField("INC", 51.6); sat.SetField("RAAN", 30.0)
    fm = g.Construct("ForceModel", f"StepRetFM{n}"); fm.SetField("CentralBody", "Earth")
    grav = g.Construct("GravityField"); grav.SetField("BodyName", "Earth"); grav.SetField("PotentialFile", "JGM2.cof")
    grav.SetField("Degree", 4); grav.SetField("Order", 4)
    fm.AddForce(grav)
    prop = g.Construct("Propagator", f"StepRetP{n}"); gator = g.Construct("PrinceDormand78", f"StepRetG{n}")
    prop.SetReference(gator); prop.SetReference(fm)
    prop.SetField("InitialStepSize", 60.0); prop.SetField("Accuracy", 1e-13); prop.SetField("MinStep", 0.0)
    prop.SetField("MaxStep", max_step); prop.SetField("MaxStepAttempts", attempts)
    g.Initialize(); prop.AddPropObject(sat); prop.PrepareInternals()
    return prop.GetPropagator()


def _run(gator, chunk):
    elapsed, falses = 0.0, 0
    while elapsed < T - 1e-9:
        dt = min(chunk, T - elapsed)
        if not gator.Step(dt):
            falses += 1
        elapsed += dt
    return np.array(list(gator.GetState())[:6]), falses


def test_step_returns_false_and_under_propagates_when_attempts_are_exhausted():
    g = load_gmat()
    ref, f0 = _run(_propagator(g, 300.0, 50), 60.0)
    assert f0 == 0
    bad, f1 = _run(_propagator(g, 300.0, 50), 3600.0)
    assert f1 == 24, "every 3600 s chunk at MaxStep 300 should exhaust the default 50 attempts"
    assert np.linalg.norm(bad[:3] - ref[:3]) > 1000.0, "the silent failure is a gross error, not a rounding one"
    good, f2 = _run(_propagator(g, 300.0, 1000), 3600.0)
    assert f2 == 0
    assert np.linalg.norm(good[:3] - ref[:3]) * 1e3 < 1e-3, "with attempts raised the same chunks are exact"
