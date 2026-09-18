"""Generate `twobody_analytic.json`: the N1 golden that needs no GMAT (docs/native-dynamics-
plan.md milestone N1; docs/adr/002-dynamics-contract.md, "Goldens").

Run explicitly, never from a test:
  .venv/bin/python goldens/gen_twobody_analytic.py --reason "..." --tolerance-m ... --tolerance-mps ...

WHY THIS GOLDEN IS PROVED AGAINST THE CLOSED-FORM KEPLER SOLUTION, NOT GMAT
-----------------------------------------------------------------------------
ADR-002's "Goldens" section requires every native model to be pinned against a GMAT reference
arc with a generator script and a recorded reason (docs/adr/002-dynamics-contract.md). This is
the one documented exception the N1 plan (docs/native-dynamics-plan.md, milestone N1) itself
calls for: a pure two-body arc (spherical-harmonic degree/order (0, 0), i.e. point-mass gravity
only) has an EXACT closed-form solution -- Kepler's equation, solved here by a Newton iteration
to machine precision, with the resulting eccentric anomaly converted to a Cartesian state by the
standard classical-elements-to-Cartesian transformation (Vallado, *Fundamentals of Astrodynamics
and Applications*, 4th ed., the COE2RV algorithm; the identical reference `docs/adr/002-
dynamics-contract.md`'s own References section already cites for relative motion). This golden
checks the native model's point-mass term and the shared `av_dynamics::integrate::Dopri5`
integrator against that first-principles analytic reference -- GMAT is not needed to validate
Kepler's equation, and running GMAT for a case this crate can already prove in closed form would
add a dependency (and a data-pack/version pin) this one golden does not need.

This is NOT docs/adr/002-dynamics-contract.md's fifth amendment ("one recorded exception" to
"goldens against GMAT"). That amendment covers a COVARIANCE golden in a ROTATING frame, proved
against a closed-form rotation built from GMAT's OWN reported R/Rdot, specifically because
GMAT's own `OrbitErrorCovariance` report was measured to omit the Rdot term (a GMAT
*implementation gap* the exception works around, while still anchoring to GMAT's own R/Rdot
numbers). This golden shares none of that: it never reads anything GMAT produced at all. It is
an ANALYTIC reference (Kepler's equation and the classical-to-Cartesian transform), not a GMAT
reference with a documented gap -- said here plainly, not folded quietly into "the same kind of
exception." Every OTHER native force this plan adds (spherical harmonics beyond degree 0, third
bodies, drag, SRP) still gets its golden against GMAT, per ADR-002's rule verbatim -- see this
crate's own N1 report for why this one golden, and only this one, is different.
"""
import argparse
import datetime
import hashlib
import json
import math
import sys
from pathlib import Path

# Earth's mu, SI (m^3/s^2), exactly the value `crates/av-orbital/src/cof.rs`'s own module doc
# records as verified directly from GMAT's `JGM2.cof` POTFIELD record ("mu and the reference
# radius are already SI in the file: 3.98600441500000e+14 ... m^3/s^2"). Hardcoded here (not
# read from the .cof file) so this generator touches no GMAT install file at all, matching this
# golden's own "no GMAT dependency" premise -- the Rust acceptance test builds its model from
# the real JGM2.cof file at degree/order (0, 0) and will therefore use the identical mu, so this
# constant is a repeated fact, not an independently chosen one.
MU_EARTH_SI = 3.986004415e14


def solve_kepler(mean_anomaly, e, tol=1e-14, max_iter=100):
    """Newton iteration for E - e*sin(E) = M, to machine precision. Standard well-guarded
    initial guess (E0 = M for low e; M + sign(sin M)*0.85*e for higher e, Vallado's own
    recommended starting point) so convergence is fast and uniform across e in [0, 0.9]."""
    m = math.remainder(mean_anomaly, 2.0 * math.pi)  # wrap to [-pi, pi] for a well-scaled start
    if e < 0.8:
        E = m
    else:
        E = m + math.copysign(0.85 * e, math.sin(m)) if math.sin(m) != 0.0 else m + 0.85 * e
    for _ in range(max_iter):
        f = E - e * math.sin(E) - m
        fp = 1.0 - e * math.cos(E)
        dE = f / fp
        E -= dE
        if abs(dE) < tol:
            break
    else:
        raise RuntimeError(f"Kepler solver did not converge: M={mean_anomaly}, e={e}, last |dE|={abs(dE)}")
    # Un-wrap back onto the same branch as the original (un-wrapped) mean anomaly, so E and the
    # caller's M stay on a consistent, monotonically-increasing footing across many periods.
    return E + (mean_anomaly - m)


def true_anomaly_from_eccentric(E, e):
    return 2.0 * math.atan2(math.sqrt(1.0 + e) * math.sin(E / 2.0), math.sqrt(1.0 - e) * math.cos(E / 2.0))


def eccentric_from_true_anomaly(nu, e):
    return 2.0 * math.atan2(math.sqrt(1.0 - e) * math.sin(nu / 2.0), math.sqrt(1.0 + e) * math.cos(nu / 2.0))


def coe_to_cartesian(a, e, i, raan, argp, nu, mu):
    """Classical orbital elements -> Cartesian state, SI (m, m/s). Vallado's COE2RV: build the
    state in the perifocal (PQW) frame, then rotate PQW -> IJK with the standard 3-1-3 Euler
    sequence transformation matrix (elements written out directly, not composed from three
    intermediate rotation matrices, to avoid a matrix-multiply transcription bug)."""
    p = a * (1.0 - e * e)
    r_mag = p / (1.0 + e * math.cos(nu))
    r_pqw = (r_mag * math.cos(nu), r_mag * math.sin(nu), 0.0)
    root_mu_p = math.sqrt(mu / p)
    v_pqw = (-root_mu_p * math.sin(nu), root_mu_p * (e + math.cos(nu)), 0.0)

    cO, sO = math.cos(raan), math.sin(raan)
    ci, si = math.cos(i), math.sin(i)
    co, so = math.cos(argp), math.sin(argp)
    # R = R3(-raan) . R1(-i) . R3(-argp), written out element-by-element (Vallado eq. 2-25 et
    # seq.); r_ijk = R . r_pqw, v_ijk = R . v_pqw (r_pqw/v_pqw's own z component is always 0).
    r11, r12 = cO * co - sO * so * ci, -cO * so - sO * co * ci
    r21, r22 = sO * co + cO * so * ci, -sO * so + cO * co * ci
    r31, r32 = so * si, co * si

    def rot(v):
        return (r11 * v[0] + r12 * v[1], r21 * v[0] + r22 * v[1], r31 * v[0] + r32 * v[1])

    return list(rot(r_pqw)), list(rot(v_pqw))


def propagate_kepler(a, e, i, raan, argp, nu0, mu, dt_s):
    """Exact two-body propagation by dt_s seconds: elements -> mean anomaly -> advance linearly
    -> solve Kepler's equation for the new eccentric anomaly -> back to elements -> Cartesian.
    a/i/raan/argp/e are constant for an unperturbed two-body orbit; only the anomaly moves."""
    E0 = eccentric_from_true_anomaly(nu0, e)
    M0 = E0 - e * math.sin(E0)
    n = math.sqrt(mu / a**3)
    M1 = M0 + n * dt_s
    E1 = solve_kepler(M1, e)
    nu1 = true_anomaly_from_eccentric(E1, e)
    return coe_to_cartesian(a, e, i, raan, argp, nu1, mu)


# -- Self-tests: prove the Kepler solver and the element<->Cartesian conversion independently,
#    before either is trusted to build the golden (this generator's own "test it independently"
#    requirement). Run unconditionally on every invocation, not gated behind a test framework,
#    so a broken generator can never silently emit a wrong golden.
def _self_test():
    # Kepler solver: for a spread of (M, e), the returned E must satisfy the defining equation
    # to within a few ULPs, and e=0 must return E == M exactly (a circular orbit's "anomaly" is
    # its own mean anomaly, no iteration needed in principle, though the Newton loop still runs).
    max_residual = 0.0
    for e in (0.0, 0.001, 0.3, 0.6, 0.8, 0.9):
        for m_deg in range(-720, 721, 15):
            m = math.radians(m_deg)
            E = solve_kepler(m, e)
            residual = abs(E - e * math.sin(E) - m)
            max_residual = max(max_residual, residual)
    assert max_residual < 1e-12, f"Kepler solver residual too large: {max_residual:e}"
    E0 = solve_kepler(0.0, 0.5)
    assert abs(E0 - 0.0) < 1e-14, f"E(M=0) must be 0 for any e, got {E0}"

    # True anomaly <-> eccentric anomaly round trip.
    for e in (0.0, 0.3, 0.6, 0.9):
        for nu_deg in range(-170, 171, 10):
            nu = math.radians(nu_deg)
            E = eccentric_from_true_anomaly(nu, e)
            nu_back = true_anomaly_from_eccentric(E, e)
            assert abs(math.remainder(nu_back - nu, 2 * math.pi)) < 1e-12, f"nu round trip failed: {nu} -> {E} -> {nu_back}"

    # coe_to_cartesian: four hand-computed special cases at e=0 (so r_mag is exactly a and
    # v_mag is exactly sqrt(mu/a), no root-finding involved) that specifically pin the SIGN of
    # the raan/argp rotation -- the energy/|h|/|e|/inclination-from-h_z checks below are all
    # invariant to a sign flip of raan or argp (a wrong-signed rotation still conserves energy
    # and angular-momentum magnitude, and h_z = |h|*cos(i) does not depend on raan/argp at all),
    # so this block is the one that would actually catch that class of bug. Each expected vector
    # was derived by hand from the R11/R12/R21/R22/R31/R32 formula above (see this task's own
    # N1 report for the by-hand derivation) -- not by running coe_to_cartesian itself.
    mu_t = MU_EARTH_SI
    a_t = 7000e3
    v_circ = math.sqrt(mu_t / a_t)
    special_cases = [
        # (raan_deg, argp_deg, inc_deg) -> (expected r, expected v), all at e=0, nu=0.
        (0.0, 0.0, 0.0, (a_t, 0.0, 0.0), (0.0, v_circ, 0.0)),
        (90.0, 0.0, 0.0, (0.0, a_t, 0.0), (-v_circ, 0.0, 0.0)),
        (0.0, 0.0, 90.0, (a_t, 0.0, 0.0), (0.0, 0.0, v_circ)),
        (0.0, 90.0, 0.0, (0.0, a_t, 0.0), (-v_circ, 0.0, 0.0)),
    ]
    max_special_case_err = 0.0
    for raan_deg, argp_deg, inc_deg, r_expect, v_expect in special_cases:
        r, v = coe_to_cartesian(a_t, 0.0, math.radians(inc_deg), math.radians(raan_deg), math.radians(argp_deg), 0.0, mu_t)
        err = max(abs(r[k] - r_expect[k]) for k in range(3)) / a_t
        err = max(err, max(abs(v[k] - v_expect[k]) for k in range(3)) / v_circ)
        max_special_case_err = max(max_special_case_err, err)
    assert max_special_case_err < 1e-12, f"hand-computed special-case check failed (raan/argp sign?): {max_special_case_err:e}"

    # coe_to_cartesian: the resulting state must satisfy vis-viva energy, the angular-momentum
    # magnitude sqrt(mu*p), and recover the same eccentricity and inclination from its own
    # angular-momentum and eccentricity vectors -- four more INDEPENDENT closed-form checks on
    # the Cartesian output (covering e>0 and a spread of raan/argp/nu the special cases above do
    # not), none of which reuse coe_to_cartesian's own arithmetic.
    mu = MU_EARTH_SI
    max_rel_energy = max_rel_h = max_rel_e = max_rel_i = 0.0
    for a in (7000e3, 20000e3, 42164e3):
        for e in (0.0, 0.2, 0.6, 0.85):
            for i_deg in (0.1, 28.5, 63.4, 98.0):
                for nu_deg in (0.0, 45.0, 137.0, 250.0):
                    i, raan, argp, nu = math.radians(i_deg), math.radians(40.0), math.radians(75.0), math.radians(nu_deg)
                    r, v = coe_to_cartesian(a, e, i, raan, argp, nu, mu)
                    r_mag = math.sqrt(sum(c * c for c in r))
                    v_mag = math.sqrt(sum(c * c for c in v))
                    energy = 0.5 * v_mag * v_mag - mu / r_mag
                    energy_expect = -mu / (2.0 * a)
                    max_rel_energy = max(max_rel_energy, abs((energy - energy_expect) / energy_expect))

                    h = (r[1] * v[2] - r[2] * v[1], r[2] * v[0] - r[0] * v[2], r[0] * v[1] - r[1] * v[0])
                    h_mag = math.sqrt(sum(c * c for c in h))
                    h_expect = math.sqrt(mu * a * (1.0 - e * e))
                    max_rel_h = max(max_rel_h, abs((h_mag - h_expect) / h_expect) if h_expect > 0 else abs(h_mag))

                    # Eccentricity vector: e_vec = (v x h)/mu - r/|r|.
                    vxh = (v[1] * h[2] - v[2] * h[1], v[2] * h[0] - v[0] * h[2], v[0] * h[1] - v[1] * h[0])
                    e_vec = (vxh[0] / mu - r[0] / r_mag, vxh[1] / mu - r[1] / r_mag, vxh[2] / mu - r[2] / r_mag)
                    e_mag = math.sqrt(sum(c * c for c in e_vec))
                    max_rel_e = max(max_rel_e, abs(e_mag - e) / max(e, 1e-12) if e > 1e-9 else abs(e_mag))

                    cos_i_from_h = h[2] / h_mag
                    max_rel_i = max(max_rel_i, abs(cos_i_from_h - math.cos(i)))
    assert max_rel_energy < 1e-9, f"vis-viva energy check failed: {max_rel_energy:e}"
    assert max_rel_h < 1e-9, f"angular-momentum magnitude check failed: {max_rel_h:e}"
    assert max_rel_e < 1e-7, f"eccentricity-vector check failed: {max_rel_e:e}"
    assert max_rel_i < 1e-9, f"inclination-from-h check failed: {max_rel_i:e}"
    print(
        f"[gen_twobody_analytic self-test] kepler_residual={max_residual:.3e} special_case_err={max_special_case_err:.3e} "
        f"energy_rel={max_rel_energy:.3e} h_rel={max_rel_h:.3e} e_rel={max_rel_e:.3e} i_rel={max_rel_i:.3e}",
        file=sys.stderr,
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True)
    ap.add_argument("--tolerance-m", type=float, required=True, help="Measured, not asserted (this task's own rule): run the Rust acceptance test first, record the residual, then pass a value just above it.")
    ap.add_argument("--tolerance-mps", type=float, required=True)
    args = ap.parse_args()

    _self_test()

    duration_s = 86400.0
    cases = []
    # Case 1: near-circular LEO. Picked distinct from goldens/leo_1day_jgm2_8x8_sunmoon.json's
    # own elements on purpose, so this golden is not read as "the same orbit, just point mass".
    for case_name, a, e, i_deg, raan_deg, argp_deg, nu0_deg in [
        ("circular_like", 7000e3, 0.001, 45.0, 10.0, 20.0, 0.0),
        ("eccentric_e0p6", 20000e3, 0.6, 28.5, 50.0, 15.0, 0.0),
    ]:
        i, raan, argp, nu0 = math.radians(i_deg), math.radians(raan_deg), math.radians(argp_deg), math.radians(nu0_deg)
        r0, v0 = coe_to_cartesian(a, e, i, raan, argp, nu0, MU_EARTH_SI)
        r1, v1 = propagate_kepler(a, e, i, raan, argp, nu0, MU_EARTH_SI, duration_s)
        period_s = 2.0 * math.pi * math.sqrt(a**3 / MU_EARTH_SI)
        cases.append(
            {
                "case": case_name,
                "elements": {
                    "sma_m": a, "ecc": e, "inc_deg": i_deg, "raan_deg": raan_deg, "argp_deg": argp_deg, "ta0_deg": nu0_deg,
                    "period_s": period_s,
                },
                "initial_state": r0 + v0,
                "final_state": r1 + v1,
            }
        )

    golden = {
        "name": "twobody_analytic",
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "mu_m3_per_s2": MU_EARTH_SI,
        "mu_source": "crates/av-orbital/src/cof.rs module doc, verified against GMAT's JGM2.cof POTFIELD record",
        "central_body": "Earth",
        "duration_s": duration_s,
        "units": {"position": "m", "velocity": "m/s"},
        "force_model": {"central_body": "Earth", "gravity": {"degree": 0, "order": 0}, "point_masses": [], "srp": False, "drag": None},
        "propagator": {"integrator": "Dopri5 (av_dynamics::integrate)", "rtol": 1e-12, "atol": 1e-12, "initial_step_s": 30.0, "max_step_s": 600.0},
        "reference": "closed-form two-body: Kepler's equation (Newton iteration, this script's solve_kepler) + classical-elements-to-Cartesian (Vallado COE2RV, this script's coe_to_cartesian)",
        "tolerance_m": args.tolerance_m,
        "tolerance_mps": args.tolerance_mps,
        "cases": cases,
    }
    body = json.dumps(golden, indent=2, sort_keys=True)
    golden["sha256"] = hashlib.sha256(body.encode()).hexdigest()
    out = Path(__file__).with_name(golden["name"] + ".json")
    out.write_text(json.dumps(golden, indent=2, sort_keys=True) + "\n")
    print(f"wrote {out}", file=sys.stderr)


if __name__ == "__main__":
    main()
