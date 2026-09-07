#!/usr/bin/env python3
"""M24.2d diagnostic v6: does `emulation SetGlobalAdvanceImmediately true` change the ticker
platform's wall-time-per-virtual-second the way M24.1's bare-loop spike found (mean 0.1s/step
default throttled -> mean 0.0644s/step with it on)? Also records whether AdvanceImmediately
changes how many ticks make it into the UART log for the same nominal virtual duration (10s),
since diag_ttc_trace3.py already showed the tick count is sensitive to how RunFor calls are
chunked -- if it's also sensitive to AdvanceImmediately, that is further evidence the failure
is a scheduling/pacing artifact, not a fixed hardware-model miscalculation."""
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

    def read_until_marker(self, marker, poll_timeout=0.5, grace=0.05, overall_timeout=600.0):
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


def run_once(port, resc, advance_immediately, run_seconds):
    renode = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
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
            return
        client.read_until_idle(idle_gap=0.3, overall_timeout=5.0)
        client.send(f"include @{resc}")
        client.read_until_marker(b"zynqmp-rpu-ticker) ", overall_timeout=60.0)

        if advance_immediately:
            client.send("emulation SetGlobalAdvanceImmediately true")
            out = client.read_until_idle(idle_gap=0.3, overall_timeout=10.0)
            print(f"  SetGlobalAdvanceImmediately true -> {clean(out)!r}", flush=True)

        t0 = time.monotonic()
        client.send(f'emulation RunFor "{run_seconds}"; currentTime')
        out = client.read_until_marker(b"Current real time:", overall_timeout=600.0)
        wall = time.monotonic() - t0
        print(f"  AdvanceImmediately={advance_immediately}: RunFor {run_seconds}s took "
              f"{wall:.3f}s driver-observed wall time; reply: {clean(out)!r}", flush=True)

        rundir = "/".join(resc.split("/")[:-1])
        with open(f"{rundir}/ticker_uart.log", "rb") as f:
            content = f.read()
        n_ta = content.count(b"rtems_clock_get_tod")
        print(f"  UART lines with a tick print: {n_ta}; content: {content!r}", flush=True)

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
        print(f"  renode final returncode: {proc.returncode}", flush=True)


rundir = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"
resc = f"{rundir}/ticker_noquit.resc"

print("=== default (AdvanceImmediately off) ===", flush=True)
run_once(3460, resc, False, 10)

print("\n=== AdvanceImmediately on ===", flush=True)
run_once(3461, resc, True, 10)
