#!/usr/bin/env python3
"""M24.4 item 3 -- proves the UART1-to-TCP-terminal wiring actually works on this Renode
build against our platform file and the real cFS ELF: launches Renode, sets the
$bridgeport monitor variable, includes the wiring resc, runs a few virtual seconds, then
connects a plain TCP client to that port and confirms the connection is accepted (not
refused/reset) -- the minimum bar for "an external process can reach the UART1 byte
stream," independent of whether any bytes flow yet (the guest's io_lockstep app is not
running -- see item 3's own "not done" note)."""
import argparse
import socket
import subprocess
import sys
import time


def wait_for_port(host, port, deadline):
    while time.monotonic() < deadline:
        try:
            return socket.create_connection((host, port), timeout=1.0)
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f"port {port} never accepted a connection")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--renode", default=(
        "/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/"
        "Renode.app/Contents/MacOS/renode"))
    ap.add_argument("--resc", default=(
        "/Users/probe/code/AltaVista/third_party/renode/M24_4/uart1_bridge_wiring_test.resc"))
    ap.add_argument("--monitor-port", type=int, default=15008)
    ap.add_argument("--bridge-port", type=int, default=15009)
    args = ap.parse_args()

    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4/uart1_bridge_wiring_test.renode_log.txt"
    with open(log_path, "wb") as logf:
        proc = subprocess.Popen(
            [args.renode, "--disable-gui", "--hide-log", "-P", str(args.monitor_port)],
            stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
        )
        try:
            t0 = time.monotonic()
            sock = wait_for_port("127.0.0.1", args.monitor_port, t0 + 30.0)
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

            send(f"$bridgeport = {args.bridge_port}")
            print("set $bridgeport reply:", drain()[:500])

            send(f"include @{args.resc}")
            print("include reply:", drain(timeout=15.0)[:1000])

            send('emulation RunFor "2"')
            print("RunFor reply:", drain(timeout=30.0)[:500])

            # Now try to connect an independent TCP client to the bridge port.
            try:
                client = socket.create_connection(("127.0.0.1", args.bridge_port), timeout=5.0)
                print(f"BRIDGE PORT {args.bridge_port}: CONNECTION ACCEPTED")
                client.settimeout(1.0)
                try:
                    data = client.recv(4096)
                    print(f"  received {len(data)} bytes on connect: {data!r}")
                except socket.timeout:
                    print("  no bytes received within 1s (expected -- no app driving UART1 yet)")
                client.close()
                result = "PASS: bridge TCP terminal accepted a connection"
            except OSError as e:
                result = f"FAIL: could not connect to bridge port {args.bridge_port}: {e}"
            print(result)

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

    print(result)
    return 0 if result.startswith("PASS") else 1


if __name__ == "__main__":
    sys.exit(main())
