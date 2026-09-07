# M24.4c -- posix-container vs Renode `RunProducts` byte comparison (closes M24)

status: **in progress**

Task: `docs/open-questions.md` questions 145, 153, 156, 157, 164; `docs/sil-plan.md`'s M24;
builds directly on `third_party/renode/M24_4b_REPORT.md` (root-caused and fixed the `IO_LOCKSTEP`
UART handshake; built and proved `third_party/renode/M24_4b/renode_bridge.py` end to end over
real gRPC; authored and hash-validated `drms/demo_attitude_control_controller_renode.system.yaml`)
and `third_party/renode/M24_4_REPORT.md` (the platform file, TTC rate, WFI-idle fix).

Written incrementally per question 157's own rule. Sized for ~300 tool uses; long Renode runs are
backgrounded, not polled tightly.

## Not done yet (running list, updated as items close)

- [ ] 1. `crates/av-kernel/tests/drm_attitude_control_renode.rs`: a Rust test that runs the
      authored DRM through `av_kernel::drm::execute` with the `"controller"` instance bound to
      the real Renode/RTEMS/`IO_LOCKSTEP` guest via `renode_bridge.py` + `crates/av-lockstep-shim`
      as plain child processes (M13.2's `container.address`-only already-running-process path,
      not `ContainerBinding.image`).
- [ ] 2. Per-step byte comparison of port traffic between the posix-container binding
      (`drms/demo_attitude_control_controller_cfs.system.yaml`) and the Renode binding, over the
      identical DRM/seed -- question 145's port-boundary determinism bar. FSW internal task order
      recorded (in the run's own stdout/log), never asserted.
- [ ] 3. Renode-gated with a visible skip reason (M15.3's convention) when Renode/the cross-built
      ELF/the bridge/the Docker image is unavailable.
- [ ] 4. Question 156 housekeeping applied to this task's own Docker usage: label every
      container/image this test creates and prune by that label before starting, using the
      already-built `av_lockstep::docker::{prune_stale_test_resources, test_label_args,
      test_run_id}` (built for exactly this, already used by
      `crates/av-kernel/tests/drm_container.rs`'s own Docker-lifecycle test, but **not** yet used
      by the pre-existing `crates/av-kernel/tests/drm_attitude_control_cfs.rs`, M23.4 -- that gap
      is disclosed, not fixed, since that file is not this task's own file to edit unless needed).
- [ ] 5. Full verification: `cargo test -p av-kernel` (665 passed baseline), `cargo test
      --workspace --exclude av-kernel`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo deny check`, `.venv/bin/pytest -q` (404 passed baseline).

## Design, decided before writing code

- **Binding shape.** `drms/demo_attitude_control_controller_renode.system.yaml`'s own header
  comment already commits to this: `container.address` (M13.2's already-running-process path),
  never `ContainerBinding.image` -- there is no Docker image lifecycle for a Renode-bound
  instance, `docker run` is never invoked for it. `crates/av-kernel/src/drm/binding.rs`'s
  `materialize_container` does **not** retry the Docker-path's connect-then-Bind readiness loop
  for the `container.address` path (`spec.image.is_some()` gates that retry) -- so this test must
  itself poll `av_lockstep::BlockingLockstepClient::connect_plaintext` until the shim's gRPC
  server is genuinely serving (which only happens after `av-lockstep-shim`'s own Unix-socket
  accept + lockstep-local handshake with the bridge completes) before calling `execute()`, the
  same pattern `crates/av-kernel/tests/drm_container.rs::spawn_lockstep_ref` already uses for the
  Python reference peer.
- **What "byte comparison" means here.** Not two Renode runs (that would be
  `drm_attitude_control_cfs.rs`'s own within-binding determinism shape, already covered at
  M23.4). This is a **cross-binding** comparison: the identical DRM/SosConfiguration content
  (same truth plant, same star-tracker/IMU noise, same seed) run once with `"controller"` bound to
  the posix-container `altavista-cfs-lockstep:local` image and once bound to the Renode/RTEMS
  guest -- both running the byte-identical `services/cfs/apps/adcs` C source, differing only in
  OSAL/platform underneath (question 147: "same app source, different OSAL; the P2
  identical-traffic criterion becomes a portability test"). If the control law's floating-point
  arithmetic is genuinely architecture-independent (no transcendental calls in `adcs_control.c`'s
  P/D law -- multiply-add only), byte-identical sensor packets in should produce byte-identical
  wheel-torque packets out at every step, and since each run's own truth trajectory is driven
  entirely by its own commanded torque, the entire trajectory should stay byte-identical
  step-for-step by induction. This is a real, falsifiable prediction, stated before measuring.
- **Comparison fields, and the one deliberately excluded field.** Mirrors
  `drm_attitude_control_cfs.rs::byte_identical_run_products_across_two_separately_spawned_cfs_containers`
  exactly: `trajectories[name].samples`/`.event_ids`/`.state_space_id`/segment
  `start_tai_ns`/`end_tai_ns`/`dynamics_model` compared for every instance, plus `products.events`
  and `products.scores`. `dynamics_hash` is compared for the native (truth/star_tracker/imu)
  segments (should genuinely match -- same GMAT settings, unaffected by the controller's binding
  kind) but excluded for the `"controller"` instance's own segment, for the identical
  already-established reason `drm_attitude_control_cfs.rs` excludes it: `ModelInfo::settings_hash`
  folds in `container.address`, which is inherently different between the two runs (a Docker
  ephemeral host port vs the shim's own ephemeral gRPC port) -- pure local plumbing no port
  message or trajectory sample ever carries. Top-level `RunProducts.provenance` and each
  trajectory's own `Provenance` (which embed `run_id`/`sos_hash`/`drm_hash`/`sys_hash` -- distinct
  by construction between two runs against two different `SystemDefinition`s) are excluded too,
  for the same class of reason, and disclosed as excluded rather than silently skipped.
- **Wall-clock budget, reasoned before running.** `M24_4_REPORT.md`'s own measured rate (1.548 s
  wall per virtual s while ticking) and `M24_4b_REPORT.md`'s own measured per-step monitor
  overhead (each `WriteChar`-chunk/`RunFor` monitor round trip pays a ~0.2-0.3 s idle-gap drain,
  independent of actual compute) put a 10 Hz, N-step run's own wall time at roughly `N * 0.6-0.9 s`
  plus a one-time ~15 s HELLO/BIND handshake -- a short (single-digit-second) DRM window is
  chosen deliberately to keep this within the test-tool timeout while still exercising enough
  steps for a real, multi-sample comparison; the exact duration and the measured wall time are
  recorded below once run.
- **Only one Renode-bound `#[test]` in this file.** `renode_bridge.py`'s own `start()` writes a
  fixed, unparameterized path (`third_party/renode/M24_4b/renode_bridge_generated.resc`) --
  two Renode-bound tests running concurrently in the same `cargo test` process would clobber each
  other's `.resc` file. Rather than patch that pre-existing M24.4b script (out of this task's
  stated scope unless needed, and the byte comparison itself does not need a second Renode
  scenario), this file deliberately contains exactly one Renode-driving `#[test]`.
- **Process cleanup beyond question 156's own container/image scope.** `renode_bridge.py` owns
  its own Renode subprocess internally; a plain `SIGKILL` of the bridge's own Python process (the
  default `Drop`-guard behaviour used elsewhere in this codebase) would orphan Renode, since
  Python's `finally: bridge.stop()` never runs against `SIGKILL`. The bridge child is spawned in
  its own process group (`std::os::unix::process::CommandExt::process_group(0)`) and its guard
  signals the *whole group* (`kill -TERM -<pgid>`, then `-KILL` as a fallback) on drop, so a
  panicking assertion cannot leave a Renode process running the way an interrupted Docker test
  once left containers running for four hours (question 156's own amendment).

## A real defect found by actually running a real DRM step (not the M24.4b smoke test's empty-input case)

**What was measured, before assuming anything.** The posix-container half of the comparison
(`altavista-cfs-lockstep:local`, `services/cfs/apps/adcs`) ran cleanly end to end (3 s @ 10 Hz in
10.7-18.7 s wall time across two runs -- Docker daemon load varies run to run). The Renode half
booted, handshook, and BIND'd correctly (UART0 transcript: `IO_LOCKSTEP: bound, entering step
loop`, `CFE_ES_Main entering OPERATIONAL state`, `IO_LOCKSTEP: first release_tick(...) accepted`,
`SCH_LOCKSTEP: first wakeup transmit`, `ADCS: first wakeup received`), then the very first STEP
carrying real star-tracker/IMU packets hung: the kernel's `Step` RPC failed with `"the peer
closed the connection cleanly between frames"` after `renode_bridge.py` itself raised
`TimeoutError: timed out` reading the guest's reply (`bridge_stderr.log`, captured after adding
per-process stdout/stderr log files to this test's own scratch dir specifically to diagnose this
-- an unclaimed `Stdio::piped()` would have hidden this traceback entirely). The UART0 transcript
never advanced past `"ADCS: first wakeup received"` -- no further guest execution happened at
all, confirmed by a 30-real-second blocking read producing zero bytes (a paused emulated CPU
cannot ever produce more output no matter how long a caller waits for it).

**Root cause, pinned to the actual bridge code, not assumed.** `renode_bridge.py`'s own `STEP`
handling (pre-fix) issued `RunFor(delta_s)` -- the DRM's own nominal step period (100 ms at
10 Hz) -- before checking for the guest's reply. That is enough virtual time for `HELLO`/`BIND`
(no cross-app coordination needed -- `IO_LOCKSTEP` alone parses and replies) but was not measured
against a real `STEP`'s actual chain: decode+transmit the input packets, `psp_lockstep_
release_tick`, then `SCH_LOCKSTEP` (polling `psp_lockstep_tick_count()` every
`SCH_LOCKSTEP_POLL_DELAY_MS` = 1 ms) dispatches `ADCS`'s control law, which publishes onto the
`wheel_torque_out` pipe `IO_LOCKSTEP`'s own `CFE_SB_ReceiveBuffer` is waiting on
(`IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS` = 2000 ms). Renode's own `RunFor` returns as soon as its own
virtual-time budget is spent, whether or not the guest has finished that tick's work -- so a
100 ms budget left `IO_LOCKSTEP` genuinely still blocked inside `CFE_SB_ReceiveBuffer` with the
CPU paused, not merely slow. This is exactly the "not yet measured" risk `M24_4b_REPORT.md`'s own
"Final status" section flagged in advance (item 2: "whether a real DRM's own step period... is
long enough once real sensor packets are flowing every step... was not measured").

**Why granting more virtual time is safe, not a fudge -- verified by reading the actual dispatch
code, not assumed.** `services/cfs/apps/sch_lockstep/fsw/src/sch_lockstep_app.c`'s own top
comment (an M23.4 fix already recorded there) establishes that the one schedule slot's wakeup is
dispatched strictly `for (; last_seen_tick_count < current_tick_count; ...)` against
`psp_lockstep_tick_count()` -- a counter incremented exactly once per
`psp_lockstep_release_tick()` call, never by a free-running timer. Granting the guest more real
(virtual) CPU time to finish a tick it has already been released into cannot cause a second,
spurious wakeup/control-law evaluation for that same tick -- there is nothing periodic in this
path left to over-fire. This ruled out the riskier alternative (an incremental "grant a little
more time, then retry the read" loop mirroring `wait_for_rxen`'s own pattern) which would have
needed to abandon a partially-read TCP frame on a short per-attempt timeout -- `read_exact`'s own
buffer is local to one call and is not resumable, so a timed-out partial read silently drops
already-consumed bytes and desynchronizes all framing after it. A single, generous, fixed `RunFor`
floor before ever attempting the read avoids that risk entirely.

**The fix**, `third_party/renode/M24_4b/renode_bridge.py`'s `STEP` handling: `RunFor(max(delta_s,
STEP_MIN_VIRTUAL_S))` with `STEP_MIN_VIRTUAL_S = 3.0` -- the identical value (`>=
IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS` plus margin) `bridge_smoke_client.py` already proved live for the
harder (always-times-out) empty-input case, reused here rather than inventing a new number.
Disclosed cost: every `STEP` now costs roughly 3 virtual seconds regardless of the DRM's own
nominal step period, which is why this comparison's own scenario length
(`COMPARISON_DURATION_S`) is deliberately short -- see the "measured result" section below for the
real wall-clock cost this produced.

## Measured result (filled in once a clean run completes)

(placeholder -- filled in next)

## Not done / disclosed gaps (updated as the task proceeds)

- `renode_bridge.py`'s `STEP_MIN_VIRTUAL_S = 3.0` is a safe, generous, *measured-precedent* floor,
  not a tightly-measured minimum -- the real minimum virtual time a warmed-up `ADCS` control-law
  evaluation needs under Renode was not separately isolated (would need its own instrumented
  probe, out of this task's own budget once the byte comparison itself was the priority). Disclosed,
  not hidden: a future task could tighten this if per-step wall time ever becomes a real
  constraint.
