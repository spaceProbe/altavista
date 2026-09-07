#!/usr/bin/env python3
"""M24.4b -- direct register instrumentation of uart1 (Cadence UART @ 0xff010000) during a
live handshake attempt, to localize the IO_LOCKSTEP handshake failure to either (a) Renode's
CreateServerSocketTerminal/connector wiring never depositing TCP bytes into the peripheral's
RX FIFO at all, or (b) the guest's own driver/control-register state (RXEN never set, IRQ
never enabled/delivered) leaving bytes sitting in the FIFO unread. This does not rely on any
assumption about what "should" be true -- it reads the real registers off the running
peripheral, independent of the guest CPU, using the exact `sysbus ReadDoubleWord` + last-hex-
token pattern ttc_rate_test.py already proved correct against this Renode build.

Register offsets confirmed directly from this exact BSP's own header
(third_party/rtems-container/work/rtems-src/bsps/include/dev/serial/zynq-uart-regs.h), not
guessed from generic ARM UART documentation:
  0x00 control      (bit2 RXEN, bit4 TXEN, bit3 RXDIS, bit5 TXDIS)
  0x14 irq_sts       (bit0 RTRIG -- RX FIFO trigger level reached)
  0x2C channel_sts   (bit1 REMPTY -- RX FIFO empty, bit0 RTRIG, bit2 RFUL)
  0x30 tx_rx_fifo    (read pops one byte off the RX FIFO -- read LAST, it is destructive)
uart1 base address 0xff010000, confirmed from third_party/renode/platforms/cpus/zynqmp.repl.
"""
import re
import socket
import subprocess
import sys
import time

HELLO_MAGIC = b"AVL1"
PROTOCOL_VERSION = 1

UART1_BASE = 0xFF010000
REG_CONTROL = UART1_BASE + 0x00
REG_IRQ_EN = UART1_BASE + 0x08
REG_IRQ_STS = UART1_BASE + 0x14
REG_CHANNEL_STS = UART1_BASE + 0x2C
REG_TX_RX_FIFO = UART1_BASE + 0x30

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")
HEX_RE = re.compile(r"0x[0-9A-Fa-f]{8}")

CONTROL_RXEN = 1 << 2
CONTROL_TXEN = 1 << 4
CHANNEL_STS_REMPTY = 1 << 1
CHANNEL_STS_RFUL = 1 << 2
CHANNEL_STS_RTRIG = 1 << 0
IRQ_STS_RTRIG = 1 << 0


def build_hello_frame():
    payload = HELLO_MAGIC + PROTOCOL_VERSION.to_bytes(2, "little")
    length = 1 + len(payload)
    return length.to_bytes(4, "little") + bytes([0x01]) + payload


def clean(raw: bytes) -> str:
    return ANSI_RE.sub(b"", raw).decode(errors="replace").strip()


class MonitorClient:
    def __init__(self, host, port, timeout=10.0):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.sock.settimeout(timeout)

    def read_until_idle(self, idle_gap=0.3, overall_timeout=30.0):
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

    def send(self, line: str):
        self.sock.sendall((line + "\r\n").encode())


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
    monitor_port = 15029
    bridge_port = 15030
    uart_log = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart1_register_probe_v3_uart0.log"
    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart1_register_probe_v3.renode_log.txt"

    samples = []

    with open(log_path, "wb") as logf:
        proc = subprocess.Popen(
            [renode, "--disable-gui", "--hide-log", "-P", str(monitor_port)],
            stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
        )
        try:
            t0 = time.monotonic()
            mon = wait_for_port("127.0.0.1", monitor_port, t0 + 30.0)
            client = MonitorClient.__new__(MonitorClient)
            client.sock = mon
            client.sock.settimeout(10.0)
            try:
                client.sock.recv(65536)
            except OSError:
                pass

            client.send(f"$bridgeport = {bridge_port}")
            client.read_until_idle()
            client.send(f"include @{resc}")
            print("include reply:", clean(client.read_until_idle(overall_timeout=20.0))[:500])

            # Baseline register read BEFORE the peer connects or any RunFor -- establishes
            # reset-state control/channel_sts values.
            ctrl0, _ = read_reg_last_hex(client, REG_CONTROL)
            sts0, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
            print(f"BASELINE (pre-boot) control=0x{ctrl0:08x} channel_sts=0x{sts0:08x}")
            samples.append({"phase": "pre-boot", "control": ctrl0, "channel_sts": sts0})

            bridge_client = socket.create_connection(("127.0.0.1", bridge_port), timeout=5.0)
            print("bridge client connected")

            hello = build_hello_frame()
            bridge_client.sendall(hello)
            print(f"sent HELLO frame (attempt 1): {hello!r}")

            # Continuously resend HELLO across EVERY step (not just the first second) so that
            # whenever the guest actually opens /dev/ttyS1 and flips RXEN, freshly-sent bytes
            # are in flight for Renode to (attempt to) deliver -- this closes the gap in the
            # first version of this probe, where resending stopped after 1s of virtual time and
            # RXEN did not flip to 1 until sometime after that, so no bytes were in flight at
            # the moment the port actually became receptive.
            num_steps = 80
            rxen_seen_at = None
            for i in range(num_steps):
                client.send('emulation RunFor "0.05"')
                client.read_until_idle(overall_timeout=10.0)
                try:
                    bridge_client.sendall(hello)
                except OSError as e:
                    print(f"send failed on attempt {i+2}: {e}")

                ctrl, _ = read_reg_last_hex(client, REG_CONTROL)
                sts, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
                irq, _ = read_reg_last_hex(client, REG_IRQ_STS)
                if (ctrl & CONTROL_RXEN) and rxen_seen_at is None:
                    rxen_seen_at = i
                rec = {"phase": f"step{i}", "control": ctrl, "channel_sts": sts, "irq_sts": irq}
                samples.append(rec)
                print(f"step {i:2d}: control=0x{ctrl:08x} (RXEN={bool(ctrl & CONTROL_RXEN)} "
                      f"TXEN={bool(ctrl & CONTROL_TXEN)}) channel_sts=0x{sts:08x} "
                      f"(REMPTY={bool(sts & CHANNEL_STS_REMPTY)} RFUL={bool(sts & CHANNEL_STS_RFUL)} "
                      f"RTRIG={bool(sts & CHANNEL_STS_RTRIG)}) irq_sts=0x{irq:08x} "
                      f"(RTRIG={bool(irq & IRQ_STS_RTRIG)})")
                # Once RXEN has been observed, keep sending and sampling for 20 more steps past
                # that point, then stop early (no need to run the full num_steps budget).
                if rxen_seen_at is not None and i >= rxen_seen_at + 20:
                    break

            print(f"RXEN first observed at step {rxen_seen_at}")

            ctrl_f, _ = read_reg_last_hex(client, REG_CONTROL)
            sts_f, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
            irq_f, _ = read_reg_last_hex(client, REG_IRQ_STS)
            print(f"FINAL: control=0x{ctrl_f:08x} channel_sts=0x{sts_f:08x} irq_sts=0x{irq_f:08x}")

            # Destructive: pop the RX FIFO to see whether any byte at all is actually sitting
            # there (do this LAST, after every non-destructive read above).
            popped = []
            for _ in range(8):
                sts_check, _ = read_reg_last_hex(client, REG_CHANNEL_STS)
                if sts_check & CHANNEL_STS_REMPTY:
                    break
                fifo_val, _ = read_reg_last_hex(client, REG_TX_RX_FIFO)
                popped.append(fifo_val & 0xFF)
            print(f"POPPED FROM RX FIFO: {popped!r} ({len(popped)} bytes)")

            bridge_client.settimeout(2.0)
            guest_bytes = b""
            try:
                while True:
                    chunk = bridge_client.recv(4096)
                    if not chunk:
                        break
                    guest_bytes += chunk
            except socket.timeout:
                pass
            print(f"guest sent {len(guest_bytes)} bytes on the bridge TCP port: {guest_bytes!r}")

            # v3 deliberately has no uart0 file backend (isolating whether a second, uart0-
            # attached backend interferes with uart1's RX delivery), so there is no UART0
            # transcript to check here.
            handshake_failed = None
            print("UART0 not backed in this variant (isolating uart1-only wiring); skipped")

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

    import json
    result = {
        "samples": samples,
        "rxen_first_observed_at_step": rxen_seen_at,
        "final": {"control": ctrl_f, "channel_sts": sts_f, "irq_sts": irq_f},
        "popped_fifo_bytes": popped,
        "guest_bytes_len": len(guest_bytes),
        "guest_bytes_hex": guest_bytes.hex(),
        "handshake_failed_in_log": handshake_failed,
    }
    with open("/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart1_register_probe_v3_result.json", "w") as f:
        json.dump(result, f, indent=2)
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
