#!/usr/bin/env python3
"""Diagnostic v3: send `include @hello.resc` (the real file, unmodified command sequence)
over the monitor socket and print EVERY byte that comes back over a generous window, to see
the actual error/output rather than guessing. Not part of the reviewable pipeline."""
import socket
import subprocess
import sys
import time

RENODE = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
PORT = 3422
RESC = sys.argv[1] if len(sys.argv) > 1 else "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode/hello.resc"

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
        print("BANNER:", sock.recv(65536))
    except OSError:
        pass

    cmd = f"include @{RESC}\r\n"
    print(f">>> {cmd!r}", flush=True)
    sock.sendall(cmd.encode())

    end = time.monotonic() + float(sys.argv[2]) if len(sys.argv) > 2 else time.monotonic() + 20.0
    sock.settimeout(1.0)
    total = b""
    while time.monotonic() < end:
        try:
            data = sock.recv(65536)
            if not data:
                print("[connection closed]")
                break
            total += data
            print(f"CHUNK: {data!r}", flush=True)
        except socket.timeout:
            continue
    print(f"TOTAL BYTES: {len(total)}")
finally:
    proc.poll()
    if proc.returncode is None:
        proc.kill()
        try:
            proc.wait(timeout=5)
        except Exception:
            pass
    print(f"renode final returncode: {proc.returncode}")
