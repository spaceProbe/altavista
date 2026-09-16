# Roadmap status

As of 2026-09-11, against the phase table in `architecture.md`. Milestones are team 1's
(`teamlog/2026-09-02-team-1.md`) and the feasibility track's (`feasibility-plan.md`);
decisions are numbered in `open-questions.md` (1–199).

## By phase

| Phase | Exit criterion (architecture.md) | Status |
|---|---|---|
| P0 CDM, contract, ADRs, FFI spike | tests and proto lint green; altavista trajectory as a CDM message; `GetDerivatives` from Rust | **Complete.** ADRs 000–005 accepted with amendments; CDM v1 (`altavista.v1`, 9 proto files incl. run, packet, lockstep); `gmat-sys` FFI with derivatives, STM, drag/SRP, convert; goldens against GMAT for every dynamics path. |
| P1 Design and feasibility | four examples on a tiled globe and in ICRF with no jitter; kernel with GMAT and one non-space model; sweeps with MoEs in ClickHouse | **Design side complete; feasibility side complete except the store.** DRM authoring in YAML with typed loader and hashing; GMAT design-time service (Python) and Rust dynamics service; viewer modules 1–3 (frame graph, floating origin, tiled globe) plus the UI rework (tiling, multi-viewport, panels); kernel runs GMAT, native attitude, sensors, wheels, controller and ground-station models in one shared run with router, faults, maneuvers with the Gates model, covariance, scoring, events. First demo (question 5) closed: DRM → GMAT dynamics → globe and ICRF, reproducible from its hash. Feasibility F1–F5 (`av-sweep`): a sweep declared in YAML and hashed, grid and dispersed draws with seeds derived per sample, samples run in parallel processes with the Gates model sampled, per-point aggregates, results written through a store trait (file-backed), `altavista.feasibility` authoring and the `/api/cdm/sweep` route, a feasibility panel with a study default layout, and a worked drag-sail study (`studies/drag-sail-vs-burn.md`) reproducible from its hashes. **Not started:** the ClickHouse implementation of the store trait; a job runner beyond one host's processes. |
| P2 SIL | same binary in container and Renode gives identical port traffic; replay bit-identical; p99 budget measured | **Container half complete; Renode half partial; edge and spoore engine not started.** Lockstep protocol, shim, Docker lifecycle; cFS on OSAL-posix closes the attitude loop in a container, byte-identical at the port boundary; RTEMS 6.1 toolchain and lockstep BSP in a container; cFS boots to OPERATIONAL on Renode's Zynq UltraScale+ RPU; bridge live with one byte-exact step; **port traffic from step 2 onward does not deliver (question 171, no-go for now)**, so identical traffic is verified posix-only. Ground segment, telecommands through the real command state machine, the GMAT-side consume, telemetry as CDM measurements into the viewer, the port traffic sidecar and bit-identical replay with a replay binding are all in (M25 complete). **Edge boundary E1–E5 landed (2026-09-12, `edge-plan.md`):** signed, chained, labelled batches over the system OpenSSL; identity from a locally run seccert with real ACME issuance; the ingest service and its per-partition chained log as the ledger with evidence and verify endpoints; the simulated-asset plugin as a network-isolated container; the spoore engine consuming accepted measurements into tracks within 2 mm of truth; edge-to-accept p99 about 8 ms on this host, fsync-bound. E6 capture-only while disconnected with ordered replay and deduplication, the registry-driven frame adapter, `av-codec` shared by kernel and edge, the plugin's control matrix (2026-09-13): **`edge-plan.md` delivered in full.** Open: the Docker-expressible container hardening and the ADS-B replay plugin (question 207); the p99 line item was taken on a host that never went idle. |
| P3 HIL and heavy track | board-in-the-loop replay; 10 GB overlay streams; native dynamics within tolerance of GMAT | **Heavy track chartered (2026-09-15, `heavy-plan.md`, questions 216); board half and native dynamics not started.** Board binding refused at load by design; ZCU102/104 chosen (question 144). Object store, catalog, job runner and tiler, tile gateway, streaming layers, entity module: H1–H7 in progress. Native Rust orbital dynamics: none (all space dynamics still through the GMAT FFI, which is the validated path). |
| P4 AI plane and command path | a model proposes, a human authorizes, a simulated asset acts, replay reproduces the trail | **Command path complete through dispatch and replay; AI plane half built (2026-09-13, `aiplane-plan.md`).** `av-command`: the state machine as a service over OpenSSL-backed tonic with a hash-chained ledger, Rego policy at CHECKED (`regorus`, OPA-compatible), OIDC principals with role gating, MFA for hazardous classes and expiring delegations, an RFC 5424 audit line per transition; dispatch into the kernel's telecommand path with deadline, idempotency and three ack levels; a replayed run reproduces the trail byte for byte. `av-gateway`: the read-only label-aware gateway as gRPC and MCP with a deny-by-default tool list and a propose-only tool, evidence on the ledger. Service principals on the dispatch RPCs; `av-proposer`, the deterministic rule-based sidecar under spoore's `ModelService` contract on an internal network; the command console in the execution profile's default layout, driven by the lead in a browser; the replayed decision trail; control matrices and one evidence bundle for both services (2026-09-13): **`aiplane-plan.md` delivered in full.** Open: the console's human step needs an automatic `Check` and a CHECKED list (question 209); the gateway authenticates no caller yet (question 208); the proposer container has not run end to end on this host because the Colima VM disk is full (question 196(d)); no envelope is enabled (question 53). |
| P5 Air-gap kit, SBOMs, accreditation | zero-egress install; 3-node target | **P5 chartered and in progress (2026-09-15, `p5-plan.md`, question 213).** Control matrices for six components, hash-chained ledgers with verify endpoints, `cargo deny` bans with `sha2` gone, FIPS rules, the no-network-at-test rule with one-time fetches recorded by hash; suite declarations for eight components validated by secdeploy's own verify and plan, deterministic CycloneDX SBOMs for ten components, the kit builder and manifest (D1, D2, D3's first half, held for two test fixes). **Not yet:** the zero-egress install proof, the evidence bundle, three placements, the Fedora runbook. |

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
