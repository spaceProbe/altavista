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

Question 171 (q171-c; STEP 2 stalled because the guest's termios dropped the tail of the 305-byte
frame -- see `RenodeBridge.inject_frame`) added, all optional environment variables:
  AV_BRIDGE_RX_CHUNK_BYTES   max bytes injected before the guest runs (default 64; 0 = the old
                             single burst, which reproduces the stall)
  AV_BRIDGE_GDB_PORT         if set, a STEP whose reply is missing 5 s after its grant starts
                             Renode's GDB stub on that port and holds (`hold_for_debugger`); see
                             `third_party/renode/q171_gdb/` for the gdb attach tooling
  AV_BRIDGE_GDB_HOLD_S       hold duration limit (default 900); AV_BRIDGE_GDB_CLUSTER (default
                             cluster1); AV_BRIDGE_STEP_REPLY_PROBE_S (default 5)
q171-d added AV_BRIDGE_STEP_EARLY_STOP (default 0): 1 stops granting a STEP its virtual time as
soon as the whole reply frame is buffered on the hook socket (`hook_reply_available`).
and an always-on frame log, `<scratch>/frames.jsonl` (one JSON line per frame in either direction,
with its bytes in hex).
"""
import argparse
import json
import os
import re
import select
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


class MonitorDesyncError(RuntimeError):
    """Raised when a Monitor reply cannot be safely attributed to the command that asked for it --
    e.g. bytes were already sitting in the socket, unconsumed, before a new command was even sent
    (proof an earlier command's reply was never fully drained), or the peer closed mid-reply.
    Distinct from the plain `TimeoutError` this module already used for "no reply arrived at all
    within budget" -- a caller (or a human reading a bridge stdout log) can tell "a reply went to
    the wrong place" apart from "nothing came back."

    M24_4g_REPORT.md's "Root cause" section: `MonitorClient` used to decide a reply was complete
    after a fixed 0.3s idle gap. Renode echoes a command's own text back almost instantly but can
    take far longer than 0.3s to actually finish it (`emulation RunFor` blocks for real virtual
    time; M24.4d measured monitor round trips up to 5.1-5.7s under this project's own multi-worker
    contention) -- so the idle gap fired on the echo alone, `cmd()` returned early, and the
    command's *real* completion bytes landed on the socket a little later and were read by
    whichever *next*, unrelated `cmd()` call happened to be waiting -- exactly the recorded
    `wait_for_rxen()` -> `read_reg_last_hex()` crash (a ReadDoubleWord call consuming a RunFor's
    own echo/completion bytes and finding no hex value in them)."""


class MonitorClient:
    """A serialising client for the Renode Monitor TCP protocol -- see `MonitorDesyncError`'s own
    docstring for the bug this replaces. Renode's monitor has no request-id field; the only
    reliable "this command is fully done" signal is its own prompt (`(<machine-name>) `)
    reappearing in the reply stream *after* the text of the command that was just sent. Design:

    1. Every command is tagged (`self._seq`, `[cmd#N]` in error text) purely for diagnostics.
    2. Exactly one outstanding command at a time: `self.lock` still serialises send+read as one
       critical section, and a non-blocking peek immediately before every `send()` checks the
       socket for bytes that this call did not itself request -- if any are found, that is direct
       proof a previous command's reply was not fully drained, and this raises
       `MonitorDesyncError` rather than silently letting the next parse find them.
    3. One command is terminated by a lone `\\n` (`TERMINATOR`), never `\\r\\n`: Renode's monitor
       takes CR and LF each as a line end, so CRLF drew two prompts per command (question 239).
       A reply is accepted only once (a) the accumulated bytes contain this command's own echoed
       text and (b) the known prompt string reappears *after* that echo -- never on a fixed idle
       gap. This is what actually prevents the desync (item 2's stray-byte check is a loud
       backstop, not the primary fix): `run_for()` no longer returns until Renode's own reply is
       genuinely finished, so there is nothing left over for the next command to misread.
    """

    def __init__(self, sock, prompt=None):
        self.sock = sock
        self.lock = threading.Lock()
        self._seq = 0
        # The exact, literal prompt text for this bridge's own Renode machine (e.g.
        # "(av-m24-4b-bridge-54022)"), known deterministically from the machine name this bridge
        # itself picks -- set via `set_prompt()` before the first `cmd()` once that name is known
        # (RenodeBridge.start() does this immediately after constructing this object). A literal
        # substring, not a shaped regex, so a parenthesised token inside some command's own output
        # can never be mistaken for the monitor's own prompt.
        self.prompt = prompt
        # Bytes the post-reply `_grace_drain` found (question 239): the backstop's own tally.
        self.grace_drain_bytes = 0
        self.grace_drain_unexpected_bytes = 0
        self.grace_drain_events = []

    def set_prompt(self, machine_name: str):
        self.prompt = f"({machine_name})"

    # The monitor command terminator: LF only, never CRLF. Renode 1.16.1's monitor treats CR and
    # LF each as a line terminator, so `<cmd>\r\n` is the command line followed by an EMPTY line,
    # and the empty line draws a second prompt (question 239, measured against the real Renode
    # with `monitor_probe.py`: `emulation\r\n` -> 2 prompts, `emulation\n` -> 1, a bare `\r\n` -> 2,
    # five CRLF commands in one send -> 10 prompts, five LF commands -> 5). Under CRLF every reply
    # was followed by a surplus prompt that this client could only sweep up with a timed grace
    # drain, and a surplus prompt arriving after that window was read as a stray by the next
    # command (`MonitorDesyncError`, drm_attitude_control_renode STEP 87 of 100, cmd#1098).
    TERMINATOR = "\n"
    # What follows the `(<name>)` text of one prompt on the wire (the colour reset of its
    # `\x1b[33;1m(<name>) \x1b[0m`); see `_grace_drain`.
    PROMPT_TAIL = b" \x1b[0m"

    def send(self, line: str):
        self.sock.sendall((line + self.TERMINATOR).encode())

    def _peek_stray_bytes(self) -> bytes:
        """Non-blocking check for bytes already queued on the socket, without consuming them.
        Uses `settimeout(0)` (Python's documented equivalent of `setblocking(False)`) only for the
        duration of this one call, then restores whatever timeout was in effect -- so it never
        disturbs any other caller's blocking/timeout expectations."""
        prior = self.sock.gettimeout()
        self.sock.settimeout(0)
        try:
            return self.sock.recv(65536, socket.MSG_PEEK)
        except (BlockingIOError, socket.timeout, OSError):
            return b""
        finally:
            self.sock.settimeout(prior)

    def _check_no_stray_bytes(self, tag):
        stray = self._peek_stray_bytes()
        if stray:
            raise MonitorDesyncError(
                f"[cmd#{tag}] {len(stray)} byte(s) already queued on the monitor socket before "
                f"this command was even sent -- an earlier command's reply was not fully drained "
                f"and would have been misattributed to whatever command asked next: {clean(stray)!r}"
            )

    def _grace_drain(self, idle_gap=0.15, overall_timeout=0.5, what="") -> bytes:
        """The settle read after a reply is complete: a backstop, not the mechanism.

        M24_4g found, live, that a command's echo-plus-prompt is not always the literal last
        thing Renode sends for it, and attributed the trailing bytes (a truncated
        `\\x1b[33;...m` after a `RunFor`'s prompt; the `Command setup_uart_tx_bridge failed,
        returning "0".` line after `include`) to Renode-side buffering. Question 239 re-measured
        that against the real Renode 1.16.1 (`monitor_probe.py`): the monitor treats CR and LF
        each as a line terminator, so the `\\r\\n` this client used to send was a command line plus
        an EMPTY line, and the empty line drew a SECOND PROMPT -- the "trailing ANSI fragment"
        was that second prompt (the stray in the STEP 87 failure, cmd#1098, was exactly its
        remainder, `1m(<name>) \\x1b[0`), arriving later than this drain's window under load. Now
        that commands end in a lone `\\n` (see `TERMINATOR`) one command draws one prompt, and
        what this drain can still legitimately find is only the tail of that same prompt: Renode
        writes `\\x1b[33;1m(<name>) \\x1b[0m` in many tiny segments, so the `(<name>)` text can be
        in hand before its last few bytes (` \\x1b[0m`). `grace_drain_summary()` separates that
        expected tail from anything else; "anything else" must be zero over a run. (The `include`
        line's origin was not re-derived; with LF-only commands `include`'s reply carried it
        and the drain found nothing, in the question 239 probe.)

        Fix (M24_4g, kept): once a command's PRIMARY completion signal (echo + trailing prompt) is confirmed, do
        one short, bounded settle read for anything that follows immediately -- the same shape of
        fix M24.1's own virtual-time spike already used for an analogous problem ("a short (30ms)
        grace timeout only after the marker is seen", `third_party/renode/REPORT.md`). This is
        NOT a reversion to the original idle-gap bug: that bug waited an idle gap for a command's
        *slow, still-executing* primary reply (seconds, e.g. a real `RunFor`); this waits a short,
        fixed grace *after* the primary reply is already confirmed complete, purely to catch an
        immediately-trailing straggler a few ms behind it.

        Deliberately NOT `_drain_idle` (that helper retries for its *entire* `overall_timeout` when
        nothing ever arrives -- correct for the one-time startup banner, where Renode's timing to
        even start printing is unknown, but wrong here: this runs after EVERY command, so the
        overwhelmingly common case is "nothing follows," and that case must be cheap, not cost a
        full 0.5s every single time). This bails out after exactly one `idle_gap` wait when nothing
        is pending at all; `overall_timeout` only bounds the (rare) case where something keeps
        trickling in one `idle_gap`-spaced piece at a time."""
        chunks = []
        deadline = time.monotonic() + overall_timeout
        while time.monotonic() < deadline:
            self.sock.settimeout(max(0.01, min(idle_gap, deadline - time.monotonic())))
            try:
                data = self.sock.recv(65536)
            except (socket.timeout, OSError):
                break  # nothing more within one idle_gap -- the common case, done cheaply
            if not data:
                break
            chunks.append(data)
        drained = b"".join(chunks)
        if drained:
            # Counted, not just swallowed (question 239). Renode writes its prompt in many tiny
            # TCP segments (`\x1b[33;1m(<name>) \x1b[0m`), so a reader that has just seen the
            # `(<name>)` text may not yet have the prompt's own trailing ` \x1b[0m`: those bytes
            # are the rest of the SAME prompt, expected, and counted as `grace_drain_bytes` only.
            # Anything else the drain finds (a second prompt, other text) is `unexpected` and
            # must be zero over a run now that one command draws exactly one prompt.
            self.grace_drain_bytes += len(drained)
            unexpected = 0 if self.PROMPT_TAIL.endswith(drained) else len(drained)
            self.grace_drain_unexpected_bytes += unexpected
            self.grace_drain_events.append((what, len(drained), unexpected, clean(drained)[:80]))
        return drained

    def grace_drain_summary(self) -> str:
        unexpected = [e for e in self.grace_drain_events if e[2]]
        return (f"monitor grace-drain bytes: total={self.grace_drain_bytes} (rest of the same "
                f"prompt's colour reset) unexpected={self.grace_drain_unexpected_bytes} "
                f"in {len(self.grace_drain_events)} drain(s)"
                f"{'' if not unexpected else ': ' + repr(unexpected[:5])}")

    def _read_reply_for(self, line: str, tag: int, timeout: float) -> bytes:
        assert self.prompt, "MonitorClient.set_prompt() must be called before any cmd()"
        deadline = time.monotonic() + timeout
        buf = b""
        while True:
            text = clean(buf)
            if line in text:
                after_echo = text[text.index(line) + len(line):]
                if self.prompt in after_echo:
                    return buf + self._grace_drain(what=f"cmd#{tag} {line[:40]}")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(
                    f"[cmd#{tag}] {line!r}: timed out after {timeout}s waiting for its own reply "
                    f"(prompt {self.prompt!r} not yet seen after the echo; have {len(buf)} bytes: {text!r})"
                )
            self.sock.settimeout(max(0.05, min(0.5, remaining)))
            try:
                data = self.sock.recv(65536)
            except socket.timeout:
                continue
            if not data:
                raise MonitorDesyncError(
                    f"[cmd#{tag}] {line!r}: peer closed the monitor socket mid-reply (have {len(buf)} bytes: {text!r})"
                )
            buf += data

    def cmd(self, line, timeout=30.0):
        with self.lock:
            self._seq += 1
            tag = self._seq
            self._check_no_stray_bytes(tag)
            self.send(line)
            return self._read_reply_for(line, tag, timeout)

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
        drains all of their replies before returning. Cuts wall time roughly N-fold for an N-byte
        frame -- STEP frames carrying real sensor packets are tens to well over a hundred bytes,
        and a synchronous per-byte round trip (each paying at least a full reply wait) made a real
        DRM run impractically slow.

        M24_4g fix: draining used to stop after a fixed 0.2s idle gap (the same class of bug as
        `cmd()`'s own, see `MonitorDesyncError`'s docstring) -- now it stops only once exactly as
        many completion prompts have been observed as commands were sent, counted by literal
        occurrences of the known prompt string, which cannot be fooled by a byte value that
        happens to repeat within the chunk (a substring match on the last line's own echo could
        be). The count is exact only because each command line ends in a lone `\\n` (question 239:
        with `\\r\\n` every command drew two prompts, 2N arrived for N sent, and the batch was
        declared drained about halfway through, the rest being left to the grace drain). The whole chunk is sent and drained while still holding `self.lock`, so from any
        other caller's perspective there is still only ever one outstanding request *unit* at a
        time -- this batch never leaves any of its own N replies undrained for an unrelated later
        command to misattribute, the same invariant `cmd()` enforces for a single command."""
        for start in range(0, len(data), chunk_size):
            chunk = data[start:start + chunk_size]
            with self.lock:
                self._seq += 1
                tag = self._seq
                self._check_no_stray_bytes(tag)
                lines = "".join(f"sysbus.uart1 WriteChar {b}{self.TERMINATOR}" for b in chunk)
                self.sock.sendall(lines.encode())
                self._read_n_replies(len(chunk), tag, timeout=max(8.0, 0.2 * len(chunk)))

    def _read_n_replies(self, expected_count: int, tag: int, timeout: float):
        assert self.prompt, "MonitorClient.set_prompt() must be called before any cmd()"
        deadline = time.monotonic() + timeout
        buf = b""
        while True:
            text = clean(buf)
            if text.count(self.prompt) >= expected_count:
                return buf + self._grace_drain(what=f"cmd#{tag} WriteChar x{expected_count}")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(
                    f"[cmd#{tag}] WriteChar batch of {expected_count}: timed out after {timeout}s "
                    f"(saw {text.count(self.prompt)}/{expected_count} completion prompts; have {len(buf)} bytes)"
                )
            self.sock.settimeout(max(0.05, min(0.5, remaining)))
            try:
                data = self.sock.recv(65536)
            except socket.timeout:
                continue
            if not data:
                raise MonitorDesyncError(
                    f"[cmd#{tag}] WriteChar batch of {expected_count}: peer closed mid-batch "
                    f"(saw {text.count(self.prompt)}/{expected_count} completion prompts)"
                )
            buf += data

    def run_for(self, seconds: float, timeout=60.0):
        self.cmd(f'emulation RunFor "{seconds:.6f}"', timeout=timeout)

    def drain_startup_banner(self, idle_gap=0.3, overall_timeout=10.0):
        """One-time, best-effort drain of whatever Renode prints on connect (its own startup
        banner/default prompt) before any real command is ever sent -- an idle-gap heuristic is
        fine here specifically because no command/reply correlation is at stake yet (nothing has
        been sent), unlike the fixed-idle-gap bug `cmd()` itself no longer has. Not doing this
        thoroughly would leave banner bytes sitting in the socket for `_check_no_stray_bytes` to
        (correctly, but unhelpfully) trip on for the very first real command."""
        self._drain_idle(idle_gap, overall_timeout)

    def _drain_idle(self, idle_gap, overall_timeout) -> bytes:
        chunks = []
        deadline = time.monotonic() + overall_timeout
        got_any = False
        while time.monotonic() < deadline:
            remaining = deadline - time.monotonic()
            self.sock.settimeout(max(0.01, min(idle_gap, remaining)))
            try:
                data = self.sock.recv(65536)
            except socket.timeout:
                if got_any:
                    return b"".join(chunks)
                continue
            except OSError:
                return b"".join(chunks)
            if not data:
                return b"".join(chunks)
            chunks.append(data)
            got_any = True
        return b"".join(chunks)


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
        self.scratch_dir = None
        self.frame_log_path = None

    def log_frame(self, direction, frame_type, raw):
        """Appends one JSON line per relayed frame to `<scratch>/frames.jsonl` (question 171,
        q171-c): `direction` is "shim->guest" (frame as received from the shim, before it is
        injected) or "guest->shim" (frame as received from the guest's uart1), `raw` is the whole
        frame including its 4-byte length field. Always on: a frame log is a few hundred bytes per
        STEP and is the only record of exactly what was injected when a stall is being debugged."""
        if not self.frame_log_path:
            return
        with open(self.frame_log_path, "a") as f:
            f.write(json.dumps({"t": round(time.time(), 3), "dir": direction, "type": frame_type, "len": len(raw), "hex": raw.hex()}) + "\n")

    def hold_for_debugger(self, reason):
        """Env-gated (inert unless `AV_BRIDGE_GDB_PORT` is set; question 171, q171-c; a
        re-introduction of the temporary AV_M24_4E_GDB_PORT hold M24.4e removed). Called when a
        STEP's reply has not arrived after its virtual-time grant: starts Renode's GDB stub
        (`machine StartGdbServer <port> false "cluster1"`) and holds this bridge -- and therefore the emulation,
        which only advances inside `emulation RunFor` -- paused, so an external gdb/lldb/script can
        inspect the stalled guest. Writes `<scratch>/gdb_hold_ready.txt` (port + monitor port),
        then polls (every 0.5 s) for:
          - `<scratch>/hold_cmd`: monitor commands, one per line, run through this bridge's own
            `MonitorClient` (the only monitor connection that is safe to use); each command and its
            reply are appended to `<scratch>/hold_cmd.out`, and the file is deleted when done;
          - `<scratch>/gdb_hold_release`: ends the hold (also ends after `AV_BRIDGE_GDB_HOLD_S`,
            default 900 s). The caller then carries on exactly as it would have without the hold."""
        port = os.environ.get("AV_BRIDGE_GDB_PORT")
        if not port or not self.scratch_dir:
            return
        hold_s = float(os.environ.get("AV_BRIDGE_GDB_HOLD_S", "900"))
        # This platform has CPUs of two architectures (cluster0 = ARMv8-A APUs, cluster1 = the
        # ARMv7-R RPUs the guest runs on), so a bare `StartGdbServer <port>` is refused; name the
        # cluster, and `false` keeps the emulation paused (it is advanced only by `RunFor`).
        cluster = os.environ.get("AV_BRIDGE_GDB_CLUSTER", "cluster1")
        reply = clean(self.mon.cmd(f'machine StartGdbServer {int(port)} false "{cluster}"', timeout=20.0))
        print(f"bridge: GDB HOLD ({reason}); StartGdbServer reply: {reply[:300]!r}", flush=True)
        ready = os.path.join(self.scratch_dir, "gdb_hold_ready.txt")
        with open(ready, "w") as f:
            f.write(f"gdb_port={int(port)}\nmonitor_port={self.monitor_port}\nreason={reason}\n")
        cmd_path = os.path.join(self.scratch_dir, "hold_cmd")
        out_path = os.path.join(self.scratch_dir, "hold_cmd.out")
        release = os.path.join(self.scratch_dir, "gdb_hold_release")
        deadline = time.monotonic() + hold_s
        while time.monotonic() < deadline and not os.path.exists(release):
            if os.path.exists(cmd_path):
                with open(cmd_path) as f:
                    lines = [ln.strip() for ln in f if ln.strip()]
                os.remove(cmd_path)
                with open(out_path, "a") as out:
                    for ln in lines:
                        try:
                            out.write(f"> {ln}\n{clean(self.mon.cmd(ln, timeout=120.0))}\n")
                        except Exception as e:  # keep the hold alive for the debugger
                            out.write(f"> {ln}\nERROR {e!r}\n")
                        out.flush()
                    out.write("--- done ---\n")
            time.sleep(0.5)
        print("bridge: GDB HOLD ended", flush=True)

    def start(self, log_path):
        self.scratch_dir = os.path.dirname(os.path.abspath(log_path)) or "."
        self.frame_log_path = os.path.join(self.scratch_dir, "frames.jsonl")
        logf = open(log_path, "wb")
        self.proc = subprocess.Popen(
            [self.renode_bin, "--disable-gui", "--hide-log", "-P", str(self.monitor_port)],
            stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
        )
        t0 = time.monotonic()
        sock = wait_for_port("127.0.0.1", self.monitor_port, t0 + 30.0)
        self.mon = MonitorClient(sock)
        self.mon.drain_startup_banner()

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
        # M24_4g fix: this bridge picks its own machine name, so the exact prompt string
        # (`MonitorClient.cmd`/`_read_reply_for`'s own completion signal) is known deterministically
        # before the machine even exists -- set it now, before the first real `cmd()` (`include`,
        # just below), not after some later command "discovers" it.
        self.mon.set_prompt(machine_name)

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
        # M24_4g finding: `include` invokes `setup_uart_tx_bridge` (a Python-hook-defined macro)
        # inside the included script, and this build prints that macro's own return-value
        # dispatch as a SEPARATE, deferred line ("Command setup_uart_tx_bridge failed, returning
        # \"0\".") a few ms after the prompt that already signalled `include` itself was done --
        # `cmd()`'s own `_grace_drain()` (see its docstring) sweeps this up as part of `include`'s
        # own reply, so nothing further is needed here.
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
        self.log_frame("guest->shim", frame_type, raw)
        return frame_type, payload, raw

    def hook_reply_available(self):
        """True iff one whole lockstep-local frame is already buffered on the hook socket. Peeks
        (never consumes) and never blocks: `select` with a zero timeout first, because a socket
        with a timeout set would otherwise wait for data inside `recv`."""
        def readable():
            r, _w, _x = select.select([self.hook_conn], [], [], 0)
            return bool(r)
        if not readable():
            return False
        head = self.hook_conn.recv(4, socket.MSG_PEEK)
        if len(head) < 4:
            return False
        (length,) = struct.unpack("<I", head)
        have = self.hook_conn.recv(4 + length, socket.MSG_PEEK)
        return len(have) >= 4 + length

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

    def inject_frame(self, raw_bytes):
        """Host -> guest delivery of one whole frame at UART line rate, not as one instantaneous
        burst. Question 171 ROOT CAUSE (q171-c; evidence in docs/sil-plan.md's q171 status
        and third_party/renode/REPORT.md): `WriteChar`
        puts a byte in the Cadence UART's RX FIFO while emulated time is frozen, and Renode's model
        of that FIFO is unbounded (`Cadence_UART.WriteChar`: `Count < fifoCapacity ||
        !EnableRxOverflow`). Injecting a whole frame before the guest executes one instruction
        therefore hands the RTEMS driver (`zynq_uart_interrupt`: at most 32 bytes per interrupt,
        and the level-triggered interrupt re-fires until the FIFO is empty) the entire frame in one
        interrupt storm during which the reader task is never dispatched, and termios'
        `rtems_termios_enqueue_raw_characters` drops every byte that does not fit its 256-byte raw
        input ring (255 usable; `rawInBufDropped`; cpukit/libcsupport/src/termios.c, `newTail !=
        head`). STEP 1 (17 bytes), HELLO (11) and BIND (154) fit; STEP 2 -- the first STEP carrying
        sensor inputs -- is 305 bytes: the guest kept the first 255, dropped the last 50, and
        `read_all` waited forever for them. A real UART delivers a byte every 10/115200 s and holds
        at most 64, so: inject at most `AV_BRIDGE_RX_CHUNK_BYTES` (default 64, the FIFO depth) at a
        time and let the guest run for that chunk's time on the wire before the next. `0` restores
        the old single burst (reproduces the stall; for A/B measurement only). Virtual time spent
        here is harmless to determinism: the cFE clock is driven by the lockstep tick, not by
        Renode's virtual time."""
        chunk = int(os.environ.get("AV_BRIDGE_RX_CHUNK_BYTES", "64"))
        if chunk <= 0:
            chunk = max(1, len(raw_bytes))
        prev = 0
        for start in range(0, len(raw_bytes), chunk):
            if prev:
                self.mon.run_for(max(0.001, prev * 10 / 115200.0))
            piece = raw_bytes[start:start + chunk]
            self.mon.write_chars(piece)
            prev = len(piece)

    def deliver_to_guest(self, raw_bytes, run_seconds):
        self.inject_frame(raw_bytes)
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
            self.log_frame("shim->guest", frame_type, raw)
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
                self.inject_frame(raw)
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
                # q171-d: AV_BRIDGE_STEP_EARLY_STOP=1 stops granting virtual time as soon as the
                # whole reply frame is already buffered on the hook socket (checked between
                # chunks, without consuming it); unset/0 (the default) grants the full
                # `granted_s` as before. The guest is tick-driven, so the time after its reply
                # is idle; the port-traffic A/B in `drm_attitude_control_renode.rs`'s doc
                # comment shows whether the choice changes what crosses the port boundary.
                early_stop = os.environ.get("AV_BRIDGE_STEP_EARLY_STOP", "0") == "1"
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
                    if early_stop and self.hook_reply_available():
                        break
                print(f"bridge: STEP sequence={req.sequence} granted {elapsed_s:.2f} s of {granted_s:.2f} s virtual time (early stop {'on' if early_stop else 'off'}); {self.mon.grace_drain_summary()}", flush=True)
                last_tai_ns = req.until_tai_ns
                distinct_pcs = sorted({pc for _, pc, _sts in pc_trace if pc is not None})
                if len(distinct_pcs) <= 1:
                    print(f"bridge: STEP idle-CPU trace (informational, not a stall -- guest_read_frame reads from the M24.4f CharReceived hook socket): distinct PC values: {distinct_pcs}")
                if os.environ.get("AV_BRIDGE_GDB_PORT"):
                    # q171-c: probe briefly for the reply, and if it is not there hold for a
                    # debugger (see `hold_for_debugger`), then wait the usual way.
                    try:
                        _ft, _pl, guest_raw = self.guest_read_frame(timeout=float(os.environ.get("AV_BRIDGE_STEP_REPLY_PROBE_S", "5")))
                    except TimeoutError:
                        self.hold_for_debugger(f"STEP sequence={req.sequence} reply not received after a {granted_s} s grant")
                        _ft, _pl, guest_raw = self.guest_read_frame(timeout=60.0)
                else:
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
            print(f"bridge: {self.mon.grace_drain_summary()}", flush=True)
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
