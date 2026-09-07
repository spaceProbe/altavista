#!/usr/bin/env python3
"""Run one RTEMS sample's .resc script on the host's Renode via the Monitor's `-P <port>` TCP
protocol (NOT by passing the script as a CLI positional argument -- that mode was tried first
for hello.resc and hung indefinitely: 5m46s wall time, 5.48s CPU time, zero bytes on stdout,
no UART file ever created; recorded in REPORT.md as a finding, not worked around silently).

This follows the exact pattern already proven to work by the M24.1 spike
(third_party/renode/spike/measure_virtual_time.py): launch `renode --disable-gui --hide-log
-P <port>` as its own process (stdin explicitly /dev/null, so nothing can block on a stdin
read), connect a plain TCP socket to the monitor port, send `include @<script.resc>` (the
monitor command that actually runs a .resc file -- confirmed from measure_virtual_time.py's
own working usage), and wait for the process to exit on its own (the .resc ends with `quit`).
Renode's own reported exit code and wall time are recorded; the UART capture file the script
itself wrote (via `CreateFileBackend`) is read back and printed by the caller, not by this
script -- this script's only job is driving Renode to completion or reporting exactly how it
failed to.
"""
import argparse
import socket
import subprocess
import sys
import time


def wait_for_port(host, port, deadline):
    while time.monotonic() < deadline:
        try:
            s = socket.create_connection((host, port), timeout=1.0)
            return s
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f"renode monitor port {port} never accepted a connection")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--renode", required=True)
    ap.add_argument("--resc", required=True)
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--renode-log", required=True)
    ap.add_argument("--startup-timeout", type=float, default=30.0)
    ap.add_argument("--run-timeout", type=float, default=120.0)
    args = ap.parse_args()

    with open(args.renode_log, "wb") as logf:
        proc = subprocess.Popen(
            [args.renode, "--disable-gui", "--hide-log", "-P", str(args.port)],
            stdin=subprocess.DEVNULL,
            stdout=logf,
            stderr=subprocess.STDOUT,
        )

        t_launch = time.monotonic()
        try:
            sock = wait_for_port("127.0.0.1", args.port, t_launch + args.startup_timeout)
        except TimeoutError as e:
            proc.kill()
            print(f"FAIL: {e}", file=sys.stderr)
            return 1
        print(f"monitor port {args.port} accepted connection after "
              f"{time.monotonic() - t_launch:.2f}s", flush=True)

        sock.settimeout(2.0)
        # Drain the connection banner (best-effort; not required for correctness).
        try:
            sock.recv(65536)
        except OSError:
            pass

        include_cmd = f"include @{args.resc}\r\n"
        sock.sendall(include_cmd.encode())
        print(f"sent: {include_cmd.strip()}", flush=True)

        t_run_start = time.monotonic()
        run_deadline = t_run_start + args.run_timeout
        # The .resc itself ends with `quit`, so the expected steady state is: Renode
        # processes the whole script (reset macro, start, RunFor, quit) and the process
        # exits on its own. Poll the process, not the socket -- the socket may go idle
        # well before the process actually tears down its .NET runtime.
        rc = None
        while time.monotonic() < run_deadline:
            rc = proc.poll()
            if rc is not None:
                break
            time.sleep(0.5)

        elapsed = time.monotonic() - t_run_start
        if rc is None:
            print(f"FAIL: renode did not exit within {args.run_timeout}s of sending "
                  f"'include @{args.resc}' (elapsed {elapsed:.1f}s) -- killing it", file=sys.stderr)
            proc.kill()
            proc.wait(timeout=10)
            sock.close()
            return 1

        sock.close()
        print(f"renode exited with code {rc} after {elapsed:.2f}s "
              f"(from sending include to process exit)", flush=True)
        return 0 if rc == 0 else rc


if __name__ == "__main__":
    sys.exit(main())
