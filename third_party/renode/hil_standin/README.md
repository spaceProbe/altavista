# The stand-in HIL guest (question 242 (b), hilprep-6)

**Renode 1.16.1 emulating the ZynqMP RPU. This is not the ZCU104, and nothing here is a board
result.** The board is not on hand; this is the same code path a board run takes (pacing, serial
transport, the signed I/O log, replay) exercised against the real reproducible RTEMS ELF
(`core-cpu1.exe`, SHA-256 `de96907f...caf3` since the hilprep-6 tick-count fix, `a5a5fe7b...2eb5` before it; question 240) in an emulator, so that the HIL code is
not first tried on hardware.

- `renode_realtime_pty.py`: starts Renode with this repository's `zynqmp.repl` and the ELF, lets it
  run free (`start`, never `RunFor`), bridges UART1 to a host pseudo-terminal, holds virtual time at
  or below the wall clock, reports the virtual-to-wall ratio. Its module docstring is the reference.
- `uart_rx_paced_hook.py`: the Renode-side hook that delivers host-to-guest bytes at the baud rate
  **in virtual time**.
- The test that drives it: `crates/av-kernel/tests/hil_standin_renode.rs` (opt-in,
  `AV_HIL_STANDIN_TESTS=1`; its module doc is the run's description). It starts the helper, then
  `av-edge-board --port-device /dev/ttysNNN@115200 ...` on the pty, binds the controller of
  `drms/demo_attitude_control` as a board, runs 10 s at 10 Hz twice (no faults; a `power_cycle`
  `HARDWARE` fault at 5 s), and replays each run from its signed edge log.

Run the helper alone (it prints `READY pty=... slave=/dev/ttysNNN` when the guest can take the one
HELLO, and stops on SIGTERM or when `--stop-file` exists):

```text
.venv/bin/python third_party/renode/hil_standin/renode_realtime_pty.py \
  --renode-bin <renode> --platform third_party/renode/platforms/cpus/zynqmp.repl --elf <core-cpu1.exe> \
  --workdir <dir> --uart0-log <dir>/uart0.log --pty-path <dir>/uart1.pty --monitor-port <free port> \
  --ready-file <dir>/ready.json --stats-file <dir>/stats.json --stop-file <dir>/stop
```

## What "real time" means for Renode 1.16.1 here

Measured on this Mac, idle guest (waiting for HELLO), 40 s after the guest was ready, while the host
was loaded by other jobs (load average 12 to 18), `--pacing none` unless stated (virtual seconds per
wall second):

| setting | whole window | 5 s windows |
|---|---|---|
| defaults (`AdvanceImmediately` false, 100 us quantum) | 0.48 | 0.00 0.01 0.08 0.01 0.02 0.09 0.18 0.38 0.36 0.56 0.38 0.56 0.48 0.50 0.50 |
| `SetAdvanceImmediately true` | 0.79 | 0.08 0.02 0.10 0.39 0.58 0.68 0.77 0.85 0.84 0.89 0.85 |
| `SetAdvanceImmediately false` (explicit) | 0.92 | 0.10 0.09 0.47 0.90 0.84 0.91 1.01 0.86 0.98 0.92 |
| defaults, 1 ms quantum (`SetGlobalQuantum`) | 1.28 | 0.10 0.06 0.93 2.08 1.90 1.00 1.00 1.00 1.00 1.00 |
| defaults, `--pacing pause` | 0.55 | 0.10 0.02 0.11 0.39 0.61 0.57 0.59 0.41 0.59 0.49 0.54 |

(`ratio_cfgs` output kept in the task's scratch; the first windows are the guest still booting.)

- Renode has **no real-time mode that caps virtual time**. `AdvanceImmediately` (default false)
  decides whether a sleeping CPU's idle time is waited out in wall time (false) or skipped (true);
  neither bounds a busy guest, and the quantum (default 100 us) only sets how often the CPU threads
  synchronise. With a quantum of 1 ms an idle guest holds 1.00 exactly, after a catch-up at 2.08x
  (253 ms ahead of the wall clock at the worst). With the default quantum on a loaded host the sync
  overhead dominates and an idle guest runs at 0.3 to 0.9. A busy guest (cFE boot: 0.1 to 0.2) is as
  fast as the host lets it.
- So virtual time can run **ahead** (2x while catching up) or **far behind** (0.01 to 0.6 on a
  loaded host) of the wall clock, and nothing in Renode bounds the first. `--pacing pause` (the
  helper's default, used by the test with `--quantum-us 1000`) does: it keeps a reference line that
  advances with the wall clock but never more than 100 ms ahead of the guest, and pauses the
  emulation (`emulation PauseAll`, resumed by `StartAll`) whenever the guest comes within 10 ms of
  it, resuming when it is 40 ms behind. Over the test's runs the ratio between the run's start and
  end marks was 0.28 and 0.91 (proof 1), 0.80 and 0.99 (proof 2); the pacer paused 15 to 110 times
  per run, mostly while the guest sat in its 2 s output waits. The overshoot above the line is
  hundreds of milliseconds at worst (a poll takes ~25 ms and Renode runs up to 2x between polls),
  which is why the test accepts a ratio up to 1.05 over a 10 s window.
- The guest is slow when the host is busy. Everything the pacing report shows about lateness is
  therefore the emulator's, not a board's: at ratio r the 100 ms control period is 100 r ms of guest
  time. The stand-in measures the HIL **code path**; it is no estimate of a board's latency.

## Why the UART is bridged the way it is

Both findings were reproduced with the real service, not assumed.

1. **Host to guest must be paced in virtual time.** Bytes pushed into UART1 at the wall-clock rate
   of the edge service (64-byte chunks, 8N1 wire time between them) reach a guest that runs at 0.1
   to 0.6 of real time, i.e. at 2 to 10 times the line rate. The guest's UART driver moves every
   received byte into termios' 256-byte raw input ring from the interrupt handler, and the first
   STEP that carries sensor inputs (305 bytes, STEP 2) did not survive: the guest read an
   incomplete frame and never answered. That is the mechanism question 238 root-caused for the
   bridge (`rawInBufDropped`); here it is inferred, not re-observed with a debugger: at the stall
   UART1's receive FIFO was empty (SR 0xa) and the PC in the idle loop, so the bytes had been taken,
   and the stall disappears when the line is paced in virtual time or slowed to 19200 baud. With the bytes taken straight from Renode's pty terminal (`--rx-path pty`):
   `hil_standin_renode` failed 3 of 3 times at STEP 2 (480 bytes in, 71 out, the UART1 FIFO empty,
   PC in the idle loop, `Step got no reply within 20000 ms`), and the replay of recorded frames
   through the same path stalled in 2 of 5 trials at 115200 baud (at STEP 2 and STEP 3); at 19200 baud
   3 of 3 trials, and with `--rx-path paced` every run (3 of 3 driver trials, 3 of 3 test runs), ran
   to the end. `uart_rx_paced_hook.py` schedules a recurring
   machine action every 1 ms of virtual time that writes the bytes the line could have carried since
   the last tick, so the guest sees exactly 115200 baud whatever the host's speed. (The bridge in
   `M24_4b` delivers its frames while the machine is stopped between `RunFor` slices, which is why
   it never met this.)
2. **Guest to host through the `CharReceived` hook.** Renode's pty terminal also carries the guest's
   transmissions, and the helper counts both paths; in every run the two agree byte for byte
   (`stats.relay`: for example 4290 and 4290), so the loss that question 171's work found in
   `CreateServerSocketTerminal` was not met on the pty terminal in these runs. The hook (the bridge's
   own) remains the default because it was proven against an independent capture (M24.4f);
   `--tx-path pty` is available for the A/B.
3. **Renode's pty terminal is attached in both modes** because Renode must have a terminal on UART1
   for the TX path to be observed, and because `--rx-path pty` needs it. Its inbound direction works
   while the machine runs (a HELLO written to the pty slave of a free-running machine was answered);
   the "a PTY terminal never delivers" finding of `M24_4b_REPORT.md` was made with the machine
   stopped between `RunFor` slices.
4. The pty the edge service opens is a second pair made with `openpty` in the helper, because
   `av-edge-board` accepts only device paths under `/dev/` (Renode's own pty terminal is a symlink
   elsewhere), and because the helper has to own the master to put the hook's bytes on it.

## Things the stand-in showed that are the guest's, not the harness's

- **The first STEP takes the guest's whole 2000 ms output wait.** STEP 1 carries no inputs, so
  `IO_LOCKSTEP` waits `IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS` (2000 ms of guest time) for an output that
  cannot come before the FSW has seen a measurement. At ratio r that is 2/r seconds of wall time
  (3.7 s to 12 s in the recorded runs), after which every later tick is late by that much, because
  pacing catches up without skipping and never re-anchors. So the pacing report shows an overrun on
  97 to 100 of the 100 ticks, a worst overrun of 2.8 to 12 s at the start of the run, and a final
  lateness that is 0 only when the host was quiet enough for the guest to recover it.
- **After the in-place RESET the controller was silent for as many ticks as had passed before it (a
  defect in the flight software, found by this stand-in, root-caused and FIXED in hilprep-6).** A
  power-cycle fault makes the kernel ask the edge service to run the power channel and then send the
  guest a RESET frame; `io_lockstep`'s `handle_reset` calls `psp_lockstep_init(tai)`, which used to set
  `g_tick_count = 0` (`services/cfs/psp-lockstep/src/psp_lockstep.c`). `sch_lockstep`'s main loop
  dispatches one wakeup for each tick it has not yet seen, `for (; last_seen_tick_count <
  current_tick_count; ...)` (`services/cfs/apps/sch_lockstep/fsw/src/sch_lockstep_app.c`), and its
  `last_seen_tick_count` is a local nothing reset, so after the counter dropped to 0 no wakeup was sent
  until it had climbed back to where it was. The ADCS got no wakeup, published nothing, and every STEP
  was answered empty after `IO_LOCKSTEP`'s full 2000 ms output wait. Evidence: the recorded-frame
  driver with a RESET after 8 STEPs gave exactly 8 empty STEP_DONEs and then outputs again; after 5 and
  10 STEPs 5 and 10; in the 5 s fault run (50 STEPs before the RESET) all 50 remaining STEPs came back
  empty and the run ended before the controller recovered. It is platform independent: the posix
  container image failed the same way (0 outputs in 50 post-RESET steps).
  The fix: the tick count is monotonic (`psp_lockstep_init` no longer zeroes it; the clock still starts
  over at the new epoch), with a backstop in `sch_lockstep` that restarts its last-seen count if a count
  is ever seen going backwards. A reset-time hook in `sch_lockstep` alone was not sound: after a RESET at
  count 1 the first new tick has count 1 == `last_seen`, which no comparison can tell from "no new
  tick", so a tick would be lost; a counter that never goes backwards has no such case. The test
  asserts the controller publishes again within two steps of the RESET and for all but two of the
  following steps; with the fix it published on the very next step and on 50 of 50. The ELF is
  therefore `de96907f...caf3` now (it was `a5a5fe7b...2eb5`; two builds from different staging paths
  agree), and the test's `board.step_timeout_ms` is 60 s instead of the 180 s the silent steps needed.

## What HIL day changes, as the code has it

- **Device and baud.** The edge service's `--port-device` and `BoardBinding.port_devices` name the
  host's device for the board's UART (`/dev/cu.usbserial-*` on a Mac, `/dev/ttyUSBn` on a Linux edge
  node: one of the four channels of the ZCU104's on-board USB-UART bridge, the one wired to the RPU's
  UART1, which the ELF opens as `/dev/ttyS1`), `@115200` unless the boot image configures another
  rate, 8N1, no flow control (`av_edge::board::parse_port_device`). In this stand-in the device is
  the `/dev/ttysNNN` of the helper's pty, different every run.
- **How the board is booted.** Here the helper `LoadELF`s the ELF into Renode and waits for the cFE
  console to say OPERATIONAL and the UART1 receiver to be enabled. On the board the RPU boots from the
  SD-card image the (not yet booted) `third_party/zcu104-boot` recipe builds, and the edge service
  must be started after the board's UART is up: the guest reads the one HELLO once and a HELLO sent
  earlier is lost (`av-edge-board`'s README). Nothing in the repository detects a board's readiness;
  here the helper does.
- **Power control.** `power_control` is `cmd:<absolute path>` of an executable that really cycles the
  board's supply or reset (a relay, a managed PDU, ...), run by the edge service. Here it is a fake
  that only records its call, so the guest stays up and the kernel's RESET is handled in place. A
  real power cycle drops the link: the edge service would have to re-handshake (HELLO once) and the
  kernel would have to Bind again before the RESET, and neither exists (the README of `av-edge-board`
  lists it as a HIL-day item).
- **Time.** On the board there is no virtual time: the guest's CPU is the real R5 at full speed, the
  line is a real UART, the wire-time pacing of the edge service is the line rate it has to match, and
  the helper's pacer and the virtual-time receive path disappear. The overrun statistics become
  measurements of the board's latency and the host's link; the numbers from this stand-in are not
  predictions of them.
- **Replay** is unchanged: it needs the signed edge log and the certificate, never the board.
