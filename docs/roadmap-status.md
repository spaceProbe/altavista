# Roadmap status

As of 2026-10-09, against the phase table in `architecture.md`. Decisions are numbered in
`open-questions.md` (1–244); each track's plan records its rounds and its `## Delivered` section.

## By phase

| Phase | Exit criterion (architecture.md) | Status |
|---|---|---|
| P0 CDM, contract, ADRs, FFI spike | tests and proto lint green; altavista trajectory as a CDM message; `GetDerivatives` from Rust | **Complete.** ADRs 000–005 accepted with amendments; CDM v1 (`altavista.v1`); `gmat-sys` FFI with derivatives, STM, drag/SRP, convert; goldens against GMAT for every dynamics path, each tolerance read from its own golden (question 230). |
| P1 Design and feasibility | four examples on a tiled globe and in ICRF with no jitter; kernel with GMAT and one non-space model; sweeps with MoEs in ClickHouse | **Complete except the ClickHouse store.** DRM authoring and hashing, the GMAT and Rust dynamics services, the viewer (frame graph, floating origin, tiled globe, tiling UI), the multi-model kernel, and feasibility sweeps (`feasibility-plan.md`, delivered). Sweep results go through a store trait with a file-backed implementation; the ClickHouse implementation and a multi-host job runner wait for a host that runs ClickHouse (question 237). |
| P2 SIL | same binary in container and Renode gives identical port traffic; replay bit-identical; p99 budget measured | **Complete** (2026-10-05). Lockstep protocol, shim and container binding; cFS closes the attitude loop in a container; bit-identical replay; ground segment and telecommands (`sil-plan.md`); edge boundary delivered (`edge-plan.md`). Question 171 closed: the bridge injected each frame in one burst and overflowed the guest's 256-byte termios ring at STEP 2; it now injects at UART line rate, and `drm_attitude_control_renode.rs` asserts all 998 port-traffic records identical, container against Renode, over 100 steps (opt-in, `AV_RENODE_TESTS=1`, about 5 min). The cFS image, shim included, now builds reproducibly. |
| P3 HIL and heavy track | board-in-the-loop replay; 10 GB overlay streams; native dynamics within tolerance of GMAT | **Complete except the board.** Heavy track delivered (`heavy-plan.md`, H1–H7, plus a post-delivery cleanup round): object store, catalog, jobs and tiler, tile gateway, streaming layers, Layers panel with catalog selection, entities in the viewer with real covariance, glTF models at true size, entity framing; 34 GB streamed in 393 s at a 68 MB peak with every frame under 6 ms. Native dynamics delivered (`native-dynamics-plan.md`, N1–N6): native gravity, third bodies, drag, SRP and STM against GMAT goldens, a GMAT-free kernel and `av-run`, the native model selectable by name, a C ABI, and the native-versus-GMAT difference as a `Score` (12.5 µm over two hours). **Board-in-the-loop prepared against stand-ins, the board itself not yet on hand** (questions 241–244): the board is a ZCU104; real-time pacing whenever a board is bound, the `av-edge-board` service (serial or UDP), a signed and hash-chained I/O log, the kernel's board binding, replay of a board from its log, power control as an edge-service operation, a byte-reproducible SD-card boot-image recipe, and a stand-in HIL run (the reproducible ELF in Renode, in real time, behind a pty) whose 998 port-traffic records equal the lockstep reference. No board result is claimed. |
| P4 AI plane and command path | a model proposes, a human authorizes, a simulated asset acts, replay reproduces the trail | **Complete** (`aiplane-plan.md`): the command state machine with Rego policy, OIDC roles, MFA and a hash-chained ledger; the label-aware read-only gateway over gRPC and MCP; the isolated deterministic proposer; the command console; a replayed decision trail. |
| P5 Air-gap kit, SBOMs, accreditation | zero-egress install; 3-node target | **Complete** (`p5-plan.md`): suite declarations, deterministic CycloneDX SBOMs regenerated at every merge (`scripts/kit/regenerate_compliance.py`), the kit builder, a proven zero-egress install, the evidence bundle, and three placements measured against a baseline (question 221). |

## Counts

See the team log's latest lead acceptance for the gate at the current `develop` head; the gate
recipe is in `README.md`.

## Open items and risks

- **Hardware in the loop (questions 243, 244):** needs the ZCU104 on hand, and the 2024.2 ZCU104 BSP from the user, from which the lead extracts `psu_init.c`/`psu_init.h` to build the real `BOOT.BIN` (until then the recipe builds only `BOOT.standin-zcu102-psuinit.bin`). Re-handshake and re-Bind after a real board reboot cannot be built without the board.
- **ClickHouse store and multi-host jobs:** need a host that runs ClickHouse.
- **Next heavy work (question 240):** shared entity objects are float32-quantized against the primary camera's origin, so a model in a second viewport framed 10 m away sits 4–25 cm off its true position, breaking question 46's centimetre bound in secondary viewports; `scenario_to_cdm` does not write `visual_model_uri`. Also (question 244): the tile-streaming harness `web/js/layers_stream_check.mjs` sizes its per-position dwell from one calibration fetch before the camera path, so `test_eviction_and_cancellation_both_actually_happened` flakes when path latency outruns that sample; the dwell should come from latency observed during the path. (Question 238's three heavy items are closed.)
- **Next native work (question 240):** the container instance's `dynamics_hash` still folds in its ephemeral address; the RTEMS build's apt pins depend on the live Debian mirror. Any proto compiled into `av-cdm` moves the cFS shim and the image's runtime-content hash, so both are re-pinned at every proto merge (question 244). (Question 238's three native items are closed: stable provenance, a narrowed Renode lock, a byte-reproducible ELF.)
- **Mechanitis (question 240):** scoped read-only; AltaVista changes nothing until Mechanitis settles encoding and shared types (its ADR-003). The open decisions are the user's.
- **Jacchia-Roberts drag:** the source mirror is proven to be the R2026a tree; the shipped binary's half of the provenance question is the lead's before an instrumented build.
- **With the user:** the spoore `publish = false` merge and upstream PRs (questions 75, 207), the GMAT covariance report (question 150), the secdeploy upstream proposals, the Redpanda licence review (question 6).
- **Host:** the docker-test lock and two cargo slots serialise both teams (questions 207, 229, 235); a large `target/debug/deps` is moved aside at the start of a round (question 236); Colima mounts only `$HOME` into containers; the Colima VM's image GC deletes unused images above 85% disk, so dangling volumes are pruned when it climbs; the local shell policy blocks `bash <script>`.

## Next, in the order the lead would take it

1. Hardware in the loop on the ZCU104: the real `BOOT.BIN` once the user supplies the BSP, then the board day once it is on hand (question 244 lists the steps).
2. The small items above, as one round per team when the user charters them.
3. The ClickHouse store and multi-host job runner, once a host exists.
