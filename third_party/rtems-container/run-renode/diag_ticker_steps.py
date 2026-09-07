#!/usr/bin/env python3
"""Diagnostic v4: step ticker's emulation in small RunFor increments (not one big RunFor
"60"), reading the UART capture file and a few CPU/machine state monitor queries after each
step, to find out WHY ticker_uart.log's output stops advancing around virtual t=14s in two
independent full runs even though the whole script reaches `quit` normally (exit code 0) well
under the nominal 60 virtual seconds' worth of real time. Not part of the reviewable
pipeline -- a one-off root-cause tool."""
import socket
import subprocess
import sys
import time

RENODE = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
PORT = 3433
RUNDIR = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"
UART_LOG = f"{RUNDIR}/ticker_step_uart.log"

setup_cmds = [
    'using sysbus',
    'using sysbus.cluster0',
    'using sysbus.cluster1',
    'mach create "zynqmp-rpu-ticker-step"',
    'machine LoadPlatformDescription @platforms/cpus/zynqmp.repl',
    f'uart0 CreateFileBackend {UART_LOG} true',
    'macro reset\n"""\n    cluster0 ForEach IsHalted true\n    cluster1 ForEach IsHalted true\n    rpu0 IsHalted false\n\n    sysbus LoadELF ' + RUNDIR + '/ticker.exe cpu=rpu0\n"""',
    'runMacro $reset',
]


def send_and_read(sock, cmd, timeout=90.0, idle=0.3):
    sock.sendall((cmd + "\r\n").encode())
    sock.settimeout(idle)
    chunks = []
    deadline = time.monotonic() + timeout
    last_data = time.monotonic()
    while time.monotonic() < deadline:
        try:
            data = sock.recv(65536)
            if not data:
                break
            chunks.append(data)
            last_data = time.monotonic()
        except socket.timeout:
            if time.monotonic() - last_data > idle:
                break
    return b"".join(chunks)


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
        sock.recv(65536)
    except OSError:
        pass

    for cmd in setup_cmds:
        out = send_and_read(sock, cmd, timeout=15.0)
        print(f">>> {cmd.splitlines()[0]!r} -> {out[-200:]!r}", flush=True)

    for step in range(1, 9):
        t0 = time.monotonic()
        out = send_and_read(sock, 'emulation RunFor "5"; cpu PC', timeout=120.0)
        elapsed = time.monotonic() - t0
        print(f"--- step {step} (cumulative virtual ~{step*5}s), wall {elapsed:.2f}s ---", flush=True)
        print(f"    monitor reply: {out!r}", flush=True)
        try:
            with open(UART_LOG, "rb") as f:
                content = f.read()
            print(f"    UART file so far ({len(content)} bytes): {content.decode(errors='replace')!r}", flush=True)
        except FileNotFoundError:
            print("    UART file does not exist yet", flush=True)

    send_and_read(sock, "quit", timeout=10.0)
finally:
    proc.poll()
    if proc.returncode is None:
        proc.kill()
        try:
            proc.wait(timeout=5)
        except Exception:
            pass
    print(f"renode final returncode: {proc.returncode}")
