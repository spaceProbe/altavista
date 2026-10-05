"""Question 235 / round-7 decision 11: no raw control byte may live in `web/js/` source.

A raw NUL (as `web/js/layers/layer.js` once used to join layer id and local key) or a raw
`\\x01` (as `web/js/panels/layers_panel.js` once used in its state key) is invisible in editors
and diffs and makes `grep` call the file binary. The fix is always the escape (`\\u0000`,
`\\u0001`) inside the same string literal, which leaves the runtime string unchanged.
`web/js/control_bytes_check.mjs` is the lint; this file runs it under pytest so both the node
phase and the pytest phase of the gate exercise it, and proves the lint can fail.

What each test would catch:

* ``test_real_tree_has_no_raw_control_bytes``: a commit that re-introduces a raw control byte
  into any text source under web/js (the check names it as `file:line:col: 0xNN`), or adds a
  file whose extension the check does not classify.
* ``test_check_refuses_raw_nul_and_raw_soh``: a lint that always exits 0 (or one that only
  looks for NUL, or only the first hit per file): the scratch tree holds one raw NUL and one raw
  `\\x01` in the SAME file plus a clean file, and the run must exit 1 naming both with the exact
  line and column. A tab, LF and CR in the clean file must NOT be reported (allowed bytes).
* ``test_check_does_not_sniff_binary_by_content``: a lint that skips a file because it "looks
  binary": a `.js` file whose only odd content is a NUL must still be reported, while a `.png`
  that is full of control bytes is skipped by extension.
* ``test_check_refuses_unclassified_extension``: a lint that silently ignores a file kind it
  has never heard of.
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "control_bytes_check.mjs"
NODE = shutil.which("node")


def _run(*args: str) -> subprocess.CompletedProcess[str]:
    if NODE is None:
        pytest.skip("node is not installed in this environment; control_bytes_check.mjs needs it")
    return subprocess.run(
        [NODE, str(CHECK), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=60,
        check=False,
    )


def test_real_tree_has_no_raw_control_bytes() -> None:
    proc = _run()
    assert proc.returncode == 0, f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
    assert "control_bytes_check: OK" in proc.stdout
    # The count is the number of text files scanned; a lint that scanned nothing is not a pass.
    scanned = int(proc.stdout.split("OK, ")[1].split(" files")[0])
    assert scanned >= 50, proc.stdout


def test_check_refuses_raw_nul_and_raw_soh(tmp_path: Path) -> None:
    sub = tmp_path / "layers"
    sub.mkdir()
    # Line 2 col 13 is the NUL; line 3 col 13 is the SOH (columns count bytes from the last LF).
    (sub / "bad.js").write_bytes(b"const a = 1;\nconst k = `x\x00y`;\nconst s = `a\x01b`;\n")
    (tmp_path / "clean.mjs").write_bytes(b"\tconst ok = 1;\r\nconst b = 2;\n")
    proc = _run(str(tmp_path))
    assert proc.returncode == 1, f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
    assert "layers/bad.js:2:13: 0x00" in proc.stdout, proc.stdout
    assert "layers/bad.js:3:13: 0x01" in proc.stdout, proc.stdout
    assert "clean.mjs" not in proc.stdout, proc.stdout
    assert "FAILED, 2 problem(s)" in proc.stdout, proc.stdout


def test_check_does_not_sniff_binary_by_content(tmp_path: Path) -> None:
    (tmp_path / "only_nul.js").write_bytes(b"\x00")
    (tmp_path / "image.png").write_bytes(b"\x89PNG\r\n\x1a\n\x00\x01\x02")
    proc = _run(str(tmp_path))
    assert proc.returncode == 1, proc.stdout
    assert "only_nul.js:1:1: 0x00" in proc.stdout, proc.stdout
    assert "image.png" not in proc.stdout, proc.stdout


def test_check_refuses_unclassified_extension(tmp_path: Path) -> None:
    (tmp_path / "mystery.xyz").write_bytes(b"plain text\n")
    proc = _run(str(tmp_path))
    assert proc.returncode == 1, proc.stdout
    assert "mystery.xyz: unclassified file extension '.xyz'" in proc.stdout, proc.stdout
