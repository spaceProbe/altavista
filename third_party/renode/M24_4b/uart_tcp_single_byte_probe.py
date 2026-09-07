#!/usr/bin/env python3
"""M24.4b -- apples-to-apples comparison with uart1_writechar_injection_probe_v2.py: RunFor-step
until RXEN is genuinely observed on the real control register (same method, same .resc), THEN
send exactly one byte ('A', 0x41 -- the identical byte value the WriteChar probe used) over the
external TCP client socket that CreateServerSocketTerminal/connector Connect wires to uart1, THEN
immediately (no further RunFor in between) check channel_sts/tx_rx_fifo -- structured exactly
like the WriteChar probe so the two results are directly comparable and isolate whether the
defect is in the terminal/connector's inbound (TCP->WriteChar) delivery specifically, given that
WriteChar itself (called directly) is now proven to work once RXEN is on.
"""
import re
import socket
import subprocess
import sys
import time
import json

UART1_BASE = 0xFF010000
REG_CONTROL = UART1_BASE + 0x00
REG_CHANNEL_STS = UART1_BASE + 0x2C
REG_TX_RX_FIFO = UART1_BASE + 0x30

CONTROL_RXEN = 1 << 2
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
    monitor_port = 15050
    bridge_port = 15051
    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart_tcp_single_byte_probe.renode_log.txt"

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
            print("include reply:", clean(client.read_until_idle(overall_timeout=20.0))[:300])

            bridge_client = wait_for_port("127.0.0.1", bridge_port, time.monotonic() + 15.0)
            print("bridge client connected (before boot, like the real handshake test)")

            rxen_seen_at = None
            for i in range(60):
                client.send('emulation RunFor "0.05"')
                client.read_until_idle(overall_timeout=10.0)
                ctrl, _ = read_reg_last_hex(client, REG_CONTROL)
                if ctrl & CONTROL_RXEN:
                    rxen_seen_at = i
                    print(f"RXEN observed at step {i}, control=0x{ctrl:08x}")
                    break
            if rxen_seen_at is None:
                print("RXEN never observed within 60 steps -- aborting as inconclusive")
                return 2

            sts0, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
            print(f"pre-send channel_sts=0x{sts0:08x} REMPTY={bool(sts0 & CHANNEL_STS_REMPTY)}")

            bridge_client.sendall(b"A")
            print("sent single byte 0x41 ('A') over the TCP bridge client socket")
            # Give Renode's socket-terminal backend the same opportunity a real byte-arrival
            # would get: a short real-wall-time pause, then one more RunFor "0" tick to let any
            # queued host-side socket event pump before checking (still no guest-code assumption
            # -- this only gives the terminal backend a chance to process the socket, mirroring
            # what production wiring would experience: bytes do not arrive instantaneously).
            time.sleep(0.2)
            client.send('emulation RunFor "0.01"')
            client.read_until_idle(overall_timeout=10.0)

            sts1, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
            print(f"post-send channel_sts=0x{sts1:08x} REMPTY={bool(sts1 & CHANNEL_STS_REMPTY)}")

            worked = not (sts1 & CHANNEL_STS_REMPTY)
            popped = None
            if worked:
                popped, _ = read_reg_last_hex(client, REG_TX_RX_FIFO)
                print(f"popped from RX FIFO after TCP send: 0x{popped:02x}")

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

    result = {"rxen_first_observed_at_step": rxen_seen_at, "tcp_single_byte_worked": worked}
    with open("/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart_tcp_single_byte_probe_result.json", "w") as f:
        json.dump(result, f, indent=2)
    print(json.dumps(result, indent=2))
    return 0 if worked else 1


if __name__ == "__main__":
    sys.exit(main())
