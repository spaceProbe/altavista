#!/usr/bin/env python3
"""M24.4b -- the Renode bridge (docs/sil-plan.md M24 exit criterion; M24_4_REPORT.md's design,
built on M24_4b_REPORT.md's root-caused handshake fix).

Owns a Renode process, wires `uart1` to the fixed platform+ELF, and relays "lockstep-local v1"
frames (services/cfs/README.md) between `crates/av-lockstep-shim` (the *peer* from the guest's
point of view -- the shim listens, the bridge connects, exactly matching
`services/cfs/README.md`'s "who listens, who connects, who speaks first" rule, with the bridge
standing in for what a native AF_UNIX-capable guest would otherwise do directly) and the RTEMS
guest's own `IO_LOCKSTEP` app running under Renode on `/dev/ttyS1` (`uart1`).

Transport asymmetry, root-caused and proven in M24_4b_REPORT.md (not assumed): Renode's own
terminal backends (socket- and PTY-based, both tried) do not forward externally-supplied bytes
into this exact `Cadence_UART` instance's RX path on this build, while (a) the peripheral's own
`WriteChar` monitor primitive does, reliably, and (b) the *outbound* direction of the same
terminal wiring (peripheral TX -> external client) does work MOST of the time -- M24.4e
root-caused (with a live GDB-remote attach plus an independent `CreateFileBackend` capture) a
real Renode-side defect: `CreateServerSocketTerminal`'s own forwarding of a peripheral's
transmitted bytes to its socket silently drops bytes written immediately before the guest CPU
parks in WFI, exactly `IO_LOCKSTEP`'s own write-then-idle pattern. M24.4f (this file's current
state, `third_party/renode/M24_4f_REPORT.md`) replaces that entire read path with a Renode
Python hook on uart1's own `CharReceived` event (`third_party/renode/M24_4f/
uart_tx_bridge_hook.py`) forwarding straight into a socket this bridge owns -- no Renode
terminal/backend abstraction, no buffering-and-flush dependency, in the path at all. So:
  - guest -> shim (peripheral's own UART1 transmissions): a Renode-side Python hook on uart1's
    `CharReceived` event connects OUT, as a plain TCP client, to a socket this bridge already has
    bound and listening (`self.uart_port`) before Renode's `include` even runs, and forwards every
    byte the instant the peripheral's own C# event fires it -- proven-working, byte-for-byte,
    M24_4f_REPORT.md.
  - shim -> guest (bytes the peer would write to /dev/ttyS1): injected one byte at a time via
    `sysbus.uart1 WriteChar <byte>` over the existing Renode monitor connection, then the bridge
    issues `emulation RunFor` for the guest CPU to actually execute and process them. Unchanged
    by M24.4f -- this direction was never the problem.

`STEP` frames (frame_type 0x04) carry a `LockstepStepRequest` whose `until_tai_ns` is what the
kernel wants virtual time advanced to; M24.4's own TTC-rate finding (2.0e-4 relative error, our
own `platforms/cpus/zynqmp.repl`) is what makes "Renode virtual seconds elapsed" a trustworthy
proxy for "guest TAI ns elapsed", so the bridge issues `RunFor "<delta_seconds>"` for exactly that
much wall-clock-independent virtual time before checking for the guest's reply. Non-STEP frames
(HELLO/BIND/SHUTDOWN) get a small fixed RunFor -- enough for the guest to process one frame at its
own pace -- since they carry no virtual-time target of their own.

`RESET` (frame_type 0x06) is handled specially, not simply relayed: a real Renode `machine Reset`
(M24.4's own hw_reset_fault_test.py: proven to genuinely reboot cFE, a second complete boot
sequence in the UART log) is what a *hardware* power-cycle actually means for an emulated target,
more faithful than the container binding's own lighter-weight in-place `psp_lockstep_init` reset
(which is the best a real POSIX process can do, having no hardware to power-cycle). So on RESET
the bridge issues `machine Reset`, waits for the guest to reboot, replays the exact original
`HELLO`/`BIND` byte sequences it cached from this same run's own start (the freshly-rebooted guest
remembers nothing and must redo both from scratch) to re-establish the connection transparently,
and only then synthesizes and sends the shim a `RESET_ACK` carrying the original request's
`sequence` -- the shim/kernel never sees any of this reboot-and-rehandshake machinery, exactly the
way `services/cfs/README.md`'s protocol contract expects `Reset` to look from the caller's side.
"""
import argparse
import os
import re
import socket
import struct
import subprocess
import sys
import threading
import time

from altavista.pb.altavista.v1 import lockstep_pb2  # noqa: E402

UART1_BASE = 0xFF010000
REG_CONTROL = UART1_BASE + 0x00
REG_CHANNEL_STS = UART1_BASE + 0x2C
CONTROL_RXEN = 1 << 2

FRAME_HELLO = 0x01
FRAME_BIND = 0x02
FRAME_BIND_ACK = 0x03
FRAME_STEP = 0x04
FRAME_STEP_DONE = 0x05
FRAME_RESET = 0x06
FRAME_RESET_ACK = 0x07
FRAME_SHUTDOWN = 0x08
FRAME_SHUTDOWN_ACK = 0x09
FRAME_ERROR = 0xFF

HEX_RE = re.compile(r"0x[0-9A-Fa-f]{8}")
ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")


def clean(raw: bytes) -> str:
    return ANSI_RE.sub(b"", raw).decode(errors="replace").strip()


def read_exact(sock, n, deadline=None):
    buf = b""
    while len(buf) < n:
        if deadline is not None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"read_exact timed out, got {len(buf)}/{n} bytes")
            sock.settimeout(remaining)
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("peer closed mid-frame")
        buf += chunk
    return buf


def read_frame(sock, timeout=30.0):
    """Reads one lockstep-local v1 frame; returns (frame_type, payload, raw_bytes)."""
    deadline = time.monotonic() + timeout
    length_field = read_exact(sock, 4, deadline)
    (length,) = struct.unpack("<I", length_field)
    if length == 0:
        raise ValueError("zero-length frame")
    rest = read_exact(sock, length, deadline)
    frame_type = rest[0]
    payload = rest[1:]
    return frame_type, payload, length_field + rest


def build_frame(frame_type, payload):
    length = 1 + len(payload)
    return struct.pack("<I", length) + bytes([frame_type]) + payload


class MonitorClient:
    def __init__(self, sock):
        self.sock = sock
        self.lock = threading.Lock()

    def send(self, line: str):
        self.sock.sendall((line + "\r\n").encode())

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

    def cmd(self, line, timeout=30.0):
        with self.lock:
            self.send(line)
            return self.read_until_idle(overall_timeout=timeout)

    def read_reg_last_hex(self, addr, timeout=15.0):
        out = self.cmd(f"sysbus ReadDoubleWord {hex(addr)}", timeout=timeout)
        text = clean(out)
        matches = HEX_RE.findall(text)
        if len(matches) >= 2:
            return int(matches[-1], 16)
        elif len(matches) == 1:
            return int(matches[0], 16)
        raise RuntimeError(f"no hex value found reading {hex(addr)}: {text!r}")

    def write_char(self, byte_val: int):
        self.cmd(f"sysbus.uart1 WriteChar {byte_val}", timeout=8.0)

    def write_chars(self, data: bytes, chunk_size: int = 64):
        """Pipelines many `WriteChar` monitor commands: sends a whole chunk's worth of commands
        in one `sendall` (rather than one send-then-block-for-reply round trip per byte) and then
        drains all their replies with a single `read_until_idle`. Cuts wall time roughly N-fold
        for an N-byte frame -- STEP frames carrying real sensor packets are tens to well over a
        hundred bytes, and a synchronous per-byte round trip (each paying at least
        `read_until_idle`'s own idle-gap wait) made a real DRM run impractically slow."""
        with self.lock:
            for start in range(0, len(data), chunk_size):
                chunk = data[start:start + chunk_size]
                lines = "".join(f"sysbus.uart1 WriteChar {b}\r\n" for b in chunk)
                self.sock.sendall(lines.encode())
                # Each WriteChar reply is short; give the whole chunk a budget proportional to
                # its size rather than one fixed timeout, but keep the idle-gap small so it
                # returns promptly once Renode's monitor has caught up.
                self.read_until_idle(idle_gap=0.2, overall_timeout=max(8.0, 0.2 * len(chunk)))

    def run_for(self, seconds: float, timeout=60.0):
        self.cmd(f'emulation RunFor "{seconds:.6f}"', timeout=timeout)


def wait_for_port(host, port, deadline):
    while time.monotonic() < deadline:
        try:
            return socket.create_connection((host, port), timeout=1.0)
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f"port {port} never accepted a connection")


HOOK_SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "M24_4f", "uart_tx_bridge_hook.py")


def read_frame_from_growing_file(path, offset, timeout=30.0, poll_interval=0.02):
    """Reads one lockstep-local v1 frame starting at byte `offset` of a file that some OTHER
    process (Renode, via `CreateFileBackend`) keeps appending to -- the M24.4e replacement for
    `read_frame`'s socket-based read, used for the guest->host direction. Returns
    (frame_type, payload, raw_bytes, new_offset); never truncates or rewinds `path`, matching
    `CreateFileBackend`'s own observed append-forever behavior across a `machine Reset` (M24.4's
    own hw_reset_fault_test.py already relies on the identical property for uart0's log).
    Polls plain file reads rather than blocking I/O since there is no way to `select()` on a
    file's own growth on POSIX."""
    deadline = time.monotonic() + timeout
    while True:
        try:
            with open(path, "rb") as f:
                f.seek(offset)
                buf = f.read()
        except FileNotFoundError:
            buf = b""
        if len(buf) >= 4:
            (length,) = struct.unpack_from("<I", buf, 0)
            if length == 0:
                raise ValueError("zero-length frame")
            if len(buf) >= 4 + length:
                raw = buf[: 4 + length]
                frame_type = raw[4]
                payload = raw[5:]
                return frame_type, payload, raw, offset + 4 + length
        if time.monotonic() >= deadline:
            raise TimeoutError(f"no full frame appended to {path} at offset {offset} within {timeout}s (have {len(buf)} bytes)")
        time.sleep(poll_interval)


class RenodeBridge:
    def __init__(self, renode_bin, platform, elf, monitor_port, uart_port, uart0_log=None, uart1_raw_log=None):
        self.renode_bin = renode_bin
        self.platform = platform
        self.elf = elf
        self.monitor_port = monitor_port
        self.uart_port = uart_port
        self.uart0_log = uart0_log
        # M24.4f (third_party/renode/M24_4f_REPORT.md): no longer a read source (that was M24.4e's
        # own file-polling workaround for `CreateServerSocketTerminal`'s TX-drop defect, replaced
        # by the `CharReceived` hook below). When set, ONLY used as an independent, second capture
        # attached BESIDE the hook for `verify_against_file_backend()`'s own post-run proof.
        self.uart1_raw_log = uart1_raw_log
        self.proc = None
        self.mon = None
        # M24.4f (third_party/renode/M24_4f_REPORT.md): the guest->host read source. `hook_srv` is
        # a plain TCP server socket THIS bridge binds and listens on before Renode's `include` even
        # runs; the Renode-side Python hook (`uart_tx_bridge_hook.py`, attached to uart1's own
        # `CharReceived` event) connects to it as a client and forwards every byte the peripheral
        # transmits. `hook_conn` is the accepted connection, read with the same length-prefixed
        # `read_frame` every other socket-based reader in this codebase already uses -- no special
        # framing logic needed once the byte source itself is reliable.
        self.hook_srv = None
        self.hook_conn = None
        # Kept for the M24.4f "prove it" step ONLY (question 157/164: pin a pass condition against
        # a captured real artifact before trusting it): when `uart1_raw_log` is set, the generated
        # `.resc` ALSO attaches an independent `CreateFileBackend` to uart1 BESIDE the hook (the
        # same mechanism M24.4e already proved captures every byte uart1 transmits), and every raw
        # frame this bridge relays via the hook is additionally accumulated here so
        # `verify_against_file_backend()` can compare the two captures byte-for-byte after a run.
        # Not read from during ordinary operation -- the hook socket is the only read source.
        self._hook_captured_bytes = bytearray()
        self.cached_hello = None
        self.cached_bind = None

    def start(self, log_path):
        logf = open(log_path, "wb")
        self.proc = subprocess.Popen(
            [self.renode_bin, "--disable-gui", "--hide-log", "-P", str(self.monitor_port)],
            stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
        )
        t0 = time.monotonic()
        sock = wait_for_port("127.0.0.1", self.monitor_port, t0 + 30.0)
        self.mon = MonitorClient(sock)
        self.mon.sock.settimeout(10.0)
        try:
            self.mon.sock.recv(65536)
        except OSError:
            pass

        # M24.4d root cause (third_party/renode/M24_4d_REPORT.md): this file used to be written to
        # a HARDCODED path shared by every invocation of this script on the host
        # (".../M24_4b/renode_bridge_generated.resc"), regardless of which caller or scratch_dir
        # started it. Directly reproduced as a real, evidenced race (not assumed): two Renode
        # processes started close together in time, both writing that identical shared path, can
        # have one process's `include` genuinely load the OTHER process's script content -- caught
        # live via the monitor's own machine-name prompt reporting the wrong machine after
        # `include` (`M24_4d_REPORT.md`'s "concurrency_probe.py" run). This project's own multi-
        # worker workflow makes two near-simultaneous invocations of this script on one host a
        # real scenario, not a hypothetical one. Fix: derive the `.resc` path from the caller's
        # own `log_path` directory (already a per-run scratch dir -- `drm_attitude_control_renode
        # .rs::spawn_renode_bridge` passes `scratch_dir.join("renode_monitor.log")`), so no two
        # runs ever share a path, and additionally suffix it with this process's own pid as a
        # second, independent guard against any caller that ever reuses one scratch dir across
        # runs.
        resc_dir = os.path.dirname(os.path.abspath(log_path)) or "."
        resc_path = f"{resc_dir}/renode_bridge_generated.{os.getpid()}.resc"
        uart0_line = f"uart0 CreateFileBackend @{self.uart0_log} true" if self.uart0_log else ""
        # M24.4f (third_party/renode/M24_4f_REPORT.md): `uart1_raw_log`, if the caller set one, is
        # ONLY the "prove it beside the hook" cross-check capture -- a second, independent
        # `CreateFileBackend` on uart1 attached alongside the hook, read back and compared against
        # what the hook actually delivered by `verify_against_file_backend()` after a run. It is
        # NOT a read source any more (that was M24.4e's own fix, since replaced).
        uart1_file_line = f"uart1 CreateFileBackend @{self.uart1_raw_log} true" if self.uart1_raw_log else ""
        machine_name = f"av-m24-4b-bridge-{os.getpid()}"

        # M24.4f fix (third_party/renode/M24_4f_REPORT.md "Root cause"/"Fix applied"): this bridge
        # now owns a plain TCP server socket (`self.hook_srv`, bound and LISTENing before `include`
        # even runs) and a Renode-side Python hook (`uart_tx_bridge_hook.py`) is included into the
        # emulation and attached to uart1's own `CharReceived` event -- the same event Renode's own
        # bundled `scripts/monitor.py::mc_uart_connect` uses to mirror guest output, read directly
        # from that file, not guessed. The hook connects OUT to this socket and forwards every byte
        # the peripheral transmits the instant its own C# event fires, with no Renode
        # terminal/backend abstraction (and therefore no buffering-around-WFI dependency, M24.4e's
        # own root cause for `CreateServerSocketTerminal`) anywhere in the path. `CreateServerSocketTerminal`
        # itself is REMOVED from the generated script entirely -- it was never required for
        # `WriteChar` or the RXEN register read (both operate directly on the peripheral object,
        # confirmed by reading the monitor command dispatch: neither takes a terminal/backend
        # argument), only for the now-abandoned socket-based read this task replaces. Smoke-tested
        # first on uart0 against a known-good `CreateFileBackend` capture of a real boot banner
        # (5178/5178 bytes identical, zero drops -- `third_party/renode/M24_4f/
        # smoke_test_hook_uart0.py`) before being wired to uart1 here.
        self.hook_srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.hook_srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.hook_srv.bind(("127.0.0.1", self.uart_port))
        self.hook_srv.listen(1)

        with open(resc_path, "w") as f:
            f.write(f'''# generated by renode_bridge.py (pid {os.getpid()})
:name: {machine_name}
using sysbus
using sysbus.cluster0
using sysbus.cluster1
mach create "{machine_name}"
machine LoadPlatformDescription @{self.platform}
{uart0_line}
{uart1_file_line}
include @{HOOK_SCRIPT}
setup_uart_tx_bridge sysbus.uart1 {self.uart_port}
macro reset
"""
    cluster0 ForEach IsHalted true
    cluster1 ForEach IsHalted true
    rpu0 IsHalted false
    sysbus LoadELF @{self.elf} cpu=rpu0
"""
runMacro $reset
''')
        reply = self.mon.cmd(f"include @{resc_path}", timeout=20.0)
        print("bridge: Renode booted, include reply:", clean(reply)[:200])

        # The hook connects out to `self.hook_srv` from inside the SAME sequential `.resc`
        # execution that used to make `CreateServerSocketTerminal`'s own listener take ~2.4-5.7s
        # to appear (M24.4d's own measured figures, dominated by `machine LoadPlatformDescription`)
        # -- so this accept() needs the same class of generous, contention-aware budget, not
        # because accepting a connection is itself slow, but because the hook's own `Connect` call
        # is downstream of that same slow platform load. A Renode process that genuinely never
        # reaches the `setup_uart_tx_bridge` line still times out here and fails loudly.
        self.hook_srv.settimeout(60.0)
        self.hook_conn, _addr = self.hook_srv.accept()
        self.hook_conn.settimeout(30.0)
        print("bridge: uart1 TX hook connected")

    def wait_for_rxen(self, max_steps=80, step_seconds=0.05):
        for i in range(max_steps):
            self.mon.run_for(step_seconds)
            ctrl = self.mon.read_reg_last_hex(REG_CONTROL)
            if ctrl & CONTROL_RXEN:
                return i
        raise TimeoutError("RXEN never observed within budget")

    def guest_read_frame(self, timeout=30.0):
        # M24.4f fix (third_party/renode/M24_4f_REPORT.md): read from the hook socket, not the
        # M24.4e file-backend workaround it replaces. `self.hook_conn` is an ordinary, already-
        # connected TCP socket the Renode-side `CharReceived` hook writes every transmitted byte to
        # directly -- so the existing length-prefixed `read_frame` (the same reader every other
        # socket-based caller in this codebase already uses) is sufficient; no polling, no
        # re-issuing a monitor command to force a flush, no growing-file offset bookkeeping. Every
        # raw frame is also appended to `self._hook_captured_bytes` for
        # `verify_against_file_backend()`'s own post-run cross-check.
        frame_type, payload, raw = read_frame(self.hook_conn, timeout=timeout)
        self._hook_captured_bytes.extend(raw)
        return frame_type, payload, raw

    def verify_against_file_backend(self):
        """M24.4f 'prove it' step: compares everything the hook has delivered so far
        (`self._hook_captured_bytes`) against the independent `CreateFileBackend` capture attached
        BESIDE the hook in `start()` (`self.uart1_raw_log`) -- the same capture technique M24.4e
        already proved reliable, used here as ground truth for the hook, not the other way around.
        Returns (ok: bool, hook_len: int, file_len: int, first_mismatch_offset: int | None). Forces
        a fresh `CreateFileBackend` attach first (M24.4e's own finding: Renode's file backend must
        be detached/reattached to flush what it has buffered internally before an outside reader
        can see it)."""
        if not self.uart1_raw_log:
            raise RuntimeError("verify_against_file_backend() requires uart1_raw_log to have been set before start()")
        self.mon.cmd(f"uart1 CreateFileBackend @{self.uart1_raw_log} true", timeout=8.0)
        time.sleep(0.2)
        with open(self.uart1_raw_log, "rb") as f:
            file_bytes = f.read()
        hook_bytes = bytes(self._hook_captured_bytes)
        common = min(len(hook_bytes), len(file_bytes))
        for i in range(common):
            if hook_bytes[i] != file_bytes[i]:
                return False, len(hook_bytes), len(file_bytes), i
        if len(hook_bytes) != len(file_bytes):
            return False, len(hook_bytes), len(file_bytes), common
        return True, len(hook_bytes), len(file_bytes), None

    def deliver_to_guest(self, raw_bytes, run_seconds):
        self.mon.write_chars(raw_bytes)
        self.mon.run_for(run_seconds)

    def do_machine_reset_and_rehandshake(self):
        print("bridge: RESET -- issuing a real `machine Reset` (faithful hardware power-cycle)")
        self.mon.cmd("machine Reset", timeout=30.0)
        self.wait_for_rxen()
        assert self.cached_hello is not None, "RESET before any HELLO was ever cached"
        self.deliver_to_guest(self.cached_hello, 0.2)
        _ft, _pl, _raw = self.guest_read_frame(timeout=15.0)  # guest's own HELLO reply, discarded
        if self.cached_bind is not None:
            self.deliver_to_guest(self.cached_bind, 0.5)
            self.guest_read_frame(timeout=15.0)  # guest's own BIND_ACK, discarded
        print("bridge: post-reset re-handshake (and re-bind, if applicable) complete")

    def run(self, shim_sock):
        last_tai_ns = None
        while True:
            frame_type, payload, raw = read_frame(shim_sock, timeout=300.0)
            if frame_type == FRAME_HELLO:
                self.cached_hello = raw
                self.wait_for_rxen()
                self.deliver_to_guest(raw, 0.2)
                _ft, _pl, guest_raw = self.guest_read_frame()
                shim_sock.sendall(guest_raw)
                print("bridge: relayed HELLO both ways")
            elif frame_type == FRAME_BIND:
                self.cached_bind = raw
                req = lockstep_pb2.LockstepBindRequest.FromString(payload)
                last_tai_ns = req.start_tai_ns
                self.deliver_to_guest(raw, 0.5)
                _ft, _pl, guest_raw = self.guest_read_frame()
                shim_sock.sendall(guest_raw)
                print(f"bridge: relayed BIND (start_tai_ns={req.start_tai_ns})")
            elif frame_type == FRAME_STEP:
                req = lockstep_pb2.LockstepStepRequest.FromString(payload)
                delta_s = max(0.0, (req.until_tai_ns - (last_tai_ns or req.until_tai_ns)) / 1e9)
                self.mon.write_chars(raw)
                # M24.4c found that `delta_s` alone (the DRM's own nominal step period) was not
                # enough virtual time for a real first STEP and introduced a floor
                # (`STEP_MIN_VIRTUAL_S`, raised 3.0 -> 10.0 across that task).
                #
                # M24.4e root cause (third_party/renode/M24_4e_REPORT.md): what looked like a
                # guest-side stall (`guest_read_frame` timing out regardless of how much virtual
                # time was granted -- M24.4d tried 10.0s and 30.0s, this task additionally tried a
                # single unchunked `RunFor` and a 3.0s grant, all with the identical outcome) was
                # never a guest stall at all. Proven with a live GDB-remote attach to Renode's own
                # `machine StartGdbServer`: `IO_LOCKSTEP`'s own task had already returned from
                # `handle_step` (its saved stack was found blocked in the *next*
                # `lockstep_read_frame` call, not anywhere inside `handle_step`/`doTransmit`) --
                # meaning `lockstep_write_frame(..., LOCKSTEP_FRAME_STEP_DONE, ...)` had already
                # completed successfully. `uart1`'s own driver-level context
                # (`zynqmp_uart_instances[1]`, read live) showed `transmitting=False,
                # tx_queued=0` -- fully drained, not stuck. The actual defect: a second,
                # independent `CreateFileBackend` attached to `uart1` in the same run captured the
                # STEP_DONE frame byte-for-byte (frame_type 0x05, sequence=1, ...) that
                # `CreateServerSocketTerminal`'s own socket forwarding never delivered -- proof the
                # guest transmitted correctly and Renode's own socket-terminal forwarding is what
                # silently dropped it (this run's own `netstat` also showed zero bytes ever queued
                # on either end of that TCP connection at the OS level, ruling out a bridge-side
                # read bug). M24.4e's own fix (`self.guest_read_frame` reading from an always-
                # reappended `CreateFileBackend` log via `read_frame_from_growing_file`, now dead
                # code kept only for its own report's sake) got one real STEP further but was not
                # reliable for STEP 2+; M24.4f (`third_party/renode/M24_4f_REPORT.md`) replaces
                # that read path entirely with the `CharReceived` hook -- see `guest_read_frame`.
                #
                # The chunked `RunFor` + `rpu0 PC`/`channel_sts` trace below is kept (not removed)
                # per this task's own brief -- it is what let this root cause be found in the first
                # place, and it costs nothing now that `guest_read_frame` itself is fixed: it simply
                # never observes a real stall in a passing run, and remains available to diagnose
                # any future genuine guest-side one.
                STEP_MIN_VIRTUAL_S = 10.0
                granted_s = max(delta_s, STEP_MIN_VIRTUAL_S)
                chunk_s = 0.5
                elapsed_s = 0.0
                pc_trace = []
                while elapsed_s < granted_s - 1e-9:
                    this_chunk = min(chunk_s, granted_s - elapsed_s)
                    self.mon.run_for(this_chunk, timeout=15.0)
                    elapsed_s += this_chunk
                    pc_text = clean(self.mon.cmd("rpu0 PC", timeout=5.0))
                    pc_match = HEX_RE.findall(pc_text)
                    sts = None
                    try:
                        sts = hex(self.mon.read_reg_last_hex(REG_CHANNEL_STS, timeout=5.0))
                    except Exception:
                        pass
                    pc_trace.append((round(elapsed_s, 2), pc_match[-1] if pc_match else None, sts))
                last_tai_ns = req.until_tai_ns
                distinct_pcs = sorted({pc for _, pc, _sts in pc_trace if pc is not None})
                if len(distinct_pcs) <= 1:
                    print(f"bridge: STEP idle-CPU trace (informational, not a stall -- guest_read_frame reads from the M24.4f CharReceived hook socket): distinct PC values: {distinct_pcs}")
                _ft, _pl, guest_raw = self.guest_read_frame(timeout=60.0)
                shim_sock.sendall(guest_raw)
            elif frame_type == FRAME_RESET:
                req = lockstep_pb2.LockstepResetRequest.FromString(payload)
                self.do_machine_reset_and_rehandshake()
                last_tai_ns = req.tai_ns
                ack = lockstep_pb2.LockstepResetResponse(sequence=req.sequence)
                shim_sock.sendall(build_frame(FRAME_RESET_ACK, ack.SerializeToString()))
                print(f"bridge: synthesized RESET_ACK(sequence={req.sequence})")
            elif frame_type == FRAME_SHUTDOWN:
                self.deliver_to_guest(raw, 0.2)
                _ft, _pl, guest_raw = self.guest_read_frame()
                shim_sock.sendall(guest_raw)
                print("bridge: relayed SHUTDOWN, exiting relay loop")
                return
            else:
                raise ValueError(f"unexpected frame_type from shim: 0x{frame_type:02x}")

    def stop(self):
        if self.mon is not None:
            try:
                self.mon.send("quit")
            except OSError:
                pass
        if self.proc is not None:
            try:
                self.proc.wait(timeout=15.0)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=15.0)
        if self.hook_conn is not None:
            try:
                self.hook_conn.close()
            except OSError:
                pass
        if self.hook_srv is not None:
            try:
                self.hook_srv.close()
            except OSError:
                pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--shim-socket", required=True)
    ap.add_argument("--elf", default="/Users/probe/code/AltaVista/third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe")
    ap.add_argument("--platform", default="/Users/probe/code/AltaVista/third_party/renode/platforms/cpus/zynqmp.repl")
    ap.add_argument("--renode-bin", default="/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode")
    ap.add_argument("--monitor-port", type=int, default=15200)
    ap.add_argument("--uart-port", type=int, default=15201)
    ap.add_argument("--uart0-log", default=None)
    ap.add_argument("--uart1-raw-log", default=None, help="M24.4f: if set, an independent CreateFileBackend is attached to uart1 BESIDE the CharReceived hook for a post-run byte-for-byte cross-check (verify_against_file_backend); not used as a read source.")
    ap.add_argument("--renode-log", default="/Users/probe/code/AltaVista/third_party/renode/M24_4b/renode_bridge.renode_log.txt")
    args = ap.parse_args()

    bridge = RenodeBridge(args.renode_bin, args.platform, args.elf, args.monitor_port, args.uart_port, args.uart0_log, args.uart1_raw_log)
    bridge.start(args.renode_log)

    print(f"bridge: connecting to shim's Unix socket at {args.shim_socket}")
    shim_sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    deadline = time.monotonic() + 30.0
    while True:
        try:
            shim_sock.connect(args.shim_socket)
            break
        except OSError:
            if time.monotonic() > deadline:
                raise
            time.sleep(0.2)
    print("bridge: connected to shim; entering relay loop")

    try:
        bridge.run(shim_sock)
        # M24.4f "prove it" step: only reached after a clean SHUTDOWN relay, i.e. every frame the
        # guest transmitted this whole run (HELLO_ACK, BIND_ACK, every STEP_DONE, SHUTDOWN_ACK) has
        # already been accumulated into `bridge._hook_captured_bytes` via `guest_read_frame`. If
        # the caller asked for a cross-check file (`--uart1-raw-log`), compare it now, print a
        # clearly-labeled PASS/FAIL line (read by the caller from this process's own stdout log,
        # not inferred from the exit code alone), and exit non-zero on a mismatch so this is a real
        # failure signal, not just a printed note.
        if args.uart1_raw_log:
            ok, hook_len, file_len, mismatch_at = bridge.verify_against_file_backend()
            if ok:
                print(f"bridge: M24.4F_VERIFY PASS -- hook and file-backend captures byte-for-byte identical, {hook_len} bytes")
            else:
                print(f"bridge: M24.4F_VERIFY FAIL -- hook_len={hook_len} file_len={file_len} first_mismatch_offset={mismatch_at}")
                sys.exit(1)
    finally:
        bridge.stop()
        shim_sock.close()


if __name__ == "__main__":
    main()
