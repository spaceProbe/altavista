#!/usr/bin/env python3
"""M24.4b -- v2 of the WriteChar RX-injection probe. The v1 probe (uart1_writechar_injection_probe.py)
called `sysbus.uart1 WriteChar <byte>` immediately after `include`ing the .resc, with NO
`emulation RunFor` ever issued -- so the guest CPU never actually ran and the UART's control
register was still at its POR default (0x128 = RXDIS|TXDIS|STPBRK), not the post-boot 0x114
(RXEN|TXEN|STPBRK) uart1_register_probe.py's own steps 22-42 measured. A UART that has never had
its receiver enabled is *correctly* expected to drop injected bytes -- v1's negative result is
confounded by this, not evidence that Renode's WriteChar mechanism itself is broken.

v2 fixes this: RunFor-steps first (reusing uart1_register_probe.py's exact 0.05s-step loop) until
RXEN is observed on the real control register (mirroring the real boot), THEN calls WriteChar,
THEN reads channel_sts/tx_rx_fifo. This isolates whether Renode's own peripheral-level RX
injection works once the guest has genuinely enabled the receiver -- independent of the
TCP-terminal path uart1_register_probe.py already showed does not deliver bytes.
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
REG_IRQ_STS = UART1_BASE + 0x14
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
    monitor_port = 15040
    bridge_port = 15041
    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart1_writechar_injection_probe_v2.renode_log.txt"

    samples = []
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

            rxen_seen_at = None
            for i in range(60):
                client.send('emulation RunFor "0.05"')
                client.read_until_idle(overall_timeout=10.0)
                ctrl, _ = read_reg_last_hex(client, REG_CONTROL)
                samples.append({"phase": f"step{i}", "control": ctrl})
                if ctrl & CONTROL_RXEN:
                    rxen_seen_at = i
                    print(f"RXEN observed at step {i}, control=0x{ctrl:08x}")
                    break
            if rxen_seen_at is None:
                print("RXEN never observed within 60 steps -- aborting WriteChar test as inconclusive")
                worked = None
            else:
                sts0, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
                print(f"pre-WriteChar channel_sts=0x{sts0:08x} REMPTY={bool(sts0 & CHANNEL_STS_REMPTY)}")

                client.send("sysbus.uart1 WriteChar 0x41")
                reply = clean(client.read_until_idle(overall_timeout=8.0))
                print(f"WriteChar reply: {reply!r}")

                sts1, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
                print(f"post-WriteChar channel_sts=0x{sts1:08x} REMPTY={bool(sts1 & CHANNEL_STS_REMPTY)}")

                worked = not (sts1 & CHANNEL_STS_REMPTY)
                if worked:
                    popped, _ = read_reg_last_hex(client, REG_TX_RX_FIFO)
                    print(f"popped from RX FIFO after WriteChar: 0x{popped:02x} ({chr(popped & 0xFF)!r})")

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

    result = {
        "rxen_first_observed_at_step": rxen_seen_at,
        "writechar_worked": worked,
        "samples": samples,
    }
    with open("/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart1_writechar_injection_probe_v2_result.json", "w") as f:
        json.dump(result, f, indent=2)
    print(json.dumps({k: v for k, v in result.items() if k != "samples"}, indent=2))
    return 0 if worked else 1


if __name__ == "__main__":
    sys.exit(main())
