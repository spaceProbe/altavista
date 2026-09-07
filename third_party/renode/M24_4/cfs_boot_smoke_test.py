#!/usr/bin/env python3
"""M24.4 item 3 prep -- first attempt to boot core-cpu1.exe (M24.3's cFS artifact) under
Renode. Reconnaissance, not a pass/fail gate: prints UART0 (cFE's debug console) content
after each chunk and the final rpu0 PC/ExecutedInstructions, so whatever happens (clean boot,
a crash, a hang, silence) is captured and reported honestly rather than assumed."""
import argparse
import re
import socket
import subprocess
import sys
import time

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")
VIRT_RE = re.compile(rb"Current virtual time:\s*([0-9:.]+)")
HEX_RE = re.compile(r"0x[0-9A-Fa-f]{8}")


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


def clean(raw: bytes) -> str:
    return ANSI_RE.sub(b"", raw).decode(errors="replace").strip()


def run_step(client, seconds):
    t0 = time.monotonic()
    client.send(f'emulation RunFor "{seconds}"; currentTime')
    raw = client.read_until_marker(b"Current virtual time:", overall_timeout=180.0)
    t1 = time.monotonic()
    out = clean(raw)
    m = VIRT_RE.search(out.encode())
    virt_s = parse_hms_ns(m.group(1).decode()) / 1e9 if m else None
    return t1 - t0, virt_s


def read_hex(client, cmd):
    client.send(cmd)
    out = clean(client.read_until_idle(idle_gap=0.3, overall_timeout=15.0))
    matches = HEX_RE.findall(out)
    return matches[-1] if matches else out


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
        "/Users/probe/code/AltaVista/third_party/renode/M24_4/cfs_boot_smoke_test.resc"))
    ap.add_argument("--port", type=int, default=15006)
    ap.add_argument("--chunks", type=int, default=6)
    ap.add_argument("--chunk-seconds", type=float, default=5.0)
    args = ap.parse_args()

    uart_log = "/Users/probe/code/AltaVista/third_party/renode/M24_4/cfs_boot_uart0.log"
    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4/cfs_boot_smoke_test.renode_log.txt"

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
            client.read_until_idle(idle_gap=0.5, overall_timeout=60.0)

            client.send("emulation SetGlobalAdvanceImmediately true")
            client.read_until_idle(idle_gap=0.3, overall_timeout=10.0)

            for i in range(args.chunks):
                try:
                    wall_s, virt_s = run_step(client, args.chunk_seconds)
                except TimeoutError as e:
                    print(f"chunk {i+1}: TIMEOUT: {e}", flush=True)
                    break
                pc = read_hex(client, "rpu0 PC")
                instr = read_hex(client, "rpu0 ExecutedInstructions")
                print(f"chunk {i+1}: wall={wall_s:.3f}s virtual={virt_s} PC={pc} instr={instr}", flush=True)
                try:
                    with open(uart_log, "rb") as f:
                        content = f.read()
                    print(f"  UART0 so far ({len(content)} bytes): {content[-500:]!r}", flush=True)
                except FileNotFoundError:
                    print("  UART0 log not created yet (zero bytes written)", flush=True)

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

    print("=== FINAL UART0 CONTENT ===")
    try:
        with open(uart_log, "rb") as f:
            print(f.read())
    except FileNotFoundError:
        print("(no UART0 output was ever captured)")


if __name__ == "__main__":
    sys.exit(main())
