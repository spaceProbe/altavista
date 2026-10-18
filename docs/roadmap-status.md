# Roadmap status

As of 2026-10-18, against the phase table in `architecture.md`. Decisions are numbered in
`open-questions.md` (1–253); each track's plan records its rounds and its `## Delivered` section.

## By phase

| Phase | Exit criterion (architecture.md) | Status |
|---|---|---|
| P0 CDM, contract, ADRs, FFI spike | tests and proto lint green; altavista trajectory as a CDM message; `GetDerivatives` from Rust | **Complete.** ADRs 000–005 accepted with amendments; CDM v1 (`altavista.v1`); `gmat-sys` FFI with derivatives, STM, drag/SRP, convert; goldens against GMAT for every dynamics path, each tolerance read from its own golden (question 230). |
| P1 Design and feasibility | four examples on a tiled globe and in ICRF with no jitter; kernel with GMAT and one non-space model; sweeps with MoEs in ClickHouse | **Complete on this host** (2026-10-17). DRM authoring and hashing, the GMAT and Rust dynamics services, the viewer (frame graph, floating origin, tiled globe, tiling UI), the multi-model kernel, and feasibility sweeps (`feasibility-plan.md`, delivered). Sweep results go to a file store or a ClickHouse store whose studies read back bit-equal, with MoEs computed in ClickHouse bit-identical to the Rust aggregation; sweeps fan out as Kubernetes Jobs on k3s, byte-identical to the same image under Docker (question 249). ClickHouse is not in the air-gap kit until the user's licence review (question 247); multi-host means several pods on one node until a second node exists. |
| P2 SIL | same binary in container and Renode gives identical port traffic; replay bit-identical; p99 budget measured | **Complete** (2026-10-05). Lockstep protocol, shim and container binding; cFS closes the attitude loop in a container; bit-identical replay; ground segment and telecommands (`sil-plan.md`); edge boundary delivered (`edge-plan.md`). `drm_attitude_control_renode.rs` asserts all 998 port-traffic records identical, container against Renode (question 238). Since then (question 249): two spacecraft in one run, each through its own cFS container; a re-handshake and re-Bind after a real restart of the peer, logged and replayed; the RTEMS ELF build pinned to `snapshot.debian.org`. |
| P3 HIL and heavy track | board-in-the-loop replay; 10 GB overlay streams; native dynamics within tolerance of GMAT | **Complete except the board.** Heavy track delivered (`heavy-plan.md`, H1–H7, plus post-delivery rounds): object store, catalog, jobs and tiler, tile gateway, streaming layers, Layers panel, entities with real covariance, glTF models, entity framing; 34 GB streamed in 393 s at a 68 MB peak with every frame under 6 ms. Native dynamics delivered (`native-dynamics-plan.md`, N1–N6). **Board-in-the-loop prepared against stand-ins** (questions 241–244, 249): pacing, the `av-edge-board` service, a signed I/O log, the board binding and its replay, power control, a reboot's re-handshake and re-Bind (proven against a restarted container), a byte-reproducible boot-image recipe, and a stand-in HIL run on Renode. The board work itself is deferred by the user (question 245); no board result is claimed. |
| P4 AI plane and command path | a model proposes, a human authorizes, a simulated asset acts, replay reproduces the trail | **Complete** (`aiplane-plan.md`). |
| P5 Air-gap kit, SBOMs, accreditation | zero-egress install; 3-node target | **Complete** (`p5-plan.md`). Since question 251 the kit carries no GPL or LGPL code: images ship as application layers built offline by the installer on operator-supplied, digest-verified bases; GMAT, numpy (it bundles `libgfortran`) and Python ≥ 3.11 are operator-supplied and verified; a no-copyleft proof checks every kit file against both base images (`docs/kit-operator-supplied.md`). |

## Counts

See the team log's latest lead acceptance for the gate at the current `develop` head; the gate
recipe is in `README.md`.

## Open items and risks

- **Hardware in the loop:** deferred by the user (question 245). When resumed: the 2024.2 ZCU104 BSP from the user for the real `BOOT.BIN` (question 243), then the board day (question 244).
- **Reproducibility across platforms (question 248):** run bytes reproduce on the same build target and libm, not across macOS and Linux; a portable libm in `av-orbital` would change that and re-pin every golden it moves. The user's call whether it is wanted.
- **The kit (question 251):** a Linux GMAT pin is not recorded yet (only macOS arm64).
- **`serde_yaml` (questions 246, 251):** gone from AltaVista's crates (`av-yaml` over `serde-saphyr`); the `RUSTSEC-2024-0449` ignore stays for spoore's `spoore-tree` until the user applies `docs/spoore-upstream.md`'s sixth proposal. MSRV is now 1.89, declared but not measured.
- **Heavy (question 253):** question 240's secondary-viewport defect is closed (every viewport under 1 cm at LEO, GEO and lunar distance). Open: `FileStudyStore` joins a `sweep_id` such as `../x` onto its root (validate it as a path component); study mode refuses only after a whole study has run; picking in an extra viewport, and bodies and the globe, still use the primary's placement.
- **Native (question 249):** `GroundStationModel` keeps only the last frame on its port per step; one framed-out port per system; a Docker-free replay cannot reproduce a container's `dynamics_hash` (the Bind version is not in the log); the Linux `av-edge-board` build is not byte-reproducible; the other `docker rmi` calls are not audited against the containerd behaviour `ImageTagGuard` was fixed for; duplicated test helpers in `drm_container.rs` and `drm_shared_run.rs`.
- **Mechanitis (question 240):** scoped read-only; AltaVista changes nothing until Mechanitis settles encoding and shared types (its ADR-003).
- **Jacchia-Roberts drag:** the shipped binary's half of the provenance question is the lead's before an instrumented build.
- **With the user:** the ClickHouse licence review (question 247), the Redpanda licence review (question 6), whether this Colima switches to the grpc port forwarder (it carries no UDP today; switching restarts the shared VM), about 520 dangling Docker volumes from earlier registries, the spoore `publish = false` merge and upstream PRs (questions 75, 207), the GMAT covariance report (question 150), the secdeploy upstream proposals.
- **Host:** the docker-test lock and two cargo slots serialise both teams (questions 207, 229, 235); a large `target/debug/deps` is moved aside at the start of a round (question 236); Colima mounts only `$HOME` into containers; the Colima VM's image GC deletes unused images above 85% disk; on this host's containerd image store `docker rmi` of an image's last name deletes the image; the local shell policy refuses some commands, which are never run through a wrapper; lead tools live in `/Users/probe/code/altavista-lead/` (the session scratchpad does not survive OS updates); the `docker events` logger writes `/Users/probe/code/altavista-docker-events.log`.

## Next, in the order the lead would take it

1. The open heavy and native items above (the `sweep_id` path check first), as one round per team when the user charters them.
2. Cross-platform reproducibility (question 248), if the user wants it.
3. Hardware in the loop, when the user resumes it.
