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
