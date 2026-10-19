# Roadmap status

As of 2026-10-19, against the phase table in `architecture.md`. Decisions are numbered in
`open-questions.md` (1–258); each track's plan records its rounds and its `## Delivered` section.

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
- **Reproducibility across platforms (questions 248, 258):** run bytes reproduce on the same build target and libm, not across macOS and Linux; the user chose a portable maths implementation for native runs, measured first (question 258, native round after question 257). GMAT-backed runs stay per-platform.
- **MinIO (questions 255, 256):** its pinned image can no longer be fetched; the heavy team is trialling SeaweedFS and Versity Gateway (Apache-2.0) to replace it.
- **The kit (questions 251, 255):** GMAT is pinned for macOS arm64 (this host's install) and Linux x86_64 (NASA's Ubuntu archive); NASA publishes no Linux aarch64 build; the RHEL 7 archive is being pinned as a second variant (question 257).
- **`serde_yaml` (questions 246, 251):** gone from AltaVista's crates; the `RUSTSEC-2024-0449` ignore stays for spoore's `spoore-tree` until the user applies `docs/spoore-upstream.md`'s sixth proposal. MSRV 1.89, measured.
- **Heavy (question 255):** globe tile selection plans for the primary only; `enableGlobe` builds WGS84 for any body; `layers_stream_check.mjs` exits 0 by construction (it needs a live gateway); two writers racing on case variants of one id on a case-sensitive filesystem can both publish; two typed store-write windows remain (an unreadable ClickHouse outcome, a rename failing after ClickHouse committed), each printing its repair.
- **Native (question 255):** a mis-named but well-shaped emit port is not caught; `integral` over radians is `UnitCompositionUndefined`; classic-builder image ids drift across cache chains (content hashes hold).
- **Mechanitis (question 240):** scoped read-only; AltaVista changes nothing until Mechanitis settles encoding and shared types (its ADR-003).
- **Jacchia-Roberts drag:** the shipped binary's half of the provenance question is the lead's before an instrumented build.
- **With the user:** the ClickHouse licence review (question 247), the Redpanda licence review (question 6), the MinIO replacement (question 255), whether this Colima switches to the grpc port forwarder (it carries no UDP today; switching restarts the shared VM), the spoore `publish = false` merge and upstream PRs (questions 75, 207), the GMAT covariance report (question 150), the secdeploy upstream proposals.
- **Host:** the docker-test lock and two cargo slots serialise both teams (questions 207, 229, 235); a large `target/debug/deps` is moved aside at the start of a round (question 236); Colima mounts only `$HOME` into containers; k3s's kubelet image GC deletes every unused image above 85% of the Colima VM's image disk (it took the pinned MinIO and base images on 2026-10-18; check the disk with `colima ssh -- df -h /var/lib/docker` and prune dangling anonymous volumes with the user's approval); no process is ever killed by pattern; on this host's containerd image store `docker rmi` of an image's last name deletes the image; the local shell policy refuses some commands, which are never run through a wrapper; lead tools live in `/Users/probe/code/altavista-lead/` (the session scratchpad does not survive OS updates); the `docker events` logger writes `/Users/probe/code/altavista-docker-events.log`.

## Next, in the order the lead would take it

1. The MinIO replacement (question 256, heavy round running) and the RHEL 7 GMAT pin (question 257, native round).
2. The open heavy and native items above, as one round per team when the user charters them.
3. Cross-platform reproducibility for native runs (question 258), after the RHEL 7 round.
4. Hardware in the loop, when the user resumes it.
