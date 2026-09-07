# ADR-000: Scope, lineage and the decision ledger

- **Status:** Accepted (drafted 2026-09-02; accepted by the user 2026-09-02)
- **Date:** 2026-09-02
- **Plan reference:** [architecture.md](../architecture.md) sections 1–3 and 9; [open-questions.md](../open-questions.md) questions 1–6
- **Implemented by:** the repository layout (`proto/`, `crates/`, `services/`, `viewer/`, `plugins/`, `drms/`, `docs/adr`)
- **Depends on:** spoore ADR-000 through ADR-006 (adopted, see below)

## Context

gmatviz proved one slice: a mission built in Python with GMAT, shown in shared browsers. The
stated goal is much larger: one configurable infrastructure for multi-domain mission
**design**, **feasibility**, **analysis** and **execution**, with secure ingestion at the edge,
a dual-track processing engine, a unified Earth-and-orbit 3D viewer, an isolated AI
ecosystem that can propose commands, design reference missions that reconfigure into
software- and hardware-in-the-loop runs with the same dynamics, and natural-language design
as a first-class way of working.

Two existing bodies of work bear on it. **spoore** (`~/code/spoore`) has already decided how
to represent dynamic systems for estimation and how to carry them on a deterministic,
replayable, air-gap-first substrate; its decisions are measured and recorded. **GMAT
R2026a** ships validated force models, integrators, coordinate systems, ephemerides and time
systems under Apache 2.0, with a Python API whose `ODEModel.GetDerivatives` lets an external
integrator drive its force models directly.

The platform's own scope was resolved through 71 explicit forks on 2026-09-02, logged with
answers in [open-questions.md](../open-questions.md). This ADR records the framing decisions
those answers imply; ADR-001 through ADR-004 record the technical ones for P0.

## Decision

**1. One platform, four profiles, one representation.** Design, feasibility, analysis and
execution are *profiles* of one system. A profile selects components (plugins, engines,
services, viewer layers, labels, authority policy); it never changes model behaviour or
numerical settings. Anything that affects a result is declared in a design reference
mission or a system definition and hashed (question 11).

**2. spoore is a dependency, and its CDM is a compatible subset of ours.** spoore's crates
enter by git/path dependency; `altavista.v1` keeps every `spoore.v0` message shape and field
number it reuses, adds fields and messages, and never redefines a shared field's meaning
without a recorded conversion. Changes spoore needs go upstream as pull requests
(question 2). spoore ADR-000 (Gaussian rule), 001 (comparison currency), 002 (factorized
hypothesis space), 004 (determinism), 005 (heavy models as logged evidence), 006 (taxonomy as
data) and 009–011 are adopted unchanged and cited, not restated.

**3. GMAT is validated dynamics, not a process the platform depends on.** GMAT's models enter
behind an engine-neutral dynamics contract at three depths (ADR-002); the platform never
requires a running GMAT for anything but design-time solvers and golden generation.

**4. The kernel is the engine.** The simulation kernel described in the architecture is
built; exporting the dynamics library to other engines (C ABI, gRPC, FMU) is a hedge, not a
driver (question 1).

**5. Open core with proprietary plugins.** The core (CDM, kernel, engine wrappers, viewer,
services, deployment definitions) is Apache 2.0 and carries no copyleft or BSL code; edge
plugins, security modules and customer DRMs may be private. Operational dependencies with
other licenses (Redpanda BSL, MinIO AGPL) run unmodified and are never linked into the open
core (question 6).

**6. Name and languages.** The project is AltaVista, internal only; crates are `av-*`, the
proto package is `altavista.v1`; gmatviz remains the name of the Python GMAT lineage. Rust
for the kernel, engines and services; TypeScript for the viewer; Python for authoring,
GMAT hosting and model authoring (questions 3, 4).

**7. The decision ledger.** [open-questions.md](../open-questions.md) is this ADR's appendix.
Each later ADR cites the questions it resolves by number. New forks are appended there with
the same shape (fork, default, what the answer changes) before an ADR is written for them.

**8. The first demonstration is the design profile**: a DRM authored in Python, propagated
with GMAT dynamics, shown on the custom Three.js globe and in ICRF, reproducible from its
configuration hash (question 5). P1's exit criteria are written to it.

## Alternatives considered

**A fork of spoore, changed freely.** Fastest now; rejected because every improvement to the
tracker (spoore is still evolving on its own branches) would have to be ported by hand, and
the CDM would diverge silently. The dependency-plus-superset rule keeps one estimator core.

**Merging spoore into one workspace.** Cleanest build; rejected because spoore is a product
with its own papers, goldens and licensing measurements, and its ADR discipline depends on
its goldens staying pinned in its own tree.

**GMAT as the simulation engine.** GMAT can propagate, target and optimize, and gmatviz drove
it in real time. Rejected as the *engine* because it holds one configuration per process,
cannot bind flight software or hardware, and its solvers do not cover the other domains.
It is the reference implementation of space dynamics instead (ADR-002).

**Separate products per profile.** A design tool, an analysis tool, an operations tool.
Rejected because the design-to-execution continuity is the point: the entity designed on
Monday is the entity tracked on Friday, with its planned trajectory as the prior, and that
needs one representation and one substrate.

**A fully proprietary codebase.** Rejected by the user in favour of open core; the
consequence is the boundary discipline below.

## Consequences

**The open/closed boundary is a build artifact.** Crates and services in the open core must
not depend on anything under `plugins/private` or a customer DRM; CI enforces it with a
dependency check, and the SBOM per component (ADR-004) makes the boundary auditable.

**spoore must accept upstream changes.** The TAI time scale under `epoch_ns` (ADR-001), the
rskafka client (ADR-003) and the space subtree (ADR-002) are spoore PRs. Until they land, the
platform pins spoore at a commit and carries the changes as patches in `crates/av-*`.

**Profiles cannot be used to tune.** A team that wants a faster or looser run must change
the DRM or system definition, which changes the hash. This is deliberate: the same design
gives the same numbers in every profile, which is what makes a feasibility verdict and a
HIL run comparable.

**The ledger grows.** Every new fork is appended to open-questions.md before it is decided
in an ADR. This is cheap and it is what kept the first 71 honest.

## What would falsify this

The premise is that one representation and one substrate can serve design, feasibility,
analysis and execution without the profiles diverging into separate products.

The observable: a profile that needs a change to a *message* or to *numerical behaviour*
that the others cannot carry. If the execution profile needs a belief representation the
design profile cannot produce, or the feasibility profile needs a trajectory format the
kernel cannot replay, rule 1 has broken and the platform is two products sharing a name.
The first place this would show is the CDM changelog under strict compatibility (ADR-001):
a breaking change requested by one profile alone.

## References

- spoore, `docs/adr/000-cdm-rule.md` through `011-sequential-expansion.md`, and
  `docs/system-review.md` (2026-08-17).
- GMAT R2026a, `License.txt` (Apache 2.0) and `api/Ex_R2020a_BasicForceModel.py`
  (`ODEModel.GetDerivatives`).
- [architecture.md](../architecture.md), [open-questions.md](../open-questions.md).
