"""M23.2 (docs/open-questions.md question 153): proves the cFS-side "lockstep-local v1" frame
implementation (`services/cfs/apps/io_lockstep/fsw/src/lockstep_local_framing.c`,
`lockstep_local_io.c`) against `services/cfs/README.md`'s own byte-exact worked example (the
SHUTDOWN frame carrying `LockstepShutdownRequest { run_id: "run-1" }`) and, separately, against
a real `socketpair(AF_UNIX, ...)` round trip and the documented "reject a hostile length field
before allocating" rule.

This C implementation is independent of `crates/av-lockstep-shim`'s (a from-scratch second
implementation of the same spec, not a port of the first -- see lockstep_local_framing.h's own
module doc comment), so agreement here is agreement with the *specification*
(services/cfs/README.md), not merely with itself.
"""
from __future__ import annotations

import subprocess

from _cbuild import compile_cached, run_compiled
from pathlib import Path

CFS_DIR = Path(__file__).resolve().parent.parent
INC = CFS_DIR / "apps" / "io_lockstep" / "fsw" / "inc"
FRAMING_SRC = CFS_DIR / "apps" / "io_lockstep" / "fsw" / "src" / "lockstep_local_framing.c"
IO_SRC = CFS_DIR / "apps" / "io_lockstep" / "fsw" / "src" / "lockstep_local_io.c"

# services/cfs/README.md's own worked example, verbatim.
SHUTDOWN_WORKED_EXAMPLE_HEX = "08000000080a0572756e2d31"


def _compile(tmp_path: Path, name: str, c_src: str) -> Path:
    """Question 172: cached by source hash and pre-warmed untimed. See _cbuild.py."""
    return compile_cached(name, c_src, [FRAMING_SRC, IO_SRC], INC)


def test_shutdown_frame_matches_readme_worked_example(tmp_path: Path) -> None:
    """Writes a SHUTDOWN frame with the exact payload the README derives by hand
    (`LockstepShutdownRequest { run_id: "run-1" }`'s protobuf encoding, 0A 05 72 75 6E 2D 31)
    into a pipe and checks the raw bytes match the README's own 12-byte worked example exactly.
    Fails against a wrong length computation (e.g. counting `length` inclusive of itself, or
    forgetting the frame_type byte in the count) or a big-endian length field."""
    c_src = r"""
    #include <stdio.h>
    #include <unistd.h>
    #include "lockstep_local_framing.h"
    #include "lockstep_local_io.h"
    int main(void) {
        int fds[2];
        if (pipe(fds) != 0) { perror("pipe"); return 1; }
        static const uint8_t payload[] = { 0x0A, 0x05, 0x72, 0x75, 0x6E, 0x2D, 0x31 };
        lockstep_io_status_t st = lockstep_write_frame(fds[1], LOCKSTEP_FRAME_SHUTDOWN, payload, sizeof(payload));
        if (st != LOCKSTEP_IO_OK) { fprintf(stderr, "write_frame failed: %d\n", (int)st); return 1; }
        close(fds[1]);
        uint8_t buf[64];
        ssize_t n = read(fds[0], buf, sizeof(buf));
        if (n != 12) { fprintf(stderr, "expected 12 bytes on the wire, got %zd\n", n); return 1; }
        for (ssize_t i = 0; i < n; ++i) printf("%02x", buf[i]);
        printf("\n");
        return 0;
    }
    """
    binary_path = _compile(tmp_path, "shutdown_example", c_src)
    result = run_compiled(binary_path)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == SHUTDOWN_WORKED_EXAMPLE_HEX


def test_socketpair_round_trip(tmp_path: Path) -> None:
    """A real AF_UNIX socketpair (the same kernel object the real deployment's Unix domain
    socket is, minus the filesystem path) round-trips a HELLO frame and a larger frame with a
    non-trivial payload byte-for-byte."""
    c_src = r"""
    #include <stdio.h>
    #include <string.h>
    #include <sys/socket.h>
    #include "lockstep_local_framing.h"
    #include "lockstep_local_io.h"
    int main(void) {
        int fds[2];
        if (socketpair(AF_UNIX, SOCK_STREAM, 0, fds) != 0) { perror("socketpair"); return 1; }

        uint8_t hello[LOCKSTEP_HELLO_PAYLOAD_LEN];
        lockstep_encode_hello(LOCKSTEP_PROTOCOL_VERSION, hello);
        if (lockstep_write_frame(fds[0], LOCKSTEP_FRAME_HELLO, hello, sizeof(hello)) != LOCKSTEP_IO_OK) { fprintf(stderr, "write hello failed\n"); return 1; }

        uint8_t frame_type;
        uint8_t payload_buf[512];
        size_t payload_len;
        if (lockstep_read_frame(fds[1], &frame_type, payload_buf, sizeof(payload_buf), &payload_len) != LOCKSTEP_IO_OK) { fprintf(stderr, "read hello failed\n"); return 1; }
        if (frame_type != LOCKSTEP_FRAME_HELLO || payload_len != LOCKSTEP_HELLO_PAYLOAD_LEN || memcmp(payload_buf, hello, payload_len) != 0) {
            fprintf(stderr, "hello frame round trip mismatch\n"); return 1;
        }
        char magic[4]; uint16_t version;
        if (lockstep_decode_hello(payload_buf, payload_len, magic, &version) != LOCKSTEP_FRAMING_OK) { fprintf(stderr, "decode_hello failed\n"); return 1; }
        if (memcmp(magic, "AVL1", 4) != 0 || version != 1) { fprintf(stderr, "decoded hello wrong\n"); return 1; }

        /* A larger, non-trivial payload (300 bytes, not a round power of two) exercises the
         * retry-on-short-read/write loop across more than one syscall's worth of data on most
         * platforms' default socket buffer granularity. */
        uint8_t big_payload[300];
        for (size_t i = 0; i < sizeof(big_payload); ++i) big_payload[i] = (uint8_t)(i * 7 + 3);
        if (lockstep_write_frame(fds[0], LOCKSTEP_FRAME_STEP, big_payload, sizeof(big_payload)) != LOCKSTEP_IO_OK) { fprintf(stderr, "write big failed\n"); return 1; }
        if (lockstep_read_frame(fds[1], &frame_type, payload_buf, sizeof(payload_buf), &payload_len) != LOCKSTEP_IO_OK) { fprintf(stderr, "read big failed\n"); return 1; }
        if (frame_type != LOCKSTEP_FRAME_STEP || payload_len != sizeof(big_payload) || memcmp(payload_buf, big_payload, payload_len) != 0) {
            fprintf(stderr, "big frame round trip mismatch\n"); return 1;
        }

        printf("OK\n");
        return 0;
    }
    """
    binary_path = _compile(tmp_path, "roundtrip", c_src)
    result = run_compiled(binary_path)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "OK"


def test_peer_closing_mid_frame_is_a_typed_error_not_a_silent_short_frame(tmp_path: Path) -> None:
    """The peer closing its write end after sending only the length field (never the
    frame_type/payload) must surface as LOCKSTEP_IO_ERR_PEER_CLOSED, never a frame silently
    treated as zero-length or complete."""
    c_src = r"""
    #include <stdio.h>
    #include <unistd.h>
    #include "lockstep_local_framing.h"
    #include "lockstep_local_io.h"
    int main(void) {
        int fds[2];
        if (pipe(fds) != 0) { perror("pipe"); return 1; }
        uint8_t length_field[4];
        lockstep_encode_length_field(10, length_field); /* claims 10 more bytes are coming */
        write(fds[1], length_field, sizeof(length_field));
        close(fds[1]); /* ...then the peer vanishes without sending them */

        uint8_t frame_type;
        uint8_t payload_buf[64];
        size_t payload_len;
        lockstep_io_status_t st = lockstep_read_frame(fds[0], &frame_type, payload_buf, sizeof(payload_buf), &payload_len);
        if (st != LOCKSTEP_IO_ERR_PEER_CLOSED) { fprintf(stderr, "expected LOCKSTEP_IO_ERR_PEER_CLOSED, got %d\n", (int)st); return 1; }
        printf("OK\n");
        return 0;
    }
    """
    binary_path = _compile(tmp_path, "peer_closed", c_src)
    result = run_compiled(binary_path)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "OK"


def test_oversized_length_field_is_rejected_before_reading_the_payload(tmp_path: Path) -> None:
    """A length field claiming more than LOCKSTEP_LOCAL_MAX_FRAME_LEN bytes is refused as soon
    as the 4-byte length field itself is parsed -- before this module would ever try to read (or
    sanity-check a caller's buffer against) that many bytes. Proven without actually sending
    16 MiB: the garbled length field alone is enough to trigger the refusal."""
    c_src = r"""
    #include <stdio.h>
    #include <unistd.h>
    #include "lockstep_local_framing.h"
    #include "lockstep_local_io.h"
    int main(void) {
        int fds[2];
        if (pipe(fds) != 0) { perror("pipe"); return 1; }
        uint8_t hostile_length[4] = { 0xFF, 0xFF, 0xFF, 0xFF }; /* ~4 GiB, far past the 16 MiB bound */
        write(fds[1], hostile_length, sizeof(hostile_length));

        uint8_t frame_type;
        uint8_t payload_buf[64];
        size_t payload_len;
        lockstep_io_status_t st = lockstep_read_frame(fds[0], &frame_type, payload_buf, sizeof(payload_buf), &payload_len);
        if (st != LOCKSTEP_IO_ERR_FRAME_TOO_LARGE) { fprintf(stderr, "expected LOCKSTEP_IO_ERR_FRAME_TOO_LARGE, got %d\n", (int)st); return 1; }
        printf("OK\n");
        return 0;
    }
    """
    binary_path = _compile(tmp_path, "oversized", c_src)
    result = run_compiled(binary_path)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "OK"
