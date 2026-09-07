#!/usr/bin/env python3
"""M24.4 item 3 -- the real test of whether the UART1 bridge wiring plus the /cf fix let
`io_lockstep`'s lockstep-local v1 handshake actually succeed: connects to the UART1 TCP
terminal early (before the guest has had a chance to attempt and fail its handshake),
proactively sends a real "AVL1" v1 HELLO frame (the shim always speaks first --
services/cfs/README.md's own documented convention), and checks (a) whether the guest replies
with its own HELLO frame on the wire, and (b) that the UART0 debug console log does NOT
contain "lockstep-local handshake failed" this time (it did, byte-for-byte, in
cfs_boot_smoke_test.py's run with nothing connected to uart1 at all)."""
import re
import socket
import subprocess
import sys
import time

HELLO_MAGIC = b"AVL1"
PROTOCOL_VERSION = 1


def build_hello_frame():
    payload = HELLO_MAGIC + PROTOCOL_VERSION.to_bytes(2, "little")
    length = 1 + len(payload)  # frame_type + payload
    return length.to_bytes(4, "little") + bytes([0x01]) + payload


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
    resc = "/Users/probe/code/AltaVista/third_party/renode/M24_4/cfs_handshake_test.resc"
    monitor_port = 15011
    bridge_port = 15012
    uart_log = "/Users/probe/code/AltaVista/third_party/renode/M24_4/cfs_handshake_uart0.log"
    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4/cfs_handshake_test.renode_log.txt"

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
            print("include reply:", drain(timeout=15.0)[:500])

            # Connect the fake peer BEFORE running any virtual time, so it is present the
            # instant io_lockstep opens /dev/ttyS1 and attempts its handshake.
            bridge_client = socket.create_connection(("127.0.0.1", bridge_port), timeout=5.0)
            print("bridge client connected")

            # Shim speaks first (services/cfs/README.md) -- send our own HELLO immediately,
            # and keep re-sending it on a tight loop through fine-grained RunFor steps, in
            # case io_lockstep's own UART open/handshake happens before Renode's UART model
            # has actually wired the bytes through, or io_lockstep's own read is a single
            # non-blocking attempt rather than a retrying one.
            hello = build_hello_frame()
            bridge_client.sendall(hello)
            print(f"sent HELLO frame (attempt 1): {hello!r}")

            for i in range(20):
                send('emulation RunFor "0.05"')
                drain(timeout=10.0)
                try:
                    bridge_client.sendall(hello)
                except OSError as e:
                    print(f"send failed on attempt {i+2}: {e}")
                    break

            # Run the remainder of virtual time forward.
            send('emulation RunFor "4"')
            run_reply = drain(timeout=60.0)
            print("RunFor reply:", run_reply[:300])

            # Read whatever the guest sent back on the bridge port.
            bridge_client.settimeout(3.0)
            guest_bytes = b""
            try:
                while True:
                    chunk = bridge_client.recv(4096)
                    if not chunk:
                        break
                    guest_bytes += chunk
            except socket.timeout:
                pass
            print(f"guest sent {len(guest_bytes)} bytes on the bridge port: {guest_bytes!r}")

            with open(uart_log, "rb") as f:
                uart_content = f.read()
            handshake_failed = b"lockstep-local handshake failed" in uart_content
            print(f"UART0 'handshake failed' present: {handshake_failed}")
            print("UART0 tail:", uart_content[-800:])

            bridge_client.close()
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

    # Interpret: a reply frame starting with the same 4-byte length+type-01 HELLO shape (or at
    # minimum: non-empty bytes AND no "handshake failed" in the log) is the pass condition.
    got_hello_back = guest_bytes[:5] == (7).to_bytes(4, "little") + bytes([0x01])
    print(f"got_hello_back={got_hello_back} handshake_failed_in_log={handshake_failed}")
    passed = (not handshake_failed) and len(guest_bytes) > 0
    print("PASS" if passed else "FAIL (or inconclusive)")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
