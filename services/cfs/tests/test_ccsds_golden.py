"""M23.2 (docs/open-questions.md question 149; docs/sil-plan.md M23): proves
`services/cfs/apps/shared/ccsds/src/ccsds_codec.c` -- the CCSDS Space Packet codec both the
lockstep I/O app and the reference ADCS app use to move packets between the cFE software bus
and lockstep-local (M23.4 moved this file here from `services/cfs/apps/io_lockstep/fsw/src/` so
the two apps share one implementation instead of each carrying an independent copy -- see that
file's own module doc comment) -- produces and consumes *the same bytes
`crates/av-kernel/src/codec.rs`'s own `encode_packet` produces*, not merely bytes this C port
agrees with itself about.

The fixtures under services/cfs/tests/fixtures/ccsds_golden_*.json were captured by actually
running `cargo run --manifest-path services/cfs/tests/golden_gen/Cargo.toml -- <case>`, which
calls `av_kernel::codec::encode_packet` directly (see that crate's own module doc comment for
why it is a separate, non-workspace-member Cargo project rather than a change to
`crates/av-kernel`, which this task may not touch). This test does not regenerate them (that
needs a GMAT-linked build environment this test should not require to run); it only compiles
and runs a tiny C harness against the checked-in fixture bytes. To regenerate after an
intentional codec change:
    cargo run --manifest-path services/cfs/tests/golden_gen/Cargo.toml -- imu > \
        services/cfs/tests/fixtures/ccsds_golden_imu.json
    (same for wheel_torque)

What a wrong C implementation fails here: any bit-offset, byte-order, sign, or
packet-data-length arithmetic mistake that disagrees with the Rust codec -- e.g. the exact
"packet_data_length = user_data_bytes - 1" off-by-one question 149 already names as a defect a
prior draft made and review caught (the module doc comment on both codec.rs and this file's own
`ccsds_codec.c` calls this out explicitly).
"""
from __future__ import annotations

import json
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[3]
CFS_DIR = REPO_ROOT / "services" / "cfs"
CODEC_SRC = CFS_DIR / "apps" / "shared" / "ccsds" / "src" / "ccsds_codec.c"
CODEC_INC = CFS_DIR / "apps" / "shared" / "ccsds" / "inc"
FIXTURES = Path(__file__).resolve().parent / "fixtures"

CASES = {
    "imu": {
        "field_order": ["wx", "wy", "wz", "ax", "ay", "az"],
        "secondary_header_bytes": 0,
    },
    "wheel_torque": {
        "field_order": ["tau_1", "tau_2", "tau_3"],
        "secondary_header_bytes": 0,
    },
}

C_HARNESS_TEMPLATE = r"""
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include "ccsds_codec.h"

static void hex_decode(const char *hex, uint8_t *out, size_t out_len) {{
    for (size_t i = 0; i < out_len; ++i) {{
        unsigned int byte;
        sscanf(hex + 2 * i, "%2x", &byte);
        out[i] = (uint8_t)byte;
    }}
}}

int main(void) {{
    static const ccsds_field_t fields[] = {{
{field_decls}
    }};
    ccsds_codec_t codec = {{
        .id = "{codec_id}",
        .apid = {apid},
        .is_command = {is_command},
        .secondary_header_bytes = {secondary_header_bytes},
        .user_data_bytes = {user_data_bytes},
        .fields = fields,
        .field_count = {field_count},
    }};
    static const double expected_values[] = {{ {expected_values} }};
    uint8_t golden[{total_bytes}];
    hex_decode("{golden_hex}", golden, sizeof(golden));

    /* 1) decode the Rust-produced bytes and check every field matches, bit-exact
     *    (IEEE-754 round trip through the same bit pattern, so exact equality is the right
     *    check -- not a tolerance). */
    double decoded[{field_count}];
    ccsds_status_t st = ccsds_decode_packet(&codec, golden, sizeof(golden), decoded);
    if (st != CCSDS_OK) {{
        fprintf(stderr, "decode failed: %s\n", ccsds_status_str(st));
        return 1;
    }}
    for (size_t i = 0; i < {field_count}; ++i) {{
        if (decoded[i] != expected_values[i]) {{
            fprintf(stderr, "field %zu mismatch: decoded %.17g, expected %.17g\n", i, decoded[i], expected_values[i]);
            return 1;
        }}
    }}

    /* 2) encode the same field values and check the bytes are IDENTICAL to the Rust codec's
     *    own output -- the actual "same bytes the kernel encodes" assertion. */
    uint8_t encoded[{total_bytes}];
    size_t encoded_len = 0;
    st = ccsds_encode_packet(&codec, {sequence_count}, NULL, expected_values, encoded, sizeof(encoded), &encoded_len);
    if (st != CCSDS_OK) {{
        fprintf(stderr, "encode failed: %s\n", ccsds_status_str(st));
        return 1;
    }}
    if (encoded_len != sizeof(golden) || memcmp(encoded, golden, sizeof(golden)) != 0) {{
        fprintf(stderr, "encoded bytes differ from the kernel's own codec output\n");
        fprintf(stderr, "  got: ");
        for (size_t i = 0; i < encoded_len; ++i) fprintf(stderr, "%02x", encoded[i]);
        fprintf(stderr, "\n  want: ");
        for (size_t i = 0; i < sizeof(golden); ++i) fprintf(stderr, "%02x", golden[i]);
        fprintf(stderr, "\n");
        return 1;
    }}

    printf("OK\n");
    return 0;
}}
"""

FIELD_TYPE_MAP = {"FLOAT64": "CCSDS_FIELD_FLOAT64"}


def _build_and_run(case_name: str, tmp_path: Path) -> subprocess.CompletedProcess:
    fixture = json.loads((FIXTURES / f"ccsds_golden_{case_name}.json").read_text())
    field_order = CASES[case_name]["field_order"]
    values = fixture["values"]
    expected_values = [values[name] for name in field_order]
    user_data_bytes = fixture["user_data_bytes"]
    bit_width = 64
    field_decls = "\n".join(
        f'        {{ .name = "{name}", .bit_offset = {i * bit_width}, .bit_width = {bit_width}, '
        f".type = CCSDS_FIELD_FLOAT64, .scale = 1.0, .offset = 0.0 }},"
        for i, name in enumerate(field_order)
    )
    total_bytes = 6 + CASES[case_name]["secondary_header_bytes"] + user_data_bytes

    c_src = C_HARNESS_TEMPLATE.format(
        field_decls=field_decls,
        codec_id=f"{case_name}_codec",
        apid=fixture["apid"],
        is_command="true" if fixture["is_command"] else "false",
        secondary_header_bytes=CASES[case_name]["secondary_header_bytes"],
        user_data_bytes=user_data_bytes,
        field_count=len(field_order),
        expected_values=", ".join(repr(v) for v in expected_values),
        total_bytes=total_bytes,
        golden_hex=fixture["encoded_hex"],
        sequence_count=fixture["sequence_count"],
    )

    src_path = tmp_path / f"test_{case_name}.c"
    src_path.write_text(c_src)
    binary_path = tmp_path / f"test_{case_name}"

    compile_cmd = ["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", f"-I{CODEC_INC}", str(src_path), str(CODEC_SRC), "-o", str(binary_path), "-lm"]
    compiled = subprocess.run(compile_cmd, capture_output=True, text=True)
    assert compiled.returncode == 0, f"cc failed:\n{compiled.stdout}\n{compiled.stderr}"

    return subprocess.run([str(binary_path)], capture_output=True, text=True)


@pytest.mark.parametrize("case_name", sorted(CASES))
def test_ccsds_codec_matches_kernel_golden(case_name: str, tmp_path: Path) -> None:
    result = _build_and_run(case_name, tmp_path)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "OK"


def test_ccsds_codec_rejects_wrong_apid(tmp_path: Path) -> None:
    """A packet whose primary header names a different APID than the codec being used to
    decode it is a typed refusal (CCSDS_ERR_UNKNOWN_APID), never a silent misdecode -- this is
    the C-side half of question 149's "never a silent drop" rule."""
    fixture = json.loads((FIXTURES / "ccsds_golden_imu.json").read_text())
    src_path = tmp_path / "test_wrong_apid.c"
    wrong_apid_template = textwrap.dedent(
        r"""
        #include <stdio.h>
        #include "ccsds_codec.h"
        static void hex_decode(const char *hex, uint8_t *out, size_t out_len) {
            for (size_t i = 0; i < out_len; ++i) {
                unsigned int byte;
                sscanf(hex + 2 * i, "%2x", &byte);
                out[i] = (uint8_t)byte;
            }
        }
        int main(void) {
            static const ccsds_field_t fields[] = {
                { .name = "wx", .bit_offset = 0, .bit_width = 64, .type = CCSDS_FIELD_FLOAT64, .scale = 1.0, .offset = 0.0 },
            };
            ccsds_codec_t codec = { .id = "wrong", .apid = 999, .is_command = false,
                .secondary_header_bytes = 0, .user_data_bytes = 48, .fields = fields, .field_count = 1 };
            uint8_t golden[GOLDEN_LEN];
            hex_decode("GOLDEN_HEX", golden, sizeof(golden));
            double out_values[1];
            ccsds_status_t st = ccsds_decode_packet(&codec, golden, sizeof(golden), out_values);
            if (st != CCSDS_ERR_UNKNOWN_APID) {
                fprintf(stderr, "expected CCSDS_ERR_UNKNOWN_APID, got %s\n", ccsds_status_str(st));
                return 1;
            }
            printf("OK\n");
            return 0;
        }
        """
    )
    src_path.write_text(
        wrong_apid_template.replace("GOLDEN_LEN", str(6 + fixture["user_data_bytes"])).replace("GOLDEN_HEX", fixture["encoded_hex"])
    )
    binary_path = tmp_path / "test_wrong_apid"
    compiled = subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", f"-I{CODEC_INC}", str(src_path), str(CODEC_SRC), "-o", str(binary_path), "-lm"], capture_output=True, text=True)
    assert compiled.returncode == 0, f"cc failed:\n{compiled.stdout}\n{compiled.stderr}"
    result = subprocess.run([str(binary_path)], capture_output=True, text=True)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
