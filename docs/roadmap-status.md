# Roadmap status

As of 2026-10-05, against the phase table in `architecture.md`. Decisions are numbered in
`open-questions.md` (1–237); each track's plan records its rounds and its `## Delivered` section.

## By phase

| Phase | Exit criterion (architecture.md) | Status |
|---|---|---|
| P0 CDM, contract, ADRs, FFI spike | tests and proto lint green; altavista trajectory as a CDM message; `GetDerivatives` from Rust | **Complete.** ADRs 000–005 accepted with amendments; CDM v1 (`altavista.v1`); `gmat-sys` FFI with derivatives, STM, drag/SRP, convert; goldens against GMAT for every dynamics path, each tolerance read from its own golden (question 230). |
| P1 Design and feasibility | four examples on a tiled globe and in ICRF with no jitter; kernel with GMAT and one non-space model; sweeps with MoEs in ClickHouse | **Complete except the ClickHouse store.** DRM authoring and hashing, the GMAT and Rust dynamics services, the viewer (frame graph, floating origin, tiled globe, tiling UI), the multi-model kernel, and feasibility sweeps (`feasibility-plan.md`, delivered). Sweep results go through a store trait with a file-backed implementation; the ClickHouse implementation and a multi-host job runner wait for a host that runs ClickHouse (question 237). |
| P2 SIL | same binary in container and Renode gives identical port traffic; replay bit-identical; p99 budget measured | **Complete except Renode traffic past the first step.** Lockstep protocol, shim and container binding; cFS closes the attitude loop in a container, byte-identical at the port boundary; bit-identical replay; ground segment and telecommands (`sil-plan.md`). Edge boundary delivered (`edge-plan.md`). On Renode, cFS boots on the Zynq UltraScale+ RPU and STEP 1 is byte-exact, but STEP 2 onward does not arrive (question 171), so identical traffic is verified container-only. **In progress (question 237):** the native team's GDB-remote attach to close question 171. |
| P3 HIL and heavy track | board-in-the-loop replay; 10 GB overlay streams; native dynamics within tolerance of GMAT | **Complete except the board.** Heavy track delivered (`heavy-plan.md`, H1–H7): object store, catalog, jobs and tiler, tile gateway, streaming layers, Layers panel with catalog selection, entities in the viewer with real covariance; 34 GB streamed in 393 s at a 68 MB peak with every frame under 6 ms. Native dynamics delivered (`native-dynamics-plan.md`, N1–N6): native gravity, third bodies, drag, SRP and STM against GMAT goldens, a GMAT-free kernel build, the native model selectable by name, a C ABI, and the native-versus-GMAT difference as a `Score` (12.5 µm over two hours). **Not started:** board-in-the-loop on a ZCU102/104, waiting on the user's hardware decision (questions 144, 237). |
| P4 AI plane and command path | a model proposes, a human authorizes, a simulated asset acts, replay reproduces the trail | **Complete** (`aiplane-plan.md`): the command state machine with Rego policy, OIDC roles, MFA and a hash-chained ledger; the label-aware read-only gateway over gRPC and MCP; the isolated deterministic proposer; the command console; a replayed decision trail. |
| P5 Air-gap kit, SBOMs, accreditation | zero-egress install; 3-node target | **Complete** (`p5-plan.md`): suite declarations, deterministic CycloneDX SBOMs regenerated at every merge (`scripts/kit/regenerate_compliance.py`), the kit builder, a proven zero-egress install, the evidence bundle, and three placements measured against a baseline (question 221). |

## Counts

Lead's clone gate at `4544deb` (2026-09-30):

| | |
|---|---|
| Rust tests | 1501 workspace (excluding the kernel) + 887 kernel; without GMAT: 653 kernel, 142 orbital |
| Python tests | 965 passed, 17 visible skips, 0 failed |
| Decisions logged | 237 questions; ADRs 000–005 with amendments |

## Open items and risks

- **Question 171:** Renode port traffic past the first step (in progress).
- **Hardware in the loop:** needs a ZCU102/104 board.
- **ClickHouse store and multi-host jobs:** need a host that runs ClickHouse.
- **Post-delivery cleanup (in progress, question 237):** merged-view layer updates, a glTF model producer, a control-byte lint, entity framing, the `drag_srp` golden, two missing tests.
- **Reproducible cFS shim build (in progress, question 237):** the image's runtime hash moved with its build environment.
- **Jacchia-Roberts drag:** the source mirror is proven to be the R2026a tree; the shipped binary's half of the provenance question is the lead's before an instrumented build.
- **With the user:** the spoore `publish = false` merge and upstream PRs (questions 75, 207), the GMAT covariance report (question 150), the secdeploy upstream proposals, the Redpanda licence review (question 6).
- **Host:** the docker-test lock and two cargo slots serialise both teams (questions 207, 229, 235); a large `target/debug/deps` is moved aside at the start of a round (question 236); the Colima VM's image GC deletes unused images above 85% disk, so dangling volumes are pruned when it climbs.

## Next, in the order the lead would take it

1. Close question 171 and land the cleanup round (both dispatched 2026-10-05).
2. Hardware in the loop on a ZCU102/104, once the board exists.
3. The ClickHouse store and multi-host job runner, once a host exists.
