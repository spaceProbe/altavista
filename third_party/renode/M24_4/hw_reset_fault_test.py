#!/usr/bin/env python3
"""M24.4 item 4 (partial, standalone) -- demonstrates the Renode-side mechanism a
`FAULT_TARGET_KIND_HARDWARE` reset fault (docs/open-questions.md question 120: "HARDWARE, for
containers now and Renode and boards later") would need on the guest side: boot cFE to
OPERATIONAL (as cfs_boot_smoke_test.py already proved), issue a machine-level reset through
the monitor, and confirm cFE genuinely reboots (a fresh boot banner and a fresh
POWER-ON-RESET sequence in the UART0 log), not merely that the command returned success.
This is the mechanism a bridge's `Reset` RPC handler (lockstep.proto's `Reset`, already wired
to a power-cycle fault per question 120) would drive -- the bridge process itself is not
built yet (see M24_4_REPORT.md item 3's "not done"), so this test drives the identical
monitor command directly, proving the mechanism Renode offers is real and usable, not
assuming it from documentation."""
import re
import socket
import subprocess
import sys
import time

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")
VIRT_RE = re.compile(rb"Current virtual time:\s*([0-9:.]+)")


def parse_hms_ns(s):
    hh, mm, rest = s.split(":")
    ss, frac = rest.split(".")
    return ((int(hh) * 3600 + int(mm) * 60 + int(ss)) * 1_000_000_000) + int(frac.ljust(9, "0")[:9])


class MonitorClient:
    def __init__(self, host, port, timeout=10.0):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.sock.settimeout(timeout)

    def read_until_idle(self, idle_gap=0.4, overall_timeout=30.0):
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

    def read_until_marker(self, marker, poll_timeout=0.5, grace=0.05, overall_timeout=120.0):
        chunks, buf = [], b""
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
            raise TimeoutError(f"marker {marker!r} not seen; buffer: {buf!r}")
        if found_at is None:
            raise TimeoutError(f"marker {marker!r} not seen; buffer: {buf!r}")
        return buf

    def send(self, line):
        self.sock.sendall((line + "\r\n").encode())


def clean(raw):
    return ANSI_RE.sub(b"", raw).decode(errors="replace").strip()


def run_step(client, seconds):
    client.send(f'emulation RunFor "{seconds}"; currentTime')
    raw = client.read_until_marker(b"Current virtual time:", overall_timeout=120.0)
    out = clean(raw)
    m = VIRT_RE.search(out.encode())
    return parse_hms_ns(m.group(1).decode()) / 1e9 if m else None


def wait_for_port(host, port, deadline):
    while time.monotonic() < deadline:
        try:
            return socket.create_connection((host, port), timeout=1.0)
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f"port {port} never accepted a connection")


def main():
    renode = ("/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/"
              "Renode.app/Contents/MacOS/renode")
    resc = "/Users/probe/code/AltaVista/third_party/renode/M24_4/cfs_boot_smoke_test.resc"
    port = 15010
    uart_log = "/Users/probe/code/AltaVista/third_party/renode/M24_4/cfs_boot_uart0.log"
    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4/hw_reset_fault_test.renode_log.txt"

    with open(log_path, "wb") as logf:
        proc = subprocess.Popen(
            [renode, "--disable-gui", "--hide-log", "-P", str(port)],
            stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
        )
        try:
            t0 = time.monotonic()
            sock = wait_for_port("127.0.0.1", port, t0 + 30.0)
            client = MonitorClient.__new__(MonitorClient)
            client.sock = sock
            sock.settimeout(2.0)
            try:
                sock.recv(65536)
            except OSError:
                pass

            client.send(f"include @{resc}")
            client.read_until_idle(idle_gap=0.6, overall_timeout=60.0)
            client.send("emulation SetGlobalAdvanceImmediately true")
            client.read_until_idle(idle_gap=0.3, overall_timeout=10.0)

            # Boot to OPERATIONAL (per cfs_boot_smoke_test.py's own finding, ~5-10 virtual
            # seconds is ample).
            run_step(client, 10)
            with open(uart_log, "rb") as f:
                pre_reset = f.read()
            print(f"pre-reset UART0 bytes: {len(pre_reset)}")
            print("pre-reset tail:", pre_reset[-300:])
            pre_reset_had_operational = b"CFE_ES_Main entering OPERATIONAL state" in pre_reset

            # The hardware-fault mechanism itself: a real machine reset via the monitor,
            # exactly what a bridge's Reset-RPC handler would issue for a
            # FAULT_TARGET_KIND_HARDWARE power-cycle fault (question 120).
            client.send("machine Reset")
            reset_reply = clean(client.read_until_idle(idle_gap=0.6, overall_timeout=30.0))
            print("machine Reset reply:", reset_reply)

            run_step(client, 10)
            with open(uart_log, "rb") as f:
                post_reset = f.read()
            print(f"post-reset UART0 bytes: {len(post_reset)}")
            print("post-reset tail:", post_reset[-500:])

            # A real reboot means a SECOND full boot sequence appended to the log (the file
            # backend appends, never truncates) -- not just "more of the same" text.
            second_boot_seen = post_reset.count(b"CFE_ES_SetupResetVariables") >= 2
            second_operational_seen = post_reset.count(b"CFE_ES_Main entering OPERATIONAL state") >= 2

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

    print(f"pre_reset_had_operational={pre_reset_had_operational}")
    print(f"second_boot_seen={second_boot_seen}")
    print(f"second_operational_seen={second_operational_seen}")
    passed = pre_reset_had_operational and second_boot_seen and second_operational_seen
    print("PASS" if passed else "FAIL")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
