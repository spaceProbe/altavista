#!/usr/bin/env python3
"""Diagnostic v5 (fixes diag_ticker_steps.py's own bug: it hand-rolled monitor commands with
wrong path syntax, missing `$ORIGIN`, so LoadELF/CreateFileBackend both failed to parse and
every 'step' ran on a machine with nothing loaded). This one `include`s the real, working
ticker_noquit.resc (identical to the production ticker.resc through `runMacro $reset`, with
the final `emulation RunFor "60"; quit` removed) so setup is byte-identical to what already
works, then drives RunFor in small increments from Python, reading the real ticker_uart.log
after each step, to find where the sample's output actually stops advancing and why."""
import socket
import subprocess
import sys
import time

RENODE = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
PORT = 3444
RUNDIR = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"
RESC = f"{RUNDIR}/ticker_noquit.resc"
UART_LOG = f"{RUNDIR}/ticker_uart.log"


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

    out = send_and_read(sock, f"include @{RESC}", timeout=30.0)
    print(f">>> include @{RESC} -> {out!r}", flush=True)

    for step in range(1, 9):
        t0 = time.monotonic()
        out = send_and_read(sock, 'emulation RunFor "5"', timeout=180.0)
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
