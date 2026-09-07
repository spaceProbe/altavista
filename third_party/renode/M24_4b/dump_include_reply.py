#!/usr/bin/env python3
"""Diagnostic-only: dump the FULL, untruncated monitor reply to `include @uart1_register_probe.resc`
(including the LoadELF inside the macro) to a file, to check for any error/warning text the
earlier probes' 500-char-truncated prints might have hidden -- specifically anything from
`emulation CreateServerSocketTerminal` or `connector Connect` for uart1."""
import re
import socket
import subprocess
import sys
import time

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")


def clean(raw: bytes) -> str:
    return ANSI_RE.sub(b"", raw).decode(errors="replace")


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
    resc = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart1_register_probe_v2.resc"
    monitor_port = 15025
    bridge_port = 15026
    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/dump_include_reply.renode_log.txt"

    with open(log_path, "wb") as logf:
        proc = subprocess.Popen(
            [renode, "--disable-gui", "--hide-log", "-P", str(monitor_port)],
            stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
        )
        try:
            t0 = time.monotonic()
            sock = wait_for_port("127.0.0.1", monitor_port, t0 + 30.0)
            sock.settimeout(2.0)
            try:
                sock.recv(65536)
            except OSError:
                pass

            def send(line):
                sock.sendall((line + "\r\n").encode())

            def drain(timeout=5.0):
                sock.settimeout(timeout)
                chunks = []
                try:
                    while True:
                        data = sock.recv(65536)
                        if not data:
                            break
                        chunks.append(data)
                except socket.timeout:
                    pass
                return b"".join(chunks)

            send(f"$bridgeport = {bridge_port}")
            drain()
            send(f"include @{resc}")
            full_reply = drain(timeout=20.0)

            with open("/Users/probe/code/AltaVista/third_party/renode/M24_4b/full_include_reply.txt", "w") as f:
                f.write(clean(full_reply))

            print(f"wrote {len(full_reply)} raw bytes / {len(clean(full_reply))} cleaned chars")

            send("quit")
            try:
                proc.wait(timeout=15.0)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=15.0)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=15.0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
