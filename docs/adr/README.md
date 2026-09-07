# Architecture decision records

Every analytical, security or structural decision in AltaVista is recorded here before it
is relied on. The format and the discipline are spoore's (`~/code/spoore/docs/adr`): an ADR
is reviewed like code, states its alternatives, records the measurement that decided it
where one exists, and names what would falsify it.

The 71 design forks resolved on 2026-09-02 are logged, with answers, in
[../open-questions.md](../open-questions.md). ADR-000 adopts that log as its appendix; the
ADRs below turn its answers into decisions with consequences.

## Contents

| ADR | Decision | Status |
|---|---|---|
| [000](000-scope-and-lineage.md) | AltaVista is one configurable platform for design, feasibility, analysis and execution; spoore is a dependency and its CDM a compatible subset; GMAT is validated dynamics, not a process; open core with proprietary plugins | Accepted 2026-09-02 |
| [001](001-cdm-v1.md) | CDM v1: TAI nanoseconds, SI metres, a frame registry with entity-relative frames, entity identity, trajectory, event, asset reference, envelope and command as first-class messages; strict compatibility from P0 | Accepted 2026-09-02 |
| [002](002-dynamics-contract.md) | One dynamics contract (`derivatives`, `step`, `propagate`, `solve`); GMAT at three depths behind it; our integrator validated against GMAT's; parity goldens in CI | Accepted 2026-09-02 |
| [003](003-substrate-and-deployment.md) | Redpanda via rskafka, ClickHouse, MinIO, a separate training store; AltaVista ships as SecRouter-suite tiers deployed by secdeploy on FIPS Fedora | Accepted 2026-09-02 |
| [004](004-security-boundary-and-evidence.md) | seccert and secsso identity; hardened units and rootless podman isolation; per-batch signatures; single handling level with labels everywhere; the suite's evidence conventions verbatim; CMMC Level 2 as the target | Accepted 2026-09-02 |
| [005](005-simulation-kernel.md) | The simulation kernel: trait-object models and a registry, integer TAI clock with a base period, interpolation by component class, ports and the lockstep binding protocol, seeded faults, a total expression language for objectives and measures of effectiveness, hashing and provenance | Accepted |
| 006 | The viewer: frame graph, configurable floating origin, tiled globe, streaming layers (P1) | Planned |
| 007 | The AI plane and command authority: secrouter, MCP tools, secagent harness, the command state machine (P4) | Planned |

## Conventions

**Numbered, immutable, superseded rather than edited.** When a decision changes, a new ADR
supersedes the old one and the old one's status becomes `Superseded by ADR-NNN`.

**Every ADR states its alternatives.** A decision with no alternatives is a default.

**Every ADR names what would falsify it.** The premise of each decision is a claim about the
world; naming the observation that would break it is what makes the ADR revisitable.

**Status `Proposed` until the user accepts.** Per question 57, the user approves and I draft.
An accepted ADR changes its status line and gains an acceptance date; nothing else in it
changes. ADR-000 through ADR-005 were accepted on 2026-09-02.

## Relationship to spoore's ADRs

spoore's ADR-000 (the CDM rule), 001 (the comparison currency), 002 (factorized hypothesis
space), 004 (determinism), 005 (heavy models as logged evidence), 006 (taxonomy as data) and
009–011 are adopted unchanged; this repo's ADRs cite them rather than restating them. Where
an AltaVista decision constrains a spoore decision (for example the time scale under
spoore's `epoch_ns`), the AltaVista ADR says so and the change goes upstream as a spoore PR.
