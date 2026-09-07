#!/usr/bin/env python3
"""M24.2d diagnostic v9 -- confirm the bsp_reset() spin-loop hypothesis directly.

Source-level finding (read from the container-fetched RTEMS 6.1 source, not guessed):
  - testsuites/samples/ticker calls rtems_test_exit(0) on completion ->
    cpukit/libtest/testexit.c: rtems_shutdown_executive(0) ->
    cpukit/sapi/src/exshutdown.c: _Terminate(RTEMS_FATAL_SOURCE_EXIT, 0) ->
    cpukit/score/src/interr.c: _Terminate() calls _User_extensions_Fatal(), which invokes the
    BSP's fatal extension (bsps/shared/start/bspfatal-default.c: bsp_fatal_extension()), which
    -- ONLY if BSP_RESET_BOARD_AT_EXIT or BSP_PRESS_KEY_FOR_RESET is defined -- calls
    bsp_reset(source, code) and never returns; otherwise _Terminate() falls through to
    _CPU_Thread_Idle_body(0) (the same WFI idle loop as normal operation).
  - spec/build/bsps/optreset.yml: BSP_RESET_BOARD_AT_EXIT defaults to VALUE 1 (enabled-by:
    true) -- i.e. RTEMS's own build system turns this on by default for a BSP that does not
    override it. bsps/arm/xilinx-zynqmp-rpu carries no override (grepped, none found), so this
    BSP builds with it ON.
  - bsps/arm/xilinx-zynqmp-rpu/start/bspreset.c: bsp_reset() is
    `while (true) { *reset_ctrl |= 0x10; }` at CRL_APB_RESET_CTRL, 0xff5e0218 -- a real-hardware
    soft-reset request that only terminates because the real SoC actually resets.
  - platforms/cpus/zynqmp.repl (this Renode distribution's own shipped platform file) declares
    `Tag <0x00ff5e0000 0x28c> "CRL_APB"` -- i.e. CRL_APB (which covers 0xff5e0218) is NOT a
    functional peripheral model in this Renode build, just a stub/tag. A write there does not
    reset anything, so bsp_reset()'s loop can never terminate on this platform.
  - Disassembly (arm-rtems6-objdump -d, via a fresh debian:bookworm-slim container with the
    host's already-built output/toolchain bind-mounted read-only, since the toolchain is a
    Linux aarch64 ELF): bsp_reset is at 0x40007060; the loop body is exactly
    0x4000706c (ldr.w r3,[r2,#0x218]) / 0x40007070 (orr.w r3,r3,#16) /
    0x40007074 (str.w r3,[r2,#0x218]) / 0x40007078 (b.n 0x4000706c) -- 4 instructions, NO wfi,
    branching forever. Compare: the normal idle loop is wfi+b.n at 0x400045e4/0x400045e6.

This diagnostic runs `ticker` far enough (past the ~1/3-TTC-rate-corrected ~102s nominal
virtual-second completion point established by M24_2d_REPORT.md finding 5) to reach its own
"*** END OF TEST CLOCK TICK ***" line (note: the CORRECT substring, in this word order --
diag_full_completion_check.py checked for "END OF CLOCK TICK TEST", the wrong word order,
which is a bug: it never detected completion and so never stopped early, which is the
likely reason it kept issuing RunFor calls into the post-completion bsp_reset() spin window and
saw the "catastrophic slowdown"), then keeps stepping in small increments while sampling
`rpu0 PC` and `rpu0 ExecutedInstructions` and per-step wall time, to see directly:
  1. does PC move from the idle-loop address (0x400045e4/6) into the bsp_reset loop address
     range (0x4000706c-0x40007078) at/after completion?
  2. does wall-time-per-virtual-second change at that same boundary?
"""
import re
import socket
import subprocess
import sys
import time

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")


class MonitorClient:
    def __init__(self, host, port, timeout=10.0):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.sock.settimeout(timeout)

    def read_until_idle(self, idle_gap=0.3, overall_timeout=30.0):
        chunks = []
        deadline = time.monotonic() + overall_timeout
        self.sock.settimeout(idle_gap)
        while time.monotonic() < deadline:
            try:
                data = self.sock.recv(65536)
                if not data:
                    break
                chunks.append(data)
            except socket.timeout:
                if chunks:
                    break
                continue
        return b"".join(chunks)

    def read_until_marker(self, marker, poll_timeout=0.5, grace=0.05, overall_timeout=900.0):
        buf = b""
        deadline = time.monotonic() + overall_timeout
        found_at = None
        while time.monotonic() < deadline:
            self.sock.settimeout(grace if found_at is not None else poll_timeout)
            try:
                data = self.sock.recv(65536)
                if not data:
                    break
                buf += data
            except socket.timeout:
                if found_at is not None:
                    break
                continue
            if found_at is None and marker in buf:
                found_at = time.monotonic()
        if found_at is None:
            raise TimeoutError(f"marker {marker!r} not seen; buffer so far: {buf!r}")
        return buf

    def cmd(self, line, marker=b"zynqmp-rpu-ticker) ", overall_timeout=900.0):
        self.sock.sendall((line + "\r\n").encode())
        return self.read_until_marker(marker, overall_timeout=overall_timeout)

    def close(self):
        self.sock.close()


def clean(raw):
    return ANSI_RE.sub(b"", raw).decode(errors="replace").strip()


def last_hex(raw):
    """Extract the last 0x... token in a cleaned monitor reply (the returned value, not an
    echoed command/address -- see M24_2d_REPORT.md finding re: diag_ttc_trace2.py's bug)."""
    text = clean(raw)
    toks = re.findall(r"0x[0-9A-Fa-f]+", text)
    return toks[-1] if toks else None


renode = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
rundir = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"
resc = f"{rundir}/ticker_noquit.resc"
port = 3481

proc = subprocess.Popen([renode, "--disable-gui", "--hide-log", "-P", str(port)],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    deadline = time.monotonic() + 30
    client = None
    while time.monotonic() < deadline:
        try:
            client = MonitorClient("127.0.0.1", port)
            break
        except OSError:
            time.sleep(0.2)
    if client is None:
        print("FAIL: never connected")
        sys.exit(1)
    client.read_until_idle(idle_gap=0.3, overall_timeout=5.0)

    client.sock.sendall(f"include @{resc}\r\n".encode())
    client.read_until_marker(b"zynqmp-rpu-ticker) ", overall_timeout=60.0)
    print(">>> include done", flush=True)

    client.cmd("emulation SetGlobalAdvanceImmediately true")
    print(">>> AdvanceImmediately on", flush=True)

    cumulative = 0.0

    def uart_content():
        with open(f"{rundir}/ticker_uart.log", "rb") as f:
            return f.read()

    def step(nominal_s, label):
        global cumulative
        t0 = time.monotonic()
        out = client.cmd(f'emulation RunFor "{nominal_s}"; currentTime', overall_timeout=1800.0)
        wall = time.monotonic() - t0
        cumulative += nominal_s
        pc_raw = client.cmd("rpu0 PC", overall_timeout=30.0)
        pc = last_hex(pc_raw)
        ei_raw = client.cmd("rpu0 ExecutedInstructions", overall_timeout=30.0)
        ei = clean(ei_raw)
        content = uart_content()
        done = b"END OF TEST CLOCK TICK" in content  # CORRECT word order, confirmed from
        # actual UART captures (ticker_uart.log.12, hello_uart.log convention) -- NOT the
        # brief's pre-run guess "END OF CLOCK TICK TEST", which diag_full_completion_check.py
        # used and which never matches real output.
        print(f"[{label}] cumulative nominal {cumulative:.0f}s: wall {wall:.2f}s, "
              f"PC={pc}, ExecutedInstructions_reply={ei!r}, UART={len(content)}B, "
              f"COMPLETE={done}", flush=True)
        return done, wall, pc

    # Phase 1: coarse chunks with NO polling between them, up to nominal 90s (well before the
    # ~102s completion point implied by the 1/3 TTC rate finding), to get there efficiently.
    for i in range(1, 10):
        t0 = time.monotonic()
        out = client.cmd('emulation RunFor "10"; currentTime', overall_timeout=600.0)
        wall = time.monotonic() - t0
        cumulative += 10
        print(f"[coarse {i}/9] cumulative nominal {cumulative:.0f}s: wall {wall:.2f}s", flush=True)

    # Phase 2: fine 2s steps with PC/instruction/UART polling each step, bracketing the
    # expected completion boundary and continuing past it to observe the bsp_reset loop.
    done = False
    for j in range(1, 26):  # up to +50s more (cumulative up to 140s), matching the earlier
        # investigation's range where the "catastrophic slowdown" was found.
        d, wall, pc = step(2, f"fine {j}/25")
        if d and not done:
            print(f">>> COMPLETION DETECTED at cumulative nominal {cumulative:.0f}s "
                  f"(PC={pc}) -- continuing a few more steps to observe post-completion PC/rate",
                  flush=True)
            done = True
        if done and j > 0:
            # keep going a bit past first detection, but bail out early once we've clearly
            # observed the post-completion regime (a handful of steps) to save wall time.
            pass

    print("\n>>> Final summary printed above per-step. Sending quit.", flush=True)
    client.sock.sendall(b"quit\r\n")
    time.sleep(1)
    client.close()
finally:
    proc.poll()
    if proc.returncode is None:
        proc.kill()
        try:
            proc.wait(timeout=5)
        except Exception:
            pass
    print(f"renode final returncode: {proc.returncode}", flush=True)
