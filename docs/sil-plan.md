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
