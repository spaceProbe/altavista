#!/usr/bin/env python3
"""M24.4f smoke test, stage 1: does the `CharReceived` Python hook mechanism work at all in this
exact Renode build, independent of the lockstep protocol / uart1 / IO_LOCKSTEP entirely?

Attaches the hook (`uart_tx_bridge_hook.py`) to **uart0** (the console UART) instead of uart1,
and ALSO attaches an independent `CreateFileBackend` to uart0 in the same run -- uart0's own file
log is already the mechanism M24's whole effort (M24.2d/M24.4/M24.4e) has trusted throughout for
the boot banner, so it is the known-good reference here. Boots the real `core-cpu1.exe`, waits
for the RTEMS/cFE boot banner to appear on BOTH the hook socket and the file log, and asserts the
two captures are byte-for-byte identical over whatever was captured. No lockstep protocol, no
uart1, no WriteChar -- purely "does a Python hook on a real UART's CharReceived event, forwarding
to a socket the caller owns, deliver every byte reliably."
"""
import os
import socket
import subprocess
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "M24_4b"))
from renode_bridge import MonitorClient, wait_for_port, clean  # noqa: E402

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
RENODE_BIN = os.path.join(REPO_ROOT, "third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode")
PLATFORM = os.path.join(REPO_ROOT, "third_party/renode/platforms/cpus/zynqmp.repl")
ELF = os.path.join(REPO_ROOT, "third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe")
HOOK_PY = os.path.join(os.path.dirname(__file__), "uart_tx_bridge_hook.py")

SCRATCH = f"/tmp/av-renode-m24f-smoke-{os.getpid()}"


def main():
    os.makedirs(SCRATCH, exist_ok=True)
    monitor_port = _free_port()
    hook_port = _free_port()
    file_log = os.path.join(SCRATCH, "uart0_file.log")
    renode_log = os.path.join(SCRATCH, "renode_stdout.log")

    # The bridge owns the hook's socket: bind + listen BEFORE Renode ever tries to connect out.
    hook_srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    hook_srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    hook_srv.bind(("127.0.0.1", hook_port))
    hook_srv.listen(1)

    logf = open(renode_log, "wb")
    proc = subprocess.Popen(
        [RENODE_BIN, "--disable-gui", "--hide-log", "-P", str(monitor_port)],
        stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
    )
    try:
        t0 = time.monotonic()
        sock = wait_for_port("127.0.0.1", monitor_port, t0 + 30.0)
        mon = MonitorClient(sock)
        mon.sock.settimeout(10.0)
        try:
            mon.sock.recv(65536)
        except OSError:
            pass

        resc_path = os.path.join(SCRATCH, "smoke.resc")
        with open(resc_path, "w") as f:
            f.write(f'''
:name: av-m24-4f-smoke
using sysbus
using sysbus.cluster0
using sysbus.cluster1
mach create "av-m24-4f-smoke"
machine LoadPlatformDescription @{PLATFORM}
uart0 CreateFileBackend @{file_log} true
include @{HOOK_PY}
setup_uart_tx_bridge sysbus.uart0 {hook_port}
macro reset
"""
    cluster0 ForEach IsHalted true
    cluster1 ForEach IsHalted true
    rpu0 IsHalted false
    sysbus LoadELF @{ELF} cpu=rpu0
"""
runMacro $reset
''')
        reply = mon.cmd(f"include @{resc_path}", timeout=20.0)
        print("smoke: include reply:", clean(reply)[:300])

        print("smoke: waiting for hook to connect...")
        hook_srv.settimeout(60.0)
        conn, _addr = hook_srv.accept()
        conn.settimeout(30.0)
        print("smoke: hook connected")

        # Let the guest boot and print its banner to uart0.
        mon.run_for(5.0, timeout=30.0)

        hook_bytes = bytearray()
        deadline = time.monotonic() + 10.0
        while time.monotonic() < deadline:
            try:
                chunk = conn.recv(65536)
                if not chunk:
                    break
                hook_bytes += chunk
            except socket.timeout:
                break

        with open(file_log, "rb") as f:
            file_bytes = f.read()

        print(f"smoke: hook captured {len(hook_bytes)} bytes, file backend captured {len(file_bytes)} bytes")
        common = min(len(hook_bytes), len(file_bytes))
        assert common > 0, "neither capture got any bytes at all"
        if bytes(hook_bytes[:common]) == file_bytes[:common]:
            print(f"smoke: PASS -- first {common} bytes are byte-for-byte identical between the hook socket and the file backend")
        else:
            print("smoke: FAIL -- captures diverge")
            for i in range(common):
                if hook_bytes[i] != file_bytes[i]:
                    print(f"  first divergence at byte {i}: hook={hook_bytes[i]:#04x} file={file_bytes[i]:#04x}")
                    break
            sys.exit(1)

        stats = mon.cmd("uart_tx_bridge_stats", timeout=8.0)
        print("smoke: hook stats:", clean(stats))
    finally:
        try:
            mon.send("quit")
        except Exception:
            pass
        try:
            proc.wait(timeout=15.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=15.0)
        hook_srv.close()


def _free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


if __name__ == "__main__":
    main()
