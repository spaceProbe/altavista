#!/usr/bin/env python3
"""M24.4d: validates the actual fix applied to renode_bridge.py -- imports the REAL,
now-patched `RenodeBridge` class (not a reimplementation) and drives its real `.start()` method
against a fresh per-run scratch dir, confirming:
  1. the generated `.resc` file lands under the scratch dir (not the old hardcoded M24_4b path),
     named with this process's own pid;
  2. `start()` completes and the uart terminal socket is reachable;
  3. the old hardcoded path is untouched (does not even get created) by this run.
"""
import os
import sys
import time

sys.path.insert(0, "/Users/probe/code/AltaVista/third_party/renode/M24_4b")
sys.path.insert(0, "/Users/probe/code/AltaVista")
import renode_bridge as rb  # noqa: E402

REPO = "/Users/probe/code/AltaVista"
SCRATCH = f"{REPO}/third_party/renode/M24_4d/validate_fix_scratch"
os.makedirs(SCRATCH, exist_ok=True)

old_hardcoded_resc = f"{REPO}/third_party/renode/M24_4b/renode_bridge_generated.resc"
had_old_file_before = os.path.exists(old_hardcoded_resc)
old_mtime_before = os.path.getmtime(old_hardcoded_resc) if had_old_file_before else None


def free_tcp_port():
    import socket
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


monitor_port = free_tcp_port()
uart_port = free_tcp_port()
renode_log = f"{SCRATCH}/renode_monitor.log"
uart0_log = f"{SCRATCH}/uart0.log"

bridge = rb.RenodeBridge(
    renode_bin=f"{REPO}/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode",
    platform=f"{REPO}/third_party/renode/platforms/cpus/zynqmp.repl",
    elf=f"{REPO}/third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe",
    monitor_port=monitor_port,
    uart_port=uart_port,
    uart0_log=uart0_log,
)

t0 = time.monotonic()
try:
    bridge.start(renode_log)
    elapsed = time.monotonic() - t0
    print(f"start() succeeded in {elapsed:.2f}s")
    expected_resc = f"{SCRATCH}/renode_bridge_generated.{os.getpid()}.resc"
    print(f"expected per-run resc path exists: {os.path.exists(expected_resc)} ({expected_resc})")
    print(f"scratch dir contents: {sorted(os.listdir(SCRATCH))}")
finally:
    bridge.stop()

had_old_file_after = os.path.exists(old_hardcoded_resc)
old_mtime_after = os.path.getmtime(old_hardcoded_resc) if had_old_file_after else None
print(f"old hardcoded path existed before: {had_old_file_before} (mtime {old_mtime_before})")
print(f"old hardcoded path exists after:   {had_old_file_after} (mtime {old_mtime_after})")
print(f"old hardcoded path UNTOUCHED by this run: {old_mtime_before == old_mtime_after}")
