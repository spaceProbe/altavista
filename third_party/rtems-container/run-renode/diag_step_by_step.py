#!/usr/bin/env python3
"""Diagnostic (not part of the reviewable pipeline): launch Renode with -P <port> and send
the hello.resc commands ONE AT A TIME over the monitor socket, printing the reply after each,
to find exactly which command hangs. Used to root-cause why `include @hello.resc` (and the
earlier CLI-positional-argument invocation) both ran forever with zero UART output."""
import socket
import subprocess
import sys
import time

RENODE = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
PORT = 3399
RUNDIR = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"

cmds = [
    'mach create "zynqmp-rpu-hello-diag"',
    f'machine LoadPlatformDescription @platforms/cpus/zynqmp.repl',
    f'uart0 CreateFileBackend @{RUNDIR}/diag_uart.log true',
    'sysbus.cluster0 ForEach IsHalted true',
    'sysbus.cluster1 ForEach IsHalted true',
    'rpu0 IsHalted false',
    f'sysbus LoadELF {RUNDIR}/hello.exe cpu=rpu0',
    'start',
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
        print(f">>> {cmd}", flush=True)
        sock.sendall((cmd + "\r\n").encode())
        t0 = time.monotonic()
        chunks = []
        sock.settimeout(8.0)
        try:
            while True:
                data = sock.recv(65536)
                if not data:
                    print("<<< [connection closed]")
                    break
                chunks.append(data)
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
