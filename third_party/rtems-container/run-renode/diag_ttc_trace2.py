#!/usr/bin/env python3
"""M24.2d diagnostic v2 (not part of the pipeline; supersedes diag_ttc_trace.py, which had a
read-buffering race that misattributed register values to the wrong step -- values were
still correct, just printed under the wrong step label. This version reuses the *proven*
MonitorClient.read_until_marker method from third_party/renode/spike/measure_virtual_time.py
(measured exact-to-the-nanosecond over 1000 steps) for the RunFor+currentTime pair, which
removes the race for the one command that has genuinely variable wall-clock duration.
Register reads are near-instant (no emulation advance) and use a short idle-based read.

Ground truth per step: Renode's OWN reported "Current virtual time" (from `currentTime`),
compared directly against TTC0.COUNT_VALUE / 100e6 (the driver's assumed reference clock).
If these diverge, that is direct, first-party evidence of a TTC counting-rate bug in
Renode's model (or a "clock gated by CPU activity" behaviour), independent of anything else
this task has guessed at."""
import argparse
import re
import socket
import sys
import time

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")
VIRT_RE = re.compile(rb"Current virtual time:\s*([0-9:.]+)")
REAL_RE = re.compile(rb"Current real time:\s*([0-9:.]+)")

TTC0_BASE = 0xff110000
REGS = {
    "CLK_CNTRL": 0x00,
    "CNT_CNTRL": 0x0C,
    "COUNT_VALUE": 0x18,
    "INTERVAL_VAL": 0x24,
    "MATCH_0": 0x30,
    "ISR": 0x54,
    "IER": 0x60,
}


def parse_hms_ns(s: str) -> int:
    hh, mm, rest = s.split(":")
    ss, frac = rest.split(".")
    return ((int(hh) * 3600 + int(mm) * 60 + int(ss)) * 1_000_000_000) + int(frac.ljust(9, "0")[:9])


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

    def read_until_marker(self, marker: bytes, poll_timeout=0.5, grace=0.05, overall_timeout=600.0):
        chunks = []
        buf = b""
        deadline = time.monotonic() + overall_timeout
        found_at = None
        while time.monotonic() < deadline:
            self.sock.settimeout(grace if found_at is not None else poll_timeout)
            try:
                data = self.sock.recv(65536)
                if not data:
                    break
                chunks.append(data)
                buf += data
            except socket.timeout:
                if found_at is not None:
                    break
                continue
            if found_at is None and marker in buf:
                found_at = time.monotonic()
        else:
            raise TimeoutError(f"marker {marker!r} not seen within {overall_timeout}s; buffer so far: {buf!r}")
        if found_at is None:
            raise TimeoutError(f"marker {marker!r} not seen within {overall_timeout}s; buffer so far: {buf!r}")
        return buf

    def send(self, line: str):
        self.sock.sendall((line + "\r\n").encode())

    def close(self):
        self.sock.close()


def clean(raw: bytes) -> str:
    text = ANSI_RE.sub(b"", raw).decode(errors="replace")
    return text.strip()


def read_reg_value(client, addr):
    client.send(f"sysbus ReadDoubleWord {hex(addr)}")
    out = client.read_until_idle(idle_gap=0.3, overall_timeout=15.0)
    text = clean(out)
    m = re.search(r"0x[0-9A-Fa-f]{8}", text)
    return int(m.group(0), 16) if m else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=3447)
    ap.add_argument("--steps", type=int, default=20)
    ap.add_argument("--step-seconds", type=float, default=1.0)
    args = ap.parse_args()

    renode = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
    rundir = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"
    resc = f"{rundir}/ticker_noquit.resc"

    import subprocess
    proc = subprocess.Popen([renode, "--disable-gui", "--hide-log", "-P", str(args.port)],
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        deadline = time.monotonic() + 30
        client = None
        while time.monotonic() < deadline:
            try:
                client = MonitorClient("127.0.0.1", args.port)
                break
            except OSError:
                time.sleep(0.2)
        if client is None:
            print("FAIL: never connected")
            sys.exit(1)
        client.read_until_idle(idle_gap=0.3, overall_timeout=5.0)

        client.send(f"include @{resc}")
        boot = client.read_until_marker(b"zynqmp-rpu-ticker) ", overall_timeout=60.0)
        print(f">>> include done: {clean(boot)!r}", flush=True)

        for i in range(1, args.steps + 1):
            cmd = f'emulation RunFor "{args.step_seconds}"; currentTime'
            t0 = time.monotonic()
            client.send(cmd)
            out = client.read_until_marker(b"Current real time:", overall_timeout=600.0)
            wall = time.monotonic() - t0
            text = ANSI_RE.sub(b"", out)
            vm = VIRT_RE.search(text)
            rm = REAL_RE.search(text)
            virt_ns = parse_hms_ns(vm.group(1).decode()) if vm else None
            real_ns = parse_hms_ns(rm.group(1).decode()) if rm else None

            regvals = {name: read_reg_value(client, TTC0_BASE + off) for name, off in REGS.items()}

            client.send("rpu0 PC")
            pc_out = clean(client.read_until_idle(idle_gap=0.3, overall_timeout=10.0))
            client.send("rpu0 IsHalted")
            halted_out = clean(client.read_until_idle(idle_gap=0.3, overall_timeout=10.0))
            client.send("rpu0 ExecutedInstructions")
            instr_out = clean(client.read_until_idle(idle_gap=0.3, overall_timeout=10.0))

            count_val = regvals.get("COUNT_VALUE")
            match_val = regvals.get("MATCH_0")
            ttc_seconds = (count_val / 100_000_000.0) if count_val is not None else None
            print(
                f"step {i}: renode_virtual={virt_ns/1e9 if virt_ns else None}s "
                f"renode_real={real_ns/1e9 if real_ns else None}s driver_wall={wall:.3f}s | "
                f"TTC COUNT_VALUE=0x{count_val:08X} ({ttc_seconds:.6f}s @100MHz) "
                f"MATCH_0=0x{match_val:08X} ISR=0x{regvals['ISR']:08X} IER=0x{regvals['IER']:08X} "
                f"CNT_CNTRL=0x{regvals['CNT_CNTRL']:08X} CLK_CNTRL=0x{regvals['CLK_CNTRL']:08X} | "
                f"PC={pc_out!r} IsHalted={halted_out!r} ExecutedInstructions={instr_out!r}",
                flush=True,
            )

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
        print(f"renode final returncode: {proc.returncode}")


if __name__ == "__main__":
    main()
