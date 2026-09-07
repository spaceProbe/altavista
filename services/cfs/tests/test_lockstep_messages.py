"""M23.2 (docs/open-questions.md question 153): proves
`services/cfs/apps/io_lockstep/fsw/src/lockstep_messages.c` (the hand-written protobuf
encode/decode for the `altavista.v1` lockstep messages, built on `pbmini.c`) against bytes a
real `prost::Message::encode_to_vec()` call produced -- the same encoding
`crates/av-lockstep-shim` speaks to the kernel and that a real shim would send/expect over the
Unix socket -- not against this C code's own round trip alone.

Fixtures under services/cfs/tests/fixtures/lockstep_*.json were captured with:
    cargo run --manifest-path services/cfs/tests/golden_gen/Cargo.toml -- lockstep_step_request
    (same for lockstep_bind_request, lockstep_step_response, lockstep_bind_response)
"""
from __future__ import annotations

import json
import subprocess

from _cbuild import compile_cached, run_compiled
from pathlib import Path

CFS_DIR = Path(__file__).resolve().parent.parent
INC = CFS_DIR / "apps" / "io_lockstep" / "fsw" / "inc"
SRC_DIR = CFS_DIR / "apps" / "io_lockstep" / "fsw" / "src"
SOURCES = [SRC_DIR / "pbmini.c", SRC_DIR / "lockstep_messages.c"]
FIXTURES = Path(__file__).resolve().parent / "fixtures"


def _compile_and_run(tmp_path: Path, name: str, c_src: str) -> subprocess.CompletedProcess:
    """Question 172: cached by source hash and pre-warmed untimed. See _cbuild.py."""
    return run_compiled(compile_cached(name, c_src, list(SOURCES), INC))


def test_decode_step_request_matches_prost_encoding(tmp_path: Path) -> None:
    fixture = json.loads((FIXTURES / "lockstep_step_request.json").read_text())
    c_src = f"""
    #include <stdio.h>
    #include <string.h>
    #include "lockstep_messages.h"
    static void hex_decode(const char *hex, uint8_t *out, size_t out_len) {{
        for (size_t i = 0; i < out_len; ++i) {{ unsigned int b; sscanf(hex + 2*i, "%2x", &b); out[i] = (uint8_t)b; }}
    }}
    int main(void) {{
        static const char *hex = "{fixture['encoded_hex']}";
        size_t len = strlen(hex) / 2;
        uint8_t buf[512];
        hex_decode(hex, buf, len);
        lockstep_step_request_t req;
        pbmini_status_t st = lockstep_decode_step_request(buf, len, &req);
        if (st != PBMINI_OK) {{ fprintf(stderr, "decode failed: %d\\n", (int)st); return 1; }}
        if (req.sequence != 5) {{ fprintf(stderr, "sequence = %llu\\n", (unsigned long long)req.sequence); return 1; }}
        if (req.until_tai_ns != 123456789000LL) {{ fprintf(stderr, "until_tai_ns = %lld\\n", (long long)req.until_tai_ns); return 1; }}
        if (req.input_count != 1) {{ fprintf(stderr, "input_count = %zu\\n", req.input_count); return 1; }}
        if (strcmp(req.inputs[0].port, "imu_meas") != 0) {{ fprintf(stderr, "port = %s\\n", req.inputs[0].port); return 1; }}
        if (req.inputs[0].tai_ns != 123456789000LL) {{ fprintf(stderr, "input tai_ns = %lld\\n", (long long)req.inputs[0].tai_ns); return 1; }}
        if (req.inputs[0].payload_len != 48 + 6) {{ fprintf(stderr, "payload_len = %zu\\n", req.inputs[0].payload_len); return 1; }}
        printf("OK\\n");
        return 0;
    }}
    """
    result = _compile_and_run(tmp_path, "decode_step_request", c_src)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "OK"


def test_decode_bind_request_matches_prost_encoding(tmp_path: Path) -> None:
    fixture = json.loads((FIXTURES / "lockstep_bind_request.json").read_text())
    c_src = f"""
    #include <stdio.h>
    #include <string.h>
    #include "lockstep_messages.h"
    static void hex_decode(const char *hex, uint8_t *out, size_t out_len) {{
        for (size_t i = 0; i < out_len; ++i) {{ unsigned int b; sscanf(hex + 2*i, "%2x", &b); out[i] = (uint8_t)b; }}
    }}
    int main(void) {{
        static const char *hex = "{fixture['encoded_hex']}";
        size_t len = strlen(hex) / 2;
        uint8_t buf[512];
        hex_decode(hex, buf, len);
        lockstep_bind_request_t req;
        pbmini_status_t st = lockstep_decode_bind_request(buf, len, &req);
        if (st != PBMINI_OK) {{ fprintf(stderr, "decode failed: %d\\n", (int)st); return 1; }}
        if (strcmp(req.run_id, "run-cfs-1") != 0) {{ fprintf(stderr, "run_id = %s\\n", req.run_id); return 1; }}
        if (strcmp(req.instance, "adcs_flight") != 0) {{ fprintf(stderr, "instance = %s\\n", req.instance); return 1; }}
        if (req.start_tai_ns != 100000000000LL) {{ fprintf(stderr, "start_tai_ns = %lld\\n", (long long)req.start_tai_ns); return 1; }}
        if (req.base_period_ns != 100000000LL) {{ fprintf(stderr, "base_period_ns = %lld\\n", (long long)req.base_period_ns); return 1; }}
        if (req.step_period_ns != 100000000LL) {{ fprintf(stderr, "step_period_ns = %lld\\n", (long long)req.step_period_ns); return 1; }}
        if (req.seed != 42) {{ fprintf(stderr, "seed = %llu\\n", (unsigned long long)req.seed); return 1; }}
        printf("OK\\n");
        return 0;
    }}
    """
    result = _compile_and_run(tmp_path, "decode_bind_request", c_src)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "OK"


def test_encode_step_response_matches_prost_encoding(tmp_path: Path) -> None:
    fixture = json.loads((FIXTURES / "lockstep_step_response.json").read_text())
    imu = json.loads((FIXTURES / "ccsds_golden_wheel_torque.json").read_text())
    c_src = f"""
    #include <stdio.h>
    #include <string.h>
    #include "lockstep_messages.h"
    static void hex_decode(const char *hex, uint8_t *out, size_t out_len) {{
        for (size_t i = 0; i < out_len; ++i) {{ unsigned int b; sscanf(hex + 2*i, "%2x", &b); out[i] = (uint8_t)b; }}
    }}
    int main(void) {{
        lockstep_port_message_t out_msg;
        memset(&out_msg, 0, sizeof(out_msg));
        strcpy(out_msg.port, "wheel_torque_out");
        out_msg.tai_ns = 123456789000LL;
        out_msg.payload_len = {imu['user_data_bytes'] + 6};
        hex_decode("{imu['encoded_hex']}", out_msg.payload, out_msg.payload_len);

        uint8_t encoded[256];
        size_t encoded_len;
        pbmini_status_t st = lockstep_encode_step_response(5, 123456789000LL, &out_msg, 1, encoded, sizeof(encoded), &encoded_len);
        if (st != PBMINI_OK) {{ fprintf(stderr, "encode failed: %d\\n", (int)st); return 1; }}

        static const char *want_hex = "{fixture['encoded_hex']}";
        size_t want_len = strlen(want_hex) / 2;
        uint8_t want[256];
        hex_decode(want_hex, want, want_len);
        if (encoded_len != want_len || memcmp(encoded, want, want_len) != 0) {{
            fprintf(stderr, "encoded step response differs from prost's own encoding\\n  got_len=%zu want_len=%zu\\n  got: ", encoded_len, want_len);
            for (size_t i = 0; i < encoded_len; ++i) fprintf(stderr, "%02x", encoded[i]);
            fprintf(stderr, "\\n  want: ");
            for (size_t i = 0; i < want_len; ++i) fprintf(stderr, "%02x", want[i]);
            fprintf(stderr, "\\n");
            return 1;
        }}
        printf("OK\\n");
        return 0;
    }}
    """
    result = _compile_and_run(tmp_path, "encode_step_response", c_src)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "OK"


def test_encode_bind_response_matches_prost_encoding(tmp_path: Path) -> None:
    fixture = json.loads((FIXTURES / "lockstep_bind_response.json").read_text())
    c_src = f"""
    #include <stdio.h>
    #include <string.h>
    #include "lockstep_messages.h"
    static void hex_decode(const char *hex, uint8_t *out, size_t out_len) {{
        for (size_t i = 0; i < out_len; ++i) {{ unsigned int b; sscanf(hex + 2*i, "%2x", &b); out[i] = (uint8_t)b; }}
    }}
    int main(void) {{
        uint8_t encoded[256];
        size_t encoded_len;
        pbmini_status_t st = lockstep_encode_bind_response(true, "deadbeef", "io_lockstep/0.1", "", encoded, sizeof(encoded), &encoded_len);
        if (st != PBMINI_OK) {{ fprintf(stderr, "encode failed: %d\\n", (int)st); return 1; }}
        static const char *want_hex = "{fixture['encoded_hex']}";
        size_t want_len = strlen(want_hex) / 2;
        uint8_t want[256];
        hex_decode(want_hex, want, want_len);
        if (encoded_len != want_len || memcmp(encoded, want, want_len) != 0) {{ fprintf(stderr, "mismatch\\n"); return 1; }}
        printf("OK\\n");
        return 0;
    }}
    """
    result = _compile_and_run(tmp_path, "encode_bind_response", c_src)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "OK"


def test_decode_rejects_truncated_message(tmp_path: Path) -> None:
    """A protobuf message truncated mid-varint or mid-length-delimited field is a typed error
    (nonzero pbmini_status_t), never a successful decode with garbage values."""
    fixture = json.loads((FIXTURES / "lockstep_step_request.json").read_text())
    truncated_hex = fixture["encoded_hex"][: len(fixture["encoded_hex"]) // 2]
    c_src = f"""
    #include <stdio.h>
    #include <string.h>
    #include "lockstep_messages.h"
    static void hex_decode(const char *hex, uint8_t *out, size_t out_len) {{
        for (size_t i = 0; i < out_len; ++i) {{ unsigned int b; sscanf(hex + 2*i, "%2x", &b); out[i] = (uint8_t)b; }}
    }}
    int main(void) {{
        static const char *hex = "{truncated_hex}";
        size_t len = strlen(hex) / 2;
        uint8_t buf[512];
        hex_decode(hex, buf, len);
        lockstep_step_request_t req;
        pbmini_status_t st = lockstep_decode_step_request(buf, len, &req);
        if (st == PBMINI_OK) {{ fprintf(stderr, "expected a decode failure on truncated input\\n"); return 1; }}
        printf("OK\\n");
        return 0;
    }}
    """
    result = _compile_and_run(tmp_path, "truncated", c_src)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "OK"
