"""M24.4g (docs/open-questions.md questions 145, 153, 156, 157, 164;
third_party/renode/M24_4g_REPORT.md) -- replays the exact response-desync interleaving that broke
`cargo test -p av-kernel --test drm_attitude_control_renode`:

    RuntimeError: no hex value found reading 0xff010000:
      '(av-m24-4b-bridge-54022) \\n\\r(av-m24-4b-bridge-54022) emulation RunFor "0.050000"'

Root cause (third_party/renode/M24_4g_REPORT.md's own "Root cause" section, not re-derived here):
`renode_bridge.py`'s `MonitorClient` used to decide a Monitor reply was complete after a fixed
0.3s idle gap. Renode echoes a command's own text back almost instantly but can take far longer
than 0.3s to actually finish it (`emulation RunFor` genuinely blocks for that much virtual time;
M24.4d measured monitor round trips stretching to 5.1-5.7s under this project's own multi-worker
contention) -- so the idle gap fired on the echo alone, `cmd()` returned early, and the command's
*real* completion bytes landed on the socket a little later and were read by whichever *next*,
unrelated `cmd()` call happened to be reading -- exactly `wait_for_rxen()`'s own
`run_for()` -> `read_reg_last_hex()` call pair.

Two things are proven here, deterministically, with no real Renode process and no real sockets
(a scripted fake socket instead) -- no sleeping, no timing race to get unlucky on:

1. `test_desync_reproduces_against_frozen_unfixed_snapshot` -- a frozen copy of the ORIGINAL,
   unpatched `MonitorClient` (`third_party/renode/M24_4g/renode_bridge_UNFIXED_snapshot.py`, saved
   before this task's fix was applied) really does raise the exact recorded `RuntimeError`,
   including the literal captured string, when driven through this interleaving. This is the
   proof the task brief asked for: run against the unfixed code, and it fails exactly as recorded.
2. `test_fixed_monitor_client_returns_the_real_register_value` -- the SAME class of interleaving
   (a slow-to-complete `RunFor`, with its own real completion arriving only after the echo), driven
   through the CURRENT, fixed `MonitorClient` in `third_party/renode/M24_4b/renode_bridge.py`, does
   not misattribute anything: `read_reg_last_hex` returns the real register value that was
   scripted, not a corrupted read of RunFor's own echo.
"""
from __future__ import annotations

import importlib.util
import socket
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
FIXED_BRIDGE_PATH = REPO_ROOT / "third_party" / "renode" / "M24_4b" / "renode_bridge.py"
UNFIXED_SNAPSHOT_PATH = REPO_ROOT / "third_party" / "renode" / "M24_4g" / "renode_bridge_UNFIXED_snapshot.py"

# The literal, exact bytes captured from the real crash (M24_4g_REPORT.md's own "the exact bug"
# section) -- used verbatim, not paraphrased, so this replays the real interleaving, not an
# analogous one.
CAPTURED_LEFTOVER_TEXT = '(av-m24-4b-bridge-54022) \n\r(av-m24-4b-bridge-54022) emulation RunFor "0.050000"'
MACHINE_NAME = "av-m24-4b-bridge-54022"
PROMPT = f"({MACHINE_NAME})"
REG_CONTROL = 0xFF010000
CONTROL_RXEN_VALUE = 0x00000004  # CONTROL_RXEN = 1 << 2, in renode_bridge.py


class _Timeout:
    """Sentinel placed inside a `ScriptedSocket` chunk list: the next `recv()` call raises
    `socket.timeout()` instead of returning data, modelling "nothing arrives yet" -- needed to
    reproduce the UNFIXED client's idle-gap-based early return, which depends on a real gap with
    no data, not merely on data being available later in the script."""


TIMEOUT = _Timeout()


def _load_module(name: str, path: Path):
    if not path.is_file():
        pytest.fail(f"expected {path} to exist -- it is this test's own fixture, not optional")
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ScriptedSocket:
    """A fake socket standing in for the live TCP connection to Renode's Monitor.

    `reply_batches` is a list of *batches*, one per expected `sendall()` call: batch N is only
    unlocked (its chunks become visible to a blocking `recv()`, one chunk per call) once the N-th
    `sendall()` has actually happened. This matters for fidelity: in the real protocol the peer
    only sends a reply after receiving the request that provoked it, so nothing from a LATER
    command's reply can leak into an EARLIER command's read (including its post-completion grace
    drain) just because it happens to be next in a flat list -- exactly the property that made an
    earlier, flatter version of this fake wrongly let `run_for()`'s own post-completion grace
    drain "see" the NEXT command's reply before that command had even been sent.

    A batch may contain the `TIMEOUT` sentinel: the next `recv()` call raises `socket.timeout()`
    instead of returning data, modelling "nothing arrives yet" for that step.

    A non-blocking `MSG_PEEK` (what the M24.4g fix's stray-byte check uses) only ever reports what
    is ALREADY pending from a prior blocking `recv()`; it never unlocks a new batch and never pulls
    a new chunk out of the current one -- matching a real non-blocking `recv()`, which raises
    rather than returning data (or b"") when nothing is actually queued yet.
    """

    def __init__(self, reply_batches):
        self._pending = bytearray()
        self._reply_batches = list(reply_batches)
        self._current_batch = []
        self.sent = []
        self._timeout = None

    def settimeout(self, t):
        self._timeout = t

    def gettimeout(self):
        return self._timeout

    def sendall(self, data):
        self.sent.append(data)
        self._current_batch = list(self._reply_batches.pop(0)) if self._reply_batches else []

    def recv(self, bufsize, flags=0):
        peek = bool(flags & socket.MSG_PEEK)
        if not self._pending:
            if peek:
                # Nothing genuinely queued right now -- a real non-blocking socket raises rather
                # than returning b"" (which would mean "peer closed"), and MonitorClient's own
                # `_peek_stray_bytes` treats that exception as "no stray bytes," so mirror it here.
                raise BlockingIOError("no data queued (fake non-blocking peek)")
            if self._current_batch:
                nxt = self._current_batch.pop(0)
                if nxt is TIMEOUT:
                    raise socket.timeout()
                self._pending.extend(nxt)
            else:
                raise socket.timeout()
        data = bytes(self._pending[:bufsize])
        if not peek:
            del self._pending[: len(data)]
        return data


# ---------------------------------------------------------------------------------------------
# 1. Reproduction against the frozen, unfixed snapshot -- proves the bug is real and this test
#    replays it faithfully, including the literal captured string.
# ---------------------------------------------------------------------------------------------

def test_desync_reproduces_against_frozen_unfixed_snapshot():
    unfixed = _load_module("renode_bridge_m24_4g_unfixed_snapshot", UNFIXED_SNAPSHOT_PATH)

    # run_for()'s own command gets ONLY its echo, then an idle gap (the TIMEOUT sentinel) -- the
    # unfixed `read_until_idle` gives up right there (idle_gap=0.3s of silence, chunks already
    # non-empty), exactly reproducing "RunFor's own real completion has not arrived yet when the
    # idle gap fires." run_for() never inspects its own return value (renode_bridge.py:run_for),
    # so what it actually read back does not matter. What matters is the SECOND real chunk: the
    # literal bytes captured from the real crash, delivered as whatever is sitting in the socket
    # when read_reg_last_hex's own read starts next -- exactly what a RunFor reply arriving late
    # looks like from the next, unrelated command's point of view.
    sock = ScriptedSocket([
        [f'{PROMPT} emulation RunFor "0.050000"'.encode(), TIMEOUT],  # batch for run_for()'s sendall
        [CAPTURED_LEFTOVER_TEXT.encode()],  # batch for read_reg_last_hex()'s sendall
    ])
    mon = unfixed.MonitorClient(sock)

    mon.run_for(0.05)  # completes normally against chunk 1; return value unused, matches production

    with pytest.raises(RuntimeError) as exc_info:
        mon.read_reg_last_hex(REG_CONTROL)

    # Not just "raises RuntimeError" -- the EXACT recorded message, literal string included, so
    # this is provably the same bug, not merely a similarly-shaped one.
    assert str(exc_info.value) == f"no hex value found reading {hex(REG_CONTROL)}: {CAPTURED_LEFTOVER_TEXT!r}"


# ---------------------------------------------------------------------------------------------
# 2. The fix: the same class of interleaving (echo arrives promptly, the command's real
#    completion arrives only later) driven through the CURRENT, fixed MonitorClient must not
#    misattribute anything -- read_reg_last_hex must return the real, scripted register value.
# ---------------------------------------------------------------------------------------------

def test_fixed_monitor_client_returns_the_real_register_value():
    fixed = _load_module("renode_bridge_m24_4g_fixed", FIXED_BRIDGE_PATH)

    # Two batches, one per sendall() (run_for()'s, then read_reg_last_hex()'s):
    #   A) RunFor's own echo only -- no completion prompt yet (Renode is still "executing").
    #   B) RunFor's real completion, arriving only on a LATER recv() -- this is exactly the
    #      content that, under the unfixed client, leaks into the NEXT command's read instead.
    #      (batch 1 ends there -- nothing more is unlocked until the NEXT sendall(), so a
    #      post-completion grace drain correctly finds nothing more for THIS command either.)
    #   C) ReadDoubleWord's own real reply, with the real register value -- only unlocked once
    #      read_reg_last_hex() actually sends its own command.
    sock = ScriptedSocket([
        [f'{PROMPT} emulation RunFor "0.050000"'.encode(), f'\r\n{PROMPT} '.encode()],
        [f'sysbus ReadDoubleWord {hex(REG_CONTROL)}\r\n0x{CONTROL_RXEN_VALUE:08x}\r\n{PROMPT} '.encode()],
    ])
    mon = fixed.MonitorClient(sock)
    mon.set_prompt(MACHINE_NAME)

    mon.run_for(0.05)  # must fully drain chunks A+B before returning -- proven by what follows
    value = mon.read_reg_last_hex(REG_CONTROL)

    # The real register value, not a mis-parse of RunFor's own echo/completion text (which
    # contains no "0x????????" pattern at all -- see test 1 above).
    assert value == CONTROL_RXEN_VALUE


def test_fixed_monitor_client_raises_typed_desync_error_on_stray_bytes():
    """Belt-and-suspenders check (M24_4g_REPORT.md item 3 of the fix): if bytes are somehow
    already sitting in the socket when a new command is about to be sent -- the exact situation
    that, in the unfixed client, gets silently misattributed to whatever command asks next -- the
    fixed client must refuse to proceed with a typed error, never silently parse them as if they
    were its own reply. Modelled directly (not derived from waiting on a slow RunFor) by handing
    `MonitorClient` a socket that already has bytes queued before any command is sent at all."""
    fixed = _load_module("renode_bridge_m24_4g_fixed_stray", FIXED_BRIDGE_PATH)

    sock = ScriptedSocket([])
    # Pre-seed `_pending` directly so the very first stray-byte peek (before ANY command is sent)
    # already finds it queued -- bypassing the "only reveal a batch after its own sendall()"
    # modelling convenience above, which does not apply to bytes that arrived before this client
    # even started (e.g. an earlier `cmd()` call elsewhere that itself failed to fully drain).
    sock._pending.extend(CAPTURED_LEFTOVER_TEXT.encode())

    mon = fixed.MonitorClient(sock)
    mon.set_prompt(MACHINE_NAME)

    with pytest.raises(fixed.MonitorDesyncError):
        mon.read_reg_last_hex(REG_CONTROL)
