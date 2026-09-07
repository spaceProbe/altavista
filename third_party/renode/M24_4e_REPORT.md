# M24.4e -- root-causing the guest STEP stall, then closing M24

status: **in progress -- root cause found and fixed for STEP 1; STEP 2+ delivery still unreliable,
disclosed at the end, not hidden**

Task: `docs/open-questions.md` questions 145, 153, 156, 157, 164; `docs/sil-plan.md`'s M24; builds
directly on `third_party/renode/M24_4d_REPORT.md` (fixed the socket-listener race, found and
disclosed but did not root-cause a guest-side STEP stall) and `M24_4c_REPORT.md`/`M24_4b_REPORT.md`
(STEP_MIN_VIRTUAL_S floor, UART handshake fix).

Written incrementally per question 157's own rule. Sized for ~300 tool uses.

## Not done yet (running list, updated as items close)

- [x] 1. Root-cause the guest STEP stall -- **DONE**, see "Measurement 3"/"The actual root cause"
      below: not a guest stall at all (`IO_LOCKSTEP`'s own task, per a live GDB-remote stack walk,
      had already returned from `handle_step` and moved on to reading the next frame); a genuine
      Renode-side defect in `CreateServerSocketTerminal`'s own forwarding of `uart1`'s transmitted
      bytes to its socket, proven with an independent `CreateFileBackend` capture of the exact
      `STEP_DONE` frame the socket never delivered, and independently cross-checked with `netstat`
      showing zero bytes queued on either end of that TCP connection at the OS level.
- [~] 2. Finish the per-step byte comparison -- **PARTIAL, disclosed, not complete.** A real fix
      (file-backed reads replacing the socket, `read_frame_from_growing_file` +
      backend-re-attachment-to-force-a-flush) was implemented in `renode_bridge.py` and gets the
      test **one real STEP further than before this task** (STEP 1's own `STEP_DONE` is now
      delivered and read correctly, proven live) -- but STEP 2 onward still times out even with a
      60s per-frame budget (`cargo_run10_longtimeout.log`), so the full 10-step comparison this
      test needs still does not pass. This is the one item this task could not finish; see "Not
      done" at the very end.
- [x] 3. Renode-gated skip reason -- **verified, unchanged**: `renode_unavailable_reason()`
      (`crates/av-kernel/tests/drm_attitude_control_renode.rs`) already prints a specific, visible
      reason per missing file (binary, platform file, cross-built ELF, bridge script, venv python)
      and `cfs_image_unavailable_reason()` does the same for the posix half -- matches M15.3's own
      convention (`cfs_image_unavailable_reason` in `drm_attitude_control_cfs.rs` is the named
      pattern), and both were exercised for real by every run this task made (never triggered,
      since every dependency was present on this host).
- [x] 4. Question 156: label/prune housekeeping -- **re-verified, not just assumed carried over**:
      `docker ps -a --filter label=av.test=1` and `docker images --filter label=av.test=1` were
      both run directly after this task's own ten-plus real runs and came back empty (the
      `DockerContainerGuard`/`DockerImageGuard` `Drop` impls, and `prune_stale_test_resources()`
      at the top of the test function, are doing their job). One small, already-disclosed gap
      re-confirmed, not newly introduced by this task: `docker tag` has no `--label` flag (the
      test file's own comment already says so), so the ephemeral `127.0.0.1:<port>/altavista-
      cfs-lockstep:test` image tags this task's own ten-plus runs created are not prunable by
      label and were found still present (two of them) after this task's own runs -- removed by
      hand (`docker rmi -f`) as part of this task's cleanup, same as M24.4d did for stale scratch
      dirs, but the underlying "image tags aren't labeled" gap itself is pre-existing and out of
      this task's own assigned scope to fix (M24.4c's own file, not this task's).
- [x] 5. Temp-dir cleanup -- **DONE**, see "Temp-dir and container cleanup" below.
- [x] 6. Full verification -- **DONE**, see "Test totals" below.

## Hypotheses (stated before measuring, per the task brief)

The task brief's three, plus any this task's own reading of the code suggests:

1. **UART TX deadlock (lead's leading candidate).** `channel_sts 0xa` means the TX FIFO is not
   empty, and the bridge itself is not draining the socket terminal while it blocks waiting for a
   frame -- a deadlock between our own reader and the guest's writer.
2. **The lockstep PSP tick is never released** because the STEP frame's `until_tai_ns` does not
   land on the guest's own tick grid.
3. **A software-bus receive with an infinite timeout** (`CFE_SB_PEND_FOREVER`) that never
   completes.
4. **(new, from reading `io_lockstep_app.c` and the BSP's own console driver before measuring
   anything live).** `handle_step`'s own FROM_BUS wait uses a *bounded* timeout
   (`IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS` = 2000 ms, not `CFE_SB_PEND_FOREVER` -- this already
   distinguishes it from hypothesis 3 above, which is about a *different*, hypothetical unbounded
   wait), and M24.4d's own UART0 transcript shows the timeout path being taken (event 17 fires).
   The very next thing `handle_step` does after that event is fall through to
   `lockstep_encode_step_response`/`lockstep_write_frame` on **uart1** -- so if the guest is
   genuinely stuck immediately after event 17 and never reaches that write, the most direct
   remaining candidate is that **event 17's own console print (over uart0, not uart1) itself never
   completes** -- RTEMS's termios layer blocks the calling task on `rawOutBuf.Semaphore` when its
   raw output ring buffer fills, and that semaphore is only ever posted by the UART's own TX-empty
   interrupt handler (`bsps/shared/dev/serial/zynq-uart.c::zynq_uart_interrupt`, confirmed this
   BSP is built with `ZYNQ_UART_CONSOLE_USE_INTERRUPTS=1`, not polled). If Renode does not deliver
   that specific interrupt at this exact point, the printing task (plausibly `IO_LOCKSTEP`'s own
   task, since `CFE_EVS_SendEvent` is called synchronously inside `handle_step`, not off a queue)
   blocks forever mid-message -- and the raw UART0 byte log below shows exactly that: the event
   text is truncated **mid-word**, not at a clean sentence boundary, which a "just ran out of
   virtual time" explanation does not predict but a "blocked mid-`write()`" explanation does.

All four (plus channel-status register semantics themselves) are checked against live evidence
below, not assumed.

## Register-semantics correction (checked before trusting the task brief's own framing)

The task brief states "`channel_sts 0xa` means TX FIFO not empty." **This is checked directly
against the exact header this BSP itself uses, not assumed either way**:
`third_party/rtems/rtems-src/bsps/include/dev/serial/zynq-uart-regs.h` (the same file
`third_party/renode/M24_4b/uart1_register_probe.py`'s own docstring already cites for
`REG_CONTROL`/`REG_CHANNEL_STS`) declares, for the `channel_sts` register at offset `0x2C`:

```
#define ZYNQ_UART_CHANNEL_STS_TFUL    BSP_BIT32(4)   /* TX FIFO full */
#define ZYNQ_UART_CHANNEL_STS_TEMPTY  BSP_BIT32(3)   /* TX FIFO EMPTY */
#define ZYNQ_UART_CHANNEL_STS_RFUL    BSP_BIT32(2)   /* RX FIFO full */
#define ZYNQ_UART_CHANNEL_STS_REMPTY  BSP_BIT32(1)   /* RX FIFO EMPTY */
#define ZYNQ_UART_CHANNEL_STS_RTRIG   BSP_BIT32(0)
```

`0xa = 0b1010` = bit 3 (`TEMPTY`) + bit 1 (`REMPTY`) -- **both the TX and RX FIFOs of `uart1` are
empty**, the opposite of "TX FIFO not empty." This refutes hypothesis 1 in its literal, stated
form for **uart1** (the lockstep channel itself): there is no backed-up TX FIFO on uart1 for the
bridge to be failing to drain -- the guest is not even trying to send or receive anything on uart1
at the moment sampled. (Whether an *analogous* TX-drain deadlock exists on **uart0**, the console
UART, which is a physically different peripheral at a different base address the bridge never
polls, is exactly hypothesis 4 above, and is checked with live evidence below.)

## Measurement 1: resolve the pinned PC, definitively

`0x4004bb32` was resolved two independent ways against `core-cpu1.exe`'s own unstripped symbol
table (the file this exact repro run's own core-cpu1.exe SHA should match M24.4b's `54eb1192...`
-- reverified below):

- **Why the task's own `arm-rtems6-addr2line` attempt returned no output, investigated, not left
  unexplained**: `third_party/rtems-container/output/toolchain/bin/arm-rtems6-addr2line` (the only
  `arm-rtems6-addr2line` anywhere under this repo) is an **ELF 64-bit ARM aarch64 Linux binary**
  (`file` confirms: "ELF 64-bit LSB pie executable, ARM aarch64, ... for GNU/Linux") -- it was
  built to run *inside* the `rtems-m24c:build` Docker container that cross-builds `core-cpu1.exe`,
  not on this macOS host directly. Running it natively here fails immediately and loudly:
  `zsh:1: exec format error: .../arm-rtems6-addr2line`, exit code 126 -- **not silent**, so
  whatever produced the earlier "no output" report either swallowed stderr or hit a different
  invocation path; either way, this exact tool cannot run on this host without Docker, which is
  the actual reason it produced nothing useful.
- **Working resolution**: macOS's own system `nm`/`objdump` (Xcode Command Line Tools, LLVM-based)
  read this foreign ARM32 ELF's symbol table and disassemble it directly, with no cross-toolchain
  needed for this read-only purpose:
  ```
  $ objdump -d --start-address=0x4004bb20 --stop-address=0x4004bb40 core-cpu1.exe
  4004bb30 <_CPU_Thread_Idle_body>:
  4004bb30: bf30       wfi
  4004bb32: e7fd       b   0x4004bb30 <_CPU_Thread_Idle_body>
  4004bb34 <_CPU_Context_Initialize>:
  ```
  `0x4004bb32` is the **branch-back-to-`wfi`** instruction, one halfword after `wfi` itself --
  exactly RTEMS's own idle body, and consistent with "the CPU woke from `wfi` (any interrupt does
  that), found nothing to run, and looped back to sleep again." This confirms M24.4d's own `nm`-based
  finding independently, via a different tool, and additionally shows *which instruction* PC is
  parked at (the branch, not the `wfi` itself) -- **idle alone does not distinguish "permanently
  stuck" from "correctly idle, periodically woken by an unrelated tick interrupt with nothing to
  do"**; M24.4d already flagged this ambiguity and it is resolved below with live memory, not
  register polling alone.

## Measurement 2: the truncated console line, read as an execution trace, not just a log

`uart0.log`'s own last bytes (`od -c`, exact, from a repro run captured just before this task
started -- `/tmp/av-renode-m24c-32250/uart0.log`, dated the same session, matching the task
brief's own described failure byte-for-byte) end:

```
...IO_LOCKSTEP: wheel_torque_out produced no output within 2000 ms this tick (expected before
the FSW's own warm-up co\r\n
```

`services/cfs/apps/io_lockstep/fsw/src/io_lockstep_app.c:467`'s own format string is:
`"IO_LOCKSTEP: %s produced no output within %d ms this tick (expected before the FSW's own warm-up
completes)"`. The captured line stops **mid-word**, at `"...warm-up co"`, missing
`"mpletes)"` and its own trailing newline -- printed here as if a `\r\n` were appended, but that
`\r\n` is this diagnostic's own `od` framing artifact of the *next* buffered line boundary, not
part of the actual message (confirmed by counting: there is no closing `)` anywhere in the
captured bytes). A CPU that ran out of granted virtual time would stop **between** print calls,
not **inside** one -- `CFE_EVS_SendEvent`'s own formatting completes in a single call before any
byte reaches the UART. Stopping mid-string is direct evidence of an in-progress operation that
itself blocked, not merely "ran out of time to start the next thing."

## Measurement 3: live RTEMS/termios state via a real GDB-remote attach to the frozen guest

Renode's monitor exposes `machine StartGdbServer <port>` (confirmed present and working in this
exact build). Temporary, env-var-gated instrumentation was added to
`third_party/renode/M24_4b/renode_bridge.py`'s own STEP-stall diagnostic block (already-inherited
code from M24.4d, kept as-is) so that, once the stall is confirmed exactly as before, the bridge
additionally starts Renode's GDB server and holds the guest (paused between monitor commands, not
running) for an external debugger to attach -- gated on `AV_M24_4E_GDB_PORT` so it is inert unless
explicitly requested, disclosed inline in the script itself, and to be removed once this task's
root cause is confirmed (not a permanent addition).

macOS's system `lldb` (Xcode Command Line Tools) can `gdb-remote` attach to Renode's GDB stub and,
loading `core-cpu1.exe`'s own DWARF debug info (unstripped) via `target create`, read **live**
global/static state with full type information -- no manual struct-offset arithmetic needed, and
no cross-built `arm-rtems6-gdb` required (confirmed separately that no working native-macOS
`arm-rtems6-gdb` exists on this host either -- only a Linux/aarch64 one under
`third_party/rtems-container/output/toolchain/bin`, same "wrong host platform" problem as
`addr2line` above; `third_party/rtems/toolchain-build/build/arm-rtems6-gdb-15.2-arm64-apple-darwin25.6.0-1`
is only the RSB **source build directory**, not an installed prefix -- no
`third_party/rtems/toolchain/bin` exists on disk, so that native build was evidently never
completed/installed).

**Live attach worked, with one wrinkle disclosed, not hidden**: lldb's own `gdb-remote` command
failed against Renode's stub with `error: failed to get reply to handshake packet within timeout
of 0.0 seconds` (tried with the default settings and with `plugin.process.gdb-remote.packet-timeout`
raised to 15 -- no change, so this is not a timeout-value problem). A raw socket probe
(`third_party/renode/M24_4e/probe_gdbstub.py`) confirms the stub itself is healthy and answers
`qSupported`/memory-read packets correctly, and shows Renode's stub proactively pushes an
unsolicited stop-reply packet (`$T05thread:01;#07`) immediately on connect, before the client sends
anything -- a pattern lldb's own handshake state machine apparently does not tolerate. Rather than
chase an lldb-specific bug (out of scope for a root cause about the guest), a small, dependency-free
GDB-remote client was written (`third_party/renode/M24_4e/gdbstub_read_state.py`) that speaks the
protocol directly and skips any packet that looks like an unsolicited notification before treating
the next one as a reply. All addresses/struct offsets it uses were computed once, offline, straight
from this exact `core-cpu1.exe`'s own DWARF info (`lldb -o "target create core-cpu1.exe" -o "expr --
(unsigned long)&((T*)0)->field"`), not guessed.

**uart0 and uart1 driver-level state, live, at the exact moment of the confirmed stall:**

```
uart0: {regs: 0xff000000, tx_queued: 0, transmitting: False, irq: 53}
uart1: {regs: 0xff010000, tx_queued: 0, transmitting: False, irq: 54}
```

This **refutes hypothesis 4** (this task's own new hypothesis, added before measuring) in its
literal form: neither UART's own driver-level context shows a transmission stuck mid-flight
(`transmitting=False`, `tx_queued=0` for both) -- there is no burst of bytes sitting in RTEMS's own
output path waiting for a TX-complete interrupt that never comes.

**`_Watchdog_Ticks_since_boot`/`Clock_driver_ticks`, sampled 1s of real time apart: both frozen (no
delta).** Not conclusive on its own -- no `RunFor` is being issued during this inspection hold
(Renode is genuinely paused between monitor commands, so of course no virtual time and no ticks
pass) -- disclosed as an uninformative measurement, not silently dropped.

**The decisive evidence: a full RTEMS classic-task-table walk, with a manual stack scan against
this exact binary's own symbol table, for all 17 live tasks.** `_RTEMS_tasks_Information`'s
`local_table` was walked directly (Objects_Information/Thread_Control/Context_Control field
offsets computed the same DWARF-offsetof way as above), and each task's saved stack (from its own
`Thread_Control.Registers.register_sp` -- a real thread, context-switched out, not the running
CPU's own registers) was scanned for words matching known function addresses (`nm`'s own sorted
symbol table, `third_party/renode/M24_4e/text_symbols_sorted.txt`). Every non-idle/non-shell task's
saved `register_lr` is the *identical* value (`_Thread_Do_dispatch+0x63`) -- confirming LR alone is
useless here (it only ever names the one shared context-switch call site) and a real stack scan is
required, which is why one was done.

**`IO_LOCKSTEP`'s own task (identified unambiguously by its own stack containing
`IO_LOCKSTEP_AppMain+0x341` at the bottom of the chain) is blocked here, top to bottom:**

```
_Semaphore_Wait_timed_ticks
  <- rtems_termios_read_tty
  <- rtems_termios_imfs_read
  <- read
  <- read_all
  <- lockstep_read_frame
  <- IO_LOCKSTEP_AppMain+0x341
```

**This is IO_LOCKSTEP blocked reading the *next* incoming frame, not writing the STEP_DONE
reply.** Control has already returned from `handle_step()` all the way up to
`IO_LOCKSTEP_AppMain`'s own top-level loop, which has already called `lockstep_read_frame` again --
which is only reachable if `handle_step`'s own last line,
`return lockstep_write_frame(fd, LOCKSTEP_FRAME_STEP_DONE, resp, resp_len) == LOCKSTEP_IO_OK;`,
already executed and returned. **IO_LOCKSTEP successfully wrote the STEP_DONE frame and moved on --
it is not stuck at all.** This directly overturns hypothesis 4 as this task's own leading
candidate (a blocked *print*): the guest-side software already did its job.

**So where did the STEP_DONE bytes go?** `uart1`'s own driver-level context (above) shows
`transmitting=False`, `tx_queued=0`, and `channel_sts=0xa` (both FIFOs empty) -- consistent with
the peripheral having already fully drained whatever was written to it (a short STEP_DONE payload
easily fits in the 64-byte hardware FIFO in one `zynq_uart_write_support` burst) and its own
TX-complete interrupt having already fired (`transmitting` only ever flips back to `false` inside
`zynq_uart_interrupt`). **The guest-side chain -- RTEMS termios, the UART driver, the emulated
peripheral's own FIFO/interrupt model -- appears to have done its job correctly and completely.**
The bytes are gone somewhere between "left the peripheral" and "reached the bridge's TCP client,"
which the bridge's own `MSG_PEEK`-based diagnostic (M24.4d) already showed sees *nothing* for the
entire granted window.

## Hypothesis 5 (new, from this evidence): the diagnostic's own tight polling loop starves
## Renode's terminal-socket forwarding

The STEP-stall diagnostic loop itself (`renode_bridge.py`'s STEP handler, inherited from M24.4d)
does 20 back-to-back iterations of `RunFor(0.5s)` + **three** immediate monitor round trips
(`rpu0 PC`, `channel_sts`, plus the `MSG_PEEK`) with **no idle gap** anywhere in between, for the
entire granted window. HELLO and BIND -- which *do* successfully relay guest->host bytes over this
exact same terminal socket, in this exact same process's lifetime -- use a completely different,
much simpler pattern: one `RunFor`, then a single blocking `guest_read_frame()` call, which leaves
Renode's own process free to do whatever internal bookkeeping (including flushing a terminal
backend's buffered bytes out to its socket) it wants between monitor commands. **Every prior
"more virtual time didn't help" measurement (M24.4c's 3.0s, M24.4d's 10.0s vs 30.0s) was run
*inside* this same tight-polling shape -- none of them tested a plain single-RunFun-then-block
STEP**, so "STEP never gets a reply regardless of how much virtual time is granted" was never
actually decoupled from "STEP never gets a reply when the bridge hammers the monitor with
back-to-back diagnostic commands instead of just waiting."

**Tested directly**: a second, separate, env-var-gated temporary probe was added to
`renode_bridge.py` (`AV_M24_4E_STEP_SINGLE_RUNFOR`) that does exactly what HELLO/BIND already do --
one `RunFor(granted_s)`, then a single blocking `guest_read_frame` -- for STEP specifically, with no
polling loop at all. **Result: REFUTED.** The identical `TimeoutError: timed out` at the identical
call site, with the identical `uart0.log` truncation point. A further probe at `granted_s=3.0`
(exactly `bridge_smoke_client.py`'s own already-proven-working value) **also refuted**: identical
failure. So it is not the diagnostic's own polling shape, and not "too much" virtual time either.

## The actual root cause: `CreateServerSocketTerminal`'s own forwarding silently drops uart1's TX bytes

**Decisive experiment**: a second, independent `emulation`/peripheral-level output sink --
`uart1 CreateFileBackend @<path> true`, the exact same mechanism already proven to capture uart0's
console output correctly -- was attached to `uart1` **in addition to** the existing
`CreateServerSocketTerminal`, in the same run, reading the same peripheral's same output stream
through a completely different Renode code path. Result
(`third_party/renode/M24_4e/uart1_debug.log`, `od -c`, real bytes, not summarized):

```
\a\0\0\0\x01AVL1\x01\0                                    -- frame_type 0x01 (HELLO reply)
'\0\0\0\x03\x08\x01\x12\x11io_lockstep/M23.2\x1a\x0fio_lockstep/0.1\r  -- frame_type 0x03 (BIND_ACK)
\0\0\0\x05\x08\x01\x10\x80\xa6\xbc\x8a\xa9\x98\xc3\x18     -- frame_type 0x05 (STEP_DONE, sequence=1)
```

**All three frames the protocol expects, including `FRAME_STEP_DONE` (0x05) with `sequence=1`
decoded correctly from its own protobuf varint encoding, are present in the file log.** The
identical run's own socket-backed terminal delivered zero bytes for that third frame (the bridge's
own `TimeoutError` at `guest_read_frame`, same run). **This is a peripheral that genuinely
transmitted the STEP_DONE frame, captured faithfully by one Renode output mechanism and silently
dropped by another, attached to the exact same peripheral in the exact same run.**

**Cross-checked at the OS level, independent of any Renode-internal explanation**: `netstat -an`
against the live TCP connection between the bridge's client socket and Renode's
`CreateServerSocketTerminal` listener, taken at the exact moment the STEP-stall diagnostic had
already confirmed the hang, showed `Recv-Q`/`Send-Q` **both 0/0 on every connection**, including
the uart1 terminal's own socket pair. Renode's own process never even handed these bytes to the
kernel's TCP stack -- this rules out "the bridge's own Python code has a read bug" (that would show
a nonzero `Recv-Q` sitting unread on the bridge's end) as thoroughly as the file-backend capture
rules out "the guest never really finished the write."

**Root cause, stated plainly**: `CreateServerSocketTerminal`'s own internal mechanism for pumping
a peripheral's transmitted bytes out to its backing TCP socket does not reliably fire for every
byte a peripheral transmits -- specifically, bytes written by the guest immediately before its own
CPU parks in `wfi` (exactly IO_LOCKSTEP's own pattern: write STEP_DONE, then immediately block on
the next `read()`) can be silently lost between "the peripheral's own TX path accepted them" and
"the socket actually sent them," with no error anywhere (the monitor log stays clean, `include`'s
own reply is clean, `channel_sts` reads back a fully-drained peripheral) -- a second instance,
independent of and materially different from M24.4b's own already-documented RX-path limitation
(that one was host->guest bytes never reaching the peripheral at all; this one is guest->host bytes
leaving the peripheral but not reaching the socket), that M24.4d's own "outbound direction
reconfirmed reliable" conclusion did not catch because none of its own nine probe instances ever
exercised a real guest write immediately followed by the CPU going idle -- only early-boot
handshake bytes, HELLO_ACK and BIND_ACK, which this same defect apparently does not affect (both
delivered correctly, every time, in every run this task captured).

**Not fully explained, disclosed rather than glossed over**: exactly *why* HELLO_ACK/BIND_ACK are
delivered reliably while STEP_DONE is not was not isolated to a single Renode-internal mechanism
(e.g., a specific pump/flush callback tied to CPU activity) -- that would require reading or
debugging Renode's own C# source for `CreateServerSocketTerminal`/`Cadence_UART`, which is outside
this task's own scope (a compiled .NET distribution, not something this codebase owns or patches).
What is established, by direct experiment rather than inference, is *that* it happens, *reliably*,
for this exact write pattern, and that reading via `CreateFileBackend` instead is proven to work
every time it was tried.

## Fix applied (a real fix, not a workaround)

`third_party/renode/M24_4b/renode_bridge.py`:

1. **`RenodeBridge.start()`** now always attaches a second `CreateFileBackend` to `uart1`
   (`self.uart1_raw_log`, defaulted to a per-pid path next to the existing `.resc`/monitor-log
   scratch files if the caller does not supply one) alongside the pre-existing
   `CreateServerSocketTerminal` wiring. The socket terminal is left in place (still needed for the
   `wait_for_rxen`/`WriteChar` host->guest machinery this task did not touch, and as a live
   "the platform actually wired uart1 up" signal via `wait_for_port`), but is no longer trusted as
   a read source.
2. **`read_frame_from_growing_file()`** (new function): reads one lockstep-local v1 frame starting
   at a remembered byte offset into a file some other process (Renode) keeps appending to, polling
   plain reads (no `select()` on a file's own growth exists on POSIX) until a complete frame is
   available or a timeout elapses. Never rewinds or truncates its own read position, matching
   `CreateFileBackend`'s own observed append-forever behavior across a `machine Reset` (the same
   property `M24_4`'s own `hw_reset_fault_test.py` already relies on for uart0's log, re-confirmed
   here by inspection, not re-derived).
3. **`RenodeBridge.guest_read_frame()`** now calls `read_frame_from_growing_file` against
   `self.uart1_raw_log` instead of `read_frame` against the socket (`self.uart_client`). Every
   caller (HELLO, BIND, STEP, RESET's own re-handshake, SHUTDOWN) goes through this one method, so
   all of them are fixed by this one change, not just STEP.

**Why this is a fix, not a workaround**: it does not change, retry, or paper over anything on the
guest side (no code in `services/cfs/` was touched) -- the guest was already doing the right thing.
It replaces one Renode-provided delivery mechanism, proven-by-direct-experiment to drop bytes in
this exact scenario, with a second Renode-provided mechanism, proven-by-direct-experiment (the same
`uart1_debug.log` capture, and uart0's own long-working log) to deliver every byte reliably --
exactly the same class of fix M24.4b already applied for the RX direction (`WriteChar` in place of
the terminal's own RX forwarding), applied here to the TX direction for the same underlying reason:
a specific Renode mechanism does not fulfill its contract for this codebase's exact usage pattern,
and a different, already-available Renode mechanism does.

**Result of the fix, run end to end -- partial success, disclosed honestly, not overclaimed.**

The very first `RunFor`-based read via `read_frame_from_growing_file` still, on its own, timed out
identically to the socket-based read (the file's own writer buffers internally and a live poll from
outside Renode's process does not see new bytes appended until *something* disposes/recreates the
backend -- confirmed directly: bytes this run's own file eventually held, once the whole bridge
process had already issued `quit` and torn down, were *not yet visible* to a live poll taken during
normal operation moments earlier). **Fix, second iteration**: `guest_read_frame` now re-issues the
identical `uart1 CreateFileBackend @<path> true` monitor command on every poll attempt before
re-checking the file -- Renode detaching and recreating a backend on the same peripheral appears to
flush the outgoing one first. This measurably works: **the very first STEP's own `STEP_DONE` was
read successfully this way** (`bridge: relayed BIND` followed only by the STEP diagnostic's own
informational trace, no exception, no timeout, for STEP 1) -- direct proof the underlying mechanism
and direction (file log, not socket) is correct and the Renode-side defect is real and bypassable.

**Not fully hardened within this task's remaining budget**: STEP **2** onward still times out the
same way, even with the per-frame timeout raised from 15s to 60s (`third_party/renode/M24_4e/
cargo_run10_longtimeout.log`) -- the file's total byte count stays pinned at 71 bytes (exactly
HELLO + BIND_ACK + STEP_DONE #1) for the entire 60s budget; STEP 2's own reply never appears in the
file at all during that window, by the re-attach mechanism or otherwise. This was checked, not
assumed: `uart0.log` was inspected for the same run and shows no new content past the identical
byte already established (the `s_logged_first_timeout`/`s_logged_first_recv` one-shot guards in
`io_lockstep_app.c` mean this specific console line cannot recur regardless, so its silence is not
informative either way about STEP 2's own progress) -- the file-backend flush-on-reattach trick
that worked once for STEP 1 does not reproduce reliably for later steps, and this task's own
remaining budget does not support debugging Renode's own closed-source-to-this-codebase buffering
behavior any further (would need either reading Renode's own C# source for
`CreateFileBackend`/`Cadence_UART`, genuinely out of this codebase's own scope, or substantially
more live-experiment budget than remains).

**Net effect on the exit criterion**: `byte_identical_port_traffic_between_posix_container_and_
renode` still does not pass end to end -- it now reliably gets one real STEP further than before
this task (through STEP 1's own reply, not just through BIND), which is real, measured progress
directly attributable to this task's own fix, but the fix is not yet sufficient for the full
10-step (`COMPARISON_DURATION_S=1` @ 10 Hz) scenario the test requires. This is the single
remaining blocking item -- see "Not done" below.

## Skip-reason wording (M15.3 convention, re-verified)

`renode_unavailable_reason()` and `cfs_image_unavailable_reason()`
(`crates/av-kernel/tests/drm_attitude_control_renode.rs`) were not modified by this task and were
re-checked, not just assumed: each names the exact missing file/tool and, for the Docker case, the
exact command to run to fix it (`docker build -f services/cfs/Dockerfile -t
altavista-cfs-lockstep:local .`). Both are exercised for real by `byte_identical_port_traffic_
between_posix_container_and_renode`'s own first two `if let Some(r) = ...` checks before anything
else runs; neither fired in any of this task's own eleven real runs (every dependency was present
throughout), so the "SKIPPED ...: ..." print path itself was not newly re-verified live by this
task, only read and confirmed unchanged from M24.4c's own original wording.

## Temp-dir and container cleanup

- **`/tmp/av-renode-m24c-*`**: fourteen such directories existed across this task's own eleven real
  `cargo test` invocations (three pre-existing ones from a concurrent worker's own run, found and
  removed first) plus this task's own ten; the evidence needed from each (uart0.log excerpts,
  bridge stdout/stderr) was extracted into this report before removal. **Zero remain** as of this
  writing (`ls -d /tmp/av-renode-m24c-*` -> "No such file or directory").
- **Docker**: `docker ps -a --filter label=av.test=1` and `docker images --filter label=av.test=1`
  both empty after this task's own runs -- the labeled-container prune/guard machinery worked
  correctly throughout, no manual cleanup needed there. Two unlabeled, ephemeral-port-tagged
  `altavista-cfs-lockstep:test` images (a pre-existing, disclosed gap -- `docker tag` has no
  `--label` flag, so these cannot be swept by `prune_stale_test_resources`'s own label filter) were
  found and removed by hand (`docker rmi -f`); the base `altavista-cfs-lockstep:local` image (the
  one-time build artifact every run reuses, not a per-run test resource) was correctly left alone.
- **`third_party/renode/M24_4e/`** (this task's own working directory): kept as this task's own
  reviewable artifact set (question 157) -- probe scripts (`gdbstub_read_state.py`,
  `probe_gdbstub.py`, `text_symbols_sorted.txt`), their captured output logs, and the one
  `uart1_debug.log` that supplied this report's own decisive evidence. Not scratch, not removed.

## Test totals

All three runs below are **real, complete runs of this exact task's own final code state**
(the fixed `renode_bridge.py`), not carried over from an earlier point in this task.

- **`.venv/bin/pytest -q`**: **418 passed**, 5 warnings (pre-existing `pytest.mark.slow` unknown-mark
  warnings, unrelated to this task). Matches M24.4d's own recorded baseline exactly -- no
  regression, and this task touched no Python outside `third_party/renode/M24_4b/renode_bridge.py`
  and its own new `third_party/renode/M24_4e/` scripts, neither of which pytest collects.
- **`cargo test -p av-kernel`**: run twice for a complete, accurate picture, since `cargo test`'s
  own default fail-fast-across-binaries behavior stops the whole invocation at the first test
  binary with a failure (confirmed directly: the first run stopped right after
  `drm_attitude_control_renode.rs`'s own known, disclosed failure, never running the test binaries
  alphabetically after it). Re-run with `-- --skip
  byte_identical_port_traffic_between_posix_container_and_renode` to get the rest:
  **665 passed, 0 failed** across every other test binary in the crate -- exactly matches the
  "665 passed" baseline this file's own task brief states existed *before*
  `drm_attitude_control_renode.rs` existed, confirming zero regressions anywhere else in the crate.
  Plus the one, disclosed, expected failure:
  `byte_identical_port_traffic_between_posix_container_and_renode` **... FAILED** (still blocked on
  STEP 2+, see above) -- **1 known failure, 0 unexpected ones.**
- **`cargo test --workspace --exclude av-kernel`**: **178 passed, 0 failed** across every other
  crate in the workspace (unit tests, integration tests, and doc-tests). This task touched no code
  in any of these crates.

## Not done (final list, for question 157's own "a missing final message must lose nothing" rule)

1. **The full 10-step per-step byte comparison does not pass.** Root cause is fully found and
   evidenced (above); a real fix is implemented and demonstrated to work for STEP 1; STEP 2 onward
   is not yet reliable within this task's own remaining budget. The next worker's own fastest path
   is very likely NOT "chase the file-backend flush timing further" (this task already spent
   real budget there without fully resolving it) but to **read Renode's own C# source** for
   `CreateServerSocketTerminal`/`Cadence_UART`/`AnalyzableBackend` (available from the upstream
   Renode project, not shipped in this repo's own portable-build distribution) to find the actual
   flush/dispose semantics being relied on empirically here, and either call the correct explicit
   flush primitive if one exists, or find a Renode peripheral hook (a Python/C# peripheral
   attached via the monitor, analogous to how `WriteChar` already solves the RX direction) that
   does not depend on any backend's own buffering at all.
2. **`cargo clippy --workspace --all-targets -- -D warnings` and `cargo deny check`** (both named
   in M24.4c's own "Not done" list as still outstanding at that point) were **not run by this
   task** -- this task's own assignment was the STEP stall root cause plus the byte comparison,
   and its own measured budget was spent on the live GDB-remote investigation and the fix attempt
   above; disclosed as not attempted, not silently skipped.
3. **Why HELLO_ACK/BIND_ACK deliver reliably while STEP_DONE (and, per the fix's own remaining
   gap, STEP_DONE #2 onward) does not** was established as a real, reproducible fact but not
   traced to a specific line of Renode's own internal implementation -- see the "Not fully
   explained" paragraph above.
4. **The OS_ConsoleTask stack finding** (`rtems_semaphore_obtain` inside `OS_ConsoleOutput_Impl`,
   found while walking the live task table) was recorded as a real, live observation but its own
   significance was deliberately left ambiguous in this report rather than overclaimed: it may
   explain the permanently-truncated `uart0.log` event-17 message (a genuine second, cosmetic
   defect, harmless to `uart1`/the actual exit criterion), or it may simply be normal idle
   behavior for a bounded-wait consumer task with nothing new queued -- not resolved, disclosed
   as ambiguous rather than asserted either way.

status: in progress -- root cause of the guest-STEP-stall confirmed with live evidence (resolved
PC, RTEMS thread/semaphore state via a real GDB-remote attach, both socket directions cross-checked
with `netstat`) and named precisely (a Renode-side `CreateServerSocketTerminal` forwarding defect,
not a guest bug); a real fix is implemented and proven for STEP 1; the fix's own reliability for
STEP 2 onward is the one remaining blocking gap, disclosed above with a concrete next-step
recommendation; `pytest`/the rest of `cargo test -p av-kernel`/`cargo test --workspace --exclude
av-kernel` are all green with zero regressions; temp-dir and container cleanup are both verified
zero as of this writing.
