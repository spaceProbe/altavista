# Roadmap status

As of 2026-10-05, against the phase table in `architecture.md`. Decisions are numbered in
`open-questions.md` (1–238); each track's plan records its rounds and its `## Delivered` section.

## By phase

| Phase | Exit criterion (architecture.md) | Status |
|---|---|---|
| P0 CDM, contract, ADRs, FFI spike | tests and proto lint green; altavista trajectory as a CDM message; `GetDerivatives` from Rust | **Complete.** ADRs 000–005 accepted with amendments; CDM v1 (`altavista.v1`); `gmat-sys` FFI with derivatives, STM, drag/SRP, convert; goldens against GMAT for every dynamics path, each tolerance read from its own golden (question 230). |
| P1 Design and feasibility | four examples on a tiled globe and in ICRF with no jitter; kernel with GMAT and one non-space model; sweeps with MoEs in ClickHouse | **Complete except the ClickHouse store.** DRM authoring and hashing, the GMAT and Rust dynamics services, the viewer (frame graph, floating origin, tiled globe, tiling UI), the multi-model kernel, and feasibility sweeps (`feasibility-plan.md`, delivered). Sweep results go through a store trait with a file-backed implementation; the ClickHouse implementation and a multi-host job runner wait for a host that runs ClickHouse (question 237). |
| P2 SIL | same binary in container and Renode gives identical port traffic; replay bit-identical; p99 budget measured | **Complete** (2026-10-05). Lockstep protocol, shim and container binding; cFS closes the attitude loop in a container; bit-identical replay; ground segment and telecommands (`sil-plan.md`); edge boundary delivered (`edge-plan.md`). Question 171 closed: the bridge injected each frame in one burst and overflowed the guest's 256-byte termios ring at STEP 2; it now injects at UART line rate, and `drm_attitude_control_renode.rs` asserts all 998 port-traffic records identical, container against Renode, over 100 steps (opt-in, `AV_RENODE_TESTS=1`, about 5 min). The cFS image, shim included, now builds reproducibly. |
| P3 HIL and heavy track | board-in-the-loop replay; 10 GB overlay streams; native dynamics within tolerance of GMAT | **Complete except the board.** Heavy track delivered (`heavy-plan.md`, H1–H7, plus a post-delivery cleanup round): object store, catalog, jobs and tiler, tile gateway, streaming layers, Layers panel with catalog selection, entities in the viewer with real covariance, glTF models at true size, entity framing; 34 GB streamed in 393 s at a 68 MB peak with every frame under 6 ms. Native dynamics delivered (`native-dynamics-plan.md`, N1–N6): native gravity, third bodies, drag, SRP and STM against GMAT goldens, a GMAT-free kernel and `av-run`, the native model selectable by name, a C ABI, and the native-versus-GMAT difference as a `Score` (12.5 µm over two hours). **Not started:** board-in-the-loop on a ZCU102/104, waiting on the user's hardware decision (questions 144, 237). |
| P4 AI plane and command path | a model proposes, a human authorizes, a simulated asset acts, replay reproduces the trail | **Complete** (`aiplane-plan.md`): the command state machine with Rego policy, OIDC roles, MFA and a hash-chained ledger; the label-aware read-only gateway over gRPC and MCP; the isolated deterministic proposer; the command console; a replayed decision trail. |
| P5 Air-gap kit, SBOMs, accreditation | zero-egress install; 3-node target | **Complete** (`p5-plan.md`): suite declarations, deterministic CycloneDX SBOMs regenerated at every merge (`scripts/kit/regenerate_compliance.py`), the kit builder, a proven zero-egress install, the evidence bundle, and three placements measured against a baseline (question 221). |

## Counts

See the team log's latest lead acceptance for the gate at the current `develop` head; the gate
recipe is in `README.md`.

## Open items and risks

- **Hardware in the loop:** needs a ZCU102/104 board.
- **ClickHouse store and multi-host jobs:** need a host that runs ClickHouse.
- **Next heavy work (question 238):** a model-only spacecraft is framed at marker scale, so a 1.5 m model is invisible at Focus; per-viewport Focus still uses the central-body distance; the run route carries no `model`.
- **Next native work (question 238):** a run's provenance depends on the test registry's port (`ContainerBinding.image` enters `sos_configuration_hash`); the Renode test holds the docker lock for its whole run; the RTEMS ELF build is not byte-reproducible (cFE's build date and host, and a temp name from `rtems-syms`).
- **Jacchia-Roberts drag:** the source mirror is proven to be the R2026a tree; the shipped binary's half of the provenance question is the lead's before an instrumented build.
- **With the user:** the spoore `publish = false` merge and upstream PRs (questions 75, 207), the GMAT covariance report (question 150), the secdeploy upstream proposals, the Redpanda licence review (question 6).
- **Host:** the docker-test lock and two cargo slots serialise both teams (questions 207, 229, 235); a large `target/debug/deps` is moved aside at the start of a round (question 236); Colima mounts only `$HOME` into containers; the Colima VM's image GC deletes unused images above 85% disk, so dangling volumes are pruned when it climbs; the local shell policy blocks `bash <script>`.

## Next, in the order the lead would take it

1. Hardware in the loop on a ZCU102/104, once the board exists.
2. The small items above, as one round per team when the user charters them.
3. The ClickHouse store and multi-host job runner, once a host exists.
