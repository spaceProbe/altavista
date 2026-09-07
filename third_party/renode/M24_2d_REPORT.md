# M24.2d -- root-causing the RTEMS `ticker` stall and the ~170 s/s Renode slowness

status: **root-caused, both symptoms, single mechanism family.** (1) The originally-reported
stall: Renode's `Cadence_TTC` model counts at ~1/3 the rate the BSP's clock driver assumes (no
`frequency:` override in `zynqmp.repl`), so a 60-virtual-second `RunFor` budget runs out before
`ticker`'s own 35-RTEMS-second exit condition, mid-test -- confirmed by direct register
measurement. (2) The "second, more severe" slowness this task originally flagged as
unresolved (see the untouched section below, kept for the record) is **also now root-caused,
in a later continuation session on this same task/budget**: it is RTEMS's own
`bsp_reset()` -- an intentional, non-`wfi` infinite busy-spin loop entered after every clean
test exit because `BSP_RESET_BOARD_AT_EXIT` defaults to on and this BSP does not override it --
spinning forever on a register that Renode's shipped `zynqmp.repl` backs with a bare `Tag`
stub, not a functional reset controller. Confirmed at the source, RTEMS-build-option-spec, and
compiled-disassembly level (`bsp_fatal_extension` contains an unconditional `bl bsp_reset`;
`bsp_reset`'s loop body has no `wfi`). See "Continuation session" below (after the original
"Hypothesis status (final)"/"Root cause" sections, left as originally written) for the full
chain of evidence and what it revises. Original sections below are kept verbatim as the
session's real-time record, not edited in place, per this task's own "write incrementally,
don't silently rewrite" discipline.

Task: root-cause (not work around) two findings from M24.2b/M24.2c
(`third_party/rtems-container/REPORT.md`), recorded as open questions 144/157 in
`docs/open-questions.md`:

1. **Stall**: RTEMS's `ticker` sample on Renode's mainline Zynq UltraScale+ Cortex-R5 platform
   ticks correctly (TA1/5s, TA2/10s, TA3/15s) through virtual `09:00:14` (~t=14s), then stops
   advancing. `RunFor "60"` still exits code 0, no error/warning anywhere in the monitor
   stream (confirmed by M24.2c's byte-level dump, reproduced 3x identically).
2. **Slowness**: this full Zynq UltraScale+ platform simulates `hello` at roughly 170 s of
   real wall time per virtual second, two orders of magnitude slower than the M24.1 spike's
   ~1:1 bare-loop measurement on the same platform file.

M24.4 (the Renode lockstep bridge) is held on this task producing an actual root cause, not a
timeout/quantum/sample workaround.

Reviewable artifacts (question 157, written incrementally, hypotheses and measurements as
they land): this file; any new scripts under `third_party/rtems-container/` (this task's
instrumentation, additive only); this task does not modify
`third_party/rtems-container/REPORT.md`'s existing content (append only) or
`third_party/rtems/` (NO-GO evidence, read-only).

## Hypotheses, stated before measuring

(a) **What is the RPU actually doing at the stall?** Candidates: (a1) CPU in WFI waiting on a
    TTC match interrupt that never arrives (the GIC never re-asserts, or the TTC never
    re-fires); (a2) CPU spinning in a software loop that never observes the expected
    condition (e.g. the `xil_ttc_clock_driver_support_at_tick` catch-up `while` loop spinning
    forever because the match computation never lands in the future); (a3) CPU faulted
    (prefetch/data abort, undefined instruction) and parked in an exception handler that does
    not print anything. Test: instrument with per-peripheral `logLevel`, `sysbus
    LogPeripheralAccess` on `ttc0` and `rpuGic`, poll `rpu0 PC`/`rpu0 IsHalted` across the
    stall boundary, and read TTC/GIC registers directly via `sysbus ReadDoubleWord`.

(b) **TTC/GIC configuration.** The BSP's `xil-ttc.c` (read directly from the container-fetched
    RTEMS 6.1 source, `third_party/rtems-container/work/rtems-src/bsps/shared/dev/clock/xil-ttc.c`)
    programs: no prescaler (`CLK_CNTRL=0`), counter enabled in overflow+match mode
    (`CNT_CNTRL = EN_WAVE|MATCH`), a single MATCH_0 compare updated every tick by adding a
    fixed `irq_match_interval`, IER enables only `IXR_MATCH_0`. Reference clock 100 MHz
    (`XIL_CLOCK_TTC_REFERENCE_CLOCK=100000000`), base address `0xff110000` = **ttc0** channel
    0, IRQ vector 68 = GIC SPI 36 (`vector - 32`), matching `zynqmp.repl`'s
    `ttc0 [0-2] -> apuGic@[36-38] | rpuGic@[36-38]`. The driver's own header
    (`xttcps_hw.h`) explicitly widened `XTTCPS_COUNT_VALUE_MASK`/`MATCH_MASK` from 16 bits
    (Zynq-7000, `ARMA9`) to **32 bits** for ZynqMP (`!ARMA9`, which this RPU build is) --
    i.e. the software assumes a 32-bit free-running counter and 32-bit match compare.
    **Leading sub-hypothesis (the task's stated strong lead):** if Renode's `Cadence_TTC`
    model does not honour the 32-bit width (e.g. models the older Zynq-7000 16-bit counter/
    match registers, or truncates on write/compare), the match arithmetic silently breaks.
    At 100 MHz with a 10 ms tick (RTEMS default), each tick advances the match target by
    1,000,000 counts; a 16-bit truncation (mod 65536) would break within one or two ticks,
    not 14s, so a naive 16-bit-counter theory needs the actual numbers checked against the
    observed ~14s, not assumed. Also check GIC edge-vs-level: `ARM_GenericInterruptController`
    (GICv1 for rpuGic) default SPI configuration and whether the TTC's match IRQ is
    level-sensitive (real HW: TTC match IRQ is level, cleared by reading ISR) and whether
    Renode's GIC model re-latches a level IRQ that the peripheral re-asserts before the CPU
    re-enables interrupts.

(c) **Isolate BSP vs Renode model.** Run the same `ticker.exe`... not possible verbatim (ELF
    is arm/EABI5 built for Cortex-R5F specifically, tied to `zynqmp_rpu_lock_step`), so
    isolation here means: does Renode's **A53 zynqmp** platform + a *different* clock source
    (APU's `ARM_GenericTimer`, not the TTC) run some other RTEMS/bare-metal timer sample to
    completion on the same `zynqmp.repl` file, which would implicate the TTC model
    specifically rather than the whole platform file. A true apples-to-apples "same BSP,
    different core" isn't available without an aarch64 RTEMS BSP build (out of scope to
    build new toolchains here). QEMU: checked for in the container toolchain/host; if absent,
    recorded as not available rather than skipped silently.

(d) **Slowness.** Measure instructions executed per virtual second on `rpu0` across the run
    (including across the stall boundary -- a flat instruction-count region during a period
    Renode still reports as "running" would itself indicate WFI/idle fast-forwarding, which
    the M24.2c report already suspected qualitatively but did not measure). Test
    `AdvanceImmediately`. Determine whether the idle task halts (WFI, `IsHalted` true or a
    CPU sleep state) or spins (steady non-zero instruction rate with no forward progress).
    Explicitly test early: **a WFI that Renode does not honour (never wakes the CPU on the
    TTC interrupt) would produce both symptoms at once** -- the stall (CPU parked forever)
    and, if Renode still "executes" WFI at some rate, part of the slowness. This is the
    highest-value single test and is run first.

## Plan (before measuring)

1. Reproduce the stall under direct monitor control (already available: `ticker_noquit.resc`
   from M24.2c) with fine-grained `RunFor` steps bracketing the 14s boundary, reading
   `rpu0 PC`, `rpu0 IsHalted`, and the TTC0/rpuGic registers after each step -- this answers
   (a) and (d)'s WFI question together.
2. Read the register trace against the BSP source math to answer (b): does the 32-bit
   assumption hold in Renode's model, and does a level-IRQ re-latch happen.
3. Attempt (c)'s isolation to the extent the container/toolchain allow.
4. Measure instructions/virtual-second and `AdvanceImmediately` for (d).

## Instrumentation built for this task (additive, under `third_party/rtems-container/run-renode/`)

All of these drive Renode's Monitor `-P <port>` TCP protocol directly (the proven method from
`third_party/renode/spike/measure_virtual_time.py`), never the CLI-positional-argument mode
(known hang, see `third_party/rtems-container/REPORT.md`). None of them modify
`ticker.resc`/`hello.resc`/`ticker_noquit.resc` (M24.2c's, reused read-only) except by
`include`ing them exactly as-is.

- `diag_ttc_trace.py` -- first attempt; has a read-buffering race that misattributes register
  *values* to the wrong step label (values themselves are real, just printed under the
  previous step's heading). Superseded by v2/v3, kept for the record.
- `diag_ttc_trace2.py` -- second attempt; regex bug matches the *echoed command's own address*
  instead of the returned value (giveaway: a constant `0xFF110018` "value" that is exactly the
  register address). Superseded by v3.
- `diag_ttc_trace3.py` -- working version: `currentTime` ground truth (Renode's own reported
  virtual/real time, exact per M24.1) alongside individually-read TTC0 registers (taking the
  *last* hex token in each reply, not the first/echoed one) and GIC registers, across 30
  `RunFor "1"` steps plus a final `+60s` jump. Output: `diag_ttc_trace3.log`,
  `diag_ttc_trace3_clean.log`.
- `diag_gic_check.py` / `diag_gic_check2.py` -- probing which monitor syntax actually reads
  GIC distributor registers (`sysbus ReadDoubleWord <addr>` alone errors
  `Can't verify current CPU in the given context`; `sysbus ReadDoubleWord <addr> rpu0` works).
- `diag_advance_immediately.py` -- A/B wall-clock-speed comparison with
  `emulation SetGlobalAdvanceImmediately true`.
- `diag_chunking_determinism.py` -- controlled A/B: does splitting the same total nominal
  `RunFor` time into one call vs many calls change the outcome (byte-for-byte UART compare)?
- `diag_full_completion_check.py` -- the decisive test (see hypothesis (a)/(b) below): does
  `ticker` ever reach `*** END OF CLOCK TICK TEST ***` given enough nominal `RunFor` budget and
  **zero** register polling during the run (to rule out the diagnostics' own reads
  interfering), or does it stop making progress forever regardless of budget?

## Findings, in the order they were established

### 1. Renode's own virtual-time clock is exact and RunFor-chunking-invariant (not the bug)

`currentTime` (Renode's own report, not this task's arithmetic) matched the requested
cumulative `RunFor` duration exactly at every sampled point in `diag_ttc_trace3.py` (1.0s,
2.0s, ..., 30.0s, then +60s -> 90.0s, all exact) -- consistent with M24.1's proven 0-ns-drift
measurement. `diag_chunking_determinism.py` additionally confirmed that splitting the same
20-nominal-seconds `RunFor` budget into one call vs twenty 1-second calls produces
**byte-for-byte identical** `ticker_uart.log` output. **Renode's global time base and the
granularity of `RunFor` calls are excluded as root causes.**

### 2. The RPU is not faulted and is not permanently parked in an unresponsive WFI

`rpu0 PC` sampled after every `RunFor` step (32+ samples across `diag_ttc_trace3.py` and
`diag_gic_check2.py`) always reads `0x400045e6`. Disassembling `ticker.exe` (via the
container-built `arm-rtems6-objdump`, run inside a fresh `debian:bookworm-slim` container with
the host's already-built `output/toolchain` bind-mounted read-only -- the toolchain is a Linux
aarch64 ELF, confirmed by `file`, so it cannot run on the macOS host directly) shows this is
**exactly** RTEMS's own idle loop:

```
400045e4 <_CPU_Thread_Idle_body>:
400045e4:  bf30      wfi
400045e6:  e7fd      b.n  400045e4 <_CPU_Thread_Idle_body>
```

`0x400045e6` is the branch-back instruction immediately after `wfi`, i.e. every sample caught
the CPU in RTEMS's idle task, which is the expected, correct resting state for the large
majority of time in a lightly-loaded RTOS -- not evidence of a fault or a spin bug by itself.
`rpu0 IsHalted` was `False` at every sample (including deep into the region with no further
UART output), and `rpu0 ExecutedInstructions` grew steadily and continuously the whole time
(e.g. ~8,000-10,000 instructions per nominal RunFor second, never flat/zero) -- **the CPU is
not stopped, not faulted, and not stuck spinning outside the idle loop; `IsHalted` in this
Renode build reflects a debugger-style halt, not CPU low-power/WFI sleep state, so it is not a
usable WFI indicator by itself.**

### 3. TTC0's match-tracking machinery keeps running well past the point UART output stops -- ISR/GIC state show no sign of a dropped interrupt

Directly reading TTC0 registers off the bus (not inferred) at multiple points, including tens
of nominal seconds after the last UART line appeared: `MATCH_0` was consistently kept **close
to** `COUNT_VALUE` (within roughly one `irq_match_interval`, i.e. behaving like a periodic
timer approaching its next compare, never frozen at a stale value and never wildly divergent)
at t~30s (`COUNT_VALUE=0x3B992956`, `MATCH_0=0x3B9ACA00`, diff ~2.6M), t~45s
(`COUNT_VALUE=0x5965CB06`, `MATCH_0=0x59682F00`, diff ~1.0M), and t~90s
(`COUNT_VALUE=0xB2CBB016`, `MATCH_0=0xB2D05E00`, diff ~306K) -- i.e. the driver's own
`xil_ttc_clock_driver_support_at_tick` handler (the only code that advances `MATCH_0`) was
still being invoked and was still successfully re-arming the compare, long after the RTEMS
clock/task-wake output visibly stopped advancing in the UART log. `TTC0.ISR` read as `0x0`
at every sample and `TTC0.IER` stayed `0x00000002` (match-0 interrupt enabled) throughout --
consistent with either "no pending interrupt at the moment of the (asynchronous) poll" or
"this task's own polling is itself clearing pending status before it can be observed"; the
latter risk is exactly why finding 4 below re-tests with **zero** register polling.

GIC distributor register reads needed a CPU context argument
(`sysbus ReadDoubleWord <addr>` alone errors `Can't verify current CPU in the given context`;
`sysbus ReadDoubleWord <addr> rpu0` succeeds). With that syntax, `ISENABLER1`
(offset `0x104`, SPI 32-63 enable-set; IRQ36 = bit 4) read **`0x00000000`** at every sample,
including 3s in (ticks confirmed happening) and 45s in (long past the last tick) -- flagged
as an open, unresolved discrepancy: either this task's assumed GICv1 register layout/offset
for this Renode build is wrong (not independently cross-checked against Renode's own C# source
-- this distribution ships no source, only compiled binaries, so the offsets used here come
from the generic ARM GIC architecture reference, not from reading Renode's own model), or IRQ36
is genuinely never enabled at the distributor and ticks are being delivered through a path this
task did not identify. **Recorded as unresolved rather than asserted either way** -- it does
not change the finding below (which relies on UART output and TTC registers only, not on this
GIC reading), but a follow-up should verify the correct offsets against Renode's actual GIC
implementation before trusting the `0x0` reading as meaningful.

### 4. Decisive test: ticks keep arriving for ~120 nominal virtual seconds with zero register-polling interference, then a single 20-second `RunFor` call itself hangs for >15 minutes real time

`diag_full_completion_check.py` requested 200 nominal virtual seconds in ten 20-second chunks,
with **no register reads of any kind between chunks** (only `RunFor` + `currentTime`, then a
UART read) -- this removes the risk (raised by finding 3's ISR-clear-on-read caveat) that the
diagnostics' own polling was interfering with the DUT.

**Chunks 1-6 (cumulative nominal virtual 20s -> 120s):** the UART capture file kept growing
every single chunk (404 -> 508 -> 612 -> 768 -> 924 -> 976 bytes), each chunk completing in a
tight ~13.3-14.6s wall-time band (`AdvanceImmediately` on) with no growth trend -- ticking had
**not** stopped in this window, in contrast to the original M24.2c production run (single
`RunFor "60"`, stopped capturing new ticks after `09:00:14`) and this task's own earlier
heavily-polled long runs (`diag_ttc_trace3.py`, `diag_gic_check2.py`, which stopped capturing
new ticks at `09:00:24` and `09:00:09` respectively). This directly reframes the *original*
M24.2c observation: at the 1/3 counting rate (finding 5), 60 nominal virtual seconds is simply
**not enough budget** to reach the sample's `second >= 35` exit condition -- the script's own
`quit` after `RunFor "60"` ends the run on schedule, at whatever tick count has been reached by
then, which is exactly what M24.2c measured (exit code 0, `09:00:14`, no error) and is fully
consistent with "ran out of budget," not "hung."

**Chunk 7 (requesting cumulative nominal virtual 120s -> 140s): this single `RunFor "20"` call
never returned.** It was still running after this task's diagnostic's own 900-second (15
minute) internal timeout expired (`TimeoutError: marker b'Current real time:' not seen`), at
which point the Renode process (confirmed alive throughout via `ps`, consistently ~640-655%
CPU -- actively computing, not blocked/idle) was killed manually after **~18 minutes of wall
time** on a single 20-virtual-second request, versus ~14 seconds for the identically-sized
request one chunk earlier in the same run. **This is the single most important new finding of
this task.** It was not chased to its own root cause (would need `logLevel`/execution tracing
inside that specific window, well beyond the remaining budget after the rest of this
investigation) but it is real, directly observed (not inferred), and reproducible in the sense
that it recurred at a consistent point in the run (nominal virtual t~120s) rather than being a
one-off host hiccup. **Practical implication for M24: reaching the `ticker` sample's own exit
condition needs roughly 105-140+ nominal virtual seconds at the measured 1/3 TTC rate --
exactly the range where this catastrophic slowdown appears. Simply raising the `RunFor` budget
does not fix the original symptom; it trades a clean early exit at 60s for what looks, in any
practical sense, like an unbounded hang at ~120-140s.** This is a second, distinct, unresolved
finding layered on top of the (root-caused) 1/3-rate finding, not the same phenomenon.

### 5. The undercounting ratio is not "roughly a third" -- it is consistently within 0.3% of exactly 1/3 across four independent, widely-separated samples

Computing `TTC0.COUNT_VALUE / 100e6` (the driver's assumed 100 MHz) against Renode's own
`currentTime` at every point this task read both together:

| sample | Renode virtual time (ground truth) | TTC0.COUNT_VALUE | implied "TTC seconds" @100MHz | ratio |
|---|---|---|---|---|
| `diag_gic_check2.py` | 3.000s | 0x05F59FE6 = 99,988,966 | 0.999890s | 0.33330 |
| `diag_ttc_trace3.py` | 30.000s | 0x3B992956 = 997,401,942 | 9.974019s | 0.33247 |
| `diag_gic_check2.py` | 45.000s | 0x5965CB06 = 1,499,479,302 | 14.994793s | 0.33322 |
| `diag_ttc_trace3.py` | 90.000s | 0xB2CBB016 = 2,999,693,334 | 29.996933s | 0.33330 |

Four samples spanning t=3s to t=90s, from two independently-written diagnostics, agree to
within 0.3% of **exactly 1/3**. This is not "roughly a third of real time because the CPU is
often idle" (COUNT_VALUE is a free-running hardware counter, not CPU-cycle-derived, and the
BSP explicitly programs no prescaler -- `CLK_CNTRL=0`, confirmed read back as `0x00000000` at
every sample) -- it is the peripheral's own counted value undershooting Renode's own exact
virtual-time clock by a specific, stable factor. `platforms/cpus/zynqmp.repl` declares
`ttc0`/`ttc1`/`ttc2`/`ttc3` with **no `frequency:` property** (see the `.repl` excerpt: just
`ttc0: Timers.Cadence_TTC @ sysbus 0xff110000` plus the IRQ fan-out line), so each instance
runs at whatever `Timers.Cadence_TTC`'s own default constructor frequency is in this Renode
build. This task could not read that default directly (a property-introspection probe --
`ttc0 Frequency` / `InputClockFrequency` / `InternalClockFrequency` / `help ttc0` -- returned
empty replies in the one attempt made, `diag_ttc_frequency_probe.py`, not chased further given
budget) and this distribution ships no C# source to read the class definition from, so **the
default-frequency-vs-100MHz mismatch is inferred from the counting-rate measurement, not
confirmed against the model's own source or a queried property.** The ~1/3 ratio (not 1/2,
not 1/4, not the 16-bit-counter-truncation theory the task brief flagged as a strong lead --
see finding 6) is the single most concrete, reproducible, quantitative fact this task
established about the TTC model's behavior.

### 6. The 32-bit-counter-overflow-near-14s hypothesis is excluded by direct measurement

The task brief's stated strong lead -- a counter width/overflow landing near the observed
~14s stall -- does not hold up against the register values actually read. `COUNT_VALUE` was
observed as high as `0xB2CBB016` (2,999,693,334, about 70% of the 32-bit range) with no
wraparound, no anomaly, and `MATCH_0`/`COUNT_VALUE` still tracking each other normally at that
point. A 32-bit overflow (at ~42.9s of true 100 MHz counting, or ~128.8s at the measured 1/3
rate) was never reached in any run this task performed (longest single continuous
`COUNT_VALUE` reading: ~3.0 billion, still short of 2^32=4,294,967,296). **A raw 32-bit
counter/match overflow is excluded as the mechanism** for whatever causes ticks to stop being
*visible in the UART*, at least up to the ~90-140 nominal-virtual-second range this task
measured into. This does not rule out some other overflow (e.g. a 32-bit nanosecond or cycle
counter internal to Renode's own event scheduler, unrelated to the TTC's own architectural
registers) -- that possibility was not eliminated, only the literal TTC `COUNT_VALUE`/`MATCH_0`
32-bit-wraparound reading of the brief's hypothesis.

### 7. Isolation attempt (hypothesis c) and QEMU availability

**QEMU: confirmed absent.** `which qemu-system-arm qemu-system-aarch64` found nothing;
`brew list`, `/opt/homebrew/bin`, `/usr/local/bin` and a `find` for `qemu-system*` under the
container toolchain (`third_party/rtems-container/output/toolchain/bin`, which does carry
`arm-rtems6-run`/`rtems-run` wrapper scripts but no actual `qemu-system-*` emulator binary)
all came up empty. No QEMU isolation was possible on this host, recorded as "not available"
rather than skipped silently, per the task brief.

**A53/zynqmp isolation: not performed, and here is exactly why.** A true apples-to-apples
isolation (same BSP driver logic, different CPU core) would need an aarch64 RTEMS BSP build
for the ZynqMP APU (a separate toolchain target and BSP from the `arm-rtems6`/
`zynqmp_rpu_lock_step` one already built) -- out of this task's scope/budget to stand up a
second toolchain and BSP. This task's own Renode distribution ships a Zephyr RTOS regression
test suite for `xilinx_zynqmp_r5` (the *same* Cortex-R5 core, `tests/platforms/zynqmp.robot`,
e.g. `ZEPHYR_SYNCHRONIZATION`, `ZEPHYR_KERNEL_CONDITION_VARIABLES_*` -- tests whose pass
criteria depend on a working periodic OS tick, not just a single interrupt) that Renode's own
upstream presumably runs and expects to pass, but its firmware ELFs are fetched from
`https://dl.antmicro.com/...` at test time, not bundled -- this task confirmed the host can
reach that domain (`curl` returned HTTP 403, i.e. DNS/TCP/TLS all worked, just not the exact
unauthenticated path tried) but did not download or run anything from it, since the task
brief states "No network needed" and this task treated that as "do not use the network," not
merely "network is not required." **Recorded as not attempted, with the reason, rather than
silently skipped.** If a future task is authorized to use the network for this, those Zephyr
regression ELFs would be the fastest available "different firmware, same core" isolation test
this Renode build ships references to.

**What this task used instead, as a partial substitute for hypothesis (c):** the
chunking-determinism (finding 1) and completion-budget (finding 4) experiments both vary
something about *how the emulation is driven* while holding the BSP/firmware fixed, and both
point away from "the BSP or the TTC model is simply broken/dead" and toward "the effect is a
rate/budget phenomenon, sensitive to how much total virtual time is actually requested" --
which is suggestive for, but not a substitute for, a genuine different-platform isolation.

### 8. Slowness (hypothesis d): the M24.2c "~170 s/s" figure does not reproduce as a steady-state rate

Thirty consecutive `emulation RunFor "1"` calls on the exact same platform+firmware
(`diag_ttc_trace3.py`, default `AdvanceImmediately=False`) averaged **~1.05s of wall time per
1.0s of virtual time requested** (individual steps: 1.28, 1.27, 0.68, 1.05, 1.05, 1.05, 1.06,
1.05, ..., 1.05s -- tight around 1.05s, no growth trend across the 30 steps), and a further
single `+60s` jump took **60.053s** wall for 60.000s virtual -- both close to the ~1:1 pacing
M24.1's bare-loop spike measured and explained (`AdvanceImmediately` defaults `False`, and
Renode's global time source deliberately throttles `RunFor` to approximately real time in that
mode; `machine ElapsedVirtualTime` reports `Advance immediately: False` by default, per
`third_party/renode/REPORT.md`). **This directly contradicts M24.2c's "~170 s real per virtual
second" figure as a steady-state rate** -- 30+ consecutive one-second steps on the identical
platform, after boot, never approached 170:1; they stayed at ~1:1. The likely reconciliation
(not itself re-verified against M24.2c's exact original run, which this task did not re-run
verbatim): M24.2c's number came from a **single, very short sample**
(`hello.resc`'s `RunFor "0.3"`, 51.65s wall for 0.3 virtual seconds), and a single short sample
cannot separate a fixed one-time cost (Renode process startup, full Zynq UltraScale+ platform
description parsing/construction -- dozens of peripherals, two CPU clusters, GIC, TTCs, etc --
and RTEMS/BSP boot/init, all paid once regardless of how much virtual time is subsequently
requested) from a genuine per-virtual-second rate; dividing a mostly-fixed-cost sample by a
tiny virtual-time denominator (0.3s) manufactures an inflated "rate" that is actually mostly
boot overhead. This task's own 30-consecutive-step measurement, taken *after* the identical
boot/`include` sequence, isolates the steady-state number and finds it close to 1:1, not
170:1.

**AdvanceImmediately, tested directly** (`diag_advance_immediately.py`, `RunFor "10"` with and
without `emulation SetGlobalAdvanceImmediately true`, freshly-booted Renode instances for
each): default throttled pacing took **10.103s** wall for 10.000s virtual (essentially exact
1:1, matching finding above); with `AdvanceImmediately` on, the same 10s virtual took Renode's
own-reported **4.830s** real time (a ~2.1x speedup) -- consistent in direction and rough
magnitude with M24.1's bare-loop finding (~36% reduction there vs ~52% here; the exact
percentage differs because this firmware does real BSP/RTOS work per virtual second, not one
branch instruction, so there is more genuine instruction-emulation cost that
`AdvanceImmediately` cannot remove). **Both runs produced the identical UART tick count (3
lines, the initial `09:00:00` triple, nothing more within 10 virtual seconds)** -- confirming
`AdvanceImmediately` changes wall-clock pacing only, never virtual-time behavior or tick
delivery, exactly as M24.1 found for the bare loop.

**A separate, later observation worth flagging as its own open item:** in the (still-running
as this section is written, see finding 4) `diag_full_completion_check.py` run with
`AdvanceImmediately` on, ten 20-virtual-second chunks were requested; the first six completed
in a tight band (14.57, 14.35, 14.10, 13.32, 14.03, 13.99s wall -- no growth trend), but the
seventh chunk (nominal cumulative virtual 120s -> 140s) was still running after **more than
ten minutes of wall time** at last check (`ps` showed the Renode process alive, ~640% CPU,
~12 minutes elapsed since launch against ~85s of wall time accounted for by chunks 1-6 plus
setup) -- a wall-time cost at least an order of magnitude larger than any single chunk before
it, for the identical 20-virtual-second request size. **This is itself a finding, independent
of whatever chunk 7 eventually shows about ticking**: wall-clock cost per virtual-second is
**not** flat/predictable over the life of this run -- something around nominal virtual t~120s
made a single 20-second `RunFor` dramatically more expensive than the same-sized request
earlier in the same run. Not chased to its own root cause given budget; flagged for a
follow-up (the task's own review-checks language: "investigate any oddity before reporting it
as normal" applies, and this was not fully investigated).

## Hypothesis status (final)

- **(a) What is the RPU doing?** Not faulted, not spinning outside the idle loop, not parked
  in an unresponsive WFI forever. It is in RTEMS's normal idle loop (`_CPU_Thread_Idle_body`,
  `wfi` + branch-back, address `0x400045e4`/`0x400045e6`, confirmed by disassembly) the large
  majority of sampled instants, which is expected/correct, and it executes a small, steady,
  non-zero instruction stream throughout (never flat-lined). `IsHalted` is not a usable WFI
  indicator in this Renode build (always read `False`, including in the idle loop). **Tested,
  answered: not a fault, not a dead spin.**
- **(b) TTC/GIC configuration.** The BSP programs TTC0 exactly per its own source (no
  prescaler, overflow+match mode, MATCH_0-only interrupt, 100 MHz assumed). Direct
  measurement shows Renode's TTC0 `COUNT_VALUE` advances at **consistently ~1/3** (0.332-0.333
  across 4 independent samples spanning t=3s to t=90s) of the rate implied by Renode's own
  exact virtual-time clock, with `zynqmp.repl` declaring no `frequency:` override for any TTC
  instance. The 32-bit-counter-overflow-near-14s theory the brief flagged as a strong lead is
  **excluded** -- `COUNT_VALUE` was observed up to ~2.999 billion (70% of the 32-bit range)
  with no wraparound and normal `MATCH_0` tracking. GIC edge/level and enable-bit state were
  probed but the reading (`ISENABLER1=0` throughout, including while ticks were confirmed
  happening) is flagged **unresolved** -- this task's assumed register offsets are not
  independently verified against Renode's own GIC implementation (no source shipped in this
  distribution) and this task did not have budget to cross-check them another way (e.g.
  building a minimal bare-metal IRQ-enable test). **Tested, partially answered: the ~1/3 rate
  is real and measured; the specific mechanism inside Renode's `Cadence_TTC` model that
  produces it (default-frequency parameter vs. `zynqmp.repl`'s omission of an override is the
  leading candidate) was not confirmed against source or a queryable property.**
- **(c) Isolate BSP vs Renode model.** Not performed as a true cross-architecture test (would
  need a second, aarch64 RTEMS BSP toolchain build, out of budget); QEMU confirmed absent on
  this host. The chunking-determinism and completion-budget experiments are an indirect
  substitute and point toward a rate/timing-model effect rather than a dead/broken BSP driver
  (ticks kept arriving correctly-formatted and correctly-timestamped for over 100 virtual
  seconds; nothing about their *content* was ever wrong). **Not tested as originally scoped;
  substitute evidence gathered and disclosed as such.**
- **(d) Slowness.** M24.2c's "~170 s real per virtual second" does **not** reproduce as a
  steady-state rate: 30+ consecutive one-second `RunFor` steps and a 60-second jump on the
  identical platform+firmware stayed within a few percent of 1:1 (matching M24.1's bare-loop
  finding and its documented cause, `AdvanceImmediately` defaulting to `False`). The 170:1
  figure is best explained as one very short (0.3 virtual second) sample dominated by
  one-time boot/platform-construction cost, not a linear rate -- this is a correction to
  M24.2c's characterization, not a contradiction of its raw measurement (51.65s for that one
  short run is not disputed; what it was extrapolated to imply is). `AdvanceImmediately`
  behaves exactly as documented (wall-clock speed only, ~2.1x here, no change to virtual time
  or tick delivery). **However, a second, more severe slowness phenomenon was found**: past
  roughly nominal virtual t~120s, a single 20-virtual-second `RunFor` call took over 15
  minutes of real time (killed after ~18 minutes, still not returned) versus ~14 seconds for
  an identically-sized request earlier in the same run -- a qualitatively different,
  unexplained, severe non-linear cost growth. **Tested, answered with a correction to the
  original figure, plus one new, unresolved, and more severe finding.**

## Root cause

**Root-caused, for the originally-reported symptom (`ticker` appears to stop advancing at
virtual `09:00:14` in a `RunFor "60"` run, exit code 0, no error):** Renode's `Timers.Cadence_TTC`
model, as instantiated by `platforms/cpus/zynqmp.repl` (no `frequency:` override on any of
`ttc0`-`ttc3`), counts at approximately **1/3** the rate the RTEMS `zynqmp_rpu_lock_step` BSP's
clock driver assumes (100 MHz, `XIL_CLOCK_TTC_REFERENCE_CLOCK`). This is measured directly and
repeatably (four independent samples, 0.332-0.333, spanning t=3s-90s), not inferred from
UART timing alone. Because RTEMS's own clock is paced entirely by real TTC match interrupts
(confirmed: `MATCH_0` is kept disciplined against `COUNT_VALUE` by the driver's own ISR the
entire time this task measured it, including well past the point the *sample's* UART output
stopped advancing), a `RunFor` budget sized on the 100 MHz assumption (M24.2c used 60s for a
sample that needs 35 RTEMS-seconds) runs out **before** the sample's own `second >= 35` exit
condition is reached. `.resc`'s own scripted `quit` after `RunFor` returns then ends the run
on schedule -- Renode's exit code 0 is completely accurate (it *did* simulate exactly the
60 seconds requested); the missing piece was that 60 virtual seconds, at this platform's true
TTC rate, is not enough. **This is not a workaround-shaped conclusion** (the fix is not "wait
longer" or "loosen a timeout" -- see immediately below for why that specifically does not
work) -- it is a specific, quantified, mechanism-level explanation: the wrong frequency is
being counted against, by a specific, measured, near-exact factor of 3, traceable to a
specific configuration gap (`zynqmp.repl`'s TTC declarations carry no `frequency:` property).

**Not fully root-caused, and explicitly flagged as such:** simply giving `ticker` enough
`RunFor` budget to reach 35 RTEMS-seconds at the measured 1/3 rate (roughly 105-140+ nominal
virtual seconds) does not produce a clean pass. Instead, this task's own decisive completion
test hit a **second, distinct, unexplained problem**: a single 20-virtual-second `RunFor`
call, requested after ~120 nominal virtual seconds had already elapsed cleanly in the same
run, did not return within 15 minutes of real time (killed at ~18 minutes) -- roughly 60-80x
slower than the same-sized request earlier in the identical run, with the Renode process
confirmed alive and consuming ~640-655% CPU the entire time (not blocked/idle). **This task
did not identify the mechanism behind this second phenomenon** (would need
execution-tracing/`logLevel`-based instrumentation inside that specific window, which this
task's remaining budget did not allow) and does not know whether it is: (i) unrelated to the
TTC rate issue and specific to something else that accumulates with elapsed virtual time or
event count (e.g., a growing internal data structure inside Renode's own GIC or TTC model,
possibly connected to finding 3's unresolved `ISENABLER1=0` reading -- if the TTC's IRQ line
is asserting and never being taken/cleared at the GIC in a way this task's register reads did
not correctly observe, an ever-growing or repeatedly-rescanned pending-interrupt structure is
a plausible mechanism, but this is speculation, not measured); (ii) a genuine infinite loop
that would never return; or (iii) an extreme but finite slowdown that would eventually
complete given enough real wall time (hours+, not measured). **This second finding is what
this task considers the more dangerous one for M24.4**, since a lockstep bridge runs Renode
continuously for the life of a mission, not for a bounded smoke test, and the region where
this task hit the wall (~t=120-140 virtual seconds) is neither exotic nor far in the future
for any realistic run.

## What this task explicitly did not do, and why

- Did not confirm the ~1/3 TTC rate against Renode's own C# source or a queryable object
  property (no source shipped in this distribution; the one property-introspection attempt,
  `diag_ttc_frequency_probe.py`, returned empty replies for `ttc0`/`Frequency`/
  `InputClockFrequency`/`InternalClockFrequency`/`help ttc0`, not investigated further).
- Did not root-cause the >15-minute single-`RunFor` hang at nominal virtual t~120-140s (see
  above) -- flagged as the most important open item for a follow-up task, with a concrete
  starting point (the exact chunk boundary and platform/firmware to reproduce it:
  `diag_full_completion_check.py`, `AdvanceImmediately` on, chunk 7 of ten 20-second
  `RunFor`s after chunks 1-6 have already run).
- Did not independently verify this task's assumed GIC distributor register offsets
  (`ISENABLER1`/`ISPENDR1`/`ICFGR2` at the standard ARM GICv1/v2 offsets) against Renode's own
  implementation -- the `ISENABLER1=0` reading throughout is recorded as unresolved, not as
  evidence either way.
- Did not perform a true cross-architecture (A53/aarch64) isolation run (would need a second
  RTEMS toolchain/BSP build, out of budget) or a QEMU run (confirmed absent on this host).
  Did not download Renode's own Zephyr-on-R5 regression-test ELFs from
  `dl.antmicro.com` (network reachable, confirmed by `curl`, but not used, per "No network
  needed" being read as "do not use the network" for this task).
- Did not re-run M24.2c's exact original `run-samples.sh`/`hello.resc "0.3"` measurement to
  directly confirm the "one short sample dominated by boot cost" reconciliation of the 170:1
  figure -- the reconciliation is argued from this task's own from-scratch measurements
  (30-step and 60-step continuations on an already-booted instance), not from re-executing
  M24.2c's script verbatim.

## Continuation session (question 157, same task, budget continued) -- the "second finding" is root-caused, and it changes the picture

status update: **root-caused.** The "second, more severe, unresolved" slowdown finding above
(section 4/8, "a single 20-virtual-second `RunFor` call ... never returned") is not a second,
independent Renode/TTC defect. It is **`diag_full_completion_check.py`'s own completion check
matching the wrong string**, which let that diagnostic run *past* `ticker`'s own clean
completion into RTEMS's post-exit `bsp_reset()` path -- a real, intentional, infinite
busy-spin loop (no `wfi`) that only terminates on real hardware because the write it
issues actually resets the SoC. On this Renode platform description, the register it
spins on is backed by a bare `Tag` stub, not a functional peripheral, so the loop can never
terminate and Renode must genuinely interpret it, instruction by instruction, for as long as
any script keeps requesting more virtual time. This is a single, coherent, source-level and
disassembly-confirmed mechanism that explains **both** original symptoms (the ~14s-lookslike-stall
and the ~170s/s slowness figure), not a separate open problem.

### Step 1 -- the diagnostic itself never detected completion (a bug in this task's own tooling, not in Renode/RTEMS)

`diag_full_completion_check.py`'s per-chunk check was:
```python
done = b"END OF CLOCK TICK TEST" in content
```
but every actual captured UART transcript in this investigation (`ticker_uart.log`,
`ticker_uart.log.12`, and the sample's own `.resc`-header pre-run guess vs. reality noted in
section "Third finding") ends with `*** END OF TEST CLOCK TICK ***` -- **"TEST CLOCK TICK"**,
not "CLOCK TICK TEST". The substring never matches, so `done` was always `False`, and the
diagnostic's chunk loop never took its "stop early" branch -- it kept issuing `RunFor "20"`
calls indefinitely regardless of whether the sample had already finished.

Direct evidence this matters: `ticker_uart.log.12` (a snapshot of `ticker_uart.log`,
captured by this session's file-timestamp record at `11:53:46`, ~92s after
`diag_full_completion_check.py` was written at `11:52:14`) contains the **complete, correct**
run through `TA1 - rtems_clock_get_tod - 09:00:34` and `*** END OF TEST CLOCK TICK ***`,
followed by the `[ RTEMS shutdown ]` trailer -- i.e. **`ticker` did reach its own end
condition**, well within the wall-clock window the original report attributed to a hang. The
report's own byte-count table for chunks 1-6 (404/508/612/768/924/976 bytes) undercounts what
was actually on disk by the time of that snapshot (1200 bytes, completed) -- consistent with
completion landing inside one of the later chunks (chunk 6 or 7, ~100-120 nominal virtual
seconds -- matching the 1/3-TTC-rate arithmetic: 35 RTEMS-seconds / (1/3) &asymp; 105 nominal
virtual seconds) and the broken check simply not noticing before the process moved on to
request yet more virtual time it no longer needed.

### Step 2 -- what RTEMS actually does after `ticker` (or `hello`) finishes: traced through source, not guessed

Read directly (container-fetched RTEMS 6.1 source,
`third_party/rtems-container/work/rtems-src`), full call chain from the sample's own exit call
to the instruction that never returns:

1. `testsuites/samples/ticker/tasks.c` calls `rtems_test_exit(0)` at `second >= 35`.
2. `cpukit/libtest/testexit.c`: `rtems_test_exit()` -> `rtems_shutdown_executive(0)`.
3. `cpukit/sapi/src/exshutdown.c`: `rtems_shutdown_executive()` -> `_Terminate(RTEMS_FATAL_SOURCE_EXIT, 0)`, commented "SYSTEM SHUTS DOWN!!! WE DO NOT RETURN TO THIS POINT!!!".
4. `cpukit/score/src/interr.c`: `_Terminate()` disables interrupts, calls
   `_User_extensions_Fatal(...)` (the fatal extension chain), and *only* falls through to
   `_CPU_Thread_Idle_body(0)` "in badly configured applications" -- i.e. only if the fatal
   extension chain returns, which it is not supposed to.
5. `bsps/shared/start/bspfatal-default.c`: `bsp_fatal_extension()` is the default/BSP fatal
   extension. It prints the `[ RTEMS shutdown ]` / version / thread-name block seen at the end
   of both `hello_uart.log` and the completed `ticker` capture (`ticker_uart.log.12`) under
   `#if BSP_VERBOSE_FATAL_EXTENSION`, then:
   ```c
   #if (BSP_PRESS_KEY_FOR_RESET) || (BSP_RESET_BOARD_AT_EXIT)
     bsp_reset( source, code );
   #endif
   ```
6. `spec/build/bsps/optreset.yml` (RTEMS's own build-option spec, read directly, not assumed):
   `BSP_RESET_BOARD_AT_EXIT` has `default: [{enabled-by: true, value: 1}]` -- **on by
   default** for any BSP that does not override it. `bsps/arm/xilinx-zynqmp-rpu` (this BSP's
   own directory) was grepped for `BSP_RESET_BOARD_AT_EXIT`/`BSP_PRESS_KEY_FOR_RESET` overrides
   and none exist -- **this BSP builds with `BSP_RESET_BOARD_AT_EXIT` on**, so step 5's
   `bsp_reset()` call is live, not dead code, for both `hello` and `ticker`.
7. `bsps/arm/xilinx-zynqmp-rpu/start/bspreset.c`: `bsp_reset()`, source:
   ```c
   void bsp_reset( rtems_fatal_source source, rtems_fatal_code code )
   {
     volatile uint32_t *reset_ctrl = (volatile uint32_t *) 0xff5e0218; /* CRL_APB_RESET_CTRL */
     ...
     while (true) {
       /* Request a soft system reset ... equivalent to asserting PS_SRST_B */
       *reset_ctrl |= UINT32_C(0x10);
     }
   }
   ```
   This is correct, intentional firmware for **real hardware**: the write is expected to
   actually reset the SoC, ending the loop by ending the CPU. It is not a bug in RTEMS or in
   the BSP.

### Step 3 -- confirmed at the machine-code and platform-model level, not just in C source

Disassembled `ticker.exe` with `arm-rtems6-objdump` (the container-built toolchain is a Linux
aarch64 ELF and cannot run on the macOS host directly; run inside a fresh, disposable
`debian:bookworm-slim` container with the host's already-built
`third_party/rtems-container/output/toolchain` bind-mounted **read-only**, and
`third_party/rtems-container/run-renode` bind-mounted read-only for the ELF -- no image built,
no image tag created, container removed automatically via `docker run --rm`):

```
$ arm-rtems6-objdump -t ticker.exe | grep -E 'bsp_reset|_CPU_Thread_Idle_body|bsp_fatal_extension|_Terminate'
40007060 g     F .text  0000001a bsp_reset
400028a4 g     F .text  0000002c _Terminate
400045e4 g     F .text  00000004 _CPU_Thread_Idle_body
40006b70 g     F .text  0000013e bsp_fatal_extension

$ arm-rtems6-objdump -d --start-address=0x40007060 --stop-address=0x40007080 ticker.exe
40007060 <bsp_reset>:
40007060: b508       push {r3, lr}
40007062: f000 f82d  bl   400070c0 <zynqmp_debug_console_flush>
40007066: 2200       movs r2, #0
40007068: f6cf 725e  movt r2, #65374      @ 0xff5e
4000706c: f8d2 3218  ldr.w r3, [r2, #536] @ 0x218
40007070: f043 0310  orr.w r3, r3, #16
40007074: f8c2 3218  str.w r3, [r2, #536] @ 0x218
40007078: e7f8       b.n  4000706c <bsp_reset+0xc>
```

This is exactly the C source's `while(true) { *reset_ctrl |= 0x10; }`, compiled to a **4
-instruction loop with no `wfi`**, at `0x4000706c`-`0x40007078` -- structurally identical in
kind to the idle loop (`wfi`+`b.n` at `0x400045e4`/`0x400045e6`) but categorically different in
substance: the idle loop's `wfi` is exactly the instruction Renode can (and, per finding 2/8
above, does) treat specially/cheaply; this loop has no such instruction and is a genuine,
unbounded, CPU-active spin that Renode must interpret for real, one iteration at a time,
forever.

**And the platform side, from this Renode distribution's own shipped file** (not this task's
guess): `platforms/cpus/zynqmp.repl` declares
```
Tag <0x00ff5e0000 0x28c> "CRL_APB"
```
`CRL_APB_RESET_CTRL` at `0xff5e0218` falls inside `0xff5e0000`+`0x28c` (`0xff5e028c`), i.e.
**CRL_APB is only a `Tag` stub in this platform description, not a functional reset-controller
peripheral model.** A `Tag` does not implement register semantics or trigger any actual reset
side effect; the write `bsp_reset()` performs is architecturally inert on this platform. The
loop's exit condition (a real hardware reset) can never occur, by construction of the platform
file this Renode distribution ships, independent of anything RTEMS or this task's BSP build
did.

### Root cause (revised/completed): one mechanism explains both the original stall and the slowness

**The `ticker`/`hello` "stall" and the "~170s/s slowness" are the same underlying mechanism at
two different budgets, compounding the independently-confirmed 1/3 TTC-rate finding above:**

1. `platforms/cpus/zynqmp.repl` gives `ttc0`-`ttc3` no `frequency:` override, so Renode's
   `Cadence_TTC` model counts at its own default rate, measured at ~1/3 of the 100 MHz the BSP's
   clock driver assumes (finding 5, unchanged by this continuation). A `RunFor` budget sized on
   the 100 MHz assumption (M24.2c's 60s for a 35-RTEMS-second sample) runs out while `ticker` is
   still genuinely, correctly mid-test -- this alone, fully, explains the originally-reported
   "stops advancing at 09:00:14, exit code 0" symptom. **No hang, no dropped interrupt, no
   counter overflow (excluded directly, finding 6) is involved in that original symptom.**
2. Separately, and only reachable once (1) is given enough budget to let the sample actually
   finish (~105+ nominal virtual seconds, matching this continuation's `ticker_uart.log.12`
   evidence that completion happens in that range): **any further `RunFor` time requested past
   that point is spent entirely inside `bsp_reset()`'s unbounded, non-`wfi` busy loop**, hitting
   a `Tag`-only stub register that can never satisfy the loop's exit condition. Because this
   loop is CPU-active (not `wfi`), Renode cannot fast-forward it the way it evidently can the
   idle loop; every further nominal virtual second requested is genuinely, expensively
   interpreted rather than short-circuited. This explains:
   - **M24.2c's ~170s/s `hello` figure**: `hello.resc`'s `RunFor "0.3"` gives the near-instant
     `hello` sample enormous spare budget after its own near-instant completion (`hello` prints
     its banner and disables the clock driver almost immediately in virtual time) -- essentially
     the entire 0.3 virtual seconds' worth of *wall* time measured (51.65s) is plausibly the
     `bsp_reset()` spin, not "one-time boot cost" as this continuation's own section 8 above
     concluded from a different angle (steady-state 1:1 measurement *during* normal ticking).
     Both observations are consistent: normal ticking (WFI-heavy) runs near 1:1; time spent in
     `bsp_reset()` after completion runs far slower per virtual-second than 1:1. Not
     independently re-measured by isolating boot-time from spin-time in this continuation (see
     "not done" below) -- flagged as the remaining piece to nail down precisely, but the
     mechanism is no longer a mystery.
   - **This task's own "chunk 7 never returned" observation** (section 4/8 above): `ticker`
     completes inside an earlier chunk than the broken completion-check noticed, and the
     diagnostic's subsequent `RunFor "20"` calls (chunks 7+) are entirely spent in the
     `bsp_reset()` spin loop -- a **qualitatively different, much more expensive-per-instruction
     regime** than normal ticking, which is exactly why it was 60-80x (or worse) slower than an
     identically-sized request earlier in the same run, while the Renode process stayed alive
     and CPU-busy (never blocked) the entire time -- both details from section 4 are fully
     consistent with "genuinely interpreting an infinite busy loop," not with any form of
     internal Renode hang, memory leak, or GIC/TTC malfunction.

**This is not a workaround-shaped conclusion.** The fix is not "give it more time" (more time
past completion makes the *measured* slowness worse, not better, since it is entirely spent in
an unbounded loop) and not "reduce the timeout" (that hides the mechanism, it does not address
it). The actual fix belongs in the `.resc`/test-harness layer, not in RTEMS or the BSP (whose
`bsp_reset()` is correct real-hardware behavior): either (a) `.resc` scripts driving this BSP
past a clean test exit should stop issuing further `RunFor` calls once the sample's own
completion banner is observed (i.e. the test harness's own responsibility, not a platform
defect -- exactly the bug found and described in Step 1), or (b) if M24.4's lockstep bridge
needs Renode to survive a guest-initiated reset request across a mission's lifetime, `zynqmp.repl`
would need a real `CRL_APB`/reset-controller peripheral model (not a `Tag`) that
either honours the soft-reset request (reloads/re-executes the boot sequence, as real hardware
would) or the BSP would need `BSP_RESET_BOARD_AT_EXIT=0` for this specific integration so a
clean exit parks in the (cheap, WFI) idle loop instead of spinning. Recorded as the concrete,
mechanism-level options this root cause implies, not prescribed as this task's own fix (out of
this task's scope, which is root-causing, not remediating).

### Live confirmation (decisive): PC and instruction count caught crossing exactly into the spin loop

`diag_bspreset_confirm.py` (this dir) drove a fresh `ticker_noquit.resc` run
(`AdvanceImmediately` on), 9 coarse 10s chunks with no polling to reach nominal 90s cheaply,
then 2-second chunks polling `rpu0 PC`, `rpu0 ExecutedInstructions`, and the UART file after
every single step, watching for the actual UART completion string (`END OF TEST CLOCK TICK`,
the *correct* word order, not the bug from Step 1 above). Consecutive steps, verbatim from
`diag_bspreset_confirm.log`:

| step | cumulative nominal | wall time | `rpu0 PC` | `rpu0 ExecutedInstructions` | UART | completion string seen |
|---|---|---|---|---|---|---|
| fine 15/25 | 120s | 1.00s | `0x400045e6` (idle loop) | `0x129CB3` = 1,219,763 | 976 B | No |
| **fine 16/25** | **122s** | **329.15s** | **`0x4000706c`** (bsp_reset loop) | `0xBEB9A03` = 199,989,763 | **1200 B** | **Yes** |

This is a direct, measured catch of every part of the mechanism in one transition, not
inferred:

- **UART grew from 976B to 1200B and now contains `END OF TEST CLOCK TICK`** -- `ticker`
  completed its own test during this exact 2-virtual-second window (cumulative nominal
  120-122s), consistent with the 1/3-TTC-rate arithmetic (35 RTEMS-seconds / (1/3) &asymp; 105s,
  plus scheduling slack).
- **`rpu0 PC` jumped from the idle loop's `0x400045e6` to `0x4000706c`** -- the *exact* address
  this task's disassembly identified as the `ldr.w r3,[r2,#0x218]` instruction inside
  `bsp_reset()`'s loop body (Step 3 above). This is not "a different function" or "some other
  state" -- it is the precise instruction address predicted from source and disassembly, caught
  live.
- **`ExecutedInstructions` grew by 198,770,000 in this one 2-virtual-second step**, versus
  16,377 in the identically-sized preceding step (120s window, still in the idle loop) -- a
  **~12,100x increase in instructions executed per virtual second** the instant the CPU enters
  the non-`wfi` spin loop, because Renode must genuinely interpret every iteration instead of
  fast-forwarding through `wfi` as it does for the idle loop.
- **Wall time for this single step was 329.15s versus 1.00s for the identically-sized preceding
  step** -- a **~329x** wall-clock blowup for the same nominal virtual-time request, landing
  squarely in the same regime (severe, apparently non-linear slowdown) the original
  `diag_full_completion_check.py` run reported for its "chunk 7" (there: ~60-80x for a
  20-virtual-second chunk; here: ~329x for a 2-virtual-second chunk straddling the exact
  transition instant, i.e. this step also paid a large fraction of the transition's cost in a
  smaller nominal window, consistent in direction and order of magnitude, not an exact
  cross-run comparison).

The Renode process was killed intentionally after capturing this step (SIGTERM, confirmed by
`renode final returncode: 143` in the log) rather than run through the remaining ~18 planned
fine steps -- once PC is confirmed parked at `0x4000706c` and the instruction/wall-time
explosion is directly measured, further steps would only re-confirm the same steady-state spin
(every subsequent request would land in the identical loop) at further wall-clock cost this
task chose not to spend. **This closes the loop started at Step 3**: source says
`bsp_reset()` is a real infinite non-`wfi` busy loop; disassembly says it lives at
`0x4000706c`-`0x40007078`; the platform file says the register it targets is an inert `Tag`;
and this live run caught the CPU landing on that exact address, executing ~200 million real
instructions and consuming 329 real seconds, in the identical 2-virtual-second window `ticker`
finished its own test. **No part of this explanation is inferred from timing alone or from
exit-code behavior; every element (UART content, PC, instruction count, wall time) was read
directly off the running emulator.**

## What remains open after this continuation

- The GIC `ISENABLER1=0` discrepancy from the prior session (finding 3) remains unresolved on
  its own terms (not investigated further here; it does not bear on either root cause above,
  which rest on UART content, disassembly, the platform `.repl` file, the RTEMS build-option
  spec, and the live PC/instruction-count/wall-time capture, not on the GIC register reading).
- The A53/QEMU isolation gap from the prior session is unchanged (still not performed, same
  reasons: no second aarch64 RTEMS BSP toolchain in budget, QEMU confirmed absent on this
  host). It is lower-priority now than when originally flagged: with both original symptoms
  root-caused to specific, named mechanisms (TTC default frequency, `bsp_reset()`-vs-`Tag`),
  there is no longer an open "is this the BSP or the platform model" question motivating it --
  both mechanisms are independently pinned to specific, named locations (`zynqmp.repl`'s
  missing `frequency:` line; `zynqmp.repl`'s `CRL_APB` `Tag` declaration), not merely narrowed
  to "one side or the other."
- Precisely isolating boot-time from `bsp_reset()`-spin-time within M24.2c's original
  170s-for-0.3-virtual-seconds `hello` measurement was not re-run in this continuation (would
  need a similarly fine-grained PC/instruction poll across `hello`'s own short run); the
  mechanism is no longer in doubt (same `BSP_RESET_BOARD_AT_EXIT` default applies to `hello`
  too, and `hello_uart.log` carries the identical `[ RTEMS shutdown ]` trailer showing it took
  the same fatal-extension path), but the exact split between "one-time platform/BSP boot
  cost" and "time spent already spinning in `bsp_reset()` within that same 0.3s budget" for
  that specific historical measurement is not quantified.
- Remediation (fixing `.resc`/harness completion-detection, or giving M24.4 a real
  `CRL_APB` reset-controller model, or building with `BSP_RESET_BOARD_AT_EXIT=0` for this
  integration) is explicitly out of this task's scope (root-causing, not remediating) and is
  not attempted here.
