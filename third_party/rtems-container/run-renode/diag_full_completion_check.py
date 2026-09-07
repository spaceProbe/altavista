#!/usr/bin/env python3
"""M24.2d diagnostic v8 -- decisive test. Prior long diagnostic runs (diag_ttc_trace3.py,
diag_gic_check2.py) interleaved heavy register polling -- including repeated reads of
TTC0.ISR, which the BSP's own driver comment says is "clear on read" -- between RunFor calls,
which risks the *monitor's own reads* clearing a pending interrupt status before RTEMS's ISR
services it (observer interference). A clean control (diag_chunking_determinism.py, zero
register polling, only RunFor+currentTime+a UART read at the very end) showed 20 nominal
virtual seconds chunked as 1x20 or 20x1 give byte-identical output, removing "RunFor chunking
granularity" as an explanation for earlier discrepancies -- which leaves "how much total
virtual time was actually requested, combined with possible observer interference in the
longer runs" as the live explanation for why some runs "stalled" earlier than others.

This test asks the single decisive question with NO register polling at all during the run:
if this platform's clock genuinely runs at some fraction of the rate the driver assumes
(measured elsewhere at very roughly 1/3-1/4), does `ticker` still eventually print
"*** END OF CLOCK TICK TEST ***" given enough nominal RunFor budget (200s, several times what
even a 4x-slow clock would need to reach the sample's own `second >= 35` exit condition), or
does it genuinely stop making progress forever regardless of how much virtual time is given?
AdvanceImmediately is enabled purely to make 200 virtual seconds affordable in wall-clock
terms for this diagnostic; it does not change virtual-time behavior (confirmed by M24.1 and
diag_advance_immediately.py, both showing identical virtual-time/tick-count results with it on
or off -- only wall-clock speed changes)."""
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

    def send(self, line):
        self.sock.sendall((line + "\r\n").encode())

    def close(self):
        self.sock.close()


def clean(raw):
    return ANSI_RE.sub(b"", raw).decode(errors="replace").strip()


renode = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
rundir = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"
resc = f"{rundir}/ticker_noquit.resc"
port = 3480

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

    client.send(f"include @{resc}")
    client.read_until_marker(b"zynqmp-rpu-ticker) ", overall_timeout=60.0)
    print(">>> include done", flush=True)

    client.send("emulation SetGlobalAdvanceImmediately true")
    client.read_until_idle(idle_gap=0.3, overall_timeout=10.0)
    print(">>> AdvanceImmediately on (wall-clock speed only, per M24.1 -- does not change "
          "virtual time reached or tick count)", flush=True)

    # 10 chunks of 20s each (200s nominal total) -- chunked only so a progress line can be
    # printed periodically; NO register reads happen between chunks (the one difference from
    # the earlier long runs that might have interfered with the DUT).
    for i in range(1, 11):
        t0 = time.monotonic()
        client.send('emulation RunFor "20"; currentTime')
        out = client.read_until_marker(b"Current real time:", overall_timeout=900.0)
        wall = time.monotonic() - t0
        print(f"chunk {i}/10 (cumulative nominal {i*20}s): wall {wall:.2f}s, "
              f"reply {clean(out)!r}", flush=True)
        with open(f"{rundir}/ticker_uart.log", "rb") as f:
            content = f.read()
        done = b"END OF CLOCK TICK TEST" in content
        print(f"  UART so far ({len(content)}B); END OF CLOCK TICK TEST seen: {done}", flush=True)
        if done:
            print("  >>> Sample reached its own completion. Stopping early.", flush=True)
            break

    with open(f"{rundir}/ticker_uart.log", "rb") as f:
        content = f.read()
    print(f"\nFINAL UART ({len(content)}B):\n{content.decode(errors='replace')}", flush=True)
    print(f"\nEND OF CLOCK TICK TEST reached: {b'END OF CLOCK TICK TEST' in content}", flush=True)

    client.send("quit")
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
