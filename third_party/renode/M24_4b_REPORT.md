# M24.4b -- root-causing the `IO_LOCKSTEP` UART handshake failure, then the bridge and
# identical-traffic comparison

status: **partial** -- root cause found and FIXED with live proof (no workaround); the bridge is
built and proven end-to-end (HELLO/BIND/STEP/RESET/SHUTDOWN, including a real hardware power-cycle
fault through the bridge's own `Reset` handling); the clean-fetch patch test and the image-digest
rebuild are both done. **Not done:** the `demo_attitude_control` DRM wired through
`av_kernel::drm::execute` and the posix-container-vs-Renode byte-for-byte `RunProducts`
comparison -- the YAML fixture is authored and validated, but the kernel-level test itself was not
built; see "Final status and complete 'not done' list" at the end of this file.

Task: `docs/open-questions.md` questions 145, 148, 153, 156, 157, 164; `services/cfs/README.md`
(lockstep-local v1 spec); builds on `third_party/renode/M24_4_REPORT.md` (M24.4), which left the
`IO_LOCKSTEP` handshake over `/dev/ttyS1` failing even against a live test peer resending HELLO
21 times through the exact failure window, with three unweighted suspects and root cause not
found within its own budget.

Written incrementally per question 157's own rule (a missing final chat message must lose
nothing). Sized for ~300 tool uses; long Renode runs are backgrounded, not polled tightly.

## Not done yet (running list, updated as items close)

- [x] 1. Root-cause the `IO_LOCKSTEP` handshake failure with captured-bytes evidence (no
      workaround) -- **DONE, see below**: RTEMS termios `rawInBufSemaphoreWait` is
      `calloc`-zeroed `false` at `open()` and only ever set `true` by an explicit `tcsetattr`;
      `io_lockstep_app.c`'s UART `connect_to_shim()` never called it, so every `read()` was
      effectively non-blocking and `lockstep_local_io.c`'s `read_all()` (correctly, for its only
      previously-tested transport) treated the resulting `read()==0` as `LOCKSTEP_IO_ERR_PEER_CLOSED`
      and gave up instantly, before any peer byte could matter.
- [x] 2. Fix it (a real fix, not a retry/timeout/sample change) and prove the handshake
      succeeds live -- **DONE**: `connect_to_shim()` now calls `tcgetattr`/`tcsetattr` (raw mode,
      `VMIN=1`/`VTIME=0`); `core-cpu1.exe` rebuilt; `handshake_fix_test.py` proves a live HELLO
      exchange succeeds byte-for-byte (see below).
- [x] 3. Build the bridge process (owns Renode, slaves virtual time via `RunFor`, speaks
      lockstep-local to `crates/av-lockstep-shim`) -- **DONE**, see below. Because Renode's
      own terminal backends (socket- and PTY-based, both tried) do not forward externally-supplied
      bytes into `uart1`'s RX path on this build (see "Isolating the defect further" above), the
      bridge injects host->guest bytes via the monitor's `WriteChar` primitive (proven reliable)
      rather than via a terminal socket, and reads guest->host bytes via the terminal's own
      already-proven-working outbound direction. Proven live end to end (real gRPC Bind/Step/
      Reset/Step/Shutdown against the real shim, bridge, Renode and RTEMS/cFE/IO_LOCKSTEP guest).
- [~] 4. `demo_attitude_control` DRM bound to the Renode instance via the bridge --
      **`drms/demo_attitude_control_controller_renode.system.yaml` authored and validated**
      (parses cleanly through `crate::drm::schema`, real hash computed via `cargo run -p av-kernel
      --example drm_hash`), but **`crates/av-kernel/tests/drm_attitude_control_renode.rs` itself
      was NOT built** -- see below for exactly what stands between here and it, and why this task
      stopped short of it deliberately rather than risk an unfinished, untested kernel test.
- [ ] 5. Identical port traffic, posix container vs Renode, byte-compared per step (question
      145; RTEMS task order recorded, not asserted) -- **not attempted**, blocked on item 4's
      kernel test; see below for the concrete plan and what already de-risks it.
- [x] 6. A HARDWARE fault exercised through the bridge (`machine Reset` already proven live in
      M24.4, standalone; wiring it through the bridge's `Reset` RPC is new) -- **DONE, live**: the
      bridge's own `Reset` handling issues a real `machine Reset`, confirmed by two full
      `POWER ON RESET`/`OPERATIONAL` boot sequences in the UART0 transcript, then transparently
      re-handshakes and re-binds the freshly-rebooted guest before acking the shim -- see below.
- [x] 7. Clean-fetch patch test -- **DONE**: `services/cfs/tests/test_clean_fetch_patches.py`.
      This task's own termios fix needs no patch file (see below); only the pre-existing M24.4
      `/cf`-startup-file patch (a real, in-place edit inside `third_party/cfs` itself) needs it.
- [x] 8. Rebuild `services/cfs/IMAGE_DIGEST.md`, last, after every other edit is final --
      **DONE**, see below

## Hypotheses for the handshake failure (stated before measuring, per the task brief)

M24.4's own report narrowed the field to three candidates without distinguishing between them;
this task's starting hypothesis set, in the order they will be checked, plus one additional
hypothesis this task adds from reading the actual source (not in M24.4's list):

- **H1 (M24.4's (a)).** `io_lockstep_app.c`'s UART-branch handshake read is a single
  non-blocking/short-timeout attempt, not a retrying one, so it gives up before the peer's bytes
  arrive. **Status after reading the actual source (see below): REFUTED before running anything**
  -- `lockstep_local_io.c`'s `read_all`/`write_all` retry unconditionally on every short read
  until the syscall itself errors (other than `EINTR`, which is retried) or the peer closes; this
  is not a single-attempt or short-timeout read for either transport. This is documented as a
  finding below, not assumed.
- **H2 (M24.4's (b)).** Renode's `Cadence_UART` model / `CreateServerSocketTerminal` +
  `connector Connect` wiring does not actually deliver bytes written from the TCP side into the
  peripheral's RX FIFO at all (a transport-wiring defect, independent of the guest).
- **H3 (M24.4's (c)).** RTEMS's termios layer over `/dev/ttyS1` requires configuration
  (baud/raw mode, or specifically leaves the port in canonical mode with no EOL byte in this
  binary protocol) that `io_lockstep_app.c` never performs, so bytes are misframed, discarded, or
  the read blocks forever waiting for a line terminator that never comes.
- **H4 (new, from reading `third_party/rtems-container/work/rtems-src/bsps/shared/dev/serial/
  zynq-uart.c` and this BSP's `bspopts.h`).** This exact BSP build has
  `ZYNQ_CONSOLE_USE_INTERRUPTS=1` (confirmed by reading the compiled `bspopts.h` directly, not
  assumed), so `/dev/ttyS1`'s RX path is entirely **interrupt-driven**
  (`zynq_uart_handler.mode = TERMIOS_IRQ_DRIVEN`): a byte only ever reaches the termios layer
  `read()` sees via `zynq_uart_interrupt()` firing on the `RTRIG` GIC interrupt (uart1 ->
  `rpuGic@22` in the platform file) and calling `rtems_termios_enqueue_raw_characters`. If
  Renode's GIC/interrupt delivery for `uart1`'s line is not actually working on this platform
  (an open question M24.4's own predecessor, M24.2d, already flagged as unresolved for a
  *different* peripheral's interrupt-enable register, `ISENABLER1`/TTC0, and waved off as
  irrelevant there only because TTC ticks were independently, visibly happening through *some*
  mechanism regardless), bytes could sit correctly in the RX FIFO forever without the guest's
  driver ever being told they arrived. This is a hypothesis to be measured, not assumed true --
  M24.2d's own TTC finding is explicitly **not** evidence about UART's IRQ line, since ttc0/uart1
  are different SPI IDs (36-38 vs 22) and the two peripherals' interrupt-delivery paths have
  never both been exercised in the same test.

## Evidence gathered before running anything new (re-reading M24.4's own artifacts)

**A finding M24.4's own report did not surface: the "with peer" and "without peer" UART0
transcripts are byte-identical.** `third_party/renode/M24_4/cfs_handshake_uart0.log` (the run
with a live TCP peer connected before boot, sending HELLO on connect and 20 more times across
the exact failure window) has **the same MD5** (`f375f551d64cbaa7e858af6022a6f280`) as
`third_party/renode/M24_4/cfs_boot_uart0.log.1` (`cfs_boot_smoke_test`'s own transcript, taken
with **nothing at all** connected to `uart1`). Every timestamp, every event sequence number, and
the exact instant the `IO_LOCKSTEP: lockstep-local handshake failed` event fires are identical
down to the byte, in cFE's own simulated-time log, between a run with an actively-resending live
peer and a run with no peer whatsoever.

This is stronger evidence than M24.4's own "21 attempts, still zero bytes back" framing suggests:
it is not merely that the peer's bytes did not arrive *in time* -- the guest's entire boot
transcript, including the exact moment of failure, is **provably unaffected by the peer's
presence at all**, which rules out any timing/race explanation (already mostly ruled out by
M24.4's own repeated-HELLO design) and now also weighs against H3's "read blocks forever on a
missing EOL" framing, since a `read()` that were actually blocking on termios input would leave
*some* opportunity for the peer's bytes to change what happens next; instead nothing downstream
of the failure differs by even one byte. This is consistent with a `read()` (or `open()`) that
fails via an immediate, deterministic error path independent of anything external -- pointing
initially toward H2 or H4 over H3, but not yet decisive on its own: the next step captures actual
register state to distinguish them directly, per the task brief's explicit instruction not to
treat an assertion about what should be on the wire as evidence of what is.

## Direct register instrumentation

`third_party/renode/M24_4b/uart1_register_probe.py` + `.resc`: reuses the exact
`CreateServerSocketTerminal`/`connector Connect` wiring and HELLO-resending peer design already
proven live in M24.4's `cfs_handshake_test.py`, and additionally reads `uart1`'s real registers
directly off the sysbus via the monitor (`sysbus ReadDoubleWord`, the same last-hex-token parsing
`ttc_rate_test.py` already proved correct on this build) at every step:

- `control` (`0xff010000`, bit2 `RXEN`, bit4 `TXEN`) -- does the guest's driver ever actually
  enable RX on this UART?
- `channel_sts` (`0xff01002c`, bit1 `REMPTY`, bit2 `RFUL`, bit0 `RTRIG`) -- does the RX FIFO ever
  show non-empty, independent of whether the guest's driver ever reads it?
- `irq_sts` (`0xff010014`, bit0 `RTRIG`) -- does the RX-trigger interrupt condition ever latch?
- `tx_rx_fifo` (`0xff010030`) -- read last (destructive, pops one byte) to see what, if anything,
  is actually sitting in the FIFO.

All four offsets are read directly from this exact BSP's own header
(`third_party/rtems-container/work/rtems-src/bsps/include/dev/serial/zynq-uart-regs.h`), not
assumed from generic documentation.

**Result (`uart1_register_probe_result.json`, and its `_v2`/`_v3` reruns -- all three agree):**
`control` starts at `0x128` (`RXDIS|TXDIS|STPBRK`, the documented Zynq UART POR value) and flips to
`0x114` (`RXEN|TXEN|STPBRK`) partway through boot (step 16-22 of the 0.05s-step loop, i.e. the
guest's own UART driver genuinely does enable the receiver during console/PSP init) -- so **H4
(GIC/IRQ-enable doubt) and any "the guest never turns RX on" theory are REFUTED**: RX is enabled
on the real control register, confirmed independent of the guest, well before and throughout the
whole handshake-failure window. Despite that, `channel_sts` reads `0x0a` (`REMPTY=1, TEMPTY=1`)
at **every single sample**, before and after RXEN, across 21-42 resends of a live peer's `HELLO`
frame -- **the RX FIFO never once shows non-empty**, and `popped_fifo_bytes` is empty every run.
This is the first piece of direct, captured-register evidence (not an assertion about what should
be on the wire): **bytes sent by an external TCP client to uart1's bridge port never reach the
peripheral's own RX FIFO**, full stop, independent of anything the guest does.

## Isolating the defect further: TX direction and direct WriteChar both work; TCP-side RX does not

Three follow-up probes, each removing one more variable, converge on the same precise defect:

**1. `uart1_tx_direction_probe.py` -- outbound (peripheral register -> external TCP client)
direction of this exact same wiring.** Bypasses the guest CPU entirely: forces `TXEN` on and
writes a known ASCII string one byte at a time directly into `tx_rx_fifo` via the monitor
(`sysbus WriteDoubleWord`), then reads the external TCP client socket. **Result: PASS** -- the
external client received `b'AV_M24_4B_TX_DIRECTION_PROBE\n'` byte-for-byte
(`tx_direction_run.log`). The `CreateServerSocketTerminal`/`connector Connect` wiring is
**not** dead in general -- the outbound half works perfectly.

**2. `uart1_writechar_injection_probe_v2.py` -- Renode's own peripheral-level RX-injection
method, called directly.** RunFor-steps (same method as the register probe) until `RXEN` is
genuinely observed on the real control register (step 22, `control=0x114`), confirms
`channel_sts` reads `REMPTY=1` beforehand, then issues `sysbus.uart1 WriteChar 0x41` via the
monitor -- Renode's own built-in mechanism for "a byte arrived on this UART's RX line," the same
primitive a working terminal backend would call internally. **Result: PASS** --
`channel_sts` immediately flips to `REMPTY=0`, and popping `tx_rx_fifo` returns exactly `0x41`
(`'A'`) (`writechar_v2_run.log`). (A first attempt at this test, `uart1_writechar_injection_probe.py`
v1, got a false negative by calling `WriteChar` with **no `RunFor` at all** -- the guest never ran,
`RXEN` was never set, and a receiver that is genuinely disabled is *correctly* expected to drop an
injected byte; this is noted here, corrected, and not treated as evidence of anything, per this
task's own root-cause discipline.) This directly refutes **H2 in its strong form** ("the
`Cadence_UART` model itself does not work") -- the model's RX FIFO and control-register semantics
work exactly as documented once RX is enabled, via Renode's own native injection primitive.

**3. `uart_tcp_single_byte_probe.py` -- the same RXEN-confirmed window, but the byte arrives over
the TCP client socket instead of `WriteChar`, structured identically to (2) for a direct,
apples-to-apples comparison.** RunFor-steps to the same `RXEN` confirmation (step 22, identical
`control=0x114`), confirms `channel_sts` `REMPTY=1` beforehand (identical `0x0a`), sends a single
byte `0x41` (`'A'` -- the *same* byte value probe (2) used) over the external TCP client socket
that the bridge port serves, waits 0.2s of real wall time plus one more `RunFor "0.01"` to give
Renode's own socket-terminal backend every opportunity a real byte arrival would get, then reads
`channel_sts` again. **Result: FAIL** -- `channel_sts` still reads `0x0a`, `REMPTY=1`, unchanged.
The single variable that differs between probe (2) (PASS) and probe (3) (FAIL) is: which
mechanism deposits the byte -- Renode's own `WriteChar` call, or a byte actually sent over the
socket that `CreateServerSocketTerminal`'s server-socket backend is supposed to forward into that
exact same `WriteChar` path.

**Root cause, stated precisely and pinned to captured bytes, not an assumption:** on this exact
Renode 1.16.1 macOS-arm64 build, the `CreateServerSocketTerminal` + `connector Connect
sysbus.uart1 <terminal>` wiring's **inbound** direction -- bytes written by an external TCP client
into the socket the terminal serves -- never reaches the connected UART peripheral's `WriteChar`
(equivalently, its RX FIFO), even though (a) the guest's own driver genuinely enables `RXEN` (H4
refuted), (b) the identical wiring's **outbound** direction (peripheral register writes -> TCP
client) works perfectly (probe 1), and (c) the peripheral's own RX-injection primitive works
correctly once called (probe 2). This is **not** `io_lockstep_app.c`'s fault, and not RTEMS
termios's fault (**H1 and H3 REFUTED** by this same evidence: nothing in the guest ever gets a
chance to misframe or block on a byte that never arrives at the hardware level at all -- the drop
happens entirely below the guest, in Renode's own terminal/connector plumbing for this specific
socket-terminal type, on this exact build). **Ruled out as build-config-specific, and generalized to a second, independent terminal backend
(strengthens the root cause; this is the decisive evidence):** `uart_terminal_type_probe.py`
(the `CreateServerSocketTerminal` `false`/`true` flag argument, and a trailing-newline payload) was
inconclusive on its own merits -- a scripting bug (missing `using sysbus`/`using sysbus.cluster0`
before issuing bare `cluster0`/`rpu0` commands the way every *other* working probe in this task
does) meant the guest CPU never actually ran in any of the three variants (`RXEN` never observed),
so `uart_terminal_type_probe_result.json`'s three `"worked": null` entries are **not evidence
either way** and are recorded here only so this is not silently miscounted as a fourth confirming
run later. Rather than keep tuning that one terminal type's constructor flags, `uart_pty_terminal_probe.py`
tests an **entirely different Renode terminal backend** -- `emulation CreateUartPtyTerminal`
(a real host PTY device, `/tmp/av_m24_4b_uart1.pty`, opened directly with `os.open`/`os.write`,
no TCP/sockets involved at all) wired to `sysbus.uart1` the same way. Using the same corrected
RXEN-confirmation method (`RXEN` observed at the identical step 22, `control=0x114`, matching
every other probe in this report byte-for-byte): writing `b"A"` directly into the PTY master
**also never reaches `uart1`'s RX FIFO** (`channel_sts` stays `0x0a`, `REMPTY=1`,
`pty_terminal_run2.log`). This rules out "it's specifically a TCP-socket-terminal quirk" as the
explanation and generalizes the finding: **on this exact Renode 1.16.1 macOS-arm64 build, no
generic external-terminal backend this task tried (socket-backed or PTY-backed) successfully
forwards externally-supplied bytes into this `Cadence_UART` instance's receive path, while the
peripheral's own native `WriteChar` method (invoked directly through the monitor) does, every
time, byte-for-byte, the instant `RXEN` is genuinely on.** The defect is therefore localized to
whatever internal call each terminal-backend type is (or is not) making into the attached
peripheral on receiving external bytes -- not to `io_lockstep_app.c`, not to RTEMS termios, and
not to one specific terminal constructor's flags.

**Consequence for the bridge design (not a workaround for the root cause -- a design choice that
follows directly from it):** since Renode's own `WriteChar` primitive is the one mechanism this
task measured to reliably deliver a byte into `uart1`'s receive path on this build, the bridge
(built below) injects host-to-guest bytes one at a time via `sysbus.uart1 WriteChar <byte>` over
its existing Renode monitor connection, rather than relying on a terminal backend's automatic
forwarding for that direction. It continues to use the already-proven-working
`CreateServerSocketTerminal` **outbound** direction (probe 1, above) to capture the guest's own
UART1 transmissions, since that half of the exact same wiring is independently proven correct.
This is not "a retry, a longer timeout, or a different sample" of the failing mechanism -- it is
routing around a specific, now-evidenced Renode limitation using a different, independently-proven
Renode mechanism for the one direction that limitation affects.

## The actual root cause of the handshake failure itself (not the Renode wiring -- the guest code)

The Renode-wiring investigation above fully explains why an external byte injected via a terminal
never reaches the guest. It does **not** yet explain why the very first `handshake_fix_test.py`
attempt (WriteChar-injecting the shim's `HELLO` right after `RXEN` was confirmed at step 22) still
failed, at the identical simulated timestamp (`14:03:20.59188`) M24.4's own "peer present" and
"no peer at all" runs both landed on. If the guest's `read()` were genuinely blocking and simply
never got fed a byte in time, injecting a byte after confirming `RXEN` should have worked. It
didn't -- which means the failure was not "no byte arrived in time," but something that resolves
before any byte could possibly matter, every time, deterministically.

**Reading `third_party/rtems-container/work/rtems-src/cpukit/libcsupport/src/termios.c` directly
(not assumed) finds the actual defect, in our own application code:**

- `rtems_termios_open_tty` allocates the `tty` structure with `calloc(1, sizeof(*tty))` (that
  file's own line 362) -- every field starts at its zero value, including the `bool`
  `rawInBufSemaphoreWait`.
- `rawInBufSemaphoreWait` selects, in `fillBufferQueue`'s wait step (line ~1575), between a
  **blocking** wait (`rtems_binary_semaphore_wait_timed_ticks`, when true) and a **non-blocking
  poll** (`rtems_binary_semaphore_try_wait`, when false) for the next raw input character.
- `rawInBufSemaphoreWait` is set to `true` **only** inside the `TIOCSETA`/`TIOCSETAW`/`TIOCSETAF`
  ioctl handler (that file's own lines ~808-830, reached only through an explicit `tcsetattr()`
  call) -- specifically the branch taken when `c_lflag & ICANON` is clear and `c_cc[VMIN] != 0`.
  **It is never set anywhere in the `open()` path.**
- `io_lockstep_app.c`'s UART-branch `connect_to_shim()` (before this task's fix) was a bare
  `open(device_path, O_RDWR)` -- **no `tcgetattr`/`tcsetattr` call at all.**

The consequence: on this fd, `rawInBufSemaphoreWait` stays at its `calloc`-zeroed `false` for the
device's entire life. Every `read()` -- including `do_handshake()`'s very first one, which runs
immediately after `connect_to_shim()` returns -- polls the raw input queue exactly once
(non-blocking) and, finding it empty (as it always is at that instant, since `do_handshake` runs
before any peer byte could possibly have arrived), returns having moved **zero bytes**. RTEMS's
`rtems_termios_read_tty` reports this as `RTEMS_SUCCESSFUL` with `bytes_moved = 0`, which surfaces
to the caller as a plain POSIX `read() == 0`. `lockstep_local_io.c`'s `read_all()` -- written
correctly for its only previously-exercised transport, a connected Unix-domain stream socket,
where `read() == 0` unambiguously means the peer closed the connection -- treats this `0` as
`LOCKSTEP_IO_ERR_PEER_CLOSED` and returns immediately, **without retrying**, because a genuine
"peer closed" is never something to retry. `do_handshake()` then returns `false`, and
`IO_LOCKSTEP_AppMain` logs "lockstep-local handshake failed" and calls `CFE_ES_ExitApp` -- all
within the same tick `connect_to_shim()` returned in, before the RunFor-stepped register probes'
own `RXEN`-confirmation loop would even get a chance to run further.

**This precisely explains every piece of evidence gathered by this task and by M24.4:**
- Byte-identical UART0 transcripts with and without a live, continuously-resending peer (M24.4's
  own finding) -- the read fails before any external byte could possibly matter, every time,
  regardless of what is or is not on the wire.
- `channel_sts` reading `REMPTY=1` at every sample in every register probe -- true, but beside the
  point: even if a byte *had* reached the FIFO, `IO_LOCKSTEP` had already given up and exited.
- `handshake_fix_test.py`'s first run still failing even with a correctly-delivered `WriteChar`
  injection timed to `RXEN`'s confirmation -- the guest's own read attempt, and its failure, both
  happen at essentially the same instant as `open()`, well before this task's own step-22
  detection loop (a wall-clock-driven probe, not synchronized to the guest's own instruction
  stream) could plausibly land its injection in time.

**Root cause, stated precisely:** `io_lockstep_app.c`'s UART transport never configures the port
(no `tcgetattr`/`tcsetattr`), so RTEMS's termios layer leaves the receive path in an
uninitialized, effectively non-blocking polling mode (`rawInBufSemaphoreWait == false`, its
`calloc` zero-value, never flipped by any code path this app exercises) in which `read()` returns
`0` instantly whenever the queue is momentarily empty -- and `lockstep_local_io.c`'s `read_all()`,
correct for a stream socket, misinterprets that `0` as an immediate, unretried "peer closed"
rather than "nothing yet." This is a real, narrow, verifiable defect in this platform's own code
(not RTEMS's, not Renode's, not a design flaw in the "one retrying read loop" pattern itself,
which is and remains correct for the socket transport it was written and tested against first).

## The fix (real, not a workaround) and live proof

**Fix applied**, `services/cfs/apps/io_lockstep/fsw/src/io_lockstep_app.c`, UART-branch
`connect_to_shim()`: after `open()` succeeds, call `tcgetattr`, then set the port to raw,
blocking mode -- clear `ICANON`/`ECHO`/`ECHOE`/`ECHOK`/`ECHONL`/`ISIG`/`IEXTEN` (no line editing,
echo, or signal generation on an opaque binary protocol), clear `BRKINT`/`ICRNL`/`INLCR`/`IGNCR`/
`ISTRIP`/`IXON`/`IXOFF`/`IMAXBEL` (no newline translation or software flow control -- this
protocol's payload bytes are arbitrary and must never be reinterpreted), set `CS8|CREAD|CLOCAL`,
and set `c_cc[VMIN] = 1, c_cc[VTIME] = 0` -- precisely the `TIOCSETA` branch that flips
`rawInBufSemaphoreWait` to `true`, fixing the actual defect at its source -- then call
`tcsetattr(fd, TCSANOW, &tio)`. Either `tcgetattr` or `tcsetattr` failing is treated as a hard
connect failure (closes the fd, returns -1), matching this function's existing contract, rather
than silently continuing in the broken default mode.

**Rebuild, verified by content, not exit code:** `third_party/rtems-container/build-cfs-cross.sh`
re-run against the pinned toolchain (see "Rebuilding under `--network none`" below for how the
one missing build dependency, `cmake`, was handled without test-time network use). New
`core-cpu1.exe`: `file`/`readelf` confirm the same ELF32/ARM/EABI5 static-executable shape as
every prior verified build (entry point `0x40`, 4 program headers, 40 section headers), and a new
SHA-256, **`54eb11920ff5c7e4700f1191ed7736be7ef33ceceea85420db20d3fb2b837c79`** (M24.4's own
pre-this-fix hash was `07c65280c58154b44c34ba0c1f2a9d47ea86d32338eca0f4d2911a3b3715e96a`) --
confirming the rebuild actually picked up the source change, not merely that the build tool
exited zero.

**Live proof, `third_party/renode/M24_4b/handshake_fix_test.py` (`handshake_fix_run2.log`):**
against the rebuilt ELF, RunFor-steps to the same `RXEN`-confirmed point (step 22, identical to
every prior probe in this report), injects the shim's real `HELLO` frame
(`07 00 00 00 01 41 56 4c 31 01 00`) via `WriteChar` one byte at a time, then reads the guest's
own UART1 output through the terminal's proven-working outbound direction. **Result: the guest
sends back 11 bytes, `070000000141564c310100`, byte-for-byte identical to the shim's own `HELLO`**
-- `IO_LOCKSTEP` genuinely parsed the injected `HELLO`, validated its magic and version, and
replied with its own, per `do_handshake()`'s own logic. UART0's log no longer contains
"lockstep-local handshake failed" at all (present in every run before this fix, absent in this
one), and does not show "BIND failed" either -- consistent with the app now sitting correctly
blocked inside `handle_bind()`'s own `lockstep_read_frame`, waiting for a `BIND` frame this
particular test deliberately never sends, exactly the next stage `services/cfs/README.md`'s own
protocol sequencing describes. **The handshake is fixed, and this is proven live, not asserted.**

## Rebuilding under `--network none` (question 154 precedent, not a new exception)

`third_party/rtems-container/build-cfs-cross.sh` needs `cmake`, which M24.2c's `rtems-m24c:build`
image does not carry (confirmed directly: `docker run --rm --entrypoint cmake rtems-m24c:build
--version` -> "executable file not found in $PATH", not assumed from the script's own "if not
already present" comment). Installing it fresh via `apt-get` needs the network -- exactly the
one-time, already-documented exception question 154 and this same script's own header comment
describe for image-build time, which M24.3 and M24.4 each also relied on before this task. This
task takes that precedent one step further so it is a **true one-time** cost rather than a
recurring one every worker since M24.3 has apparently had to pay again: a throwaway, labeled
container (`--label av.test=1`) runs `apt-get update && apt-get install -y cmake` once, and is
then `docker commit`-ed to a new tag, `rtems-m24c:build-with-cmake` (also labeled
`av.test=1`/`av.m24_4b=...`, question 156), so the network cost is paid exactly once by this task
and never again by a future one reusing this tag. The actual cross-build itself then runs with
**`docker run --network none`** against that image -- genuinely network-free, not merely
"expected to be" -- confirmed by the container succeeding under a flag that would hard-fail any
network access attempt. `docker ps -a`/`docker images` at the start of this task were checked for
pre-existing orphaned containers from earlier work (none from this task's own prior probes were
left running; the `rtems-m24c-build`/`rtems-container-build` containers already `Exited` from
unrelated earlier tasks were left alone, matching M24.4's own precedent of not touching containers
it did not create).

## The bridge: design, implementation, and live end-to-end proof

**File:** `third_party/renode/M24_4b/renode_bridge.py`. Owns a Renode process (launches it,
`include`s a generated `.resc` that loads our platform file (M24.4 item 1) and the fixed
`core-cpu1.exe`, wires `uart1` to `CreateServerSocketTerminal` for its proven-working *outbound*
direction only), connects to that terminal port itself as a plain TCP client (captures the guest's
own UART1 transmissions), and connects to `crates/av-lockstep-shim`'s listening Unix socket as its
one peer -- exactly `services/cfs/README.md`'s "who listens, who connects" contract, with the
bridge standing in for what a native AF_UNIX-capable guest would otherwise do directly.

**Relay loop:** reads one whole lockstep-local v1 frame at a time from the shim (`read_frame`,
length-prefixed per `services/cfs/README.md`'s own frame layout), and for each:
- **HELLO/BIND/SHUTDOWN:** injects the frame's raw bytes into `uart1` one byte at a time via
  `sysbus.uart1 WriteChar <byte>` (the root-caused fix above's own proven mechanism), issues a
  small fixed `RunFor` so the guest CPU actually executes and processes them, then reads the
  guest's own reply frame back off the outbound terminal socket and forwards it verbatim to the
  shim.
- **STEP:** decodes `LockstepStepRequest.until_tai_ns` (using this repo's own generated Python
  protobuf stubs, `altavista.pb.altavista.v1.lockstep_pb2` -- the same `.venv` this repository's
  own `services/lockstep-ref` Python service already uses, not a hand-rolled decoder) and issues
  `RunFor` for the delta since the last known TAI target before checking for the guest's reply --
  M24.4's own TTC-rate finding (2.0e-4 relative error against our platform file) is what makes
  "Renode virtual seconds elapsed" a trustworthy proxy for "guest TAI ns elapsed."
- **RESET:** handled specially, not simply relayed -- see the next section.

**RESET / the HARDWARE fault, wired through the bridge and proven live:** a real Renode
`machine Reset` (M24.4's own `hw_reset_fault_test.py`: proven to genuinely reboot cFE) is a more
faithful model of a hardware power-cycle than the container binding's own `psp_lockstep_init`
in-place reset (the best a real POSIX process can do, having no hardware to power-cycle --
`crates/av-kernel/README.md`'s own "Reset wired to power-cycle faults" section). So on a `RESET`
frame the bridge (1) issues `machine Reset` on the monitor, (2) waits for `RXEN` again (the guest
reboots from scratch and re-runs its entire boot sequence), (3) replays the *exact original raw
bytes* of the `HELLO` and `BIND` frames it cached from this same run's own start (the
freshly-rebooted guest remembers nothing and must redo both), discarding the guest's own replies
to them, then (4) synthesizes a `RESET_ACK` carrying the original request's `sequence` and sends
that to the shim. The shim, and the kernel behind it, see only an ordinary `Reset`/`RESET_ACK`
exchange on the same live connection -- exactly `lockstep.proto`'s own contract -- with the
reboot-and-rehandshake machinery entirely hidden inside the bridge.

**Live proof, end to end, real processes throughout (no stubs):**
`third_party/renode/M24_4b/bridge_smoke_client.py` drives the *real* `altavista.v1.LockstepService`
gRPC surface (this repository's own generated Python stubs, `grpc` from the project's `.venv`)
against `crates/av-lockstep-shim`'s real compiled binary, fronted by `renode_bridge.py`, fronting
the real, rebuilt (root-cause-fixed) `core-cpu1.exe` under a real Renode process:

```
=== Bind ===
lockstep_capable=True version='io_lockstep/0.1' refusal_reason=''
=== Step 0 (until_tai_ns=1003000000000) ===
reached_tai_ns=1003000000000 outputs=0
=== Step 1 (until_tai_ns=1006000000000) ===
reached_tai_ns=1006000000000 outputs=0
=== Step 2 (until_tai_ns=1009000000000) ===
reached_tai_ns=1009000000000 outputs=0
=== Reset (hardware power-cycle through the bridge) ===
reset sequence echoed=4
=== Step after Reset (until_tai_ns=1012000000000) ===
reached_tai_ns=1012000000000 outputs=0
=== Shutdown ===
PASS: full Bind/Step/Reset/Step/Shutdown cycle succeeded end to end
```

(`Step`'s `outputs=0` every time is expected and correct, not a defect: this smoke test sends no
`TO_BUS` inputs at all, so `ADCS` never receives a measurement and never publishes -- exactly the
"nothing to report yet" case `io_lockstep_app.c`'s own module doc comment describes, which is why
`base_period_ns` was deliberately set above `IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS` (2000 ms) so the
bridge's per-`STEP` `RunFor` gives the guest enough virtual time to actually hit that timeout and
reply, rather than timing out the *bridge's own* read.)

**The `Reset` genuinely power-cycled the emulated hardware, confirmed by content
(`bridge_smoke_uart0_v2.log`, 11,203 bytes, the file backend only ever appends):**
`entering OPERATIONAL state` appears **twice**, `POWER ON RESET` appears **twice**, and
`lockstep-local handshake failed` appears **zero times** anywhere in the whole transcript --
i.e. both the original boot and the post-`Reset` reboot completed a full, clean boot **and** a
successful handshake, entirely through the bridge's own machinery, with no human-run monitor
script standing in for it this time (unlike M24.4's own standalone `hw_reset_fault_test.py`).

**What this is not yet:** this smoke test drives the shim directly over gRPC from a small Python
script, not through `av_kernel`'s own `BINDING_KIND_CONTAINER` executor / a `drms/*.yaml` DRM --
see "Item 4" below for why that further step was not completed within this task's budget, and
exactly what stands between here and it.

## Clean-fetch patch test (question 148)

**File:** `services/cfs/tests/test_clean_fetch_patches.py`. Fetches `third_party/cfs` fresh into
a throwaway scratch directory (`CFS_FETCH_DEST`, a mechanism `third_party/fetch-cfs.sh` already
supported for exactly this purpose -- never the shared tree), then checks, in order: (1)
`fetch-cfs.sh` exits 0; (2) its own stdout literally reports `applying
cfe_psp_start-cf-startup-file.patch` (content evidence the patch step ran, not merely that the
overall script happened to exit 0); (3) the patched file
(`psp/fsw/pc-rtems/src/cfe_psp_start.c`) contains the patch's own marker text; (4) its SHA-256
matches a hash **recorded here from a real run of this exact test, computed independently before
the assertion existed** (`7e6d17bd067707308e53ee39c67160f040a8eed2f1bb1b5539d8acfe6ca91b57`); and
(5) that hash genuinely differs from a fresh, separately-cloned, unpatched copy of the same pinned
PSP commit (`c4b3b0b65b119e106481ad8e20976ae4d7f554e3`) -- proof the patch had a real effect, not
merely that its own context happened to already match. **Result: PASS**
(`pytest services/cfs/tests/test_clean_fetch_patches.py -v`, real run, real network fetch,
`1 passed in 24.15s`).

**A wrong assumption caught and removed, not silently worked around:** an earlier version of this
test also asserted that re-applying the same patch a second time against the already-patched tree
must fail (`patch`'s usual non-idempotency) as an additional idempotency proof. That assertion
itself **failed** on a real run -- `patch -p1`'s context-based matching re-applied this
specific additive-only hunk a second time without error (it does not detect "this insertion is
already present" the way a state-based patcher would). Investigated rather than dismissed (this
task's own "investigate any oddity before reporting it as normal" rule): this is a real, general
property of `patch` against purely-additive hunks, unrelated to whether the FIRST application had
a genuine effect, so the assertion was testing the wrong thing and was replaced with the
independent unpatched-clone comparison above, which actually proves what "the patch had a real
effect" needs proven.

**Persistence note (this task's own `io_lockstep_app.c` termios fix, in full):** the fix lives at
`services/cfs/apps/io_lockstep/fsw/src/io_lockstep_app.c`, directly under `services/cfs/` --
this repository's own source tree, not `third_party/cfs` (which `fetch-cfs.sh` alone repopulates
from a pinned upstream commit and explicitly does not version-control, per that script's own
header comment). `build-cfs-cross.sh`'s own step `[3/8]` *copies*
`services/cfs/apps/io_lockstep` (among the other lockstep apps) INTO the freshly-fetched
`third_party/cfs` tree fresh on every single build, unconditionally -- so this fix is picked up
automatically, by construction, on every future build and every future clean fetch, with no patch
file, and no addition to `third_party/renode/M24_4/patches/`, ever needed for it. Only a genuine
`third_party/cfs`-internal edit (the pre-existing M24.4 `/cf`-startup-file patch) needs this
mechanism at all.

## `services/cfs/IMAGE_DIGEST.md`, rebuilt last

Done last, after every other edit in this task was final, per this task's own brief. **Found
already stale on arrival**, independent of anything this task did: `docker image inspect
altavista-cfs-lockstep:local --format '{{.Id}}'` returned
`sha256:8d538e787baa26ea1b0956cb50f06daae06b6689be96dfa355a18c6ed96b8601` at the very start of this
task, matching neither `IMAGE_DIGEST.md`'s own recorded M24.3 value nor anything else in that
file's history -- some undocumented local build had already run on this host. This task's own
addition on top: the `io_lockstep_app.c` termios fix changes that file's content (though not the
posix build's own compiled `#else`-branch behavior, exactly the shape M24.3's own note in that
file already described for this identical file).

Rebuilt for real: `docker build -f services/cfs/Dockerfile -t altavista-cfs-lockstep:local .`
(exit 0, full log read -- not just the exit code -- confirming Docker's own layer cache was reused
for every unaffected stage and only the `io_lockstep`-touching `COPY`/build layers actually
re-ran). New digest: `sha256:27ed4ff89dee381258a9ccb8410fa8aae19312defc79bf815ddc2e2c509d21ae`,
recorded in `IMAGE_DIGEST.md` with a new "Rebuilt for M24.4b" section documenting exactly what
changed and why, following that file's own established convention from its M26.1/M24.3 entries.

**Verified, not merely asserted:** `pytest services/cfs/tests/test_image_digest.py -v` --
**PASSED** (`1 passed in 25.45s`, real `docker build` + `docker image inspect` against the updated
recorded value) -- and the **entire** `services/cfs/tests/` suite (18 tests, including this
task's own new `test_clean_fetch_patches.py`) -- **PASSED** (`18 passed in 50.06s`), closing the
one known suite failure this task's brief named, with no other regression introduced.

## Final status and complete "not done" list

**Status: partial, with the hardest and highest-value piece done.** The root cause was found (not
merely narrowed further, as M24.4 left it) and FIXED with a real code change, proven live end to
end -- no retry, no timeout, no workaround. The bridge (item 3) is built and works end to end: a
real `av-lockstep-shim` binary, fronted by `renode_bridge.py`, in front of the real, root-cause-
fixed `core-cpu1.exe` under a real Renode process, driven over the real gRPC `LockstepService`
surface through a complete `Bind`/`Step`/`Step`/`Step`/`Reset`/`Step`/`Shutdown` cycle, with the
`Reset` genuinely power-cycling the emulated hardware (two full boot sequences, confirmed by UART0
content) entirely through the bridge's own logic, not a hand-run monitor script. The clean-fetch
patch test (item 7) and the `IMAGE_DIGEST.md` rebuild (item 8) are both done and verified by real
test runs. **What remains, in priority order for a follow-up task:**

1. **Build `crates/av-kernel/tests/drm_attitude_control_renode.rs`**, paralleling
   `drm_attitude_control_cfs.rs` structurally (its own `container_sos`/`container_drm`/
   `truth_pointing_error_rad` helpers can be copied near-verbatim) but managing lifecycle for
   `av-lockstep-shim` + `renode_bridge.py` as plain child processes (`std::process::Command`,
   `Drop`-based kill/wait guards -- `crates/av-lockstep-shim/tests/end_to_end_kernel_path.rs`'s
   own `ChildGuard`/`free_tcp_port`/`wait_until` helpers are the exact, already-proven pattern to
   reuse) instead of `av_lockstep::docker::ManagedContainer`. Point
   `drms/demo_attitude_control_controller_renode.system.yaml`'s `container.address` parameter
   (already authored, already validated: `cargo run -p av-kernel --example drm_hash -- system
   drms/demo_attitude_control_controller_renode.system.yaml` succeeds, hash
   `49fa073d6eaa46c96a0f2e80857d882ec84babd52413a98b674df9d69ff08ced` recorded in the file) at
   wherever the shim actually bound its ephemeral gRPC port (`free_tcp_port()`, then rewrite the
   loaded `SystemDefinition`'s parameter before hashing/executing, exactly mirroring how the CFS
   test rewrites `container.address` implicitly via the Docker-published port).
2. **What de-risks this, already measured by this task:** the bridge's `write_chars` was
   pipelined (batches many `WriteChar` monitor commands into one `sendall` + one drained reply)
   specifically because a naive one-round-trip-per-byte version made even the empty-input smoke
   test slow; a real DRM step carries actual star-tracker/IMU packets (tens of bytes), so this
   optimization matters for a real run, not just this task's own convenience. **What is NOT yet
   measured:** whether a real DRM's own step period (this task used a deliberately generous 3s
   `RunFor` per step to clear `IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS`'s 2000ms worst case for an
   EMPTY-input step) is long enough once real sensor packets are flowing every step (ADCS should
   publish promptly once fed real measurements, per `services/cfs/IMAGE_DIGEST.md`'s own M23.4
   account, likely needing much LESS virtual time per step than this task's worst-case smoke test
   needed -- but this was not measured against the real port table/packet codecs, only reasoned
   about). A follow-up should measure this directly (send one real star-tracker + one real IMU
   packet through a single `STEP`, at the DRM's actual 10 Hz `step_period_ns`, and confirm the
   guest replies within that window) before assuming the smoke test's 3s figure either
   generalizes or needs to.
3. **The byte-comparison itself** (posix `altavista-cfs-lockstep:local` container -- already
   built, digest verified above -- vs the Renode-bound instance from step 1/2, per-step
   `RunProducts.trajectories`/`.events` compared the same way
   `byte_identical_run_products_across_two_separately_spawned_cfs_containers` already does,
   excluding the address-dependent `dynamics_hash` field for the same documented reason) --
   blocked on 1-2.
4. The GIC `ISENABLER1=0` discrepancy (M24.2d's own finding, M24.4's own re-confirmation) remains
   unresolved on its own terms, believed irrelevant for the reasons both of those reports give,
   not chased further here either.
5. **Relink `core-cpu1.exe` against the `BSP_RESET_BOARD_AT_EXIT=0` BSP** (M24.4's own item 2
   built and verified this BSP fix at the RTEMS-library level; the actual cFS artifact used by
   this task's own bridge still links against the original, reset-spins-forever BSP -- not
   exercised by anything in this task, since nothing in this task's own testing calls
   `CFE_ES_ExitApp`/an orderly shutdown path that would hit `bsp_reset`; `machine Reset` bypasses
   this entirely, which is why the HARDWARE-fault path above was unaffected). Still mechanical,
   still not attempted, exactly as M24.4 left it.
6. `crates/av-kernel/tests/drm_container.rs`'s Docker lifecycle test and
   `crates/av-lockstep/tests/docker_lifecycle.rs`'s own tests were not re-run in this task (this
   task's own changes never touched `crates/av-lockstep`/`crates/av-kernel`'s Docker-lifecycle
   code at all) -- unaffected, not re-verified.

**No file under `third_party/rtems/`, `web/`, or `altavista/` was modified** (`altavista/pb/...`
was only ever imported from, read-only, by `renode_bridge.py`/`bridge_smoke_client.py`, to reuse
this repository's own generated Python protobuf stubs rather than hand-rolling a decoder -- no
file under `altavista/` was written to). Every Docker container this task created itself was
labeled (`--label av.test=1`, question 156) and none was left running (`docker ps -a` confirmed
clean at the end of this task); the one durable image this task built to avoid paying a repeated
network cost, `rtems-m24c:build-with-cmake`, is also labeled and is a cache artifact, not a
container, so question 156's "prune before start" guard does not apply to it the way it does to
containers -- it is intentionally kept for a future task's reuse, exactly as `rtems-m24c:build`
itself (M24.2c's own image) already is. Network was used exactly twice, both matching
already-established, documented exceptions: once to install `cmake` into a throwaway container
(committed once to `rtems-m24c:build-with-cmake` so this cost is paid exactly once, not on every
future rebuild) before doing the actual cross-build under `--network none`, and once inside
`services/cfs/tests/test_clean_fetch_patches.py`'s own real run (question 148/154's own
documented exception -- that test's entire purpose is validating the network-using fetch path).
`cargo` was used (disclosed: `cargo build -p av-lockstep-shim`, `cargo run -p av-kernel --example
drm_hash`) -- `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin`, `cargo` not on `PATH` by
default on this host, same as M24.4's own disclosure.
