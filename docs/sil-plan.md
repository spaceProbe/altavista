# SIL against real flight software: phase plan

Drafted 2026-09-05 after the first demo (question 5) closed. Standing decisions this plan
builds on: cFS first, ROS 2 second (question 24); Cortex-M (STM32, nRF) under Renode with
lockstep required (25); UART and Ethernet first (26); message-level sensors first (27); all
four fault scopes (28); the ground segment is a system with the same bindings
(architecture.md); the lockstep protocol (107); ports and the router (108–110); port
commands as events (130, 137); Docker on the evaluation host, Renode not (118).

## Goal

The same flight-software binary runs in a container and in Renode, bound to the kernel
through the lockstep protocol, closing a control loop against the platform's dynamics, and
produces identical port traffic under lockstep in both bindings (P2 exit criterion in
architecture.md). A board binding with real-time pacing follows in P3.

## What exists

- Kernel: shared multi-rate run per system of systems, typed ports, deterministic router
  with latency, faults (port, dynamics, hardware power-cycle), maneuvers with the Gates
  model, covariance, scoring, `RunProducts` on the wire, the viewer bridge.
- Bindings: `crates/av-lockstep` client, `services/lockstep-ref` reference process,
  Docker pull-by-digest lifecycle, `Reset` wired to a power-cycle fault. A container that
  cannot promise lockstep is refused at load.
- Not yet: any real flight software, a CCSDS framing on a FRAMED port, sensor and actuator
  models with declared noise, a virtual clock inside flight software, Renode on any host.

## Milestones

**M22 Sensors, actuators and CCSDS framing (kernel side, no flight software yet).**
FRAMED ports with `schema = "ccsds.spp"` carry one space packet per message; a `PacketCodec`
per declared APID maps packet fields to CDM measurements and commands (declared in the
system definition, hashed). Sensor models as ordinary dynamics models: GPS (position and
velocity with seeded Gaussian noise and a declared update rate), IMU (rates and
accelerations from the truth state plus bias random walk), star tracker (quaternion plus
noise), all feeding FRAMED ports. Actuator models: thruster (impulsive and finite,
reusing the maneuver path) and reaction wheel (torque with saturation). A native
"controller" instance closes the loop first so the whole chain is proven before any
external binary is involved. Goldens against closed-form expectations and against GMAT
where GMAT has the model.

**M23 Reference flight software in a container.** NASA cFS at a pinned commit under
`third_party/cfs` with CI_LAB and TO_LAB replaced by a lockstep-aware I/O app that speaks
`LockstepService` (framed CCSDS packets in and out per step) and drives the scheduler from
the kernel's ticks, so cFS time is the kernel's time. One mission app written by the team
(orbit maintenance or attitude control, question A below) consuming the M22 sensor packets
and emitting actuator commands. Dockerfile, image pinned by digest, bound through the
existing container lifecycle. Exit: the loop closes in the container with a scored
objective, byte-identical across two runs at the port boundary.

**M24 Renode.** Renode installed at a pinned version under `third_party/renode` (portable
build) or on a Linux host if the macOS build cannot slave virtual time; the same cFS binary
cross-compiled for a Cortex-M target (STM32F7 or nRF52840, question C) with the lockstep
I/O app on UART and Ethernet peripheral bridges; a bridge process that owns Renode, slaves
its virtual time to the kernel through Renode's external interface, and speaks the lockstep
protocol. The virtual-time slaving spike comes first and its measurements decide whether
the acceptance test can depend on it (architecture.md risk). Exit: identical port traffic
container against Renode under lockstep; hardware faults (peripheral fault, reset) through
`FAULT_TARGET_KIND_HARDWARE`.

**M24 limitation, recorded at close (2026-09-07, M24.4g; `docs/open-questions.md` question
171):** the identical-port-traffic exit criterion above is verified **posix-container-only**
(`crates/av-kernel/tests/drm_attitude_control_cfs.rs`), not against Renode. Renode's own
evidence otherwise stands: virtual-time slaving is exact (0 ns drift), the real RTEMS/cFE
image boots all three lockstep apps to OPERATIONAL, and the lockstep protocol's own STEP 1
delivers byte-exact port traffic through Renode. STEP 2 onward does not reliably deliver the
guest's own transmitted frame within budget, reproduced across three different guest-to-host
transports (a Renode terminal backend, a polled file backend, and a Renode Python
`CharReceived` hook) -- narrowing the defect away from any one of those specific mechanisms
and toward something that recurs on a second write-then-WFI-park cycle specifically. See
question 171 for the precise evidence and the named next diagnostic step (a GDB-remote attach
taken during a stalled STEP 2, the same technique that already resolved the STEP-1 question).

**M25 Ground segment and replay.** The ground segment as a system with the same bindings:
DRM command events become CDM `Command`s, framed as CCSDS telecommands, delivered through
the router with the link model; telemetry framed back into CDM measurements and into the
viewer's timeline. A run replays bit-identically from its `RunProducts` and logged port
traffic with the flight software removed (P3 criterion brought forward for SIL).

**M25.4b closes the "flight software removed" half of that criterion.** `RunConfig.replay`
(`crates/av-kernel/src/drm/replay.rs`) plays one or more instances' own recorded port traffic
(`RunProducts.port_traffic_hash`'s own `PortTrafficLog` sidecar, M25.4a) back through a
`ReplayModel` in place of the process that produced it -- a `BINDING_KIND_MODEL` instance (a
native controller/sensor, no external process at all) or a `BINDING_KIND_CONTAINER` instance
(the real bound process, container included), named explicitly or -- for every
`BINDING_KIND_CONTAINER` instance at once -- left as the default. The log's own hash is
verified before any binding, any GMAT call, or any step; only FRAMED/BYTE_STREAM OUT frames are
replayed (an IN record is the receiver's own view of the same frame, never replayed a second
time), and every replayed instance's declared binding kind stays exactly what the artifact
declares (a replayed container instance still reports `BINDING_KIND_CONTAINER` everywhere it is
reported). **Verified against a real, posix-userspace cFS container**
(`crates/av-kernel/tests/drm_attitude_control_cfs.rs`, Docker-gated with a visible skip): one
run against the real container, then a second, Docker-free run replaying the same
`"controller"` instance from nothing but the recorded log, produce identical trajectory samples,
segment epochs, `event_ids`, events and `dropped_in_flight_messages` -- excluding only the
fields that can only ever come from a live `Bind` response a Docker-free replay deliberately
never makes (the replayed container segment's own `dynamics_hash`/`dynamics_model`/
`dynamics_depth`, and its `container_binding_hash` provenance attribute), each excluded field
named at its own assertion. That test's `scores` comparison is real but vacuous -- its DRM
declares no objectives -- so the scored half of the claim rests on
`crates/av-kernel/tests/replay.rs`'s own `t1b_...`, where a replayed sensor drives a closed
loop and the ENTIRE encoded `RunProducts` (trajectories, events, measurements, scores and the
port-traffic hash) matches byte for byte with nothing excluded at all. Not yet verified against
Renode or a physical board.

**The missing-frame rule, and its honest limit.** A step whose own emission epoch has no
recorded frame is a typed refusal (never interpolated, held, or synthesized) exactly when that
epoch falls strictly between the replayed instance's own first and last recorded epoch -- an
interior gap, most likely a deleted or corrupted record. A step before the first, or after the
last, recorded epoch is legitimate silence. This detects a deleted or corrupted INTERIOR
record; it cannot detect one deleted from the leading or trailing edge (an instance that
genuinely emitted nothing on its own first or last step is, from the log alone,
indistinguishable from one whose very first or very last record was quietly removed).

## Forks for the user

A. **First closed loop.** Orbit maintenance (GPS in, burn commands out; reuses the maneuver
   and Gates paths, and the demo DRM) or attitude control (star tracker and IMU in, wheel
   torques out; needs the attitude state space in the dynamics and wheel models). Lead
   recommends orbit maintenance first, attitude second.
B. **How cFS gets the kernel's clock.** (a) A platform-support layer and scheduler app we
   maintain that take ticks from `LockstepService.Step`, cFS otherwise unmodified; (b) no
   container lockstep for cFS, only Renode lockstep and real-time pacing in containers;
   (c) clock interception below cFS (fragile, not recommended). Lead recommends (a).
C. **Renode target.** STM32F767 (Nucleo-144, Ethernet on chip) or nRF52840 (DK, no
   Ethernet, UART and radio); and whether the user owns either board for P3. Lead
   recommends STM32F7 for Ethernet and the later board binding.
D. **Determinism inside flight software.** cFS is multi-threaded; lockstep guarantees the
   port boundary (outputs per step, deterministic order) but not internal thread order.
   (a) Accept port-boundary determinism, byte-identical at the boundary, FSW internal order
   recorded but not asserted; (b) require full determinism through a single-threaded OSAL
   we maintain. Lead recommends (a) with (b) as a later fidelity step.
E. **Whose flight software.** The reference cFS mission app the team writes, or the user's
   own flight software from the start (which changes M23's scope and what the team may see).

## Decisions (2026-09-05, questions 142–148)

- **A: attitude control first.** M22 builds the attitude dynamics (state space with
  quaternion and rates, wheel momentum), star tracker and IMU sensor models, reaction wheel
  actuator model; orbit maintenance follows.
- **B: lockstep PSP and scheduler app**, cFS otherwise unmodified.
- **C: Zynq UltraScale+ RPU, Cortex-R5F lockstep.** RTEMS 6.1 `zynqmp_rpu_lock_step`, Renode
  mainline UltraScale+ Cortex-R5 platform; ZCU102/ZCU104 for HIL.
- **D: port-boundary determinism.**
- **E: reference cFS ADCS app by the team.**
- **F: container runs cFS on OSAL-posix**; identical-traffic criterion is posix container
  against RTEMS under Renode.
- Research note (148): cFS RTEMS 6 support is still landing upstream; pin and record patches.

## Revised milestones

- **M22** attitude dynamics + sensors + wheels + CCSDS framing, loop closed by a native
  controller.
- **M23** cFS (pinned) on OSAL-posix in Docker with the lockstep PSP/scheduler app and the
  reference ADCS app; loop closed and scored; byte-identical at the port boundary.
- **M24** RTEMS 6.1 build of the same apps for `zynqmp_rpu_lock_step`; Renode installed under
  `third_party/renode` at a pinned version (macOS portable build, or a Linux host if virtual
  time cannot be slaved there); bridge process; virtual-time slaving spike first; identical
  port traffic posix container against Renode.
- **M25** ground segment as a system, CCSDS telecommands from DRM command events, replay with
  the flight software removed.

## M24 closing state (2026-09-06)

Renode v1.16.1 with our own platform file (TTC rate within 2.0e-4), virtual time slaved
with zero drift, reset-at-exit off (idle cheaper than ticking), RTEMS 6.1 toolchain and the
lockstep BSP built in a pinned container, cFS with all three lockstep apps booting to
OPERATIONAL on the emulated RPU, the bridge live over gRPC with a genuine emulator reset,
and one step of port traffic byte-exact. **Port traffic from the second step onward does not
deliver (question 171), so the identical-traffic criterion is verified posix-container-only
for now.** The gap is in the guest or the step sequence, not the transport.

## Status (native manager, 2026-10-05) — question 171 and a reproducible cFS shim

The round question 237 chartered: close question 171 (Renode port traffic past the first step)
starting from the named GDB-remote attach, and make the cFS image's shim build reproducible
(question 236). **Both are delivered. Question 171 is fixed, not a Renode limitation: the defect
was ours, in the bridge.** Question 145's identical-traffic criterion is now verified container
against Renode, not container-only. Four commits on `edge`, none pushed, none merged.

| Commit | What |
|---|---|
| `50eb5f9` | question 236: `services/cfs/build-shim.sh`, the reproducible shim cross-build |
| `69f38c4` | question 236: the cFS image re-pinned on that shim (digest, runtime-content hash, build commit) |
| `43d88a0` | question 171: the bridge injects host-to-guest frames at UART line rate; GDB attach tooling |
| `f319650` | question 145: the Renode test un-ignored; identical port traffic over 100 steps, asserted |
| this commit | this status section, `third_party/renode/REPORT.md`, and the question 171 lines of `docs/roadmap-status.md` |

### Question 171: where the guest was, and why

- **Setup.** The RTEMS ELF no longer existed on this host and neither did the `rtems-m24c:build`
  image. It was rebuilt from this worktree's sources with the surviving toolchain mounted
  read-only, in digest-pinned `debian:bookworm-slim`, into the scratchpad (never the main tree):
  `core-cpu1.exe` SHA-256 `b5b2eac3…`. Its 3,335 text symbols match M24.4e's table name for name
  and address for address. It boots to cFE OPERATIONAL with the three lockstep apps.
- **The attach.** `machine StartGdbServer <port>` alone is refused on this platform, which mixes
  architectures (cluster0 is the ARMv8-A APUs, cluster1 the ARMv7-R RPUs); the working form is
  `machine StartGdbServer <port> false "cluster1"`. The RTEMS toolchain's own `arm-rtems6-gdb`
  (a Linux aarch64 binary) ran in a `--rm` container reaching the stub at `host.docker.internal`,
  with full symbolic backtraces. **lldb `gdb-remote` connected this time** ("Process 1 stopped", no
  handshake error), and memory reads and DWARF expressions worked; only `register read pc`
  ("Invalid register name 'pc'") and `bt` (an empty frame #0) failed. That changes M24.4e's record,
  which said lldb failed its handshake against this stub.
- **The guest state at the STEP 2 stall.** PC in the idle thread. `IO_LOCKSTEP` blocked in
  `read_all` → `rtems_termios_read_tty` → `fillBufferQueue` → `_Semaphore_Wait_timed_ticks`, having
  consumed 250 of STEP 2's 300 payload bytes; the ttyS1 termios state has `rawInBuf` Head = Tail =
  181 and **`rawInBufDropped = 50`**; payload bytes 250–299 in its buffer are zero; the UART FIFOs
  and the GIC are empty. Every other task is in an ordinary wait.
- **Root cause.** The bridge injected each host-to-guest frame as one burst of `WriteChar`
  commands while emulated time was frozen. Renode's Cadence UART RX FIFO is unbounded unless
  `EnableRxOverflow` is set (`Cadence_UART.WriteChar`, `Count < fifoCapacity || !EnableRxOverflow`,
  renode-infrastructure `add012af`, the commit v1.16.1 pins), so the RTEMS driver's interrupt
  (`zynq_uart_interrupt`, at most 32 bytes per entry, re-entered while the trigger flag is set) moved
  the whole frame into termios' 256-byte raw input ring, and `rtems_termios_enqueue_raw_characters`
  dropped what did not fit. HELLO (11 bytes), BIND (154) and STEP 1 (17) fit; STEP 2 is the first
  STEP carrying sensor inputs, 305 bytes, and lost its last 50. That the reader task was never
  dispatched during the interrupt storm is inferred from the counts; **the decisive evidence is the
  boundary experiment**: replaying the recorded frames through the real bridge and ELF, a single
  burst and 256-byte chunks stall (256 drops one byte), 255-byte and 64-byte chunks deliver. The
  manager re-ran burst against 64 independently: STALLED against all_delivered.
- **Fix** (`renode_bridge.py`, `inject_frame`): at most 64 bytes, the real FIFO depth, per burst,
  with the guest run for that chunk's wire time at 115200 baud between bursts, for every
  host-to-guest frame. A real UART delivers at line rate into a 64-byte FIFO, so the burst was the
  unrealistic part; no guest, platform or Renode change.
- **Corrections to M24.4e–g's record.** STEP 2's failure was in the host-to-guest direction, so
  M24.4g's reading (a defect recurring on a second write-then-WFI cycle, shared by three
  guest-to-host transports) is refuted: the transports were never the issue for STEP 2. The uart0
  truncation at "…warm-up co" is OSAL's 172-byte printf buffer (`OSAL_CONFIG_PRINTF_BUFFER_SIZE`),
  not a write blocked mid-message: the fixed run truncates the same line and the console keeps
  printing. M24.4e's separate observation that `CreateServerSocketTerminal` did not deliver STEP 1's
  reply was not re-tested this round; the hook transport makes it moot.

### Question 145: identical port traffic, container against Renode

`crates/av-kernel/tests/drm_attitude_control_renode.rs` runs the same `demo_attitude_control` DRM
once bound to the posix cFS container and once to cFS on RTEMS under Renode, over **10 s, 100
steps** (the container determinism test's arc), with `products_dir` set:

- **All 998 decoded `port_traffic.pb` records are equal, in order** (six ports: 200 records each
  for the IMU and star-tracker paths in and out of the controller, 99 each for the wheel-torque
  command out and in), and the records-only hash is equal: `8e518964f625…8fd2` both sides.
- **The whole-file `port_traffic_hash` is not equal and cannot be**: the sidecar also carries
  `run_id`, `provenance.run_id`, `provenance.config_hash` and
  `provenance.attributes["sos_configuration_hash"]`, which differ by construction between two
  bindings. Its definition is not changed (question 175: it hashes the whole sidecar). The rest of
  the sidecar is asserted equal with exactly those four fields named.
- **Events** (107 each) are equal as whole values with exactly `provenance.config_hash`,
  `provenance.run_id` and, on the controller's own `run_start`/`run_end`,
  `system_definition_hash`/`_id` excluded and named. The `run_start` detail does not carry the
  container address (an earlier summary said it did; corrected). Samples, `event_ids`, segments and
  the native `dynamics_hash` remain asserted unmodified; the truth pointing error at t = 10 s is
  1.823926917407e-1 rad on both sides.
- **Wall time.** The bridge granted every STEP 10 s of virtual time before looking for the reply
  (36.4 min for the test). It now stops granting once the whole reply is buffered on the hook
  socket (`AV_BRIDGE_STEP_EARLY_STOP`, set by the test): the Renode sidecar is byte-identical with
  and without it (`8a16007f…7fff`, 83,694 bytes), and the test takes 5–7 min (305 s for the worker,
  409 s for the manager's re-run, both passing).
- **Gate.** No longer `#[ignore]`d. It skips visibly with typed reasons when the cFS image or a
  Renode file is missing, and unless `AV_RENODE_TESTS=1`. `AV_RENODE_BIN` and
  `AV_RENODE_CORE_CPU1_EXE` point it at a Renode binary and an ELF outside the tree.

### Question 236: the shim, reproducible

`services/cfs/build-shim.sh`: `rust:1.90-bookworm` by index digest (`3914072c…`; arm64 manifest
`4c632e49…`), protobuf (`3.21.12-3+deb12u1`) and binutils (`2.38-4ubuntu2.12`) apt packages pinned,
`cargo build --release --locked`, `--remap-path-prefix` for the repo mount, the spoore mount,
cargo's target dir, `CARGO_HOME` and `RUSTUP_HOME`, strip on the Dockerfile's digest-pinned ubuntu.
**Byte for byte:** two clean builds from different container mount paths and target dirs give
unstripped `708d23c7…` and stripped `8d61137c…`; the manager's own independent pair, from two
further mount paths, gives the same two hashes. Neither contains `/Users/probe`, `/usr/local/cargo`
or `/root`; a control build without the remap differs and embeds them (4, 1771, 0).
`services/cfs/tests/test_build_shim_script.py` pins the script's invariants. **The image:**
`build-image.sh` at `50eb5f9`, clean under the copied paths: image `sha256:847c9a08…`,
runtime-content hash `sha256:fa059749…`; it moved against `cc83bb68…` on exactly one file, the
shim. A `--no-cache` rebuild gives another image id (question 190) and the identical
runtime-content hash, all 12 per-file lines equal, so both halves of the image are now
reproducible, which question 185 required. Recorded in `services/cfs/IMAGE_DIGEST.md`.

### Decisions (numbered, for the lead to ratify)

1. Build products this round needed (the ELF, shim build trees) live in the scratchpad or in a
   container, never in the main tree reached through `third_party/cfs`; the Renode test takes
   `AV_RENODE_BIN` and `AV_RENODE_CORE_CPU1_EXE` instead.
2. The ELF was rebuilt in digest-pinned `debian:bookworm-slim` with the toolchain mounted
   read-only, not by rebuilding `rtems-m24c:build` (which clones RSB and RTEMS and is not needed
   for a cFS cross-build).
3. Docker on this host is Colima, which mounts only `$HOME`: container inputs were staged under
   `/Users/probe/q171-*-stage/` and removed at each task's end.
4. M24.4e's struct offsets were reused for the rebuilt ELF only on the evidence of the identical
   symbol table, and only after one known read; gdb with DWARF made them unnecessary in the end.
5. `services/cfs/IMAGE_COPIED_PATHS.txt` for the cFS image now lists `build-shim.sh`, `Cargo.toml`
   and `Cargo.lock` (the lead's ruling): with `--locked` the lock file is an input. This reverses
   question 233's exclusion for this image only.
6. Task 2 ran in two phases so the image's build-commit record points at a committed, clean tree.
7. The fix belongs in the bridge, not the guest: line-rate delivery into a 64-byte FIFO is what
   the hardware does.
8. The gdb container runs outside the docker-test lock, because the stalled test holds it; it
   carries no test label, so no prune touches it.
9. Question 145's identical port traffic is asserted on the records (and the records-only hash),
   with the sidecar's provenance fields named as exclusions; `port_traffic_hash` keeps its
   whole-sidecar definition (question 175).
10. Early stop is the test's default, on the byte-identical A/B.
11. The Renode test is opt-in (`AV_RENODE_TESTS=1`) as well as image- and file-gated, because it
    runs for minutes.

### Found, not fixed this round

1. **A run's provenance depends on a test registry's port.** The posix half pushes the image to a
   throwaway loopback registry and binds `ContainerBinding.image = 127.0.0.1:<ephemeral
   port>/altavista-cfs-lockstep`, which enters `sos_configuration_hash`, so the posix whole-file
   `port_traffic_hash` differs between two posix runs of the same code (`a514ea92…` against
   `36329922…`). The records are identical. Making it stable needs the binding's hash to rest on the
   digest rather than the reference, which is a kernel change, not a fixture change: next round.
2. **The ELF build is not byte-reproducible.** Two builds differ in 12 bytes (cFE's `BUILDDATE`
   and `BUILDHOST`); pinning those leaves 3, a random gcc temp file name inside the dl-sym object
   `rtems-syms` generates.
3. **The test holds the docker-test lock for its whole body**, including the Renode half, which
   uses no Docker. Narrowing it to the posix half is a follow-up.

## Status (native manager, 2026-10-07) — question 239's carry-over round

The round question 239 chartered from question 238's carried items: a run's provenance independent
of the test registry's port, the Renode test's docker lock narrowed to the container half, and a
byte-reproducible RTEMS ELF. **All three are delivered**, with two more: a Renode bridge defect the
round's own gate found (root-caused and fixed), and the kit's `live_evidence.py` tag restore the
lead added mid-round. Six commits on `edge`, none pushed, none merged.

| Commit | What |
|---|---|
| `4ae2b08` | the Renode test takes the docker-test lock for its posix half only |
| `4839f3d` | the cFS test registries on a fixed loopback port; two posix runs, one `port_traffic_hash` |
| `cf4d695` | `third_party/rtems-container/build-elf.sh`, the reproducible ELF recipe, and its pytest |
| `2f2f52c` | the Renode bridge ends monitor commands in LF: one command, one prompt |
| `a873ce8` | `scripts/kit/docker_image_tags.py`; `live_evidence.py` never leaves a tag it did not own |
| this commit | this status section |

### A run's provenance and the registry's port (sil-plan's "Found, not fixed" 1)

- **The lead's ruling, mid-round:** the hashing rule is not changed. ADR-005 section 7 defines the
  canonical hash as SHA-256 over the deterministic encoding with only the message's own `hash`
  cleared; dropping `ContainerBinding.image` from it would be an ADR amendment, and where an image
  is fetched from is configuration, stable in a real deployment. The instability was the
  fixture's: a throwaway registry on an ephemeral port. The manager's first decision (clear
  `image` when a digest is set, in `canonical_sos_hash`) was withdrawn before any code was written.
- **Fix.** `push_cfs_image_to_local_registry` in `drm_attitude_control_cfs.rs` and
  `drm_attitude_control_renode.rs` publishes the registry on the fixed loopback port 19031
  (below every OS ephemeral range, away from macOS AirPlay's 5000), with typed `PortOccupied` /
  `StartFailed` errors and never an ephemeral fallback; the docker-test lock, held by every
  caller, serialises the test-owned registries on it. The push is retried while the registry
  starts, and the digest comes from the `RepoDigests` entry matching the reference, not index 0.
- **Proof** (`the_port_traffic_hash_is_the_same_across_two_separately_started_registries`): two
  runs of the same DRM, same run id, against two separately started registries (the first
  asserted removed before the second starts): `sos_configuration_hash` `9e2fb427…` and whole-file
  `port_traffic_hash` `b66a00fb…` equal, each the SHA-256 of its file, the two files
  byte-identical. The manager's re-run in a separate process gave the same two values.
  **Perturbation:** run B bound to `localhost:19031/…` (the same registry and digest under
  another reference) fails on `port_traffic_hash`, `b66a00fb…` against `a52c3d1f…`; the
  worker's perturbation (run B on an ephemeral port) fails on the reference and on
  `sos_configuration_hash`. The whole `drm_attitude_control_cfs` target: 5 passed, 0 skipped.

### The Renode test's docker lock (sil-plan's "Found, not fixed" 3)

The lock, the stale-resource prune, the registry with its image tag and the posix run's managed
container now live in the posix half's own block and are dropped (registry guards, then the lock)
before `spawn_renode_bridge`; only the posix `RunProducts` leaves it. The GMAT engine lock is
still held for the whole body. **Proof**, observed from outside the process by a non-blocking
`flock` probe every 0.5 s: held by the test's pid for 5.5 s of posix half, then free for 509
consecutive polls through the bridge boot and the Renode run; the test passed (998 records,
439 s). **Perturbation:** with the whole-body lock restored, the probe saw the test's pid holding
the lock after the Renode bridge was ready. The manager's probe through the first gate saw the
same shape (the test's pid for 6 s, then other tracks' docker tests taking the lock twice during
its Renode half).

### The RTEMS ELF, byte-reproducible (sil-plan's "Found, not fixed" 2)

`third_party/rtems-container/build-elf.sh`, in the manner of `services/cfs/build-shim.sh`:

- cFS from the seven pinned commits through the local mirrors (`fetch-cfs.sh`'s own
  `CFS_FETCH_DEST` into a fresh stage under `$HOME`, `GIT_ALLOW_PROTOCOL=file` so a missing commit
  fails rather than fetching), never the mutable `third_party/cfs`;
- `debian:bookworm-slim@sha256:88200866…` with the 93-package apt closure pinned by version and the
  container's whole `dpkg-query -W` set checked against `19ed31e5…`;
- the toolchain mounted read-only and checked against a manifest hash (`b56b32d8…`, sorted path,
  file SHA-256 or symlink target), refusing on mismatch;
- cFE's `BUILDDATE=202610050000`, `HOSTNAME=altavista-elf-build`, `USER=altavista` (the variables
  `generate_build_env.cmake` reads);
- **the last three bytes, root-caused:** the C file `rtems-syms` generates is named `cc` plus its
  own pid in base 62 (pid 8 → `cciaaaaa`, 133 → `ccjcaaaa`; the question 171 ELF carries
  `cc2gbaaa.c`, pid 4270), recorded as an `STT_FILE` symbol, so the bytes followed the
  container's process history. `rtems-syms -S <file>` names it; the recipe applies
  `-S <TARGET>-dl-sym.c` by an asserted, idempotent edit of the staged `RTEMS.cmake`. No ELF bytes
  are post-processed.

**Byte for byte:** four builds from four host staging paths (three by the worker, one by the
manager) give `core-cpu1.exe` SHA-256 `a5a5fe7b…2eb5`, 7,551,228 bytes, no host path in it. The
control without `-S` differs (symbol `ccS8aaaa.c`); `BUILDDATE` (worker) and `BUILDHOST`
(manager) perturbations differ. **It boots** under Renode to cFE OPERATIONAL with `IO_LOCKSTEP`,
`SCH_LOCKSTEP` and `ADCS` (uart0 identical to the question 171 ELF's but for the Build line), and
**it passes the Renode test** in the gate below. Against the question 171 ELF (`b5b2eac3…`) every
text symbol is at the same address; 452 data symbols moved by exactly +8 bytes from
`CFE_BUILD_ENV_TABLE` on, the pinned build-environment strings being longer.
`services/cfs/tests/test_build_elf_script.py` (14 tests) pins the invariants.

### A defect the gate found: the bridge's monitor commands drew two prompts each

The first gate (at `cf4d695`) failed the Renode test at STEP 87 of 100: `MonitorDesyncError`,
30 bytes (`1m(<machine>) \x1b[0`) queued before `cmd#1098`, then the shim saw the peer close and
the kernel aborted the STEP RPC. **Root cause, measured** against Renode 1.16.1's monitor with a
raw byte trace: CR and LF each end a line, so the bridge's `<cmd>\r\n` was a command plus an
empty line, and the empty line drew a second prompt (`emulation\r\n` 2 prompts, `emulation\n` 1;
five CRLF commands in one send, 10). `_read_reply_for` returned at the first prompt and left the
second to the 150 ms grace drain, which a loaded host outruns (the stray was exactly that prompt,
its leading `\x1b[33;` already read); `_read_n_replies` counted N prompts for N `WriteChar`
commands while 2N arrived, declaring a batch drained about halfway. M24_4g's "trailing ANSI
fragment after `RunFor`", attributed then to Renode-side buffering, was this second prompt.
**Fix** (`2f2f52c`): `MonitorClient.TERMINATOR` is a lone LF for `send` and `write_chars`; every
command form the bridge sends was probed with LF against the real Renode (one prompt each,
nothing late). The grace drain stays as a backstop and now reports what it finds, separating the
expected tail of the same prompt (` \x1b[0m`, written in small segments) from anything else.
**Proof:** a fake monitor in `tests/test_renode_monitor_client.py` that draws one prompt per line
terminator, the empty-line one delayed past the grace window (12 passed; with CRLF restored, 3
fail on `MonitorDesyncError`, the worker's and the manager's perturbation alike); the Renode test
with the reproducible ELF passed three times with zero unexpected grace-drain bytes, and in the
final gate below. Why it first appeared now: narrowing the lock (`4ae2b08`) lets other tracks'
docker work run during the Renode half (the first gate's probe saw two other holders in it), so
the host was busier there than in any earlier run; the defect was older than that.

### The kit's `live_evidence.py` and the plugin tag (the lead's addition)

`av-edge-plugin:local` moved to the proof kit's 2026-09-15 image a third time; the heavy manager
root-caused it to `scripts/kit/live_evidence.py::_load_and_verify_image`, whose `docker load -i`
restores the tarball's own tag host-wide and was never undone (question 234's rule reached the kit
test in `69de194`, not this script). The snapshot / load / verify-by-id / undo / closing-assertion
logic is now one helper, `scripts/kit/docker_image_tags.py` (`ImageTagGuard`), imported by both;
the undo runs in a `finally`; `live_evidence.py` runs its container from a run-scoped test-only
tag and asserts the host's tags unchanged from its outermost `finally`. **Proof:** the host's 26
tags byte-identical after both test files in both orders and after the manager's re-run, the
image-event log showing `load` then this code's own `tag`/`untag`; the undo disabled reproduces
the incident and the closing assertion catches it; the closing assertion disabled fails its own
test. The worker's perturbation left the plugin tag on the kit image and the worker put it back
by hand to the id it held before (`9d366f65…`), recorded in its evidence.

### Gate

Final gate at `a873ce8` (the code head), every phase in sequence, whole output under the
manager's scratchpad `gate-native-co/`, 2026-10-07 17:50 to 18:26:

| Phase | Result |
|---|---|
| `buf breaking proto --against /Users/probe/code/AltaVista/proto`; `buf lint proto` | clean, clean |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | 150 binaries, 1,502 passed, 0 failed, 4 ignored |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| the same for `av-kernel` and `av-run` with `--no-default-features` | clean, clean |
| the required-features lint's steps through `cargo-slot` | 3 groups linted, clean |
| `cargo deny check` | advisories, bans, licenses, sources ok |
| `cargo test -p av-orbital --no-default-features` | 142 passed |
| `cargo test -p av-kernel --no-default-features` | 653 passed |
| `cargo test -p av-kernel` | 889 passed, 1 ignored (the Renode test skips visibly here: no Renode binary in this worktree) |
| the Renode test, `AV_RENODE_TESTS=1`, ELF `a5a5fe7b…` | passed, 998 records both sides, records-only hash `8e518964…8fd2` both, 542 s; lock released after 15.5 s |
| `pytest -q -rs` under `cargo-slot --hold` | 1,057 passed, 17 skipped, 0 failed |

The 17 skips: 13 opt-in gates (`AV_KIT_WITH_*`, `AV_SBOM_REBUILD`, `AV_CFS_RUN_REPRO_BUILD`), and
three image-digest refusals, all heavy-track images rebuilt on this host between 17:09 and 17:13
today, outside this round: `av-edge-plugin:local` (`9d366f65…` against the recorded `4a43e35b…`),
`av-proposer:local` (`11813019…` against `93da1979…`), and `av-tiles:local` (`a83606dc…` against
`58e013a7…`). They are the lead's to reconcile at the merge. No compliance test failed.
The first gate, at `cf4d695`, had every phase green except the Renode test, which found the
bridge defect above (pytest there: 1,049 passed, 15 skipped).

### Decisions (numbered, for the lead to ratify)

1. Question 239's provenance item is fixed in the fixture under the lead's ruling, not in
   `canonical_sos_hash`; ADR-005 is unchanged and no recorded hash moved (no committed SOS carries
   a `ContainerBinding.image`).
2. The fixed registry port is 19031, the same in both cFS test files, with no fallback; the
   helper stays duplicated in the two files (the pattern they already had) rather than becoming a
   shared test module this round.
3. The manager's perturbation of the provenance proof used a second reference to the same registry
   (`localhost:19031`), so it isolates the reference string as the cause.
4. In the Renode test, registry guards drop before the lock, so no Docker resource of the test
   outlives the lock that protects it from other trees' prunes.
5. The three tasks ran in parallel in this worktree on disjoint paths; the Renode test file
   carried both task 2's and task 1's changes, so task 2 was committed from its worker's exact
   file version (`git update-index --cacheinfo`) and the port change went into task 1's commit.
6. The `rtems-syms` fix uses the tool's own `-S` option on the staged `RTEMS.cmake`, applied by
   `build-elf.sh`, not `build-cfs-cross.sh`, so the direct invocation stays as it was and the
   control switch (`ELF_NO_SYMS_FIX`) is one place.
7. The apt pin is the whole 93-package closure plus the dpkg-set hash, because cmake's and the
   host compiler's behaviour depend on the transitive set; apt still uses the network once per
   build (question 154's window).
8. The pinned cFE metadata values are labels, not the building machine: `202610050000`,
   `altavista-elf-build`, `altavista`.
9. The container-side paths `/workspace` and `/output/toolchain` are fixed by the recipe (the
   toolchain cmake already hardcodes the second); host staging paths are proven not to matter,
   container paths were not varied.
10. `target/debug/deps` held 44,634 entries at the start, under `cargo-slot`'s threshold, so it was
    not moved (question 236's rule).
11. The final gate ran at `a873ce8`, the code head; this commit changes only this file.
12. `docs/roadmap-status.md`'s native line is left to the lead, because the heavy track edits the
    adjacent line in the same round.
13. The bridge defect the gate found was fixed this round rather than recorded, because it stood
    between the reproducible ELF and its required Renode proof and its root cause was measured.
14. The bridge's readers still complete on the prompt's text, not on its colour-reset tail; waiting
    for the tail would hang on an uncoloured monitor. The tail is counted, never trusted.
15. The shared tag helper lives in `scripts/kit/`, not `tests/` (production tooling must not import
    a test module) and not `altavista/` (it would change the pinned wheel's contents and hash).
16. A failed load, a missing tag and a digest mismatch now restore tags too (the undo moved into a
    `finally`); the kit test's "image still exists after untag" check applies only when a test-only
    tag was kept, which on this host changes nothing.

### Found, not fixed this round

1. **The container instance's `dynamics_hash` still varies per run.** `ModelInfo::settings_hash`
   (`binding.rs`, around line 1589) folds in the binding's image, digest and the ephemeral
   `container.address`, so the container segment's `dynamics_hash` differs between two runs; it
   never reaches `port_traffic.pb`, and the tests already exclude it, but it is the same class
   of defect as the registry port.
2. **The apt pin depends on the live Debian mirror carrying those versions;** a point release that
   drops one fails the build loudly. `snapshot.debian.org` would make it permanent.
3. **`av-edge-plugin:local` no longer matches `services/edge-plugin/IMAGE_DIGEST.md`.** The
   image-event log shows `delete sha256:4a43e35b…` (the recorded image) and a rebuild tagged
   `sha256:9d366f65…` at 2026-10-07 17:13:00 CDT, not by this round's work; the plugin container
   test skips on the mismatch until the record or the tag is reconciled (question 236's rule:
   the lead's).
4. **The cFS test helper is duplicated** in `drm_attitude_control_cfs.rs` and
   `drm_attitude_control_renode.rs`, now including the fixed port; a shared test module would
   stop the two drifting.
5. **`alpine:latest` is not the image the container executor test recorded** (`294b683c…`
   against `28bd5fe8…`), so that heavy-track test skips; it skipped the same way last round.
