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

Network use (question 196(c), round 6 ratification of question 154's original "the fetch script
clones from github.com" design): this test does NOT touch the network. `fetch-cfs.sh` records a
local bare mirror of each of the seven pinned repos under `third_party/mirrors/` (gitignored) the
FIRST time it is ever run on a host -- that one clone is question 154's permitted network window.
Every fetch after that, this test included, clones from the mirror path, never from a URL. If the
mirror does not exist yet (a host that has never run `sh third_party/fetch-cfs.sh`), this test
skips visibly with a typed reason naming the missing path and the exact command that creates it
(question 194's rule: a Docker/fetch-gated test either runs for real or prints a visible SKIPPED
with the reason, never a silent pass). `test_fetch_never_touches_the_network` below goes further
and proves the network-free claim directly, rather than merely asserting by construction.
"""
from __future__ import annotations

import hashlib
import os
import pathlib
import shutil
import subprocess
import tempfile

import pytest

REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]
FETCH_SCRIPT = REPO_ROOT / "third_party" / "fetch-cfs.sh"
PATCHES_DIR = REPO_ROOT / "third_party" / "renode" / "M24_4" / "patches"
# Honors the same CFS_MIRROR_DIR override fetch-cfs.sh honors (read only -- never written; question
# 199), so an isolated clone can point at a host that already has the mirrors instead of skipping.
MIRROR_DIR = pathlib.Path(os.environ.get("CFS_MIRROR_DIR") or (REPO_ROOT / "third_party" / "mirrors"))

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
# number, and not assumed from the patch file's own diff. Re-confirmed byte-for-byte (question
# 196(c)) against the mirror-backed fetch path this file now uses -- same hash, unchanged.
EXPECTED_PATCHED_FILE_SHA256 = "7e6d17bd067707308e53ee39c67160f040a8eed2f1bb1b5539d8acfe6ca91b57"

# The exact marker string M24_4_REPORT.md's own fix writes into the guest's /cf filesystem at
# boot -- content evidence the patch's own effect is present, not just that `patch` exited 0.
EXPECTED_MARKER_TEXT = "AltaVista M24.4 wrote a default /cf/cfe_es_startup.scr"

# Relative to a mirror dir: the PSP pin, used both by the pristine-comparison half of the main
# test and by the mirror-existence check every test in this file shares.
PSP_COMMIT = "c4b3b0b65b119e106481ad8e20976ae4d7f554e3"
PSP_MIRROR = MIRROR_DIR / "psp.git"
# The seven mirrors fetch-cfs.sh records (question 196(c)'s header comment has the full list and
# the commit each is pinned to); used only to build the visible skip reason below.
ALL_MIRROR_NAMES = (
    "bundle",
    "cfe",
    "osal",
    "psp",
    "tools/tblCRCTool",
    "tools/elf2cfetbl",
    "tools/commandline-tools",
)


def _sha256_of(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def _missing_mirrors() -> list[str]:
    """Names (relative to MIRROR_DIR) of any of the seven pinned mirrors that are absent."""
    return [name for name in ALL_MIRROR_NAMES if not (MIRROR_DIR / f"{name}.git").is_dir()]


def _mirror_skip_reason() -> str:
    missing = _missing_mirrors()
    return (
        f"local cFS mirror(s) missing under {MIRROR_DIR}: {', '.join(missing)} -- "
        f"run `sh {FETCH_SCRIPT.relative_to(REPO_ROOT)}` once (question 196(c)'s one-time, "
        "permitted network window) to record them, then re-run this test"
    )


@pytest.mark.skipif(not FETCH_SCRIPT.is_file(), reason=f"{FETCH_SCRIPT} not found")
@pytest.mark.skipif(not PATCHES_DIR.is_dir(), reason=f"{PATCHES_DIR} not found -- nothing to test")
def test_a_clean_fetch_applies_every_patch_and_the_patched_file_hash_matches():
    patch_files = sorted(PATCHES_DIR.glob("*.patch"))
    assert patch_files, f"expected at least one *.patch file under {PATCHES_DIR}"

    if _missing_mirrors():
        pytest.skip(_mirror_skip_reason())

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
            "(either the patch, the pin, or this test's own recorded hash needs to be reconciled "
            "-- never silently update the recorded hash without checking which of the three it is)"
        )

        # Independent confirmation the patch genuinely changed the file, not merely that its own
        # context happened to already match: read a second, untouched copy of the exact same
        # pre-patch commit straight out of the PSP mirror (`git show <commit>:<path>`, no working
        # tree, no network -- the mirror already has this commit, verified by fetch-cfs.sh's own
        # `ensure_mirror` before the fetch above could have used it) and diff it against the
        # patched one -- if a byte differs, the patch had a real effect; this does not rely on
        # `patch`'s own idempotency behavior for a purely additive hunk (measured separately,
        # during authoring, to sometimes re-apply "cleanly" a second time by re-inserting its own
        # block rather than failing -- not a property this test should assume either way).
        pristine = subprocess.run(
            ["git", "-C", str(PSP_MIRROR), "show", f"{PSP_COMMIT}:fsw/pc-rtems/src/cfe_psp_start.c"],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert pristine.returncode == 0, (
            f"git show against the PSP mirror {PSP_MIRROR} failed for {PSP_COMMIT}:"
            f"fsw/pc-rtems/src/cfe_psp_start.c -- stderr:\n{pristine.stderr}"
        )
        pristine_content = pristine.stdout
        pristine_hash = hashlib.sha256(pristine_content.encode("utf-8", errors="replace")).hexdigest()
        assert pristine_hash != actual_hash, (
            "the patched file is byte-identical to the pristine, unpatched pinned PSP commit -- "
            "the patch had no real effect"
        )
        assert EXPECTED_MARKER_TEXT not in pristine_content, (
            "the pristine, unpatched PSP source unexpectedly already contains the patch's own "
            "marker text -- this test's premise (the marker proves the patch's effect) would be "
            "invalid"
        )
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


@pytest.mark.skipif(not FETCH_SCRIPT.is_file(), reason=f"{FETCH_SCRIPT} not found")
def test_fetch_never_touches_the_network():
    """Proves the mirror-backed fetch path (question 196(c)) genuinely cannot reach the network,
    rather than merely happening not to. Two independent mechanisms are stacked so a git version
    or transport quirk in one does not silently defeat the assertion:

      1. GIT_ALLOW_PROTOCOL=file -- git itself refuses to open ANY transport other than local
         "file://"/plain-path access (no http, https, ssh, git://) for every git invocation this
         subprocess and its children make. If fetch-cfs.sh (or a future edit to it) tried to
         `git clone https://github.com/...` directly instead of from the mirror, git would abort
         that clone with a protocol-not-allowed error and the whole script would exit non-zero.

      2. http_proxy/https_proxy pointed at a closed local port (127.0.0.1:1) -- an independent
         belt: even a network call that somehow bypassed git's own protocol allowlist (e.g. a
         plain `curl`/`wget` a future edit might add) would be forced through a proxy that
         actively refuses the connection, rather than silently succeeding against the real
         network.

    Both are passed via `env=` to `subprocess.run`, never via `os.environ[...]`/`monkeypatch` on
    the real process environment (this repo's standing rule: no test may mutate the process
    environment -- question 199).

    The run is pointed at a throwaway CFS_FETCH_DEST and asserted to SUCCEED (exit 0) and produce
    the same patched file with the same recorded hash as the normal path -- proving the mirror
    alone is sufficient, not merely that a network-less run fails safely.
    """
    if _missing_mirrors():
        pytest.skip(_mirror_skip_reason())

    scratch = pathlib.Path(tempfile.mkdtemp(prefix="av-m24-4b-no-network-fetch-"))
    dest = scratch / "cfs"
    try:
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin:/usr/local/bin"),
            "HOME": os.environ.get("HOME", ""),
            "CFS_FETCH_DEST": str(dest),
            "CFS_MIRROR_DIR": str(MIRROR_DIR),
            # Mechanism 1: git refuses any non-file transport outright.
            "GIT_ALLOW_PROTOCOL": "file",
            # Mechanism 2: an independent belt -- anything that somehow tried a real network call
            # over http(s) is routed at a closed local port instead of succeeding.
            "http_proxy": "http://127.0.0.1:1/",
            "https_proxy": "http://127.0.0.1:1/",
            "HTTP_PROXY": "http://127.0.0.1:1/",
            "HTTPS_PROXY": "http://127.0.0.1:1/",
        }
        result = subprocess.run(
            ["sh", str(FETCH_SCRIPT)],
            cwd=str(REPO_ROOT),
            env=env,
            capture_output=True,
            text=True,
            timeout=120,
        )
        print("fetch-cfs.sh (network-blocked) stdout:\n" + result.stdout)
        print("fetch-cfs.sh (network-blocked) stderr:\n" + result.stderr)
        assert result.returncode == 0, (
            f"fetch-cfs.sh (CFS_FETCH_DEST={dest}) with GIT_ALLOW_PROTOCOL=file and a dead proxy "
            f"exited {result.returncode} -- the mirror-backed fetch path must succeed with no "
            f"network reachable at all; stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )

        patched_file = dest / EXPECTED_PATCHED_FILE_RELPATH
        assert patched_file.is_file(), (
            f"expected the patched file at {patched_file} from a network-blocked fetch"
        )
        actual_hash = _sha256_of(patched_file)
        assert actual_hash == EXPECTED_PATCHED_FILE_SHA256, (
            f"{patched_file} SHA-256 = {actual_hash}, expected {EXPECTED_PATCHED_FILE_SHA256} -- "
            "the network-blocked, mirror-backed fetch produced different content than the normal "
            "fetch path"
        )
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
