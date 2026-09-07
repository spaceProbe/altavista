# M24.4f -- Renode Python hook for the guest->host UART path, then closing M24

status: **in progress**

Task: `docs/open-questions.md` questions 145, 153, 156, 157, 164, plus the lead's own scope
decision recorded as question 170's amendment (2026-09-06): replace the guest->host UART path
with a Renode Python hook on `uart1`'s character-received event forwarding to a bridge-owned
socket, prove it byte-for-byte against a `CreateFileBackend` capture for ten steps, then finish
the per-step byte comparison -- or, if STEP 2 onward is not reliable, declare a no-go (question
171) with the limitation recorded in `docs/sil-plan.md` and `third_party/renode/REPORT.md`.

Builds directly on `third_party/renode/M24_4e_REPORT.md` (root-caused `CreateServerSocketTerminal`
silently dropping uart1's TX bytes around WFI; a file-backend-based workaround got one real STEP
further but was not reliable for STEP 2+) and `M24_4d_REPORT.md`/`M24_4b_REPORT.md` (fixed two
real bridge bugs and the UART RX handshake).

Written incrementally per question 157's own rule (a missing final chat message must lose
nothing). Sized for ~300 tool uses; long Renode/Docker runs are backgrounded, not polled tightly.

## Not done yet (running list, updated as items close)

- [ ] 1. Confirm the Python hook mechanism (`uart.CharReceived` event, `include @*.py`) actually
      works in this exact Renode build, with a small smoke test before building the full bridge
      integration.
- [ ] 2. Write the hook script and wire it into `renode_bridge.py`'s `guest_read_frame` path,
      replacing the M24.4e file-polling read (host->guest `WriteChar` path untouched).
- [ ] 3. Prove ten-step byte-for-byte equality between the hook-delivered stream and an
      independent `CreateFileBackend` capture attached beside it, in the same run.
- [ ] 4. Run the real per-step byte comparison
      (`byte_identical_port_traffic_between_posix_container_and_renode`, posix container vs
      Renode, through `av_kernel::drm::execute`) and record the result.
- [ ] 5. If STEP 2 onward is not reliable: declare the no-go, write question 171 with evidence,
      add the limitation statement to `docs/sil-plan.md` and `third_party/renode/REPORT.md`.
- [ ] 6. Cleanup: zero `/tmp/av-renode-m24*` dirs, zero labeled Docker containers/images, own
      temp dirs removed.
- [ ] 7. Full verification: `pytest -q`, `cargo test -p av-kernel`, `cargo test --workspace
      --exclude av-kernel`; record totals.

## Reading the mechanism, before writing any code

Renode's own bundled `scripts/monitor.py` (this Renode distribution's own startup macro file,
read directly, not assumed) defines `mc_uart_connect`, whose body is:

```python
uart = clr.Convert(device, Renode.Peripherals.UART.IUART)
...
uart.CharReceived += __printer      # __printer(b): sys.stdout.write(chr(b))
...
uart.WriteChar(ord(c))
```

This is decisive, first-party evidence (not inferred) of exactly the two calls this task needs:
`CharReceived` is a C# event on `IUART` that fires once per byte the **guest transmits** (this
exact macro uses it to mirror guest output to the terminal's stdout -- the guest->host direction),
and `WriteChar` is the already-proven host->guest call M24.4b's fix already uses and this task
must not touch. `scripts/pydev/nuvoton_npcx9_bootrom.py` (also bundled, read directly) confirms
IronPython code running inside Renode's own process can `clr.AddReference(...)` additional .NET
assemblies and `from System.X import Y` types directly, and `scripts/single-node/segger-rtt.py`
plus its own `include @scripts/single-node/segger-rtt.py` from `ek-ra2e1.resc` confirms `.resc`
scripts `include` a `.py` file directly (not just `.resc` files), which defines `mc_`-prefixed
top-level functions that become new monitor commands (invoked later as e.g. `setup_segger_rtt
sysbus.segger_rtt`, the peripheral token resolved to the real object and passed as the macro's
first positional argument) -- exactly the "a `.py` included into the emulation" mechanism the task
brief names.

## Stage 1: smoke-test the mechanism in isolation, before touching production code

`third_party/renode/M24_4f/uart_tx_bridge_hook.py` implements `mc_setup_uart_tx_bridge(uart, port,
host="127.0.0.1")`: connects OUT, as a plain .NET `TcpClient` (`clr.AddReference`/`from System...`,
the same CLR-interop pattern `scripts/pydev/nuvoton_npcx9_bootrom.py` already uses in this exact
Renode distribution, read directly, not guessed), to a socket the CALLER already has bound and
listening, then registers `iuart.CharReceived += _forward` where `_forward(b)` does one
`stream.Write`+`Flush` per byte. Module-level `_active_bridges` keeps every
(client, stream, handler, uart) tuple alive so IronPython's own GC cannot silently un-wire it.

`third_party/renode/M24_4f/smoke_test_hook_uart0.py` proves the mechanism BEFORE wiring it to
uart1/the lockstep protocol at all: attaches the hook to **uart0** (console) alongside an
independent `CreateFileBackend` on the same peripheral, boots the real `core-cpu1.exe`, and
compares the two captures byte-for-byte. **Real result, this run:**

```
smoke: hook captured 5178 bytes, file backend captured 5178 bytes
smoke: PASS -- first 5178 bytes are byte-for-byte identical between the hook socket and the file backend
uart_tx_bridge[0]: forwarded=5178 errors=0
```

5178/5178 bytes, zero drops, through a real RTEMS/cFE boot banner -- direct evidence the
`CharReceived` hook mechanism itself is sound in this exact Renode build before any lockstep
protocol complexity is added. (The `uart_tx_bridge_stats` monitor macro afterward printed a
cosmetic "Command ... failed, returning 0" from the monitor's own return-value handling --
harmless, unrelated to the data path, not chased further.)

## Stage 2: wired into `renode_bridge.py`, replacing the M24.4e file-polling read path

`third_party/renode/M24_4b/renode_bridge.py` changes (host->guest `WriteChar` path untouched,
per the task's own hard rule):

1. **`RenodeBridge.start()`**: binds+listens `self.hook_srv` on `self.uart_port` (the same CLI
   `--uart-port` argument the old `CreateServerSocketTerminal` used to bind -- reused, not a new
   port) BEFORE the generated `.resc`'s `include` even runs. The `.resc` now does `include
   @.../M24_4f/uart_tx_bridge_hook.py` then `setup_uart_tx_bridge sysbus.uart1 <port>`, and
   **`CreateServerSocketTerminal` is removed entirely** -- confirmed unnecessary for both
   `WriteChar` and the RXEN register read (`sysbus ReadDoubleWord`), neither of which takes a
   terminal/backend argument; both operate directly on the peripheral/bus object. When the caller
   supplies `uart1_raw_log` (new, optional), an independent `CreateFileBackend` is ALSO attached to
   uart1 beside the hook, for the ten-step proof only -- not a read source.
2. **`guest_read_frame()`**: now just `read_frame(self.hook_conn, timeout=timeout)` -- the
   original, simple length-prefixed socket reader every other caller in this codebase already
   used, restored, because the byte source underneath it is now reliable. No polling, no
   re-attach-to-force-a-flush, no growing-file offset bookkeeping (M24.4e's own workarounds,
   removed). Every raw frame is also accumulated into `self._hook_captured_bytes`.
3. **`verify_against_file_backend()`** (new): compares `self._hook_captured_bytes` against a
   fresh read of `self.uart1_raw_log`, byte-for-byte, reporting the first mismatch offset if any.
4. **`main()`**: if `--uart1-raw-log` was passed, calls `verify_against_file_backend()` right
   after `bridge.run()` returns (i.e., after a clean SHUTDOWN relay -- every frame the guest
   transmitted the whole run has been accumulated by then) and prints a clearly-labeled
   `M24.4F_VERIFY PASS`/`FAIL` line to the bridge's own stdout log, exiting non-zero on a mismatch.

`crates/av-kernel/tests/drm_attitude_control_renode.rs::spawn_renode_bridge` now passes
`--uart1-raw-log <scratch_dir>/uart1_raw_crosscheck.log`, so the ten-step proof (item 3 of the
task) runs through the REAL production stack -- real `av-lockstep-shim`, real gRPC `Bind`/`Step`
calls from `av_kernel::drm::execute` itself, real HELLO/BIND/STEP frame content -- rather than a
hand-rolled synthetic driver guessing at what `IO_LOCKSTEP` expects.

status: in progress -- both stages built and syntax-checked; the real end-to-end test
(`byte_identical_port_traffic_between_posix_container_and_renode`) is running now, see below for
its result.
