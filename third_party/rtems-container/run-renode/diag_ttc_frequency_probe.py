#!/usr/bin/env python3
"""M24.2d diagnostic v9: cheap, no-RunFor probe of ttc0's own object properties via the
monitor, to see whether Renode exposes a queryable "Frequency" (or similar) property directly,
which would let the ~1/3-rate finding (established indirectly via COUNT_VALUE growth vs
Renode's own currentTime across diag_ttc_trace3.py and diag_gic_check2.py, consistently
0.332-0.333 across 4 independent samples spanning t=3s to t=90s) be confirmed directly against
the object's own configured value rather than inferred from counting rate alone."""
import re
import socket
import subprocess
import time

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")


def clean(raw):
    return ANSI_RE.sub(b"", raw).decode(errors="replace").strip()


renode = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
rundir = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"
resc = f"{rundir}/ticker_noquit.resc"
port = 3490

proc = subprocess.Popen([renode, "--disable-gui", "--hide-log", "-P", str(port)],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    sock = None
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            sock = socket.create_connection(("127.0.0.1", port), timeout=1.0)
            break
        except OSError:
            time.sleep(0.2)
    sock.settimeout(0.5)
    time.sleep(0.5)
    try:
        sock.recv(65536)
    except OSError:
        pass

    def send_and_read(cmd, idle=0.4, timeout=15.0):
        sock.sendall((cmd + "\r\n").encode())
        sock.settimeout(idle)
        chunks = []
        deadline2 = time.monotonic() + timeout
        last = time.monotonic()
        while time.monotonic() < deadline2:
            try:
                data = sock.recv(65536)
                if not data:
                    break
                chunks.append(data)
                last = time.monotonic()
            except socket.timeout:
                if time.monotonic() - last > idle:
                    break
        return clean(b"".join(chunks))

    out = send_and_read(f"include @{resc}", timeout=30.0)
    print(f"include: {out!r}", flush=True)

    for cmd in [
        "ttc0",
        "ttc0 Frequency",
        "ttc0 InputClockFrequency",
        "ttc0 InternalClockFrequency",
        "help ttc0",
    ]:
        out = send_and_read(cmd, timeout=10.0)
        print(f"\n[{cmd}] -> {out!r}", flush=True)

    send_and_read("quit", timeout=5.0)
finally:
    proc.poll()
    if proc.returncode is None:
        proc.kill()
        try:
            proc.wait(timeout=5)
        except Exception:
            pass
    print(f"\nrenode final returncode: {proc.returncode}")
