#!/usr/bin/env python3
"""M24.1 spike measurement driver.

Drives a running Renode instance (started separately with `-P <port> --disable-gui
--hide-log`, so this script owns none of Renode's process lifecycle -- it only speaks
the Monitor's TCP protocol) through N steps of `emulation RunFor "<step_s>"`, using the
monitor's `RunFor` with a fixed quantum (the M24.1 brief's second slaving option -- chosen
over the external control API because this Renode build (1.16.1) ships no ExternalControl
plugin at all under Contents/MacOS -- see REPORT.md's "slaving method" section for the
`find`-based search that confirmed this). After each RunFor this sends `currentTime` in
the same command line (`emulation RunFor "0.1"; currentTime`) so the *wall-clock* number
comes from Renode's own real-time clock, not from this script's socket round-trip -- that
keeps python/telnet overhead out of the wall-time measurement entirely.

Output: one CSV row per step (step index, cumulative virtual seconds reported, cumulative
real/host seconds reported, per-step virtual delta, per-step real delta), plus a summary
of virtual-time exactness and the wall-time-per-step distribution, printed to stdout AND
appended to the CSV's own trailer as a comment for convenience.
"""
import argparse
import re
import socket
import statistics
import sys
import time

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")
VIRT_RE = re.compile(rb"Current virtual time:\s*([0-9:.]+)")
REAL_RE = re.compile(rb"Current real time:\s*([0-9:.]+)")


def parse_hms_ns(s: str) -> int:
    """Parse Renode's 'HH:MM:SS.fffffffff' time string into integer nanoseconds. Kept as
    an exact integer (not float seconds) end-to-end so the "virtual time reached must equal
    the request exactly" check is a real integer equality, not a float `==` that could be
    fooled by ULP-level rounding from repeated float division/subtraction."""
    hh, mm, rest = s.split(":")
    ss, frac = rest.split(".")
    return ((int(hh) * 3600 + int(mm) * 60 + int(ss)) * 1_000_000_000) + int(frac.ljust(9, "0")[:9])


class MonitorClient:
    def __init__(self, host, port, timeout=10.0):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.sock.settimeout(timeout)

    def read_until_idle(self, idle_gap=0.02, overall_timeout=30.0):
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

    def read_until_marker(self, marker: bytes, poll_timeout=0.5, grace=0.03, overall_timeout=30.0):
        """Read until `marker` bytes appear anywhere in the accumulated buffer, then do a
        couple of short (`grace`-second) non-blocking-ish polls to catch trailing prompt
        bytes, and return everything. Robust to RunFor taking an arbitrary, variable amount
        of wall time between the command echo and its result -- unlike a pure idle-gap read,
        this never mistakes "no bytes arrived yet because Renode is still executing" for
        "done". Once the marker is found, subsequent recv() calls use the short `grace`
        timeout (not the long `poll_timeout` used while waiting for the marker itself) so a
        quiet socket after the marker costs at most ~`grace` seconds, not `poll_timeout` --
        an earlier version of this method used `poll_timeout` for the post-marker drain too
        and that alone added ~0.5s of pure socket-timeout waiting to every single step's
        measured wall time (visible as the "driver-observed" number being ~0.5s higher than
        Renode's own "real" number for the same step; caught by comparing the two and is
        exactly why both are recorded rather than trusting the driver-side number alone)."""
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--setup-resc", required=True)
    ap.add_argument("--steps", type=int, default=1000)
    ap.add_argument("--step-seconds", type=float, default=0.1)
    ap.add_argument("--out-csv", required=True)
    ap.add_argument("--progress-every", type=int, default=100)
    ap.add_argument("--extra-cmd", default=None,
                     help="Extra monitor command sent right after include (e.g. "
                          "'emulation SetGlobalAdvanceImmediately true' for the "
                          "AdvanceImmediately confirmation run).")
    args = ap.parse_args()

    client = MonitorClient(args.host, args.port)
    client.read_until_idle(idle_gap=0.3, overall_timeout=5.0)  # drain banner

    client.send(f"include @{args.setup_resc}")
    boot_out = client.read_until_marker(b"zynqmp-rpu-spike) ", overall_timeout=60.0)
    boot_text = ANSI_RE.sub(b"", boot_out).decode(errors="replace")
    if args.extra_cmd:
        client.send(args.extra_cmd)
        extra_out = client.read_until_marker(b"zynqmp-rpu-spike) ", overall_timeout=10.0)
        print(f"extra-cmd output: {ANSI_RE.sub(b'', extra_out).decode(errors='replace')!r}")
    if "error" in boot_text.lower() or "failed" in boot_text.lower():
        print("WARNING: possible error during include -- inspect boot_text below", file=sys.stderr)
        print(boot_text, file=sys.stderr)

    expected_virtual_delta_ns = round(args.step_seconds * 1_000_000_000)

    rows = []
    prev_virt_ns = 0
    prev_real_ns = 0
    for i in range(1, args.steps + 1):
        cmd = f'emulation RunFor "{args.step_seconds}"; currentTime'
        t0 = time.perf_counter()
        client.send(cmd)
        out = client.read_until_marker(b"Current real time:", overall_timeout=30.0)
        t1 = time.perf_counter()
        text = ANSI_RE.sub(b"", out)
        vm = VIRT_RE.search(text)
        rm = REAL_RE.search(text)
        if not vm or not rm:
            print(f"step {i}: FAILED TO PARSE OUTPUT:\n{text.decode(errors='replace')}", file=sys.stderr)
            sys.exit(1)
        virt_ns = parse_hms_ns(vm.group(1).decode())
        real_ns = parse_hms_ns(rm.group(1).decode())
        rows.append({
            "step": i,
            "virtual_cumulative_ns": virt_ns,
            "real_cumulative_ns": real_ns,
            "virtual_delta_ns": virt_ns - prev_virt_ns,
            "real_delta_ns": real_ns - prev_real_ns,
            "driver_wall_delta_s": t1 - t0,
        })
        prev_virt_ns, prev_real_ns = virt_ns, real_ns
        if i % args.progress_every == 0 or i == args.steps:
            print(f"  step {i}/{args.steps}: virtual={virt_ns / 1e9:.9f}s real={real_ns / 1e9:.9f}s "
                  f"driver_wall_delta={t1 - t0:.6f}s", flush=True)

    client.send("quit")
    client.close()

    with open(args.out_csv, "w") as f:
        f.write("step,virtual_cumulative_ns,real_cumulative_ns,virtual_delta_ns,real_delta_ns,driver_wall_delta_s\n")
        for r in rows:
            f.write(f"{r['step']},{r['virtual_cumulative_ns']},{r['real_cumulative_ns']},"
                     f"{r['virtual_delta_ns']},{r['real_delta_ns']},{r['driver_wall_delta_s']:.9f}\n")

    # --- Summary ---
    virt_deltas_ns = [r["virtual_delta_ns"] for r in rows]
    real_deltas = [r["real_delta_ns"] / 1e9 for r in rows]
    driver_deltas = [r["driver_wall_delta_s"] for r in rows]

    max_virt_drift_ns = max(abs(v - expected_virtual_delta_ns) for v in virt_deltas_ns)
    exact_count = sum(1 for v in virt_deltas_ns if v == expected_virtual_delta_ns)

    def dist(name, xs):
        xs_sorted = sorted(xs)
        n = len(xs_sorted)
        median = xs_sorted[n // 2] if n % 2 else (xs_sorted[n // 2 - 1] + xs_sorted[n // 2]) / 2
        print(f"{name}: n={n} min={xs_sorted[0]:.6f} p50={median:.6f} "
              f"p90={xs_sorted[int(n * 0.9)]:.6f} p99={xs_sorted[int(n * 0.99)]:.6f} "
              f"max={xs_sorted[-1]:.6f} mean={statistics.mean(xs):.6f} "
              f"stdev={statistics.pstdev(xs):.6f}")

    print("\n=== SUMMARY ===")
    print(f"steps={args.steps} step_seconds={args.step_seconds}")
    print(f"virtual time per step: expected={expected_virtual_delta_ns}ns; "
          f"exact matches={exact_count}/{args.steps}; max drift={max_virt_drift_ns}ns")
    dist("real (Renode-reported) wall time per step (s)", real_deltas)
    dist("driver-observed wall time per step (s, includes socket overhead)", driver_deltas)
    final_virt_ns = rows[-1]["virtual_cumulative_ns"]
    expected_final_ns = args.steps * expected_virtual_delta_ns
    print(f"final cumulative virtual time = {final_virt_ns}ns "
          f"(expected {expected_final_ns}ns, diff={final_virt_ns - expected_final_ns}ns)")


if __name__ == "__main__":
    main()
