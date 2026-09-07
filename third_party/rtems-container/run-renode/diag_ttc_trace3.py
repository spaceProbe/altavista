#!/usr/bin/env python3
"""M24.2d diagnostic v3 (not part of the pipeline; supersedes v1/v2, which each had a
different register-read parsing bug -- v1 mislabeled which step a value printed under
(buffering race) though the values themselves were real; v2's regex matched the ECHOED
COMMAND's own address text instead of the returned value (constant 0xFF110018 giveaway).
This version: (1) confirms Renode's own ground-truth virtual clock via `currentTime` in the
same command line as RunFor (proven robust, see spike/measure_virtual_time.py), (2) reads
each register with its own send/idle-read round trip and takes the LAST 8-hex-digit token in
the reply (the echoed command's address is the FIRST occurrence; the returned value is
whatever comes after it), (3) additionally reads GIC distributor ISENABLER/ISPENDR/ICFGR for
IRQ36 (ttc0 channel 0) to check enable/pending/edge-vs-level state without triggering any
read-side-effect (IAR is deliberately never read here -- acknowledging an interrupt is a
side-effecting operation on real GIC hardware and presumably in Renode's model too)."""
import argparse
import re
import socket
import subprocess
import sys
import time

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")
VIRT_RE = re.compile(rb"Current virtual time:\s*([0-9:.]+)")
REAL_RE = re.compile(rb"Current real time:\s*([0-9:.]+)")
HEX_RE = re.compile(r"0x[0-9A-Fa-f]{8}")

TTC0_BASE = 0xff110000
TTC_REGS = {
    "CLK_CNTRL": 0x00,
    "CNT_CNTRL": 0x0C,
    "COUNT_VALUE": 0x18,
    "INTERVAL_VAL": 0x24,
    "MATCH_0": 0x30,
    "ISR": 0x54,
    "IER": 0x60,
}

GIC_DIST_BASE = 0xf9000000  # rpuGic distributor (GICv1), per zynqmp.repl
GIC_REGS = {
    "ISENABLER1": 0x104,  # IRQ 32-63 enable-set, bit4 = IRQ36
    "ISPENDR1": 0x204,    # IRQ 32-63 pending-set, bit4 = IRQ36
    "ICFGR2": 0xC08,       # IRQ 32-47 config (2 bits/IRQ); IRQ36 = bits 8-9
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
    return ANSI_RE.sub(b"", raw).decode(errors="replace").strip()


def read_reg_last_hex(client, addr):
    client.send(f"sysbus ReadDoubleWord {hex(addr)}")
    out = client.read_until_idle(idle_gap=0.3, overall_timeout=15.0)
    text = clean(out)
    matches = HEX_RE.findall(text)
    # First match is the echoed command's own address; the value is whatever comes after.
    # If only one match exists (address == value coincidentally, or echo suppressed), fall
    # back to it but flag with the raw text for manual inspection.
    if len(matches) >= 2:
        return matches[-1], text
    elif len(matches) == 1:
        return matches[0], text
    return None, text


def dump_all(client, label, log):
    print(f"=== {label} ===", flush=True)
    log.append(f"=== {label} ===")
    for name, off in TTC_REGS.items():
        val, raw = read_reg_last_hex(client, TTC0_BASE + off)
        line = f"  TTC0.{name} = {val}"
        print(line, flush=True)
        log.append(line + f"   [raw: {raw!r}]")
    for name, off in GIC_REGS.items():
        val, raw = read_reg_last_hex(client, GIC_DIST_BASE + off)
        line = f"  rpuGic.{name} = {val}"
        print(line, flush=True)
        log.append(line + f"   [raw: {raw!r}]")
    for cmd in ["rpu0 PC", "rpu0 IsHalted", "rpu0 ExecutedInstructions"]:
        client.send(cmd)
        out = clean(client.read_until_idle(idle_gap=0.3, overall_timeout=10.0))
        line = f"  {cmd} -> {out!r}"
        print(line, flush=True)
        log.append(line)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=3448)
    ap.add_argument("--steps", type=int, default=25)
    ap.add_argument("--step-seconds", type=float, default=1.0)
    ap.add_argument("--final-jump-seconds", type=float, default=0.0,
                     help="if >0, one extra RunFor of this size after the per-second loop")
    args = ap.parse_args()

    renode = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
    rundir = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"
    resc = f"{rundir}/ticker_noquit.resc"

    log = []
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
        print(f">>> include done", flush=True)

        dump_all(client, "t=0 (post-reset)", log)

        for i in range(1, args.steps + 1):
            cmd = f'emulation RunFor "{args.step_seconds}"; currentTime'
            t0 = time.monotonic()
            client.send(cmd)
            out = client.read_until_marker(b"Current real time:", overall_timeout=600.0)
            wall = time.monotonic() - t0
            text = ANSI_RE.sub(b"", out)
            vm = VIRT_RE.search(text)
            virt_s = parse_hms_ns(vm.group(1).decode()) / 1e9 if vm else None
            hdr = f"--- step {i}: renode currentTime={virt_s}s, driver_wall={wall:.3f}s ---"
            print(hdr, flush=True)
            log.append(hdr)
            dump_all(client, f"after step {i}", log)

        if args.final_jump_seconds > 0:
            cmd = f'emulation RunFor "{args.final_jump_seconds}"; currentTime'
            t0 = time.monotonic()
            client.send(cmd)
            out = client.read_until_marker(b"Current real time:", overall_timeout=1200.0)
            wall = time.monotonic() - t0
            text = ANSI_RE.sub(b"", out)
            vm = VIRT_RE.search(text)
            virt_s = parse_hms_ns(vm.group(1).decode()) / 1e9 if vm else None
            hdr = f"--- final jump +{args.final_jump_seconds}s: renode currentTime={virt_s}s, driver_wall={wall:.3f}s ---"
            print(hdr, flush=True)
            log.append(hdr)
            dump_all(client, "after final jump", log)

        with open(f"{rundir}/ticker_uart.log", "rb") as f:
            uart = f.read()
        print(f"UART final ({len(uart)}B): {uart!r}", flush=True)
        log.append(f"UART final ({len(uart)}B): {uart!r}")

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
        log.append(f"renode final returncode: {proc.returncode}")

    with open(f"{rundir}/diag_ttc_trace3_clean.log", "w") as f:
        f.write("\n".join(log) + "\n")


if __name__ == "__main__":
    main()
