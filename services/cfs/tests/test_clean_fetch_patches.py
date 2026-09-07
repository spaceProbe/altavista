"""M24.4b (docs/open-questions.md question 148; `third_party/renode/M24_4b_REPORT.md`): "a test
that a clean fetch applies every patch."

This exists precisely because "no patch is carried" was once silently, incorrectly true of
`third_party/fetch-cfs.sh` itself -- M24.4's own patch
(`third_party/renode/M24_4/patches/cfe_psp_start-cf-startup-file.patch`) existed on disk as an
in-place working-tree edit, but `fetch-cfs.sh` did not apply it, so a truly clean environment
(a fresh clone, or any CI runner that fetches `third_party/cfs` from scratch) would have silently
rebuilt the broken, unpatched behaviour. `fetch-cfs.sh` was fixed to apply every patch under
`third_party/renode/M24_4/patches/` (its own "Patches applied" section, near the end of the
script) -- this test is the guard against that specific class of regression recurring: it fetches
into a genuinely separate, throwaway temp directory (never the shared `third_party/cfs`), asserts
every patch under that directory applies with exit code 0 AND (per this task's own "an exit code
is not evidence" rule) that the resulting patched file's SHA-256 matches a hash recorded here from
a real, independently-verified-good patched file -- not merely "the patch command didn't error."

Network use (question 154): `fetch-cfs.sh` clones from github.com, which needs the network -- this
is the SAME one-time, already-documented image-build-time exception question 154 describes (this
test's entire purpose is validating that exact fetch path; skipping it because it uses network
would defeat the point of the test). Gated the same way `test_image_digest.py`'s Docker-dependent
tests are: skip with a clear, printed reason if the network is not reachable, never fail obscurely
or hang.
"""
from __future__ import annotations

import hashlib
import os
import pathlib
import shutil
import socket
import subprocess
import tempfile

import pytest

REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]
FETCH_SCRIPT = REPO_ROOT / "third_party" / "fetch-cfs.sh"
PATCHES_DIR = REPO_ROOT / "third_party" / "renode" / "M24_4" / "patches"

# Recorded expected hash of the ONE file this task's patches touch, computed from a real,
# independently-verified-good patched copy (the same file M24_4_REPORT.md's own "Verification:
# SUCCESS" section built and booted under Renode, and this task's own M24_4b_REPORT.md rebuilt
# again with the additional termios fix -- this hash is of the PRE-termios-fix, POST-/cf-patch
# state fetch-cfs.sh alone produces, since that is the only patch fetch-cfs.sh itself applies;
# the termios fix lives directly in services/cfs/apps/io_lockstep, copied in by
# build-cfs-cross.sh's own step [3/8], not by any third_party/cfs patch -- see
# M24_4b_REPORT.md's "not done" list item 7 for why that fix needs no patch file at all).
EXPECTED_PATCHED_FILE_RELPATH = "psp/fsw/pc-rtems/src/cfe_psp_start.c"
# Computed directly (`shasum -a 256`) against a real clean fetch performed by this same task,
# `CFS_FETCH_DEST` pointed at a throwaway scratch dir, immediately after confirming the patch
# step's own "applying cfe_psp_start-cf-startup-file.patch" / "patching file ..." output and
# before this test existed to check it automatically -- not copied from any prior report's
# number, and not assumed from the patch file's own diff.
EXPECTED_PATCHED_FILE_SHA256 = "7e6d17bd067707308e53ee39c67160f040a8eed2f1bb1b5539d8acfe6ca91b57"

# The exact marker string M24_4_REPORT.md's own fix writes into the guest's /cf filesystem at
# boot -- content evidence the patch's own effect is present, not just that `patch` exited 0.
EXPECTED_MARKER_TEXT = "AltaVista M24.4 wrote a default /cf/cfe_es_startup.scr"


def _network_reachable(host: str = "github.com", port: int = 443, timeout: float = 5.0) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def _sha256_of(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


@pytest.mark.skipif(not FETCH_SCRIPT.is_file(), reason=f"{FETCH_SCRIPT} not found")
@pytest.mark.skipif(not PATCHES_DIR.is_dir(), reason=f"{PATCHES_DIR} not found -- nothing to test")
def test_a_clean_fetch_applies_every_patch_and_the_patched_file_hash_matches():
    patch_files = sorted(PATCHES_DIR.glob("*.patch"))
    assert patch_files, f"expected at least one *.patch file under {PATCHES_DIR}"

    if not _network_reachable():
        pytest.skip("github.com is not reachable -- fetch-cfs.sh needs the network for a clean fetch "
                    "(question 154's own one-time, documented exception; this test's entire purpose "
                    "is validating that exact fetch, so it cannot be skipped in favor of a network-free "
                    "substitute without defeating the point)")

    scratch = pathlib.Path(tempfile.mkdtemp(prefix="av-m24-4b-clean-fetch-"))
    dest = scratch / "cfs"
    try:
        env = dict(os.environ)
        env["CFS_FETCH_DEST"] = str(dest)
        result = subprocess.run(
            ["sh", str(FETCH_SCRIPT)],
            cwd=str(REPO_ROOT),
            env=env,
            capture_output=True,
            text=True,
            timeout=900,
        )
        # Question 148's own rule: "an exit code is not evidence" -- print the full output
        # either way so a failure is diagnosable, but the exit code alone below is only the
        # FIRST of several checks, never the only one.
        print("fetch-cfs.sh stdout:\n" + result.stdout)
        print("fetch-cfs.sh stderr:\n" + result.stderr)
        assert result.returncode == 0, f"fetch-cfs.sh (CFS_FETCH_DEST={dest}) exited {result.returncode}"

        # Content evidence the patch step actually ran and reported success for every patch
        # file, not merely that the overall script exited 0 (which could mask a patch step
        # that was silently skipped, e.g. if PATCHES_DIR were empty or misdetected).
        for p in patch_files:
            assert f"applying {p.name}" in result.stdout, (
                f"fetch-cfs.sh's own stdout must report applying {p.name} -- got:\n{result.stdout}"
            )

        patched_file = dest / EXPECTED_PATCHED_FILE_RELPATH
        assert patched_file.is_file(), f"expected the patched file at {patched_file} after a clean fetch"

        content = patched_file.read_text(errors="replace")
        assert EXPECTED_MARKER_TEXT in content, (
            f"{patched_file} does not contain the expected patch marker {EXPECTED_MARKER_TEXT!r} -- "
            "the patch did not actually take effect even though `patch` may have exited 0"
        )

        actual_hash = _sha256_of(patched_file)
        assert actual_hash == EXPECTED_PATCHED_FILE_SHA256, (
            f"{patched_file} SHA-256 = {actual_hash}, expected {EXPECTED_PATCHED_FILE_SHA256} -- "
            "the patched file's content has changed from what this test recorded as known-good "
            "(either the patch, the pinned upstream commit, or this test's own recorded hash needs "
            "to be reconciled -- never silently update the recorded hash without checking which)"
        )

        # Independent confirmation the patch genuinely changed the file, not merely that its own
        # context happened to already match: fetch a second, untouched copy of the exact same
        # pre-patch commit (skipping the patch-apply step by reading the file directly out of a
        # bare clone at the pinned PSP commit) and diff it against the patched one -- if a byte
        # differs, the patch had a real effect; this does not rely on `patch`'s own idempotency
        # behavior for a purely additive hunk (measured separately, during authoring, to
        # sometimes re-apply "cleanly" a second time by re-inserting its own block rather than
        # failing -- not a property this test should assume either way).
        unpatched_dir = scratch / "cfs_unpatched_psp_only"
        subprocess.run(["git", "clone", "--quiet", "https://github.com/nasa/PSP.git", str(unpatched_dir)], check=True, timeout=300)
        subprocess.run(["git", "-C", str(unpatched_dir), "checkout", "--quiet", "c4b3b0b65b119e106481ad8e20976ae4d7f554e3"], check=True)
        unpatched_file = unpatched_dir / "fsw" / "pc-rtems" / "src" / "cfe_psp_start.c"
        assert unpatched_file.is_file(), f"expected the pinned PSP commit to contain {unpatched_file}"
        assert _sha256_of(unpatched_file) != actual_hash, (
            "the patched file is byte-identical to the pristine, unpatched pinned PSP commit -- "
            "the patch had no real effect"
        )
        assert EXPECTED_MARKER_TEXT not in unpatched_file.read_text(errors="replace"), (
            "the pristine, unpatched PSP source unexpectedly already contains the patch's own "
            "marker text -- this test's premise (the marker proves the patch's effect) would be "
            "invalid"
        )
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
