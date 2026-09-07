#!/usr/bin/env python3
"""M24.4e -- minimal, dependency-free GDB-remote-protocol client used to read LIVE guest memory
off Renode's own `machine StartGdbServer` stub while the guest is held paused at the confirmed
STEP stall (renode_bridge.py's own AV_M24_4E_GDB_PORT/AV_M24_4E_GDB_HOLD_S temporary
instrumentation). Written because lldb's own `gdb-remote` command fails its handshake against this
exact stub ("failed to get reply to handshake packet within timeout of 0.0 seconds") -- confirmed
by hand with a raw socket probe that Renode's stub answers `qSupported`/memory-read packets
correctly, so the protocol itself works; only lldb's client-side handshake state machine does not
get along with it. This script talks the protocol directly instead of fighting that.

All struct offsets below were computed once, offline, against this exact
core-cpu1.exe's own DWARF debug info via `lldb -o "target create core-cpu1.exe" -o "expr --
(unsigned long)&((T*)0)->field"` -- not guessed, not from generic RTEMS documentation, since
struct layout depends on this exact compiler/optimization level.
"""
import socket
import struct
import sys
import time

HOST = "127.0.0.1"
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 15900

# --- addresses/offsets, all resolved against THIS core-cpu1.exe (nm / lldb static analysis) ---
ZYNQMP_UART_INSTANCES = 0x400AE5D0   # zynqmp_uart_instances[2], zynq_uart_context, sizeof=44
UART_CTX_SIZE = 44
UART_CTX_OFF_REGS = 28
UART_CTX_OFF_TX_QUEUED = 32
UART_CTX_OFF_TRANSMITTING = 36
UART_CTX_OFF_IRQ = 40

RTEMS_TASKS_INFO = 0x400AE36C        # _RTEMS_tasks_Information, Objects_Information, sizeof=48
OBJINFO_OFF_MAXIMUM_ID = 0
OBJINFO_OFF_LOCAL_TABLE = 4          # Objects_Control **local_table

TCB_OFF_NAME = 12                    # Thread_Control->Object.name (packed rtems_name, 4 bytes)
TCB_OFF_CURRENT_STATE = 28           # Thread_Control->current_state (States_Control, 4 bytes)
TCB_OFF_REG_SP = 136 + 32            # Thread_Control->Registers.register_sp
TCB_OFF_REG_LR = 136 + 36            # Thread_Control->Registers.register_lr

WATCHDOG_TICKS_SINCE_BOOT = 0x401DC4F8
CLOCK_DRIVER_TICKS = 0x401DBBBC


class GdbRemote:
    def __init__(self, host, port):
        self.sock = socket.create_connection((host, port), timeout=10)
        self.sock.settimeout(5)
        self.buf = b""
        # Drain whatever unsolicited data the stub sends immediately on connect (Renode sends an
        # unsolicited stop-reply packet here -- confirmed by the earlier raw probe).
        try:
            self.buf += self.sock.recv(65536)
        except OSError:
            pass

    def _read_more(self, timeout=5.0):
        self.sock.settimeout(timeout)
        data = self.sock.recv(65536)
        if not data:
            raise ConnectionError("gdbstub closed the connection")
        self.buf += data

    def _extract_one_packet(self):
        """Pulls one $...#XX packet out of self.buf, discarding any leading +/- acks and any
        bytes before the next '$'. Returns the payload (without $ and #XX), or None if a full
        packet is not yet buffered."""
        while True:
            i = self.buf.find(b"$")
            if i == -1:
                return None
            j = self.buf.find(b"#", i)
            if j == -1 or len(self.buf) < j + 3:
                return None
            payload = self.buf[i + 1:j]
            # checksum = self.buf[j+1:j+3] -- not verified here, informational reads only
            self.buf = self.buf[j + 3:]
            return payload

    def read_packet(self, timeout=8.0):
        deadline = time.monotonic() + timeout
        while True:
            pkt = self._extract_one_packet()
            if pkt is not None:
                return pkt
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("no full gdb-remote packet within timeout")
            self._read_more(timeout=remaining)

    def send_packet(self, payload: bytes):
        checksum = sum(payload) & 0xFF
        pkt = b"$" + payload + b"#" + f"{checksum:02x}".encode()
        self.sock.sendall(b"+")  # ack anything pending, harmless if stub ignores it
        self.sock.sendall(pkt)

    def cmd(self, payload: bytes, timeout=8.0):
        self.send_packet(payload)
        # Renode's stub proactively sends unsolicited stop-reply ('T...'/'S...') and console-output
        # ('O...') packets independent of request/reply pairing (confirmed live: one such packet
        # was already sitting in the buffer before any request was ever sent). Skip any packet that
        # is not plausibly an answer to *this* request instead of trusting strict request/reply
        # ordering.
        while True:
            pkt = self.read_packet(timeout=timeout)
            if pkt[:1] in (b"T", b"S", b"O", b"W", b"X") and payload[:1] != b"X":
                continue
            return pkt

    def read_mem(self, addr: int, length: int) -> bytes:
        reply = self.cmd(f"m{addr:x},{length:x}".encode())
        if reply.startswith(b"E"):
            raise RuntimeError(f"gdbstub error reading {length} bytes at {addr:#x}: {reply!r}")
        return bytes.fromhex(reply.decode())


def u32(b, off):
    return struct.unpack_from("<I", b, off)[0]


def read_u32(gdb, addr):
    return u32(gdb.read_mem(addr, 4), 0)


def read_uart_ctx(gdb, index):
    base = ZYNQMP_UART_INSTANCES + index * UART_CTX_SIZE
    regs = read_u32(gdb, base + UART_CTX_OFF_REGS)
    tx_queued = struct.unpack("<i", gdb.read_mem(base + UART_CTX_OFF_TX_QUEUED, 4))[0]
    transmitting = gdb.read_mem(base + UART_CTX_OFF_TRANSMITTING, 1)[0]
    irq = read_u32(gdb, base + UART_CTX_OFF_IRQ)
    return {"base": base, "regs": regs, "tx_queued": tx_queued, "transmitting": bool(transmitting), "irq": irq}


STACK_SCAN_BYTES = 1024
SYMBOLS_PATH = "/Users/probe/code/AltaVista/third_party/renode/M24_4e/text_symbols_sorted.txt"


def load_symbols():
    """Parses `nm`'s sorted `<addr> <T|t> <name>` output (already sorted by address) into a list
    of (addr, name) pairs for enclosing-symbol lookups."""
    syms = []
    with open(SYMBOLS_PATH) as f:
        for line in f:
            parts = line.split()
            if len(parts) < 3:
                continue
            try:
                addr = int(parts[0], 16)
            except ValueError:
                continue
            syms.append((addr, parts[2]))
    syms.sort(key=lambda t: t[0])
    return syms


def lookup_symbol(syms, addr, max_offset=0x800):
    """Finds the function whose range plausibly contains `addr` (largest symbol addr <= addr,
    within max_offset) -- a plain, dependency-free binary search, not a full unwind."""
    if addr < 0x40000000 or addr > 0x40300000:
        return None
    lo, hi = 0, len(syms) - 1
    best = None
    while lo <= hi:
        mid = (lo + hi) // 2
        if syms[mid][0] <= addr:
            best = mid
            lo = mid + 1
        else:
            hi = mid - 1
    if best is None:
        return None
    sym_addr, sym_name = syms[best]
    if addr - sym_addr > max_offset:
        return None
    return (sym_name, sym_addr)


def decode_rtems_name(raw_u32):
    """Classic RTEMS packs a 4-char task name into a uint32, one byte per character,
    big-endian (matches rtems_build_name('I','O','_','L') style helpers)."""
    b = struct.pack(">I", raw_u32)
    return bytes(c if 32 <= c < 127 else ord('.') for c in b).decode()


def main():
    gdb = GdbRemote(HOST, PORT)
    print("=== connected, initial buffered bytes:", gdb.buf[:200])

    print("\n--- uart0 (console) live context ---")
    u0 = read_uart_ctx(gdb, 0)
    print(u0)
    print("\n--- uart1 (lockstep) live context ---")
    u1 = read_uart_ctx(gdb, 1)
    print(u1)

    print("\n--- global tick counters (two samples, 1s apart, to see if anything is still advancing) ---")
    wd1 = read_u32(gdb, WATCHDOG_TICKS_SINCE_BOOT)
    cd1 = read_u32(gdb, CLOCK_DRIVER_TICKS)
    time.sleep(1.0)
    wd2 = read_u32(gdb, WATCHDOG_TICKS_SINCE_BOOT)
    cd2 = read_u32(gdb, CLOCK_DRIVER_TICKS)
    print(f"_Watchdog_Ticks_since_boot: {wd1} -> {wd2} (delta {wd2 - wd1})")
    print(f"Clock_driver_ticks: {cd1} -> {cd2} (delta {cd2 - cd1})")

    print("\n--- RTEMS classic task table walk ---")
    max_id = read_u32(gdb, RTEMS_TASKS_INFO + OBJINFO_OFF_MAXIMUM_ID)
    local_table_ptr = read_u32(gdb, RTEMS_TASKS_INFO + OBJINFO_OFF_LOCAL_TABLE)
    print(f"maximum_id={max_id:#x} local_table@{local_table_ptr:#x}")
    # Objects_Id low 16 bits are typically the index+1 range; just scan a generous fixed range of
    # slot indices (classic RTEMS local_table is indexed 0..max_index, slot 0 usually unused).
    for i in range(0, 24):
        entry_addr = local_table_ptr + i * 4
        try:
            tcb = read_u32(gdb, entry_addr)
        except Exception as e:
            print(f"[{i}] read error: {e}")
            continue
        if tcb == 0:
            continue
        name_raw = read_u32(gdb, tcb + TCB_OFF_NAME)
        state = read_u32(gdb, tcb + TCB_OFF_CURRENT_STATE)
        sp = read_u32(gdb, tcb + TCB_OFF_REG_SP)
        lr = read_u32(gdb, tcb + TCB_OFF_REG_LR)
        print(f"[{i}] tcb={tcb:#x} name={decode_rtems_name(name_raw)!r} ({name_raw:#010x}) "
              f"current_state={state:#x} saved_sp={sp:#x} saved_lr={lr:#x}")

    print("\n--- stack-scan each task (return addresses on the stack, matched against known symbols) ---")
    symbols = load_symbols()
    for i in range(0, 24):
        entry_addr = local_table_ptr + i * 4
        tcb = read_u32(gdb, entry_addr)
        if tcb == 0:
            continue
        name_raw = read_u32(gdb, tcb + TCB_OFF_NAME)
        sp = read_u32(gdb, tcb + TCB_OFF_REG_SP)
        chunk = gdb.read_mem(sp, STACK_SCAN_BYTES)
        hits = []
        for off in range(0, len(chunk), 4):
            (word,) = struct.unpack_from("<I", chunk, off)
            sym = lookup_symbol(symbols, word)
            if sym is not None:
                hits.append((off, word, sym))
        print(f"[{i}] tcb={tcb:#x} name_raw={name_raw:#010x} sp={sp:#x}")
        for off, word, (sym_name, sym_addr) in hits:
            print(f"      sp+{off:#04x} = {word:#010x}  -> {sym_name}+{word - sym_addr:#x}")

    print("\ndone")


if __name__ == "__main__":
    main()
