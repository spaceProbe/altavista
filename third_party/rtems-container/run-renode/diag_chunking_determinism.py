#!/usr/bin/env python3
"""M24.2d diagnostic v7: controlled A/B test of RunFor chunking determinism, motivated by an
observation across diag_ttc_trace3.py (30 x RunFor "1" reached virtual ~30s with ticks through
09:00:24) and diag_gic_check2.py (RunFor "3" then RunFor "42", reaching the same cumulative
virtual ~45s, but ticks stopped at 09:00:09) -- two runs asking for a similar total amount of
virtual time produced DIFFERENT numbers of delivered ticks depending on how the RunFor calls
were split. If real, this directly threatens M24.4's lockstep bridge, which will call RunFor
in whatever step granularity the kernel dictates -- if delivered-tick count depends on that
granularity, "same inputs, same outputs" (determinism) does not hold for this platform/BSP
combination independent of any other bug.

This test holds total nominal virtual time fixed at 20s and compares:
  (A) one single `emulation RunFor "20"` call
  (B) twenty individual `emulation RunFor "1"` calls
against byte-identical reset/load. Renode's own `currentTime` confirms both reach the same
cumulative virtual time exactly; the UART capture is compared byte-for-byte."""
import re
import socket
import subprocess
import sys
import time

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")
VIRT_RE = re.compile(rb"Current virtual time:\s*([0-9:.]+)")


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


def run_case(port, resc, label, chunks):
    """chunks: list of per-call durations (strings), summing to the total nominal time."""
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
            print(f"[{label}] FAIL: never connected")
            return None
        client.read_until_idle(idle_gap=0.3, overall_timeout=5.0)
        client.send(f"include @{resc}")
        client.read_until_marker(b"zynqmp-rpu-ticker) ", overall_timeout=60.0)

        last_virt = None
        for c in chunks:
            client.send(f'emulation RunFor "{c}"; currentTime')
            out = client.read_until_marker(b"Current real time:", overall_timeout=300.0)
            vm = VIRT_RE.search(ANSI_RE.sub(b"", out))
            last_virt = clean(vm.group(1)) if vm else None

        print(f"[{label}] final currentTime={last_virt}, {len(chunks)} RunFor call(s)", flush=True)

        rundir = "/".join(resc.split("/")[:-1])
        with open(f"{rundir}/ticker_uart.log", "rb") as f:
            content = f.read()
        print(f"[{label}] UART ({len(content)}B): {content!r}", flush=True)

        client.send("quit")
        client.close()
        return content
    finally:
        proc.poll()
        if proc.returncode is None:
            proc.kill()
            try:
                proc.wait(timeout=5)
            except Exception:
                pass
        print(f"[{label}] renode final returncode: {proc.returncode}", flush=True)


rundir = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"
resc = f"{rundir}/ticker_noquit.resc"

content_a = run_case(3470, resc, "A: single RunFor 20", ["20"])
content_b = run_case(3471, resc, "B: 20x RunFor 1", ["1"] * 20)

print("\n=== COMPARISON ===")
if content_a is not None and content_b is not None:
    print(f"identical byte-for-byte: {content_a == content_b}")
    print(f"len A={len(content_a)} len B={len(content_b)}")
