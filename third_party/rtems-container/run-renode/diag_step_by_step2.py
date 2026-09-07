#!/usr/bin/env python3
"""Diagnostic v2: replicate hello.resc's exact command sequence (including `using` aliases
and the macro), sent as separate top-level monitor commands (not as one `include`), each with
a short fixed wait, to find exactly which line does not return / does not print what is
expected. Not part of the reviewable pipeline."""
import socket
import subprocess
import sys
import time

RENODE = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
PORT = 3411
RUNDIR = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"

cmds = [
    'using sysbus',
    'using sysbus.cluster0',
    'using sysbus.cluster1',
    'mach create "zynqmp-rpu-hello-diag2"',
    'machine LoadPlatformDescription @platforms/cpus/zynqmp.repl',
    f'uart0 CreateFileBackend @{RUNDIR}/diag2_uart.log true',
    'macro reset\n"""\n    cluster0 ForEach IsHalted true\n    cluster1 ForEach IsHalted true\n    rpu0 IsHalted false\n\n    sysbus LoadELF ' + RUNDIR + '/hello.exe cpu=rpu0\n"""',
    'runMacro $reset',
    'emulation RunFor "2"',
    'quit',
]

proc = subprocess.Popen([RENODE, "--disable-gui", "--hide-log", "-P", str(PORT)],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    deadline = time.monotonic() + 30
    sock = None
    while time.monotonic() < deadline:
        try:
            sock = socket.create_connection(("127.0.0.1", PORT), timeout=1.0)
            break
        except OSError:
            time.sleep(0.2)
    if sock is None:
        print("FAIL: never connected")
        sys.exit(1)
    sock.settimeout(0.5)
    time.sleep(0.5)
    try:
        banner = sock.recv(65536)
        print(f"BANNER: {banner!r}")
    except OSError:
        print("BANNER: <none within 0.5s>")

    for cmd in cmds:
        print(f">>> {cmd!r}", flush=True)
        sock.sendall((cmd + "\r\n").encode())
        chunks = []
        sock.settimeout(5.0)
        t0 = time.monotonic()
        try:
            while True:
                data = sock.recv(65536)
                if not data:
                    print("<<< [connection closed]")
                    break
                chunks.append(data)
                # if we already see a fresh prompt, no need to wait out the full timeout
                if b")" in data and b"\x1b[0m" in data:
                    # heuristic idle-check: try one more short read, if nothing, move on
                    sock.settimeout(0.4)
        except socket.timeout:
            pass
        elapsed = time.monotonic() - t0
        blob = b"".join(chunks)
        print(f"<<< ({elapsed:.2f}s, {len(blob)} bytes): {blob!r}", flush=True)
finally:
    proc.poll()
    if proc.returncode is None:
        proc.kill()
        try:
            proc.wait(timeout=5)
        except Exception:
            pass
    print(f"renode final returncode: {proc.returncode}")
