#!/usr/bin/env python3
"""M24.2d diagnostic v5: same as diag_gic_check.py but using the GIC read syntax that was
confirmed to work (`sysbus ReadDoubleWord <addr> rpu0` -- the CPU-context argument is
required for the GIC's banked registers), applied both before and after a RunFor that spans
well past the point ticking stops."""
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


def dump_gic_ttc(client, label):
    print(f"\n--- {label} ---", flush=True)
    for cmd, name in [
        ("sysbus ReadDoubleWord 0xf9000104 rpu0", "GIC.ISENABLER1 (IRQ32-63 enable, bit4=IRQ36)"),
        ("sysbus ReadDoubleWord 0xf9000204 rpu0", "GIC.ISPENDR1 (IRQ32-63 pending, bit4=IRQ36)"),
        ("sysbus ReadDoubleWord 0xf9000c08 rpu0", "GIC.ICFGR2 (IRQ32-47 edge/level cfg, bits8-9=IRQ36)"),
        ("sysbus ReadDoubleWord 0xf9000400 rpu0", "GIC.IPRIORITYR9 (IRQ36 priority, byte0)"),
        ("sysbus ReadDoubleWord 0xff110018", "TTC0.COUNT_VALUE"),
        ("sysbus ReadDoubleWord 0xff110030", "TTC0.MATCH_0"),
        ("sysbus ReadDoubleWord 0xff110054", "TTC0.ISR"),
        ("sysbus ReadDoubleWord 0xff110060", "TTC0.IER"),
        ("rpu0 PC", "rpu0.PC"),
        ("rpu0 IsHalted", "rpu0.IsHalted"),
    ]:
        client.send(cmd)
        out = clean(client.read_until_idle(idle_gap=0.3, overall_timeout=15.0))
        print(f"  {name}: {out}", flush=True)


renode = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
rundir = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"
resc = f"{rundir}/ticker_noquit.resc"
port = 3450

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

    dump_gic_ttc(client, "t=0, immediately post-reset (before RTEMS init runs)")

    # Short run: just enough for RTEMS to boot and install the clock ISR + first couple of
    # ticks, to see GIC enable/pending state while things are known-working.
    client.send('emulation RunFor "3"; currentTime')
    out = client.read_until_marker(b"Current real time:", overall_timeout=120.0)
    print(f"\nRunFor 3 reply: {clean(out)!r}", flush=True)
    dump_gic_ttc(client, "after RunFor 3s (clock driver should be installed and ticking)")

    # Now run well past the point ticking is known to stop (established: last tick at
    # nominal virtual ~24-30s in the 30-step run; go to 45s here in one jump).
    client.send('emulation RunFor "42"; currentTime')
    out = client.read_until_marker(b"Current real time:", overall_timeout=300.0)
    print(f"\nRunFor 42 more (cumulative ~45s) reply: {clean(out)!r}", flush=True)
    dump_gic_ttc(client, "after cumulative ~45s (well past the last observed tick)")

    with open(f"{rundir}/ticker_uart.log", "rb") as f:
        content = f.read()
    print(f"\nUART final ({len(content)}B): {content!r}", flush=True)

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
    print(f"\nrenode final returncode: {proc.returncode}")
