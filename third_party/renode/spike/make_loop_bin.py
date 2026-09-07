#!/usr/bin/env python3
"""Write a trivial bare-metal firmware for the Cortex-R5F (ARMv7-R, A32/ARM state) RPU core:
a single `b .` (branch-to-self) instruction, repeated to fill a small binary. This is the
"trivial firmware -- a bare loop is fine" M24.1 asks for: we are measuring Renode's time base,
not the software running on it.

Encoding: ARM (A32) unconditional branch, condition=AL(1110), op=101, link=0, 24-bit signed
offset. To branch to the current instruction's own address, the encoded offset must be -2
(the processor computes target = PC_of_branch + 8 + offset*4; PC_of_branch+8 is the ARM
pipeline's "PC" value read during execution, so offset*4 must be -8). -2 in 24-bit two's
complement is 0xFFFFFE, giving the well-known infinite-loop opcode word 0xEAFFFFFE, encoded
little-endian (ARM/Cortex-R5F default) as bytes FE FF FF EA.
"""
import pathlib

LOOP_INSN = bytes([0xFE, 0xFF, 0xFF, 0xEA])  # 0xEAFFFFFE little-endian: b .
out_path = pathlib.Path(__file__).parent / "loop.bin"
# 16 copies is plenty (64 bytes); execution never leaves the first instruction, the rest is
# just so a disassembly of the loaded region is not one lonely word.
out_path.write_bytes(LOOP_INSN * 16)
print(f"wrote {out_path} ({out_path.stat().st_size} bytes)")
