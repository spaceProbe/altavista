#!/usr/bin/env python3
"""Attach the RTEMS toolchain's real `arm-rtems6-gdb` (a Linux aarch64 binary, so it runs in a
container on this macOS host) to a Renode GDB stub that `renode_bridge.py` started on the host
(`AV_BRIDGE_GDB_PORT`; question 171, q171-c), and run a gdb command file or gdb-python script.

    python3 gdb_attach.py --port 15900 --elf core-cpu1.exe --script stall_state.py

Facts this encodes (each measured, not assumed):
  - the image `debian:bookworm-slim` (pinned by digest below) lacks gdb's two shared-library
    dependencies; `apt-get install libncursesw6 libpython3.11` fixes it (needs the network, like
    the cFS build step of question 154, and costs about 20 s per run);
  - Colima mounts only $HOME into containers, so the ELF and the script are copied to a stage
    directory under $HOME (default `~/.altavista-gdb-stage`, removed afterwards) and mounted at /w;
  - `host.docker.internal` is reached with `--add-host host.docker.internal:host-gateway`;
  - the container is a plain `--rm` one with no `av.test` label, so the test harness's prune sweeps
    never touch it. It is deliberately NOT run under the docker-test lock: the cargo test that
    owns the stalled guest holds that lock for as long as the bridge holds the guest.
  - lldb's `gdb-remote` fails its handshake against Renode's stub (M24_4e_REPORT.md); gdb does not.
Output is gdb's own; warnings about `auto-load safe-path` are harmless.
"""
import argparse
import os
import shutil
import subprocess
import sys

TOOLCHAIN = "/Users/probe/code/AltaVista/third_party/rtems-container/output/toolchain"
IMAGE = "debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--elf", required=True, help="the exact core-cpu1.exe the guest booted")
    ap.add_argument("--script", required=True, help="gdb command file (.gdb) or gdb-python script (.py)")
    ap.add_argument("--toolchain", default=TOOLCHAIN)
    ap.add_argument("--stage", default=os.path.expanduser("~/.altavista-gdb-stage"))
    ap.add_argument("--keep-stage", action="store_true")
    args = ap.parse_args()

    os.makedirs(args.stage, exist_ok=True)
    elf_name = os.path.basename(args.elf)
    script_name = os.path.basename(args.script)
    shutil.copy(args.elf, os.path.join(args.stage, elf_name))
    shutil.copy(args.script, os.path.join(args.stage, script_name))
    source = "source"  # gdb's `source` loads both command files and (by extension) python scripts
    shell = (
        "export DEBIAN_FRONTEND=noninteractive; apt-get update -qq >/dev/null 2>&1; "
        "apt-get install -y -qq --no-install-recommends libncursesw6 libpython3.11 >/dev/null 2>&1; "
        "exec /tc/bin/arm-rtems6-gdb -nx -batch -ex 'set pagination off' -ex 'set confirm off' "
        f"-ex 'file /w/{elf_name}' -ex 'set tcp connect-timeout 20' "
        f"-ex 'target remote host.docker.internal:{args.port}' -ex '{source} /w/{script_name}'"
    )
    argv = ["docker", "run", "--rm", "--add-host", "host.docker.internal:host-gateway",
            "-v", f"{args.toolchain}:/tc:ro", "-v", f"{args.stage}:/w", IMAGE, "sh", "-c", shell]
    try:
        return subprocess.call(argv)
    finally:
        if not args.keep_stage:
            shutil.rmtree(args.stage, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
