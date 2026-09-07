# M24.1 -- Renode virtual-time slaving spike

status: complete

Task: docs/sil-plan.md M24 milestone; docs/open-questions.md questions 144, 148, 154, 156, 157.
This is a spike only. No kernel code (`crates/`, `drms/`, `gmatviz/`, `web/`) is modified.
Reviewable artifacts (question 157): this file (written incrementally, measurements first),
the installed/pinned Renode under `third_party/renode`, and the spike scripts under
`third_party/renode/spike/`.

## Read first (context, not re-litigated)

- Question 144: target processor decided by the user -- Zynq UltraScale+ RPU, Cortex-R5F
  lockstep, RTEMS 6.1 `zynqmp_rpu_lock_step` BSP, **Renode's mainline UltraScale+ Cortex-R5
  platform**; ZCU102/ZCU104 for HIL.
- Question 148: cFS RTEMS 6 support still landing upstream; pin a commit and report patches.
  (Out of scope for M24.1 itself -- M24's *RTEMS* build is a separate milestone item; M24.1 is
  the virtual-time slaving spike, which only needs a trivial firmware, not the real RTEMS ADCS
  build.)
- Question 154: network permitted for the install/build step only, exactly like the cFS fetch
  (`third_party/fetch-cfs.sh`). Tests and runs afterwards must not need it.
- Question 156: container tests must not leave tagged images behind; a pull-by-digest test
  skips with a visible reason when the registry image is absent.
- Question 157: this file is the reviewable artifact; write it incrementally, measurements
  first, because a worker's final chat message might never arrive.
- `docs/sil-plan.md` M24: "Renode installed at a pinned version under `third_party/renode`
  (portable build) or on a Linux host if the macOS build cannot slave virtual time... The
  virtual-time slaving spike comes first and its measurements decide whether the acceptance
  test can depend on it."

## Host

macOS 26.6.2, arm64 (Apple Silicon). Docker v29.6.2 available. Python venv at
`/Users/probe/code/AltaVista/.venv`.

## Answers to M23.2's two open unknowns (measurements, done first, before touching Renode)

### Unknown 1: can sch_lockstep's 1us `OS_TimerSet` interval fire the callback more than once per tick on a large elapsed-time jump?

**Answer: yes, confirmed both by reading OSAL's own source and by an independent experiment
that transcribes the exact loop.** This was already found and fixed in-repo at M23.4 (see
`services/cfs/apps/sch_lockstep/fsw/src/sch_lockstep_app.c`'s top comment, which reports the
same qualitative finding from an actual `docker run`). This task re-derives it independently
from OSAL's own source, because the in-repo comment's *specific magnitude* claim ("100,000
times") does not reproduce -- see the finding below.

Evidence, from `third_party/cfs/osal/src/os/shared/src/osapi-timebase.c`
(pinned commit `d2d877a69cff47452bcca274b309147d48e6c16f`, function `OS_TimeBase_CallbackThread`,
lines 460-506, function body used by both the shared thread and the posix impl -- confirmed
`third_party/cfs/osal/src/os/posix/src/os-impl-timebase.c` installs no override of this loop,
only of `external_sync` itself):

```
saved_wait_time    = timecb->wait_time;      // captured ONCE, before this tick's subtraction
timecb->wait_time -= tick_time;              // one big subtraction of the elapsed time
while (timecb->wait_time <= 0)
{
    timecb->wait_time += timecb->interval_time;
    if (timecb->wait_time < -timecb->interval_time)      // underflow clamp
    {
        ++timecb->backlog_resets;
        timecb->wait_time = -timecb->interval_time;
    }
    if (saved_wait_time > 0 && timecb->callback_ptr != NULL)   // fires on EVERY loop iteration
    {
        (*timecb->callback_ptr)(...);
    }
    if (timecb->interval_time <= 0) break;
}
```

`saved_wait_time` is fixed for the whole catch-up loop, so the callback fires once per loop
iteration for as long as `wait_time` stays `<= 0` -- **this can be more than once per
`external_sync` call**, confirming the M23.2 unknown as "yes."

**Expected value (closed form) and measurement:** independently transcribed this loop verbatim
into `third_party/renode/spike/osal_timebase_catchup_experiment.py` (no OSAL/kernel code
touched) and ran it. Expected, from reading the clamp: a fire count bounded near
`ceil(interval_time / interval_time) + 1 ~= 2-3`, *not* proportional to `tick_time /
interval_time`, because the "only allow wait_time to underflow by one interval_time" clamp
resets the runaway on the first loop iteration regardless of how large the jump is. Measured
(`python3 third_party/renode/spike/osal_timebase_catchup_experiment.py`):

```
Case A (sch_lockstep_app.c's exact scenario): interval=1us tick=100000us -> fires_this_call=3 backlog_resets=1 final_wait_time=1
Case B (1s jump, same 1us interval): interval=1us tick=1000000us -> fires_this_call=3 backlog_resets=1 final_wait_time=1
Case C (nominal, no jump): interval=1us tick=1us -> fires_this_call=1 backlog_resets=0 final_wait_time=1
Case D (2x interval jump): interval=1us tick=2us -> fires_this_call=2 backlog_resets=0 final_wait_time=1
Case E (interval matched to step period): interval=100000us tick=100000us -> fires_this_call=1 backlog_resets=0 final_wait_time=100000
Case F step 0..4: fires_this_call=3 each (cumulative 3,6,9,12,15; backlog_resets 1,2,3,4,5) -- repeated large jumps keep firing 3x per call, not growing
Case G sweep (interval=1us): tick_time=2 -> fires=2; tick_time=10/100/1000/10000/100000/1000000/10000000 -> fires=3 for all (bounded, NOT proportional to tick_time)
```

Exactly matches the closed-form expectation stated above: **fires = 3** for the sch_lockstep
scenario (1us interval, 100ms=100,000us jump), bounded regardless of how large the jump is,
never the naive `tick_time/interval_time` ratio.

**Finding (investigated, not glossed over -- question 157's standing review point about
suspiciously round numbers applies here):** `sch_lockstep_app.c`'s own top comment claims the
callback fired "100,000 times" and "wedged for minutes" against a real `docker run`. That
number is exactly `tick_time / interval_time` -- i.e. the naive expectation if the underflow
clamp did not exist. Reading the clamp code directly (`if (timecb->wait_time <
-timecb->interval_time) { ...; timecb->wait_time = -timecb->interval_time; }`) and confirming
by experiment above shows the clamp *does* exist in the pinned OSAL commit and *does* bound the
fire count to 3, not 100,000, for this exact scenario. Two explanations are consistent with the
evidence: (a) the "100,000" figure in the comment is a mischaracterization (an agent's
after-the-fact narrative that assumed the naive ratio rather than reading the clamp), while the
qualitative conclusion (multiple fires per tick, real, worth fixing) and the M23.4 fix itself
(moving to polling `psp_lockstep_tick_count()`, which is correct regardless of the exact
multiplier) both stand; or (b) something else amplified 3 callback-thread fires into ~100,000
observed side effects in the live container (e.g. a retry/backoff loop elsewhere, or the
16-deep `CmdPipe` overflow itself cascading). This task did not have access to the original
M23.4 `docker run` to distinguish (a) from (b), and does not re-run that historical repro
(RTEMS/cFS container work is out of scope for this spike). **Recorded as a discrepancy, not
silently resolved**: the qualitative M23.2 answer ("yes, more than once per tick") is confirmed
by both source reading and independent experiment; the specific "100,000" magnitude in the
existing comment is not reproduced by the pinned OSAL commit's actual (clamped) behavior and
should not be relied on as a measured number.

### Unknown 2: cross-check io_lockstep's port table against the bound instance's declared ports

**Answer: no mismatch found.** `services/cfs/apps/io_lockstep/fsw/src/io_lockstep_port_table.c`
declares three ports (`imu_in`, `startracker_in`, `wheel_torque_out`); compared field-by-field
against `drms/demo_attitude_control_controller_cfs.system.yaml` (the bound `SystemInstance` --
see `demo_attitude_control.sos.yaml`) and the identical native
`demo_attitude_control_controller.system.yaml`:

| port | C table (`io_lockstep_port_table.c`) | YAML (`*_controller_cfs.system.yaml`) | match |
|---|---|---|---|
| `imu_in` | apid 201, TO_BUS, is_command=false, 48 bytes, fields wx,wy,wz,ax,ay,az @ offsets 0,64,128,192,256,320 (all float64) | `imu_in`, PORT_DIRECTION_IN, `controller_imu_in_codec` apid 201, same 6 fields same offsets | yes |
| `startracker_in` | apid 200, TO_BUS, is_command=false, 32 bytes, fields qx,qy,qz,qw @ 0,64,128,192 | `startracker_in`, PORT_DIRECTION_IN, `controller_star_in_codec` apid 200, same 4 fields same offsets | yes |
| `wheel_torque_out` | apid 300, FROM_BUS, is_command=true, 24 bytes, fields tau_1,tau_2,tau_3 @ 0,64,128 | `wheel_torque_out`, PORT_DIRECTION_OUT, `controller_wheel_torque_out_codec` apid 300, is_command=true, same 3 fields same offsets | yes |

Direction sense also checks out: `LOCKSTEP_PORT_TO_BUS` (wire -> cFS software bus) corresponds
to `PORT_DIRECTION_IN` (into the instance) for both sensor ports; `LOCKSTEP_PORT_FROM_BUS` (cFS
bus -> wire) corresponds to `PORT_DIRECTION_OUT` for the wheel-torque command. This
reconciliation was already done carefully at M23.4 (that file's own comment documents finding
and fixing a real `msg_id` bug -- `CCSDS_V1_MSGID` folding in the command-type bit -- via an
actual `docker run`, not by static reading alone), and this independent re-check finds nothing
further to add.

## Renode install

**Pinned: Renode v1.16.1, macOS arm64 portable (.NET) build, not Docker.** Fetched by
`third_party/renode/fetch-renode.sh` (network-once, same convention as `fetch-cfs.sh`):
asset `renode-1.16.1-dotnet.osx-arm64-portable.dmg` from
`https://github.com/renode/renode/releases/download/v1.16.1/<asset>`, SHA-256
`99b8ae5897b8926ef179868d39a504fe5296555dc9c9b973718ddf3ab09175d9` (GitHub's own reported
asset digest, cross-checked locally with `shasum -a 256` -- matches, recorded in
`renode-1.16.1-osx-arm64/PINNED_SHA256`/`PINNED_VERSION`).

**Why portable, not Docker:** the brief says prefer portable if it actually runs, else
Renode's official Docker image for linux/arm64 pinned by digest. Checked first:
`docker manifest inspect antmicro/renode:1.16.1` (and `:1.16.1` re-tried against
`registry-1.docker.io` directly) returns a **single-platform (linux/amd64) v2 manifest**,
not a multi-arch manifest list -- there is no linux/arm64 digest to pin for this or any
other tag antmicro publishes. So the Docker path would only be "amd64 under QEMU
emulation," not a true arm64 pin, which is a materially different (slower, less
representative) thing than what the brief asks for. The portable build was tried instead
and **it runs**, verified by actually invoking it (not assumed):
- `./renode --version` -> `Renode v1.16.1.16858, build: d66b0c2a-202602160921, runtime: .NET 8.0.21`
- `file renode` -> `Mach-O 64-bit executable arm64` (native, not translated)
- No quarantine xattr on the extracted `Renode.app` (`xattr -lr` shows only
  `com.apple.provenance`, no `com.apple.quarantine`) -- Gatekeeper did not block execution.
- Headless mode confirmed: `--disable-gui --console` and `--disable-gui --hide-log -P <port>`
  (TCP Monitor server) both work with no window server needed.
- Loaded `platforms/cpus/zynqmp.repl` (this build's own bundled mainline Zynq UltraScale+
  platform, question 144's target), created a machine, loaded a bare-loop firmware onto
  `rpu0` (Cortex-R5F, ARMv7-R) at `0x0` (mapped to `atcm0`), and ran it -- `rpu0 PC` stays at
  `0x0` after running, consistent with the loaded `b .` instruction executing correctly.

**Deviation disclosed:** since Docker/arm64 was never actually exercised (no image to pull),
"portable build on this Mac" is the only evidged path here; no Docker fallback was run
because the portable build works. If the portable build had failed, the fallback would have
been amd64-under-emulation, which would itself have been reported as a deviation, not run
silently as if it were the real target.

**Bugs found and fixed in the spike's own script, not in Renode** (`third_party/renode/spike/setup.resc`):
1. The original `:description:` field was multi-line; Renode's script tokenizer rejects
   that (confirmed against this build's own `scripts/single-node/zynqmp_remoteproc.resc`,
   which uses single-line `:name:`/`:description:` only) -- error was
   `Could not tokenize here: ... platform (docs/open-questions.md question 144)...`. Fixed
   by moving the long description to a `#` comment and keeping `:name:` single-line.
2. `sysbus LoadBinary $CWD/loop.bin ...` failed ("Parameters did not match the signature")
   because `$CWD` resolved to the *process's launch working directory*
   (`.../Renode.app/Contents/MacOS`, wherever `renode` was invoked from), not the directory
   containing `setup.resc` -- confirmed by grepping this build's own scripts, which use
   `$ORIGIN` for exactly this ("directory of the currently-included script"), e.g.
   `scripts/complex/fomu/renode_etherbone_fomu.resc`'s
   `machine LoadPlatformDescription $ORIGIN/fomu_led.repl`. Fixed by switching to `$ORIGIN`.

## Slaving method chosen

**Monitor's `RunFor` with a fixed quantum, not the external control API.** This build
(1.16.1, portable macOS arm64) ships no general "step the whole machine's virtual time
from an external process" control API: `find . -iname '*ExternalControl*'` over the whole
distribution returns nothing, and the only external-integration pieces present are
`plugins/SystemCModule` and `plugins/IntegrationLibrary` -- source-level SystemC/Verilator
co-simulation bridges (`renode_bridge.cpp`, `renode_dpi.cpp`, a SystemVerilog package) for
bus/DPI-level HDL co-simulation, which would require writing and building a custom
Verilated/SystemC peripheral, not a turnkey time-stepping API, and are out of scope for a
spike measuring Renode's own time base against a bare-loop firmware. The only "external
stepping" pattern actually present and directly usable, anywhere in the bundled
`scripts/`/`tests/` trees, is `emulation RunFor "<seconds>"` (used across dozens of the
product's own `.robot` platform tests, e.g. `tests/platforms/STM32L072.robot`,
`tests/unit-tests/precise-pause.robot`). `emulation RunFor` combined with
`machine ElapsedVirtualTime` / `currentTime` (which reports both
"Current virtual time" and "Current real time" for the running machine) gives an external
driver process exact, queryable control over virtual time advance one quantum at a time --
which is what M24's bridge process needs. No attempt to use an external-control API was
therefore possible in this build; this is recorded as a finding (see go/no-go), not worked
around silently.

## Spike measurements

Driver: `third_party/renode/spike/measure_virtual_time.py`, speaking Renode's `-P <port>`
Monitor TCP protocol directly (no GUI). Per step it sends
`emulation RunFor "0.1"; currentTime` as one command line and parses both
"Current virtual time" and "Current real time" out of the reply -- the wall-clock number
used for the report is **Renode's own reported real time**, not the driver's socket
round-trip time, so python/telnet overhead is excluded from the wall-time measurement.
Firmware: `spike/loop.bin` (`b .`, branch-to-self, written by `spike/make_loop_bin.py`),
loaded onto `rpu0` at `0x0`. Platform: this build's bundled
`platforms/cpus/zynqmp.repl` (question 144's Renode target) via `spike/setup.resc`.

**Expected value (closed form), stated before measuring:** `RunFor "0.1"` advances the
emulation's virtual clock by exactly 100,000,000 ns; over N steps the cumulative virtual
time must be exactly N x 100,000,000 ns, with **zero** drift per step, because Renode's
`RunFor` argument is parsed into a fixed-point/integer time span (not accumulated as
floating point) and the monitor blocks the calling connection until that exact span has
been simulated -- there is no rounding path between "requested quantum" and "clock
advanced" for this API, unlike e.g. a real OS timer with scheduling jitter.

**20-step pilot run**, `--steps 20 --step-seconds 0.1` (used to validate the driver before
committing to the full 1,000-step run): virtual time exact matches **20/20**, max drift
**0 ns**, final cumulative virtual time exactly 2,000,000,000 ns (expected
2,000,000,000 ns, diff 0 ns). Real (Renode-reported) wall time per step: min 0.072597s,
p50 0.099946s, p90 0.110094s, p99/max 0.156661s, mean 0.100006s, pstdev 0.015526s.

**Two bugs found and fixed in the driver script during this pilot (not glossed over):**
1. An idle-gap-based "response finished" detector broke out of the read loop as soon as no
   bytes arrived for 20ms -- but Renode's Monitor echoes the command line almost instantly
   and then blocks for the ~100ms `RunFor` actually takes *before* printing the result, so
   the driver was reading only the echo and failing to parse every step. Fixed by reading
   until a specific marker (`Current real time:`) appears in the buffer instead of relying
   on any idle gap.
2. After the fix above, the *driver-observed* wall time per step was reading ~0.6s against
   Renode's own ~0.1s for the identical step -- a ~500ms discrepancy between the two
   numbers that would have been a red flag if not compared. Root cause: the post-marker
   "drain trailing bytes" logic reused the same 0.5s `poll_timeout` meant for "wait for the
   marker to show up at all," so every step paid one full 0.5s socket-timeout wait after
   the marker was already found. Fixed by using a short (30ms) grace timeout only after the
   marker is seen. Post-fix pilot: driver-observed p50 0.133866s vs Renode-reported p50
   0.099946s -- the two are now close (the ~34ms gap is real python/socket/ANSI-strip
   overhead in this driver, not a Renode-side effect), which is the expected relationship
   and is why both numbers are recorded rather than only the driver's.

**1,000-step run** (`--steps 1000 --step-seconds 0.1`, `spike/measurements_1000steps.csv`,
raw per-step CSV kept as a reviewable artifact alongside this file):

- **Virtual time reached per step: exact, matches the closed-form expectation stated
  above.** 1000/1000 steps had virtual-time delta exactly 100,000,000 ns; max drift over
  all 1000 steps was **0 ns**. Final cumulative virtual time after 1000 steps: exactly
  100,000,000,000 ns (100.000000000 s), expected 100,000,000,000 ns, diff 0 ns. **This is
  the headline finding the brief asks for, and there is no drift to report** -- Renode's
  `RunFor` genuinely advances virtual time by exactly the requested quantum, every time,
  over 1000 consecutive steps.
- **Wall time per 100 ms virtual step** (Renode's own "Current real time" delta, not
  driver/socket overhead): min 0.057940s, p50 0.099999s, p90 0.100095s, p99 0.126357s, max
  0.372108s, mean 0.100000s, population stdev 0.011494s.
- **Jitter over 1,000 steps** (measure used: per-step wall-clock delta in seconds, i.e.
  wall time consumed to advance exactly 100ms of virtual time; distribution reported as
  min/p50/p90/p99/max plus mean and stdev, not just a mean): see the numbers immediately
  above. The distribution is tight around 0.1s (p50/p90 within 0.1ms of each other) with a
  long right tail (p99 0.126s, one outlier at 0.372s) -- consistent with occasional host
  scheduling noise (this machine is not real-time-scheduled) rather than anything
  Renode-internal, since the *virtual* time is unaffected (still exactly 100ms every step
  regardless of how long the step took in wall time -- that is the entire point of a
  discrete-event/quantum simulator and is exactly what the go/no-go below relies on).

**Investigated, not glossed over: why is wall time per step so close to the 100ms virtual
step size?** This is exactly the kind of round/suspicious number the standing review point
calls out. A 1:1 wall:virtual ratio for a trivial `b .` firmware is not obviously expected
-- a modern host CPU should be able to emulate a tight branch-to-self loop far faster than
"real time" for a simulated Cortex-R5F. Investigated by reading this build's own test
suite rather than guessing: `tests/platforms/nucleo_h753zi.robot` line 264 says outright
"Use AdvanceImmediately to make the RunFor take less real time," and
`emulation SetGlobalAdvanceImmediately`/`SetAdvanceImmediately` appear across several other
bundled tests. Confirmed by experiment (`--extra-cmd "emulation SetGlobalAdvanceImmediately
true"`, 20-step run): real (Renode-reported) wall time per step dropped from a 0.1s-pinned
distribution (mean 0.100006s in the earlier 20-step pilot) to mean 0.064400s (min 0.040208,
p50 0.066551, p99/max 0.092263) -- materially faster, confirming that **by default Renode's
global time source throttles/paces `RunFor` to roughly real time** (`AdvanceImmediately`
defaults to `False`, visible directly in `machine ElapsedVirtualTime`'s own printed state:
`Advance immediately: False`), and disabling that throttle makes the identical firmware run
measurably faster. The residual ~64ms (not near-zero) with throttling disabled is real JIT
translation/execution cost for the number of instructions Renode's performance model says a
100ms Cortex-R5F step represents -- AdvanceImmediately removes the artificial real-time
wait, it does not make instruction execution free. **Both defaults are usable for M24**:
the default (throttled, ~1:1) pacing is arguably a feature for a lockstep bridge that also
needs to stay roughly in step with a real-time container binding, while
`SetGlobalAdvanceImmediately true` is available if the bridge instead needs Renode to keep
up with a kernel that is stepping faster than real time.

## M23.2 unknowns -- inherited-work verification

The two M23.2 answers above (OSAL catch-up-loop fire count, io_lockstep port-table
cross-check) were already present in this file when this task picked it up -- a prior
worker had completed them before returning no final report (question 157's scenario, this
time caught before repeating the work). Spot-checked rather than re-derived from scratch,
since the task brief says not to re-litigate: `third_party/cfs/osal/src/os/shared/src/osapi-timebase.c`
lines 466-489 do contain `saved_wait_time`/`wait_time -=`/`backlog_resets` exactly as
quoted; `services/cfs/apps/io_lockstep/fsw/src/io_lockstep_port_table.c` lines 62-77 do
declare `imu_in`/apid 201, `startracker_in`/apid 200, `wheel_torque_out`/apid 300 exactly
as quoted; `drms/demo_attitude_control_controller_cfs.system.yaml` does declare
`startracker_in`/apid 200/`PORT_DIRECTION_IN`, `imu_in`/apid 201/`PORT_DIRECTION_IN`,
`wheel_torque_out`/apid 300/`PORT_DIRECTION_OUT` exactly as quoted. No discrepancy found
between the prior worker's quoted evidence and the actual files. Both answers stand as
recorded above.

## M24 go/no-go

**Go.** The M24 acceptance test ("identical port traffic, container against Renode, under
lockstep") **can depend on virtual-time slaving via Renode's Monitor `RunFor`**, on the
evidence measured above:

1. Virtual time reached is **exact** (0 ns drift over 1000 consecutive 100ms steps, every
   step individually exact) -- a lockstep lockstep protocol that hands Renode a step size
   and expects the emulation's virtual clock to land exactly on `previous + step` can rely
   on that being true, with no accumulated-rounding failure mode to design around.
2. Wall time per step is bounded and has a *tight* central distribution (p50/p90 within
   0.1ms of the 100ms target at default throttling) with a bounded tail (p99 126ms, one
   372ms outlier in 1000 steps) -- workable for a bridge process that waits for each
   `RunFor` to return before advancing the kernel's own step, as long as the bridge does
   not assume a hard real-time deadline per step (it should not, in a lockstep binding --
   lockstep by construction waits for each side, so a slow step delays the joint step
   rather than corrupting it).
3. `AdvanceImmediately` gives a documented, tested lever (not a workaround invented here)
   to trade "pace near real time" for "run as fast as the host can" if M24's actual
   acceptance run needs to go faster than the ~1:1 default.

**Caveats disclosed, not silently absorbed into the "go":**
- This spike used a **bare `b .` loop**, not the real RTEMS 6.1 `zynqmp_rpu_lock_step` ADCS
  binary M24 itself will run. The wall-time-per-step numbers above are a floor, not a
  prediction for the real firmware -- a real cFS/RTEMS boot and ADCS control loop will
  execute far more instructions per 100ms virtual step than one branch, so wall time per
  step under the real binary should be expected to be **higher** than measured here (by how
  much is not measured by this spike and is exactly what M24's own build-out will need to
  re-measure once the real RTEMS binary exists).
- No true linux/arm64 Docker path exists for Renode 1.16.1 (recorded above) -- if a future
  host requires Docker specifically (e.g., CI running only containers), the only Docker
  option is amd64-under-emulation, which was not measured here and should not be assumed to
  have the same wall-time profile.
- Renode's `ExternalControl`/co-simulation surface in this build is limited to
  SystemC/Verilator source-level bridges, not a general step API -- if a future milestone
  needs Renode embedded as a library/co-simulated process rather than driven over its
  Monitor TCP protocol, that would need custom C++/SystemC integration work this spike did
  not attempt.
- Measurements are from **one host** (this Mac, arm64, currently idle apart from this
  task) over one run each; no cross-host or under-load repetition was done given the
  ~300-tool-use budget for this spike.
- **No tolerance was loosened anywhere in this spike.** The virtual-time-exactness check
  used exact integer-nanosecond equality (`==`, not `abs(diff) < epsilon`) throughout, by
  parsing Renode's own HH:MM:SS.nnnnnnnnn strings into integer nanoseconds specifically to
  avoid float-rounding artifacts masking a real drift (an early version of the driver
  computed deltas in float seconds and saw only 2/20 steps compare `==` to expected, purely
  from float64 rounding at the 1e-16 level -- fixed by switching to integer ns before this
  number was trusted for anything in this report).

## Network use during install (question 154)

Fetched over the network exactly once, during `fetch-renode.sh` (the install/build step):
`renode-1.16.1-dotnet.osx-arm64-portable.dmg` from
`https://github.com/renode/renode/releases/download/v1.16.1/`, verified by SHA-256 before
use. `docker manifest inspect antmicro/renode:1.16.1` (against Docker Hub) was also run
once, read-only, to check for a linux/arm64 digest -- no image was pulled or run, since the
portable build was used instead. No other network access was made; the spike itself
(`measure_virtual_time.py` driving Renode over a local TCP Monitor connection to
`127.0.0.1`) makes no outbound network calls, matching "tests and runs afterwards must not
need it."

## Files under third_party/renode (reviewable artifacts)

- `fetch-renode.sh` -- pinned fetch script (this task's edits: none needed, script was
  already correct and worked as-is).
- `renode-1.16.1-osx-arm64/` -- the fetched, pinned, extracted build (not committed to git,
  same convention as `third_party/cfs`).
- `spike/setup.resc` -- machine setup (fixed by this task: single-line `:description:`,
  `$ORIGIN` instead of `$CWD`; see "Renode install" above).
- `spike/make_loop_bin.py`, `spike/loop.bin` -- trivial bare-loop firmware and its generator.
- `spike/measure_virtual_time.py` -- the measurement driver (written by this task).
- `spike/measurements_1000steps.csv` -- raw per-step data backing every number in "Spike
  measurements" above.
- `spike/probe_monitor_protocol.py`, `spike/probe_current_time.py` -- one-off protocol
  probes used to design the driver; not spike measurements themselves, kept for anyone
  auditing how the driver's parsing choices were derived.
- `spike/osal_timebase_catchup_experiment.py` -- inherited from the prior worker (M23.2
  unknown 1), spot-checked and left as-is.

## Not done / could not do

- The real RTEMS 6.1 `zynqmp_rpu_lock_step` ADCS binary was **not** built or run under
  Renode -- M24.1 is explicitly the virtual-time slaving spike, not the full M24 milestone;
  a bare-loop firmware was used deliberately per the task brief ("a bare loop is fine; you
  are measuring the time base, not the software"). Wall-time-per-step under the real
  firmware is therefore unmeasured and flagged as a caveat on the go/no-go above, not
  claimed here.
- No Linux-host or Docker-arm64-under-emulation run was performed, since the portable macOS
  build worked and made that path unnecessary; the "amd64 under emulation" fallback is
  documented as a finding but not exercised.
- No cross-host repetition (different Mac, different load conditions) of the 1000-step
  measurement -- one run, on this host, is what backs the numbers above.
- No UART/Ethernet peripheral bridging was attempted (that is M24's own bridge-process
  scope, not this spike's).
- Did not attempt to build/use `plugins/SystemCModule` or `plugins/IntegrationLibrary`
  (SystemC/Verilator co-simulation) as an alternative slaving mechanism -- out of scope for
  a spike whose job is to evaluate the two methods the brief names (external control API,
  monitor `RunFor`), and no general-purpose external control API exists in this build (see
  "Slaving method chosen").
Nothing else identified as out of scope but undone; everything the task brief asked to be
measured or answered has a number, an answer, or an explicit "not measured/could not do"
entry above.

## Verification (both green)

- `.venv/bin/pytest -q` -> **355 passed**, matches the stated baseline exactly. No kernel
  code was touched by this task, so no regression was expected or found.
- `cargo test --workspace --exclude av-kernel` -> exit code 0, every test crate's own
  summary line reads `test result: ok` (0 failed across all reported suites, including
  `model_stm`, `stm_spike`, and all doc-tests). No kernel code was touched by this task.

status: complete. Renode pinned and verified running (portable macOS arm64 build, v1.16.1);
slaving method chosen and justified (Monitor `RunFor`, no ExternalControl API in this
build); virtual-time exactness measured (0 ns drift, 1000/1000 steps, integer-nanosecond
comparison); wall-time-per-step and 1000-step jitter distribution measured and the
suspicious 1:1 ratio investigated to a confirmed root cause (`AdvanceImmediately` default);
both M23.2 unknowns answered (one inherited and spot-checked, both confirmed against the
actual pinned source/YAML files); go/no-go stated as **Go** with caveats; pytest and cargo
verification both green; "Not done" list above is exhaustive for what this task did not
attempt and why.

## GO downgraded to "GO for bare loops" (lead, 2026-09-06, after M24.2b)

This spike's GO was measured against a **bare-loop firmware**, which never exercised the clock
driver. M24.2b then ran RTEMS's own `ticker` on this platform and it **stops advancing at virtual
t~14s of the required t>=35s**, with Renode's `RunFor` still exiting 0 and no error anywhere —
and the platform ran **~170 s real per virtual second**, two orders of magnitude slower than the
bare-loop figure recorded above.

**So the GO above stands only for bare loops.** It must not be read as clearance for M24.4, which
depends on the clock driver under lockstep. M24.4 is held until M24.2d produces a root cause for
both the stall and the slowness. The exact-virtual-time and AdvanceImmediately measurements above
remain valid as measurements; what is not established is that they hold for firmware that sleeps
on a timer interrupt.

## Renode UART RX-path limitation (M24.4b): what works and what does not

Characterised while root-causing the `IO_LOCKSTEP` handshake. Two separate things were wrong; this
paragraph is about the second, which is Renode's and not ours.

**Does not work:** two independent Renode terminal backends — the socket-based one and the
PTY-based one — do **not** forward externally-supplied bytes into this UART's **RX** path. Bytes
written by a host process to either backend never reach the guest's `read()`. This was established
against a guest that we had already proven reads correctly (see below), so it is not a guest-side
symptom.

**Does work:** the peripheral's own `WriteChar` method injects bytes into the guest's RX path
correctly, and the UART's **outbound TX** direction (guest -> host) is delivered correctly through
the terminal backend. So the bridge is built on `WriteChar` for host->guest and the terminal's
outbound path for guest->host; `third_party/renode/M24_4b/renode_bridge.py` uses exactly that
pairing, and it is proven end to end over real gRPC (Bind, three Steps, Reset, Step, Shutdown).

**Not to be confused with the guest-side bug**, which was ours and is fixed: `connect_to_shim()`
opened `/dev/ttyS1` without `tcsetattr`, so RTEMS's zeroed `rawInBufSemaphoreWait` made every
`read()` return 0 on an empty queue, which `read_all()` — correct for its only prior transport, a
Unix socket — treated as `PEER_CLOSED`. Fixed with `tcgetattr`/`tcsetattr` raw mode, `VMIN=1`,
`VTIME=0`. Both faults had to be understood before the handshake could work; fixing either alone
would have left it failing, which is why the earlier evidence looked contradictory (byte-identical
logs with and without a live peer).

## Update (M24.4d): the "silent no-listener" failure was ours, not Renode's — outbound direction reconfirmed reliable

M24.4c/M24.4d hit a *different*-looking failure: `CreateServerSocketTerminal` (the same call this
section already covers, used for the proven-working outbound/guest→host direction) would
occasionally report a clean `include` reply, with no error anywhere in the monitor log, while the
bridge's own `wait_for_port` never saw a connection within its 15s budget. This looked like a
second, previously-undiscovered Renode limitation on top of the RX-path one above. It was not.

Full root-cause (`third_party/renode/M24_4d_REPORT.md`, "Measurements"/"Root cause"): direct
`lsof` polling against the live Renode process (not the monitor log, not inference) shows the
monitor's `include` reply returning in ~0.3s — a full ~2.1-2.3s **before** the terminal's own
listening socket exists at the OS level, every time, in six isolated runs. The reply is not a
completion signal for the whole script; `machine LoadPlatformDescription` (the line immediately
before `CreateServerSocketTerminal`) is genuinely still executing when the reply arrives, and it
alone consumes essentially the whole gap. Under 5-way concurrent Renode contention on the same
host this gap grew from ~2.5s to ~5.1-5.7s — real, measured, load-proportional, not a fixed
constant — while `renode_bridge.py`'s own generated `.resc` script was, independently, being
written to a single path **hardcoded and shared by every invocation of the bridge on the host**
rather than the caller's own per-run scratch dir. Two Renode processes started close together in
time were directly shown (not assumed) to race on that shared file: one process's own monitor
prompt, read live off its own connection immediately after `include`, reported having loaded the
*other* process's script content.

Neither defect is in `CreateServerSocketTerminal` or in the outbound path this section already
documents as working: across nine independent Renode instances run for this task (isolated,
under contention, and concurrent), the terminal bound correctly on plain IPv4
(`TCP *:<port> (LISTEN)`, never IPv6) every single time it was given its own private `.resc` file
and enough wall-clock time to reach that line. **Fix, in `renode_bridge.py::start()`, not a
workaround:** the generated `.resc` path is now derived from the caller's own scratch directory
and suffixed with the process's own pid, so no two invocations can ever share a path again; the
uart-terminal `wait_for_port` budget was raised from 15s to 60s, sized against the
contention-measured figure rather than the isolated one. The outbound direction itself was left
exactly as M24.4b built it — `WriteChar` for host→guest, the terminal's outbound path for
guest→host — since it was never the thing that was broken.

## M24 close (M24.4g, 2026-09-07): monitor client fixed, byte comparison verified posix-only

M24.4g's own assigned bug was a real, distinct one: `renode_bridge.py`'s `MonitorClient` decided a
Monitor reply was complete after a fixed 0.3s idle gap; a slow `RunFor` (Renode echoes a command's
text almost instantly but can take seconds to actually finish it) left its own real completion
bytes to be read by the *next*, unrelated command instead — reproduced exactly (literal captured
traceback string included) against a frozen pre-fix snapshot in `tests/test_renode_monitor_client.py`,
then fixed: every command is tagged, exactly one is ever outstanding (a non-blocking peek before
every send raises a typed `MonitorDesyncError` on any undrained leftover), and a reply is accepted
only once the monitor's own known prompt reappears after that command's own echo, never on an idle
gap alone (plus a short, bounded post-completion grace drain for genuine millisecond-scale
stragglers — a deferred macro return-value line, a trailing ANSI code — found live while proving
the fix against the real bridge, not anticipated). Full design and unit-test proof in
`third_party/renode/M24_4g_REPORT.md`.

With that fixed, `byte_identical_port_traffic_between_posix_container_and_renode` got further than
any prior run in this milestone's own history: Renode boot, HELLO and BIND relay with zero monitor
desync, and **STEP 1 completes correctly**, its own `STEP_DONE` (sequence=1) captured byte-exact by
the independent file-backend cross-check M24.4f already wired up beside the hook. **STEP 2 onward
still does not reliably deliver the guest's own transmitted frame**, now through the Python
`CharReceived` hook too — the same "STEP 1 works, STEP 2+ does not" boundary M24.4e first found
(via `CreateServerSocketTerminal`) and M24.4f found again (via a polled file backend), now a third
time via a third, materially different transport. This pattern — the same failure boundary
surviving three unrelated guest→host mechanisms — is itself evidence the defect is not in any one
of those mechanisms specifically. See `docs/open-questions.md` question 171 for the full evidence
and the named next diagnostic step.

**Net effect on the M24 exit criterion**: verified **posix-container-only**
(`crates/av-kernel/tests/drm_attitude_control_cfs.rs`); the Renode half of
`byte_identical_port_traffic_between_posix_container_and_renode` remains a recorded no-go, gated
with a visible skip reason (`renode_unavailable_reason()`/`cfs_image_unavailable_reason()`,
M15.3's own convention) so it never fails silently or fails for the wrong reason. Renode's own
established evidence otherwise stands, unchanged and un-relitigated: TTC rate deviation 2.0e-4 (a
platform-file fix, question 164), WFI idle 0.588 s/s vs 1.548 s/s ticking, cFE plus all three
lockstep apps (`IO_LOCKSTEP`, `SCH_LOCKSTEP`, `ADCS`) booting to OPERATIONAL, RTEMS's own `hello`
test completing, `ticker` completing at nominal t≈120s, virtual time exact to the nanosecond over
1000 steps (M24.1) — and now, newly, STEP 1's own real lockstep port traffic delivered byte-exact
through Renode.

## The reproducer for question 171

`crates/av-kernel/tests/drm_attitude_control_renode.rs::byte_identical_port_traffic_between_posix_container_and_renode`
is the reproducer. It is marked:

```rust
#[ignore = "question 171: Renode port traffic beyond STEP 1 does not deliver; verified posix-container-only until resolved"]
```

so a recorded no-go is a **visible ignore, not a red suite**. It still compiles and is still run on
demand:

```
cargo test -p av-kernel --test drm_attitude_control_renode -- --ignored --nocapture
```

**Do not delete it.** It carries the bridge, the shim wiring, the Renode platform and the
posix-container comparison side, and it is the fastest way back into question 171 for whoever
resumes. The named next diagnostic step is a GDB-remote attach during a stalled STEP 2, reusing
M24.4e's proven tooling — the transport is already excluded as the cause, since the identical
"STEP 1 delivers, STEP 2+ does not" boundary reproduces across `CreateServerSocketTerminal`, a
`CreateFileBackend`, and the `CharReceived` Python hook.
