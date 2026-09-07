#!/usr/bin/env python3
"""M24.4b -- identify the actual runtime type Renode instantiates for
`emulation CreateServerSocketTerminal`, and try three configuration variants against the RXEN-on
window (reusing the exact method that made WriteChar and the TCP-single-byte test directly
comparable): (A) baseline (flag=false, no EOL) -- the same as uart_tcp_single_byte_probe.py, for a
sanity re-check in this same script; (B) flag=true; (C) sending the byte followed by a newline,
in case the terminal buffers/dispatches on line boundaries. Each variant uses a fresh Renode
process (machine state does not reset cleanly enough within one process for a byte-arrival test to
be trusted otherwise).
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


def run_variant(name, monitor_port, bridge_port, flag, payload, log_dir):
    log_path = f"{log_dir}/terminal_type_probe_{name}.renode_log.txt"
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

            client.send(f'mach create "variant-{name}"')
            client.read_until_idle()
            client.send(f"machine LoadPlatformDescription @{PLATFORM}")
            client.read_until_idle(overall_timeout=15.0)
            client.send(f'emulation CreateServerSocketTerminal {bridge_port} "term_{name}" {flag}')
            reply = clean(client.read_until_idle(overall_timeout=10.0))
            print(f"[{name}] CreateServerSocketTerminal reply: {reply!r}")
            client.send(f"connector Connect sysbus.uart1 term_{name}")
            client.read_until_idle(overall_timeout=10.0)

            client.send("externals")
            print(f"[{name}] externals: {clean(client.read_until_idle(overall_timeout=8.0))}")

            client.send("cluster0 ForEach IsHalted true")
            client.read_until_idle()
            client.send("cluster1 ForEach IsHalted true")
            client.read_until_idle()
            client.send("rpu0 IsHalted false")
            client.read_until_idle()
            client.send(f"sysbus LoadELF @{ELF} cpu=rpu0")
            client.read_until_idle(overall_timeout=15.0)

            bridge_client = wait_for_port("127.0.0.1", bridge_port, time.monotonic() + 15.0)
            print(f"[{name}] bridge client connected")

            rxen_seen_at = None
            for i in range(60):
                client.send('emulation RunFor "0.05"')
                client.read_until_idle(overall_timeout=10.0)
                ctrl, _ = read_reg_last_hex(client, REG_CONTROL)
                if ctrl & CONTROL_RXEN:
                    rxen_seen_at = i
                    break
            if rxen_seen_at is None:
                print(f"[{name}] RXEN never observed")
                return {"variant": name, "rxen_seen": False, "worked": None}

            sts0, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
            bridge_client.sendall(payload)
            time.sleep(0.3)
            client.send('emulation RunFor "0.05"')
            client.read_until_idle(overall_timeout=10.0)
            sts1, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
            worked = not (sts1 & CHANNEL_STS_REMPTY)
            popped = None
            if worked:
                popped, _ = read_reg_last_hex(client, REG_TX_RX_FIFO)
            print(f"[{name}] rxen@{rxen_seen_at} pre_sts=0x{sts0:08x} post_sts=0x{sts1:08x} "
                  f"worked={worked} popped={popped}")

            client.send("quit")
            try:
                proc.wait(timeout=15.0)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=15.0)
            return {"variant": name, "rxen_seen": True, "rxen_seen_at": rxen_seen_at,
                    "pre_sts": sts0, "post_sts": sts1, "worked": worked, "popped": popped}
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=15.0)


def main():
    log_dir = "/Users/probe/code/AltaVista/third_party/renode/M24_4b"
    results = []
    results.append(run_variant("A_flag_false_no_eol", 15070, 15071, "false", b"A", log_dir))
    results.append(run_variant("B_flag_true", 15072, 15073, "true", b"A", log_dir))
    results.append(run_variant("C_flag_false_with_eol", 15074, 15075, "false", b"A\n", log_dir))
    print(json.dumps(results, indent=2))
    with open(f"{log_dir}/uart_terminal_type_probe_result.json", "w") as f:
        json.dump(results, f, indent=2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
