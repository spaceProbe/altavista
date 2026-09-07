#!/usr/bin/env python3
"""M24.4b -- isolates the OUTBOUND (guest/peripheral -> TCP client) direction of the uart1
wiring, entirely independent of the guest CPU: writes directly into uart1's own
control/tx_rx_fifo registers via the monitor (`sysbus WriteDoubleWord`), bypassing RTEMS/cFE
entirely, and checks whether those bytes appear on the external TCP client's socket. Combined
with uart1_register_probe.py's finding (inbound TCP bytes never reach channel_sts/tx_rx_fifo),
this either (a) shows uart1's connector link genuinely carries bytes in the outbound direction
only, an asymmetric defect, or (b) shows nothing moves in EITHER direction for this specific
peripheral instance/wiring, which is a different, more fundamental defect than a one-directional
bug in the RX path alone.
"""
import re
import socket
import subprocess
import sys
import time

UART1_BASE = 0xFF010000
REG_CONTROL = UART1_BASE + 0x00
REG_TX_RX_FIFO = UART1_BASE + 0x30

CONTROL_TXEN = 1 << 4
CONTROL_TXRES = 1 << 1

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")
HEX_RE = re.compile(r"0x[0-9A-Fa-f]{8}")


def clean(raw: bytes) -> str:
    return ANSI_RE.sub(b"", raw).decode(errors="replace").strip()


class MonitorClient:
    def __init__(self, sock):
        self.sock = sock

    def send(self, line: str):
        self.sock.sendall((line + "\r\n").encode())

    def read_until_idle(self, idle_gap=0.3, overall_timeout=15.0):
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
    resc = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart1_register_probe.resc"
    monitor_port = 15027
    bridge_port = 15028
    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart1_tx_direction_probe.renode_log.txt"

    with open(log_path, "wb") as logf:
        proc = subprocess.Popen(
            [renode, "--disable-gui", "--hide-log", "-P", str(monitor_port)],
            stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
        )
        try:
            t0 = time.monotonic()
            mon = wait_for_port("127.0.0.1", monitor_port, t0 + 30.0)
            client = MonitorClient(mon)
            client.sock.settimeout(10.0)
            try:
                client.sock.recv(65536)
            except OSError:
                pass

            client.send(f"$bridgeport = {bridge_port}")
            client.read_until_idle()
            client.send(f"include @{resc}")
            client.read_until_idle(overall_timeout=20.0)

            bridge_client = wait_for_port("127.0.0.1", bridge_port, time.monotonic() + 15.0)
            print("bridge client connected")

            # Force TXEN on directly (bypassing the guest entirely -- this does not depend on
            # RTEMS/cFE having booted at all) and reset the TX logic, then push several bytes
            # into tx_rx_fifo one at a time, exactly mirroring what real guest code / the polled
            # write helper would do.
            client.send(f"sysbus WriteDoubleWord {hex(REG_CONTROL)} {CONTROL_TXRES}")
            client.read_until_idle()
            client.send(f"sysbus WriteDoubleWord {hex(REG_CONTROL)} {CONTROL_TXEN}")
            client.read_until_idle()

            test_bytes = b"AV_M24_4B_TX_DIRECTION_PROBE\n"
            for b in test_bytes:
                client.send(f"sysbus WriteDoubleWord {hex(REG_TX_RX_FIFO)} {b}")
                client.read_until_idle(idle_gap=0.1, overall_timeout=5.0)

            client.send('emulation RunFor "0.5"')
            client.read_until_idle(overall_timeout=15.0)

            bridge_client.settimeout(3.0)
            received = b""
            try:
                while True:
                    chunk = bridge_client.recv(4096)
                    if not chunk:
                        break
                    received += chunk
            except socket.timeout:
                pass

            print(f"received on TCP client after direct register writes: {received!r}")
            got_it = test_bytes in received or (test_bytes.strip() in received)
            print(f"TX direction works (bytes written to tx_rx_fifo reached the TCP client): {got_it}")

            bridge_client.close()
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

    print("PASS: uart1 TX (peripheral->TCP) direction works" if got_it
          else "FAIL: uart1 TX (peripheral->TCP) direction ALSO does not deliver bytes")
    return 0 if got_it else 1


if __name__ == "__main__":
    sys.exit(main())
