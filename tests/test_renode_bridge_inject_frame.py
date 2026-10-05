"""Question 171 (q171-c): `RenodeBridge.inject_frame` delivers a host -> guest frame at UART line
rate instead of as one instantaneous burst.

Why (root cause, measured against the stalled guest with the RTEMS toolchain's own gdb): the guest's
termios raw input ring for `/dev/ttyS1` is 256 bytes (255 usable). `WriteChar` fills Renode's
unbounded RX FIFO while emulated time is frozen, so a whole frame reaches the RTEMS driver as one
interrupt storm during which the reader task is never dispatched, and termios drops every byte past
255 (`rawInBufDropped`). STEP 2 (the first STEP with sensor inputs) is 305 bytes, so the guest kept
255 and `read_all` waited forever for the other 50.

Deterministic, no Renode and no sockets: a recording stand-in for the monitor client.
"""
from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest

BRIDGE_PATH = Path(__file__).resolve().parents[1] / "third_party" / "renode" / "M24_4b" / "renode_bridge.py"

# The recorded STEP 2 frame length from the stalled run (frames.jsonl: len 305).
STEP2_FRAME_BYTES = 305
TERMIOS_RAW_INPUT_USABLE = 255


def _load_bridge_module():
    spec = importlib.util.spec_from_file_location("renode_bridge_inject_under_test", BRIDGE_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class _RecordingMonitor:
    def __init__(self):
        self.events = []  # ("write", n_bytes) or ("run", seconds)

    def write_chars(self, data, chunk_size=64):
        self.events.append(("write", len(data)))

    def run_for(self, seconds, timeout=60.0):
        self.events.append(("run", seconds))


def _bridge_with_recorder():
    mod = _load_bridge_module()
    bridge = mod.RenodeBridge("renode", "plat.repl", "core.elf", 1, 2)
    bridge.mon = _RecordingMonitor()
    return bridge


def test_default_never_injects_more_than_the_fifo_depth_before_the_guest_runs(monkeypatch):
    monkeypatch.delenv("AV_BRIDGE_RX_CHUNK_BYTES", raising=False)
    bridge = _bridge_with_recorder()
    bridge.inject_frame(bytes(range(256)) + bytes(STEP2_FRAME_BYTES - 256))
    writes = [n for kind, n in bridge.mon.events if kind == "write"]
    assert sum(writes) == STEP2_FRAME_BYTES, "every byte is delivered exactly once"
    assert max(writes) <= 64, "no burst larger than the 64-byte hardware RX FIFO"
    # The guest must get to run (service the interrupt, wake the reader) between any two bursts.
    kinds = [kind for kind, _ in bridge.mon.events]
    assert kinds[0] == "write" and kinds[-1] == "write"
    assert all(a != b for a, b in zip(kinds, kinds[1:])), f"bursts must alternate with guest run time: {kinds}"


def test_time_between_bursts_covers_the_previous_bursts_time_on_the_wire(monkeypatch):
    monkeypatch.delenv("AV_BRIDGE_RX_CHUNK_BYTES", raising=False)
    bridge = _bridge_with_recorder()
    bridge.inject_frame(bytes(130))
    events = bridge.mon.events
    assert events == [("write", 64), ("run", pytest.approx(64 * 10 / 115200.0)), ("write", 64), ("run", pytest.approx(64 * 10 / 115200.0)), ("write", 2)]


def test_zero_restores_the_single_burst_that_reproduces_the_stall(monkeypatch):
    monkeypatch.setenv("AV_BRIDGE_RX_CHUNK_BYTES", "0")
    bridge = _bridge_with_recorder()
    bridge.inject_frame(bytes(STEP2_FRAME_BYTES))
    assert bridge.mon.events == [("write", STEP2_FRAME_BYTES)]
    assert STEP2_FRAME_BYTES > TERMIOS_RAW_INPUT_USABLE


def test_a_frame_that_fits_is_one_write_with_no_extra_run(monkeypatch):
    monkeypatch.delenv("AV_BRIDGE_RX_CHUNK_BYTES", raising=False)
    bridge = _bridge_with_recorder()
    bridge.inject_frame(bytes(17))
    assert bridge.mon.events == [("write", 17)]
