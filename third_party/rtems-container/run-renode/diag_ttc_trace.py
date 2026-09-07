#!/usr/bin/env python3
"""M24.2d diagnostic (not part of the pipeline). Drives ticker_noquit.resc via the Monitor's
-P <port> protocol (same proven method as run_via_monitor.py / diag_ticker_steps2.py) and, at
each 1-virtual-second RunFor step from t=0 to t=20 (bracketing the ~14s stall observed in
M24.2c), reads back:
  - rpu0 PC and rpu0 IsHalted (WFI vs spinning vs progressing)
  - rpu0 ExecutedInstructions (cumulative instruction count -> instructions/virtual-second)
  - TTC0 registers directly off the bus: CLK_CNTRL, CNT_CNTRL, COUNT_VALUE, INTERVAL_VAL,
    MATCH_0, ISR, IER (offsets from bsps/include/dev/clock/xttcps_hw.h)
  - the growing UART capture file

All raw monitor replies are printed verbatim (not parsed/summarized away) so the actual
register values are the evidence, per the task's "no silent fallbacks" rule.
"""
import socket
import subprocess
import sys
import time

RENODE = "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
PORT = 3446
RUNDIR = "/Users/probe/code/AltaVista/third_party/rtems-container/run-renode"
RESC = f"{RUNDIR}/ticker_noquit.resc"
UART_LOG = f"{RUNDIR}/ticker_uart.log"

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


def send_and_read(sock, cmd, timeout=90.0, idle=0.4):
    sock.sendall((cmd + "\r\n").encode())
    sock.settimeout(idle)
    chunks = []
    deadline = time.monotonic() + timeout
    last_data = time.monotonic()
    while time.monotonic() < deadline:
        try:
            data = sock.recv(65536)
            if not data:
                break
            chunks.append(data)
            last_data = time.monotonic()
        except socket.timeout:
            if time.monotonic() - last_data > idle:
                break
    return b"".join(chunks)


def clean(raw):
    # Strip ANSI escape sequences and telnet negotiation bytes for readability; the raw
    # bytes are still available by not calling this on anything load-bearing.
    import re
    text = raw.decode(errors="replace")
    text = re.sub(r"\x1b\[[0-9;]*[a-zA-Z]", "", text)
    text = text.replace("\xff\xfd\x00", "").replace("\xff\xfb\x01", "")
    text = text.replace("\xff\xfb\x03", "").replace('\xff\xfc"', "")
    return text.strip()


def read_reg(sock, name, addr):
    out = send_and_read(sock, f"sysbus ReadDoubleWord {hex(addr)}", timeout=15.0)
    return name, clean(out)


proc = subprocess.Popen([RENODE, "--disable-gui", "--hide-log", "-P", str(PORT)],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    deadline = time.monotonic() + 30
    sock = None
    while time.monotonic() < deadline:
        try:
            sock = socket.create_connection(("127.0.0.1", PORT), timeout=1.0)
            break
        except OSError:
            time.sleep(0.2)
    if sock is None:
        print("FAIL: never connected")
        sys.exit(1)
    sock.settimeout(0.5)
    time.sleep(0.5)
    try:
        sock.recv(65536)
    except OSError:
        pass

    out = send_and_read(sock, f"include @{RESC}", timeout=30.0)
    print(f">>> include @{RESC} -> {clean(out)!r}", flush=True)

    def dump_state(label):
        print(f"=== {label} ===", flush=True)
        out = send_and_read(sock, "rpu0 PC", timeout=10.0)
        print(f"  rpu0 PC: {clean(out)}", flush=True)
        out = send_and_read(sock, "rpu0 IsHalted", timeout=10.0)
        print(f"  rpu0 IsHalted: {clean(out)}", flush=True)
        for attempt_cmd in ["rpu0 ExecutedInstructions", "rpu0 GetTotalExecutedInstructions"]:
            out = send_and_read(sock, attempt_cmd, timeout=10.0)
            c = clean(out)
            print(f"  {attempt_cmd}: {c}", flush=True)
        for name, off in REGS.items():
            _, val = read_reg(sock, name, TTC0_BASE + off)
            print(f"  TTC0.{name} (0x{TTC0_BASE+off:08x}): {val}", flush=True)
        try:
            with open(UART_LOG, "rb") as f:
                content = f.read()
            print(f"  UART so far ({len(content)}B): {content!r}", flush=True)
        except FileNotFoundError:
            print("  UART file does not exist yet", flush=True)

    dump_state("t=0 (post-reset, pre-run)")

    for step in range(1, 21):
        t0 = time.monotonic()
        out = send_and_read(sock, 'emulation RunFor "1"', timeout=120.0)
        elapsed = time.monotonic() - t0
        print(f"--- RunFor step {step} (cumulative virtual ~{step}s), wall {elapsed:.2f}s, "
              f"reply {clean(out)!r} ---", flush=True)
        dump_state(f"t~{step}s")

    send_and_read(sock, "quit", timeout=10.0)
finally:
    proc.poll()
    if proc.returncode is None:
        proc.kill()
        try:
            proc.wait(timeout=5)
        except Exception:
            pass
    print(f"renode final returncode: {proc.returncode}")
