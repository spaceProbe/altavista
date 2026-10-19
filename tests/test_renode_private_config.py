"""Question 254 defect D1: no Renode launched by this repo's test harness may read or write the
user's global `~/Library/Application Support/renode/{config,history}`.

Renode's AntShell rewrites the WHOLE history file on every monitor command (every injected UART
byte), the file is shared by every Renode of the user and unbounded (9.2 MB by the time
`drm_attitude_control_renode` died of `System.IO.IOException: Input/output error` rewriting it;
10.8 ms per command against it, 0.62 ms against an empty one), and `$HOME`/`XDG_*` do not move it
(the config carries an absolute `history-path`). Both launchers therefore pass `--config
<scratch>/renode_config`, written by `renode_bridge.write_renode_config`. This file pins that:
the helper's output, that both launchers' Renode command lines carry it, and (with a real Renode,
`AV_RENODE_BIN`) that a monitor command lands in the private history and nowhere else.
"""
import os
import re
import subprocess
import sys
import time
import uuid
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[1]
BRIDGE_DIR = REPO / "third_party/renode/M24_4b"
LAUNCHERS = [
    BRIDGE_DIR / "renode_bridge.py",
    REPO / "third_party/renode/hil_standin/renode_realtime_pty.py",
]
sys.path.insert(0, str(BRIDGE_DIR))


def test_helper_pins_history_inside_the_scratch_dir(tmp_path):
    import renode_bridge as rb

    cfg = Path(rb.write_renode_config(tmp_path))
    assert cfg.parent == tmp_path
    assert cfg.read_text() == f"[general]\nhistory-path = {tmp_path / 'renode_history'}\n"


@pytest.mark.parametrize("launcher", LAUNCHERS, ids=lambda p: p.name)
def test_every_renode_command_line_carries_the_private_config(launcher):
    src = launcher.read_text()
    popens = re.findall(r"subprocess\.Popen\(\s*\[[^\]]*--disable-gui[^\n]*(?:\n[^\n]*){0,2}", src)
    assert len(popens) == 1, f"{launcher.name}: expected one Renode Popen, found {len(popens)}"
    assert '"--config"' in popens[0] and "write_renode_config(" in popens[0], popens[0]


@pytest.mark.skipif(not os.environ.get("AV_RENODE_BIN"), reason="needs a real Renode: set AV_RENODE_BIN")
def test_real_renode_writes_only_the_private_history(tmp_path):
    import renode_bridge as rb

    real = Path.home() / "Library/Application Support/renode/history"
    before = real.stat().st_size if real.exists() else None
    port = 21999
    probe = f"d1-private-history-probe-{uuid.uuid4().hex}"
    cfg = rb.write_renode_config(tmp_path)
    proc = subprocess.Popen(
        [os.environ["AV_RENODE_BIN"], "--disable-gui", "--hide-log", "-P", str(port), "--config", cfg],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT,
    )
    try:
        mon = rb.MonitorClient(rb.wait_for_port("127.0.0.1", port, time.monotonic() + 40.0))
        mon.drain_startup_banner()
        mon.set_prompt("monitor")
        mon.cmd(f'echo "{probe}"', timeout=30.0)
    finally:
        proc.terminate()
        proc.wait(15)
    assert f'echo "{probe}"' in (tmp_path / "renode_history").read_text()
    # A shared global file may be appended by another Renode meanwhile; only OUR line must be absent
    # (a per-run token: a fixed one could already be there from an earlier, perturbed run).
    assert real.exists() is (before is not None)
    if before is not None:
        assert probe not in real.read_text(errors="replace")
