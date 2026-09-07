"""Runtime configuration and model constants for gmat-service.

Kept separate from :mod:`gmat_service.model` (which owns the live GMAT object graph) so
these values are importable -- by the CLI, the servicer and tests -- without touching
``gmatpy``.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path

from altavista.cdm import DEFAULT_STATE_SPACE_ID

# --------------------------------------------------------------------------- server
DEFAULT_PORT = 50061
# +100 from DEFAULT_PORT, mirroring `crates/av-dynamics-service/src/bin/server.rs`'s own
# +100 offset from ITS DEFAULT_PORT (50062 -> 50162) -- localhost-only `/admin/api/evidence`
# (ADR-004 question 63), never the same port as the gRPC service.
DEFAULT_ADMIN_PORT = 50161
SERVICE_DIR = Path(__file__).resolve().parent.parent  # services/gmat-service
DEFAULT_EVIDENCE_PATH = SERVICE_DIR / "evidence.jsonl"

# --------------------------------------------------------------------------- model identity
# Matches the task's own example id: JGM2 8x8 gravity + Sun/Moon point masses, no drag, no
# SRP -- exactly the golden's force model (goldens/leo_1day_jgm2_8x8_sunmoon.json).
MODEL_ID = "gmat.earth.jgm2_8x8.sun_moon"
# GMAT release, not a semver -- matches the golden's own recorded "gmat_version". This is
# the number that actually determines the physics; there is no independent "model version"
# to report separately.
GMAT_VERSION = "R2026a"
# Reused verbatim from altavista.cdm rather than inventing a second id for the same
# [x, y, z, vx, vy, vz] SI shape.
STATE_SPACE_ID = DEFAULT_STATE_SPACE_ID
# This service hosts exactly one GMAT CoordinateSystem ("EarthMJ2000Eq") and does not run
# a frame registry/service of its own; frame_id is that coordinate system's own name (the
# same "no separate id given -> use the frame name" convention altavista.cdm.
# frame_definition_for already follows).
FRAME_ID = "EarthMJ2000Eq"
GOLDEN_NAME = "leo_1day_jgm2_8x8_sunmoon"

# Reference epoch the Derivatives engine is built at (see model.py); GetDerivatives(state,
# dt=D) shifts the ephemeris evaluation point by D seconds from this epoch, so the exact
# value does not affect accuracy, only that it is fixed and known. Reuses the same A1MJD
# constant tests/test_cdm_v1.py's frame tests already use as "a representative epoch"
# (21545.0 = 01 Jan 2000, 12:00:00-ish A.1) rather than inventing a new one.
DERIVATIVES_BASE_EPOCH_A1MJD = 21545.0

# Force model / integrator settings. MUST match goldens/gen_leo_1day.py exactly: this is
# the model `ModelInfo.goldens` names as its pin, and tests/test_gmat_service.py checks
# Propagate against that golden directly.
SETTINGS = {
    "model_id": MODEL_ID,
    "gmat_version": GMAT_VERSION,
    "central_body": "Earth",
    "gravity": {"file": "JGM2.cof", "degree": 8, "order": 8},
    "point_masses": ["Luna", "Sun"],
    "drag": None,
    "srp": False,
    "integrator": "PrinceDormand78",
    "accuracy": 1e-13,
    "min_step_s": 0.0,
    "max_step_s": 600.0,
    "initial_step_s": 60.0,
}

# Question 82 (docs/open-questions.md, docs/adr/002-dynamics-contract.md's third amendment):
# GMAT's RelativisticCorrection::GetDerivatives fills its A-matrix/STM contribution with an
# unconditional zero (a stub, not a physically-absent term -- third_party/gmat-src/src/base/
# forcemodel/RelativisticCorrection.cpp, both the fillSTM and fillAMatrix branches write a
# zeroed buffer), so the platform declares the STM capability absent for any force model that
# includes it. This service's SETTINGS above never adds a RelativisticCorrection force (its
# force model is JGM2 8x8 + Sun/Moon point masses only, matching goldens/
# leo_1day_jgm2_8x8_sunmoon.json), so this is always False today; kept as a named constant
# (rather than an inline False scattered across model.py) so a future SETTINGS that does add
# the force flips exactly one place, and model.GmatModel.propagate_covariance /
# service._capabilities() both read it instead of hard-coding the current answer. Escalation
# note: actually *building* a RelativisticCorrection force through altavista.scenario.Scenario.
# force_model() needs a change to altavista/scenario.py (worker O's file, not this task's), so
# this constant documents the intended wiring without a way to flip it on today.
HAS_RELATIVISTIC_CORRECTION = False


def settings_hash() -> str:
    """SHA-256 hex of the canonical (sorted-key) JSON encoding of :data:`SETTINGS`.

    ``ModelInfo.settings_hash`` and every evidence-log line's ``settings_hash`` both call
    this, so a change to the force model or integrator settings is visible as a hash change
    everywhere it is recorded.
    """
    body = json.dumps(SETTINGS, sort_keys=True).encode("utf-8")
    return hashlib.sha256(body).hexdigest()
