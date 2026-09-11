# Roadmap status

As of 2026-09-11, against the phase table in `architecture.md`. Milestones are team 1's
(`teamlog/2026-09-02-team-1.md`) and the feasibility track's (`feasibility-plan.md`);
decisions are numbered in `open-questions.md` (1–199).

## By phase

| Phase | Exit criterion (architecture.md) | Status |
|---|---|---|
| P0 CDM, contract, ADRs, FFI spike | tests and proto lint green; altavista trajectory as a CDM message; `GetDerivatives` from Rust | **Complete.** ADRs 000–005 accepted with amendments; CDM v1 (`altavista.v1`, 9 proto files incl. run, packet, lockstep); `gmat-sys` FFI with derivatives, STM, drag/SRP, convert; goldens against GMAT for every dynamics path. |
| P1 Design and feasibility | four examples on a tiled globe and in ICRF with no jitter; kernel with GMAT and one non-space model; sweeps with MoEs in ClickHouse | **Design side complete; feasibility side complete except the store.** DRM authoring in YAML with typed loader and hashing; GMAT design-time service (Python) and Rust dynamics service; viewer modules 1–3 (frame graph, floating origin, tiled globe) plus the UI rework (tiling, multi-viewport, panels); kernel runs GMAT, native attitude, sensors, wheels, controller and ground-station models in one shared run with router, faults, maneuvers with the Gates model, covariance, scoring, events. First demo (question 5) closed: DRM → GMAT dynamics → globe and ICRF, reproducible from its hash. Feasibility F1–F5 (`av-sweep`): a sweep declared in YAML and hashed, grid and dispersed draws with seeds derived per sample, samples run in parallel processes with the Gates model sampled, per-point aggregates, results written through a store trait (file-backed), `altavista.feasibility` authoring and the `/api/cdm/sweep` route, a feasibility panel with a study default layout, and a worked drag-sail study (`studies/drag-sail-vs-burn.md`) reproducible from its hashes. **Not started:** the ClickHouse implementation of the store trait; a job runner beyond one host's processes. |
| P2 SIL | same binary in container and Renode gives identical port traffic; replay bit-identical; p99 budget measured | **Container half complete; Renode half partial; edge and spoore engine not started.** Lockstep protocol, shim, Docker lifecycle; cFS on OSAL-posix closes the attitude loop in a container, byte-identical at the port boundary; RTEMS 6.1 toolchain and lockstep BSP in a container; cFS boots to OPERATIONAL on Renode's Zynq UltraScale+ RPU; bridge live with one byte-exact step; **port traffic from step 2 onward does not deliver (question 171, no-go for now)**, so identical traffic is verified posix-only. Ground segment, telecommands through the real command state machine, the GMAT-side consume, telemetry as CDM measurements into the viewer, the port traffic sidecar and bit-identical replay with a replay binding are all in (M25 complete). **Not started:** edge boundary (seccert certificates, per-batch signatures, labels on ingest); spoore engine consuming simulated measurements; p99 latency budget. |
| P3 HIL and heavy track | board-in-the-loop replay; 10 GB overlay streams; native dynamics within tolerance of GMAT | **Not started.** Board binding refused at load by design; ZCU102/104 chosen (question 144). Object store, tiler, streaming layers, catalog, claim-check: none. Native Rust orbital dynamics: none (all space dynamics still through the GMAT FFI, which is the validated path). |
| P4 AI plane and command path | a model proposes, a human authorizes, a simulated asset acts, replay reproduces the trail | **Command path partial; AI plane not started.** The command state machine (PROPOSED → CHECKED → AUTHORIZED → DISPATCHED → ACKED) runs end to end in simulation with transitions as events (M25.2). No OPA policy, no human-authorization step, no sidecar isolation, no read-only gateway, no evidence log for model proposals. |
| P5 Air-gap kit, SBOMs, accreditation | zero-egress install; 3-node target | **Partial groundwork only.** Control matrices in the suite's format for the two dynamics services, hash-chained evidence log, `cargo deny` bans, FIPS rules (system OpenSSL only, SHA-256), no-network-at-test rule with one-time build fetches recorded by hash. No air-gap kit, no SBOM generation, no multi-node work. |

## Counts

| | |
|---|---|
| Rust tests | 282 fast-crate (incl. 89 in av-sweep) + 874 kernel (1 ignored: question 171's reproducer) |
| Python tests | 493 |
| Viewer headless checks | 381 named checks across 8 harnesses (`web/js/**/*_check.mjs`; four more harnesses emit measurement tables the Python tests assert on and have no named checks) |
| Decisions logged | 199 questions; ADRs 000–005 with 8 amendments (ADR-001 ×1, ADR-002 ×5, ADR-003 ×1, ADR-005 ×1) |
| Milestones accepted | M1–M26, team 2 rounds 4–7 (fault runtimes, decode errors, reproducible image, leftovers), feasibility F1–F5 merged |

## Open items and risks

- **Question 171**: Renode port traffic beyond the first step. Transport excluded as the cause; next step is a debugger attach at the guest. Blocks the "same binary, container and emulator" claim only.
- **Feasibility store**: results go through a trait with a file-backed implementation; the ClickHouse implementation waits for a host that has it.
- **spoore upstream**: the three changes question 75 asked of spoore are on its `altavista-upstream` branch (`spoore-upstream.md`), ready to submit as PRs.
- **Edge ingestion** (pillar 1) and the **AI plane** (pillar 4) have no code yet.
- **Redpanda licence review** (question 6) remains with the user.
- **Question 150**: the GMAT covariance-rotation report is with the user for upstream submission.
- **Version control**: under git since 2026-09-07 (`develop` pushed; `main` is the user's). A clean clone builds and passes from the README's steps.
- **Host friction**: the shell hook's path jail, macOS first-launch assessment of fresh binaries (question 172), and Renode's amd64-only Docker image are all recorded with remedies.

## Next, in the order the lead would take it

1. The edge boundary (P2's missing half), the AI plane and command authorization (P4), or HIL on the ZCU102/104 (P3): a product-priority choice for the user.
2. Renode port traffic (question 171): a debugger attach during a stalled second step.
3. Submit the spoore PRs and the GMAT covariance report (questions 75 and 150, with the user).
