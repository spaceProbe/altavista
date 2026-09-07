#!/usr/bin/env python3
"""M24.4b -- tries emulation CreateUartPtyTerminal (a PTY-backed terminal, rather than the
socket-backed CreateServerSocketTerminal whose inbound direction uart_tcp_single_byte_probe.py
already showed does not deliver bytes into uart1's RX FIFO on this build) as an alternative
mechanism for the bridge's RX-injection side. If a PTY terminal's inbound direction actually
works, that is a cleaner fix for the bridge than per-byte monitor WriteChar calls.
"""
import os
import pty as pty_module
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

RENODE = ("/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/"
          "Renode.app/Contents/MacOS/renode")
ELF = "/Users/probe/code/AltaVista/third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe"
PLATFORM = "/Users/probe/code/AltaVista/third_party/renode/platforms/cpus/zynqmp.repl"


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
    monitor_port = 15080
    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart_pty_terminal_probe.renode_log.txt"

    with open(log_path, "wb") as logf:
        proc = subprocess.Popen(
            [RENODE, "--disable-gui", "--hide-log", "-P", str(monitor_port)],
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

            client.send('mach create "pty-probe"')
            client.read_until_idle()
            client.send(f"machine LoadPlatformDescription @{PLATFORM}")
            client.read_until_idle(overall_timeout=15.0)

            client.send('emulation CreateUartPtyTerminal "ptyterm" "/tmp/av_m24_4b_uart1.pty" true')
            reply = clean(client.read_until_idle(overall_timeout=10.0))
            print(f"CreateUartPtyTerminal reply: {reply!r}")
            if "no such command" in reply.lower() or "error" in reply.lower():
                print("RESULT: CreateUartPtyTerminal not usable on this build")
                return 3

            client.send("connector Connect sysbus.uart1 ptyterm")
            print(f"connect reply: {clean(client.read_until_idle(overall_timeout=10.0))!r}")

            client.send("using sysbus")
            client.read_until_idle()
            client.send("using sysbus.cluster0")
            client.read_until_idle()
            client.send("using sysbus.cluster1")
            client.read_until_idle()
            client.send("cluster0 ForEach IsHalted true")
            client.read_until_idle()
            client.send("cluster1 ForEach IsHalted true")
            client.read_until_idle()
            client.send("rpu0 IsHalted false")
            client.read_until_idle()
            client.send(f"sysbus LoadELF @{ELF} cpu=rpu0")
            client.read_until_idle(overall_timeout=15.0)

            time.sleep(1.0)  # let the RTEMS side create the pty symlink/device

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
                print("RXEN never observed")
                return 2

            sts0, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
            print(f"pre-send channel_sts=0x{sts0:08x}")

            fd = os.open("/tmp/av_m24_4b_uart1.pty", os.O_RDWR | os.O_NOCTTY)
            os.write(fd, b"A")
            time.sleep(0.3)
            client.send('emulation RunFor "0.05"')
            client.read_until_idle(overall_timeout=10.0)
            os.close(fd)

            sts1, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
            worked = not (sts1 & CHANNEL_STS_REMPTY)
            print(f"post-send channel_sts=0x{sts1:08x} worked={worked}")
            if worked:
                popped, _ = read_reg_last_hex(client, REG_TX_RX_FIFO)
                print(f"popped: 0x{popped:02x}")

            client.send("quit")
            try:
                proc.wait(timeout=15.0)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=15.0)
            return 0 if worked else 1
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=15.0)


if __name__ == "__main__":
    sys.exit(main())
