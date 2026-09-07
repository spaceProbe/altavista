#!/usr/bin/env python3
"""M24.4 item 2 -- the two-part test: (a) a clean exit idles in WFI (not the non-`wfi`
`bsp_reset()` busy-spin M24_2d_REPORT.md root-caused), and (b) that idle costs no more wall
time per virtual second than the running guest did. Uses the FRESH ticker.exe built with
BSP_RESET_BOARD_AT_EXIT=0 (third_party/renode/M24_4/rtems-bsp-noreset/myout/...) against
AltaVista's own platform file (item 1's TTC fix -- both fixes are in effect together, exactly
the combination M24.4's actual lockstep guest needs).

Method: run in two phases against a single continuous Renode instance (never restarted
between phases, so "before" and "after" are the same run, same process, same boot cost):
  1. RUNNING phase: 5 x RunFor("2") while ticker is still actively ticking (nominal virtual
     0-10s, well inside its 35-RTEMS-second active window at the now-correct ~1:1 TTC rate).
     Records driver-measured wall time per step.
  2. Advance in coarse, unpolled chunks (AdvanceImmediately on) to nominal virtual ~50s,
     past ticker's own completion (35 RTEMS-seconds, plus scheduling slack).
  3. IDLE phase: 5 x RunFor("2") after completion is confirmed by UART content (checked
     against the REAL captured banner text, "END OF TEST CLOCK TICK" -- question 164's own
     warning: an earlier tool in this investigation searched for "END OF CLOCK TICK TEST"
     and never matched a real, completed run). Records driver-measured wall time per step,
     and rpu0 PC + ExecutedInstructions at every step to directly confirm WFI-idle (PC parked
     at the idle loop address, 0x400045e6 -- the exact address M24_2d_REPORT.md's
     `diag_bspreset_confirm.py` found for the *non*-fixed build's brief idle dwell before
     jumping to the reset spin at 0x4000706c) rather than inferring it from wall time alone.

Pass conditions (both required):
  (a) UART contains the real completion banner AND every idle-phase PC sample equals the
      idle-loop address AND no idle-phase PC sample equals the old busy-spin address
      (0x4000706c, kept as an explicit negative check, not just "some other address").
  (b) mean idle-phase wall-seconds-per-virtual-second <= mean running-phase
      wall-seconds-per-virtual-second * (1 + tolerance).
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

IDLE_LOOP_PC = 0x400045E6
OLD_BUSY_SPIN_PC = 0x4000706C
REAL_COMPLETION_BANNER = b"END OF TEST CLOCK TICK"  # verbatim from a captured real run
# (ticker_uart.log.12, third_party/rtems-container/run-renode/), NOT "END OF CLOCK TICK TEST"
# -- the exact substitution question 164 warns about.


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

    def read_until_marker(self, marker: bytes, poll_timeout=0.5, grace=0.05, overall_timeout=120.0):
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


def run_step(client, seconds):
    """One emulation RunFor step, timed on the driver side (wall clock), returning
    (driver_wall_s, virtual_s_reached)."""
    t0 = time.monotonic()
    client.send(f'emulation RunFor "{seconds}"; currentTime')
    raw = client.read_until_marker(b"Current virtual time:", overall_timeout=120.0)
    t1 = time.monotonic()
    out = clean(raw)
    m = VIRT_RE.search(out.encode())
    if not m:
        raise RuntimeError(f"no 'Current virtual time' in reply: {out!r}")
    virt_s = parse_hms_ns(m.group(1).decode()) / 1e9
    return t1 - t0, virt_s


def read_hex(client, cmd):
    client.send(cmd)
    out = clean(client.read_until_idle(idle_gap=0.3, overall_timeout=15.0))
    matches = HEX_RE.findall(out)
    if matches:
        return int(matches[-1], 16)
    # PC prints as plain hex without 0x sometimes; fall back to any hex-looking token
    dec = re.findall(r"\b(\d+)\b", out)
    return int(dec[-1]) if dec else None


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
        "/Users/probe/code/AltaVista/third_party/renode/M24_4/wfi_idle_test.resc"))
    ap.add_argument("--port", type=int, default=15005)
    ap.add_argument("--tolerance", type=float, default=0.5,
                     help="idle wall/virtual allowed to exceed running wall/virtual by this fraction")
    ap.add_argument("--out", default=(
        "/Users/probe/code/AltaVista/third_party/renode/M24_4/wfi_idle_test_result.json"))
    args = ap.parse_args()

    uart_log = "/Users/probe/code/AltaVista/third_party/renode/M24_4/wfi_idle_uart.log"
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

            client.send("emulation SetGlobalAdvanceImmediately true")
            client.read_until_idle(idle_gap=0.3, overall_timeout=10.0)

            running_steps = []
            print("=== RUNNING phase (ticker actively ticking) ===", flush=True)
            for i in range(5):
                wall_s, virt_s = run_step(client, 2)
                running_steps.append({"step": i + 1, "wall_s": wall_s, "cumulative_virtual_s": virt_s})
                print(f"  running step {i+1}: wall={wall_s:.4f}s virtual={virt_s:.3f}s", flush=True)

            print("=== coarse advance to nominal virtual ~50s (unpolled) ===", flush=True)
            for i in range(4):
                wall_s, virt_s = run_step(client, 10)
                print(f"  coarse step {i+1}: wall={wall_s:.4f}s virtual={virt_s:.3f}s", flush=True)

            with open(uart_log, "rb") as f:
                uart_content = f.read()
            completion_seen = REAL_COMPLETION_BANNER in uart_content
            print(f"completion banner ({REAL_COMPLETION_BANNER!r}) seen: {completion_seen}", flush=True)
            print(f"UART tail: {uart_content[-200:]!r}", flush=True)

            idle_steps = []
            print("=== IDLE phase (past completion) ===", flush=True)
            for i in range(5):
                wall_s, virt_s = run_step(client, 2)
                pc = read_hex(client, "rpu0 PC")
                instr = read_hex(client, "rpu0 ExecutedInstructions")
                idle_steps.append({
                    "step": i + 1, "wall_s": wall_s, "cumulative_virtual_s": virt_s,
                    "pc": pc, "pc_hex": hex(pc) if pc is not None else None,
                    "executed_instructions": instr,
                })
                print(f"  idle step {i+1}: wall={wall_s:.4f}s virtual={virt_s:.3f}s "
                      f"PC={hex(pc) if pc is not None else None} instr={instr}", flush=True)

            with open(uart_log, "rb") as f:
                uart_content_final = f.read()
            completion_seen_final = REAL_COMPLETION_BANNER in uart_content_final

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

    # --- Evaluate pass conditions ---
    all_pc_idle = [s["pc"] for s in idle_steps]
    pc_ok = all(pc == IDLE_LOOP_PC for pc in all_pc_idle) and all(pc != OLD_BUSY_SPIN_PC for pc in all_pc_idle)

    running_rate = sum(s["wall_s"] for s in running_steps) / sum(2.0 for _ in running_steps)
    idle_rate = sum(s["wall_s"] for s in idle_steps) / sum(2.0 for _ in idle_steps)
    rate_ok = idle_rate <= running_rate * (1.0 + args.tolerance)

    passed = completion_seen_final and pc_ok and rate_ok

    result = {
        "completion_banner_seen": completion_seen_final,
        "running_steps": running_steps,
        "idle_steps": idle_steps,
        "running_wall_s_per_virtual_s": running_rate,
        "idle_wall_s_per_virtual_s": idle_rate,
        "tolerance": args.tolerance,
        "pc_check_passed": pc_ok,
        "rate_check_passed": rate_ok,
        "passed": passed,
    }
    with open(args.out, "w") as f:
        json.dump(result, f, indent=2)
    print(json.dumps(result, indent=2))

    if not passed:
        print(f"FAIL: completion={completion_seen_final} pc_ok={pc_ok} rate_ok={rate_ok} "
              f"(running={running_rate:.4f}s/s idle={idle_rate:.4f}s/s)", file=sys.stderr)
        return 1
    print(f"PASS: idle wall/virtual={idle_rate:.4f}s/s <= running wall/virtual={running_rate:.4f}s/s "
          f"* {1+args.tolerance}, PC parked at idle loop, no busy-spin address seen")
    return 0


if __name__ == "__main__":
    sys.exit(main())
