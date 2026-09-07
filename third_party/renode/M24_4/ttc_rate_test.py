#!/usr/bin/env python3
"""M24.4 item 1 -- the gate: measure TTC0's counting rate against Renode's own exact virtual
time clock on AltaVista's own platform file (frequency: 100000000 added to ttc0-ttc3), and
assert the ratio is within 1e-3 of 1.0. Per the task brief, this must pass before anything
else in M24.4 is trusted, and per M24_2d_REPORT.md's own methodology (finding 5's register-
read pattern, "last hex token is the value, not the echoed command's own address" bug fixed
in that task's v3 diagnostic) this reads the register directly off the running emulator, not
inferred from UART timing alone.

Drives Renode via its Monitor TCP protocol exactly as
third_party/rtems-container/run-renode/run_via_monitor.py and
third_party/renode/spike/measure_virtual_time.py do (proven pattern: launch
`renode --disable-gui --hide-log -P <port>` as a subprocess, connect a plain TCP socket, never
the CLI-positional-.resc mode, which is a known hang).

Bounded to a handful of virtual seconds (default 5), well short of the ~105-122s window
M24_2d_REPORT.md's continuation session found `ticker` completes and RTEMS's own
BSP_RESET_BOARD_AT_EXIT busy-spin begins (that mechanism is item 2's concern, not this one) --
this test only needs the clock driver programming TTC0 and ticking normally, which happens in
the first few virtual seconds.
"""
import argparse
import json
import re
import socket
import subprocess
import sys
import time

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")
VIRT_RE = re.compile(rb"Current virtual time:\s*([0-9:.]+)")
HEX_RE = re.compile(r"0x[0-9A-Fa-f]{8}")

TTC0_COUNT_VALUE_ADDR = 0xFF110018  # ttc0, CNT_CNTRL[0].COUNT_VALUE, offset 0x18
BSP_ASSUMED_HZ = 100_000_000.0


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

    def read_until_marker(self, marker: bytes, poll_timeout=0.5, grace=0.05, overall_timeout=90.0):
        """Read until `marker` appears, then drain briefly. Needed (not idle-read) for any
        command whose execution time is variable (RunFor can take longer wall time than the
        idle-read gap between the command echo and the actual result) -- exactly the
        raciness M24_2d_REPORT.md's own diagnostics evolved past their first, idle-read-only
        attempt."""
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
    return ANSI_RE.sub(b"", raw).decode(errors="replace").strip()


def read_reg_last_hex(client, addr):
    client.send(f"sysbus ReadDoubleWord {hex(addr)}")
    out = client.read_until_idle(idle_gap=0.3, overall_timeout=15.0)
    text = clean(out)
    matches = HEX_RE.findall(text)
    if len(matches) >= 2:
        return int(matches[-1], 16), text
    elif len(matches) == 1:
        return int(matches[0], 16), text
    raise RuntimeError(f"no hex value found reading {hex(addr)}: {text!r}")


def wait_for_port(host, port, deadline):
    while time.monotonic() < deadline:
        try:
            return socket.create_connection((host, port), timeout=1.0)
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f"renode monitor port {port} never accepted a connection")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--renode", default=(
        "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/"
        "Renode.app/Contents/MacOS/renode"))
    ap.add_argument("--resc", default=(
        "/Users/probe/code/AltaVista/third_party/renode/M24_4/ttc_rate_test.resc"))
    ap.add_argument("--port", type=int, default=15004)
    ap.add_argument("--steps", type=int, default=5)
    ap.add_argument("--step-seconds", type=float, default=1.0)
    ap.add_argument("--tolerance", type=float, default=1e-3)
    ap.add_argument("--out", default=(
        "/Users/probe/code/AltaVista/third_party/renode/M24_4/ttc_rate_test_result.json"))
    args = ap.parse_args()

    log_path = args.out.replace(".json", ".renode_log.txt")
    with open(log_path, "wb") as logf:
        proc = subprocess.Popen(
            [args.renode, "--disable-gui", "--hide-log", "-P", str(args.port)],
            stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
        )
        try:
            t0 = time.monotonic()
            sock = wait_for_port("127.0.0.1", args.port, t0 + 30.0)
            client = MonitorClient.__new__(MonitorClient)
            client.sock = sock
            sock.settimeout(2.0)
            try:
                sock.recv(65536)
            except OSError:
                pass

            client.send(f"include @{args.resc}")
            client.read_until_idle(idle_gap=0.5, overall_timeout=30.0)

            samples = []
            cumulative_virtual_s = 0.0
            for i in range(args.steps):
                client.send(f'emulation RunFor "{args.step_seconds}"; currentTime')
                raw = client.read_until_marker(b"Current virtual time:", overall_timeout=90.0)
                out = clean(raw)
                m = VIRT_RE.search(out.encode())
                if not m:
                    raise RuntimeError(f"no 'Current virtual time' in reply: {out!r}")
                virt_ns = parse_hms_ns(m.group(1).decode())
                cumulative_virtual_s = virt_ns / 1e9

                count_value, raw = read_reg_last_hex(client, TTC0_COUNT_VALUE_ADDR)
                implied_ttc_s = count_value / BSP_ASSUMED_HZ
                ratio = implied_ttc_s / cumulative_virtual_s if cumulative_virtual_s > 0 else float("nan")
                samples.append({
                    "step": i + 1,
                    "cumulative_virtual_s": cumulative_virtual_s,
                    "ttc0_count_value": count_value,
                    "implied_ttc_seconds_at_100mhz": implied_ttc_s,
                    "ratio": ratio,
                })
                print(f"step {i+1}: virtual={cumulative_virtual_s:.6f}s "
                      f"COUNT_VALUE={count_value} (0x{count_value:08X}) "
                      f"implied={implied_ttc_s:.6f}s ratio={ratio:.6f}", flush=True)

            client.send("quit")
            try:
                proc.wait(timeout=15.0)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=15.0)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=15.0)

    final_ratio = samples[-1]["ratio"] if samples else float("nan")
    worst_dev = max(abs(s["ratio"] - 1.0) for s in samples) if samples else float("inf")
    passed = worst_dev < args.tolerance

    result = {
        "samples": samples,
        "tolerance": args.tolerance,
        "worst_deviation_from_1": worst_dev,
        "final_ratio": final_ratio,
        "passed": passed,
    }
    with open(args.out, "w") as f:
        json.dump(result, f, indent=2)

    print(json.dumps(result, indent=2))
    if not passed:
        print(f"FAIL: worst |ratio-1| = {worst_dev:.6g} >= tolerance {args.tolerance:.6g}",
              file=sys.stderr)
        return 1
    print(f"PASS: worst |ratio-1| = {worst_dev:.6g} < tolerance {args.tolerance:.6g}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
