#!/usr/bin/env python3
"""M24.4b -- tests Renode's own built-in UART RX-injection monitor command
(`<uartName> WriteChar <byte>`), which simulates a byte arriving over the wire directly on the
peripheral object, as an alternative to the CreateServerSocketTerminal/connector Connect path
already shown not to deliver externally-sent TCP bytes into uart1's RX FIFO. If this works, it
localizes the defect specifically to the terminal/connector wiring for uart1 (not to the
peripheral's RX FIFO/model itself), and gives the bridge an alternative, Renode-native
mechanism to inject bytes on uart1's RX side that does not depend on the broken path.
"""
import re
import socket
import subprocess
import sys
import time

UART1_BASE = 0xFF010000
REG_CHANNEL_STS = UART1_BASE + 0x2C
REG_TX_RX_FIFO = UART1_BASE + 0x30

CHANNEL_STS_REMPTY = 1 << 1

HEX_RE = re.compile(r"0x[0-9A-Fa-f]{8}")
ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")


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


def read_reg_last_hex(client, addr, timeout=15.0):
    client.send(f"sysbus ReadDoubleWord {hex(addr)}")
    out = client.read_until_idle(idle_gap=0.3, overall_timeout=timeout)
    text = clean(out)
    matches = HEX_RE.findall(text)
    if len(matches) >= 2:
        return int(matches[-1], 16), text
    elif len(matches) == 1:
        return int(matches[0], 16), text
    raise RuntimeError(f"no hex value found reading {hex(addr)}: {text!r}")


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
    resc = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart1_register_probe_v3_no_uart0_backend.resc"
    monitor_port = 15033
    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart1_writechar_injection_probe.renode_log.txt"

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

            client.send("$bridgeport = 15034")
            client.read_until_idle()
            client.send(f"include @{resc}")
            client.read_until_idle(overall_timeout=20.0)

            sts0, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
            print(f"baseline channel_sts=0x{sts0:08x} REMPTY={bool(sts0 & CHANNEL_STS_REMPTY)}")

            # Try the WriteChar monitor command directly on the uart1 peripheral object,
            # bypassing the terminal/connector path entirely.
            client.send("sysbus.uart1 WriteChar 0x41")
            reply = clean(client.read_until_idle(overall_timeout=8.0))
            print(f"WriteChar reply: {reply!r}")

            sts1, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
            print(f"after WriteChar channel_sts=0x{sts1:08x} REMPTY={bool(sts1 & CHANNEL_STS_REMPTY)}")

            popped = None
            if not (sts1 & CHANNEL_STS_REMPTY):
                popped, _ = read_reg_last_hex(client, REG_TX_RX_FIFO)
                print(f"popped from RX FIFO after WriteChar: 0x{popped:02x} ({chr(popped & 0xFF)!r})")

            worked = not (sts1 & CHANNEL_STS_REMPTY)
            print(f"WriteChar RX-injection works for uart1: {worked}")

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

    print("PASS: WriteChar injects into uart1 RX" if worked else "FAIL: WriteChar does not inject either")
    return 0 if worked else 1


if __name__ == "__main__":
    sys.exit(main())
