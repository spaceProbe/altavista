"""container/structural-check.py -- the STRUCTURAL check of a ZynqMP BOOT.BIN.

The board is not on hand, so the image cannot be booted; this reads it back instead. Inputs are the
text `bootgen -arch zynqmp -read BOOT.BIN` printed and the three ELFs the image was built from (read
with the toolchain's readelf). It prints the partition table (destination CPU, execution state,
exception level, load and execution addresses, sizes) and fails (exit 1) unless:

  * the boot header's FSBL execution address equals the FSBL ELF's entry point, and that is the
    start of OCM (0xFFFC0000), where the CSU boot ROM loads and starts the FSBL;
  * the PMU firmware ELF's lowest segment is the start of PMU RAM (0xFFDC0000, where the
    MicroBlaze reset vector sits), its entry point lies inside PMU RAM, and bootgen's PMU firmware
    length equals the ELF's loadable span;
  * the FSBL partition is A53-0, AArch64, EL3, loaded and executed at the FSBL ELF's addresses,
    and its length is the PMU firmware plus FSBL blocks of the boot header;
  * the RPU ELF's every PT_LOAD with file content is a partition with core r5-lockstep (and no
    partition has any other core than that for the RPU image), load address equal to the segment's
    physical address, length equal to its file size, execution state aarch-32, and the entry point
    of the ELF is the execution address of the first RPU partition.

Partition lengths and offsets in the partition header table are in 32-bit words.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys

OCM_BASE = 0xFFFC0000
PMU_RAM_BASE = 0xFFDC0000


def readelf(tool: str, path: str, *flags: str) -> str:
    return subprocess.run([tool, *flags, path], check=True, capture_output=True, text=True).stdout


def elf_info(tool: str, path: str) -> dict:
    header = readelf(tool, path, "-hW")
    entry = int(re.search(r"Entry point address:\s+(0x[0-9a-f]+)", header).group(1), 16)
    machine = re.search(r"Machine:\s+(.+)", header).group(1).strip()
    elf_class = re.search(r"Class:\s+(\S+)", header).group(1)
    loads = []
    for line in readelf(tool, path, "-lW").splitlines():
        m = re.match(
            r"\s+LOAD\s+(0x[0-9a-f]+)\s+(0x[0-9a-f]+)\s+(0x[0-9a-f]+)\s+(0x[0-9a-f]+)\s+(0x[0-9a-f]+)\s+(\S+)",
            line,
        )
        if m:
            off, va, pa, fsz, msz = (int(m.group(i), 16) for i in range(1, 6))
            loads.append({"offset": off, "vaddr": va, "paddr": pa, "filesz": fsz, "memsz": msz})
    return {"entry": entry, "machine": machine, "class": elf_class, "loads": loads}


def parse_bootgen_read(text: str) -> dict:
    out = {"header": {}, "partitions": []}
    section = None
    current = None
    for line in text.splitlines():
        m = re.match(r"\s{3}(BOOT HEADER|IMAGE HEADER TABLE|IMAGE HEADER \((.+)\)|PARTITION HEADER TABLE \((.+)\))\s*$", line)
        if m:
            section = m.group(1)
            current = None
            if section.startswith("PARTITION HEADER TABLE"):
                current = {"name": m.group(3), "attrs": {}}
                out["partitions"].append(current)
            continue
        target = out["header"] if section == "BOOT HEADER" else current
        if target is None:
            continue
        for key, val in re.findall(r"(\w+)(?:\([A-Z]\))? \(0x[0-9a-f]+\) : (0x[0-9a-f]+)", line):
            target[key] = int(val, 16)
        if current is not None:
            for key, val in re.findall(r"(\S+) \[([^\]]+)\]", line):
                current["attrs"][key] = val
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bootgen-read", required=True)
    ap.add_argument("--fsbl", required=True)
    ap.add_argument("--pmufw", required=True)
    ap.add_argument("--rpu", required=True)
    ap.add_argument("--readelf", required=True, help="readelf for the FSBL and RPU ELFs (any binutils readelf reads both)")
    ap.add_argument("--mb-readelf", required=True)
    a = ap.parse_args()

    bg = parse_bootgen_read(open(a.bootgen_read).read())
    hdr, parts = bg["header"], bg["partitions"]
    fsbl = elf_info(a.readelf, a.fsbl)
    pmufw = elf_info(a.mb_readelf, a.pmufw)
    rpu = elf_info(a.readelf, a.rpu)
    problems: list[str] = []

    def check(cond: bool, msg: str) -> None:
        print(("ok    " if cond else "FAIL  ") + msg)
        if not cond:
            problems.append(msg)

    print("== boot header")
    for k in ("fsbl_exec_address", "fsbl_sourceoffset", "pmufw_length", "pmufw_total_length", "fsbl_length", "fsbl_total_length"):
        print(f"   {k:20s} 0x{hdr[k]:08x}")
    print("== partitions (lengths in bytes = words * 4)")
    print("   %-16s %-12s %-9s %-4s %-5s %-18s %-18s %10s" % ("partition", "core", "state", "el", "tz", "load", "exec", "bytes"))
    for p in parts:
        load = (p["load_addr_hi"] << 32) | p["load_addr_lo"]
        ex = (p["exec_addr_hi"] << 32) | p["exec_addr_lo"]
        p["load"], p["exec"], p["bytes"] = load, ex, p["unencrypted_length"] * 4
        at = p["attrs"]
        print("   %-16s %-12s %-9s %-4s %-5s 0x%016x 0x%016x %10d" % (
            p["name"], at.get("core"), at.get("exec_state"), at.get("el"), at.get("trustzone"), load, ex, p["bytes"]))

    print("== ELF facts")
    for name, e in (("fsbl.elf", fsbl), ("pmufw.elf", pmufw), ("rpu", rpu)):
        print(f"   {name:10s} {e['class']} {e['machine']} entry 0x{e['entry']:x}")
        for s in e["loads"]:
            print(f"      LOAD paddr 0x{s['paddr']:08x} filesz 0x{s['filesz']:x} memsz 0x{s['memsz']:x}")

    print("== checks")
    check(len(parts) >= 2, f"partition table has {len(parts)} partitions")
    # FSBL
    check(hdr["fsbl_exec_address"] == fsbl["entry"], "boot header FSBL exec address 0x%x == FSBL ELF entry 0x%x" % (hdr["fsbl_exec_address"], fsbl["entry"]))
    check(fsbl["entry"] == OCM_BASE, "FSBL entry is the OCM base 0xfffc0000 (CSU boot ROM loads the FSBL to OCM)")
    check("AArch64" in fsbl["machine"], "FSBL ELF machine is AArch64 (%s)" % fsbl["machine"])
    p0 = parts[0]
    check(p0["name"].startswith("fsbl.elf") and p0["attrs"].get("core") == "a53-0", "first partition is the FSBL on a53-0 (%s, %s)" % (p0["name"], p0["attrs"].get("core")))
    check(p0["attrs"].get("exec_state") == "aarch-64" and p0["attrs"].get("el") == "el-3", "FSBL partition is aarch-64 at el-3")
    check(p0["exec"] == fsbl["entry"] and p0["load"] == min(s["paddr"] for s in fsbl["loads"]), "FSBL partition exec 0x%x == ELF entry, load 0x%x == lowest segment paddr" % (p0["exec"], p0["load"]))
    check(p0["bytes"] == hdr["pmufw_total_length"] + hdr["fsbl_total_length"], "FSBL partition bytes %d == PMU firmware block %d + FSBL block %d (bootgen prepends the PMU firmware)" % (p0["bytes"], hdr["pmufw_total_length"], hdr["fsbl_total_length"]))
    fl = [s for s in fsbl["loads"] if s["filesz"] > 0]
    fsbl_span = max(s["paddr"] + s["filesz"] for s in fl) - min(s["paddr"] for s in fl)
    check(hdr["fsbl_length"] == fsbl_span, "boot header FSBL length %d == the ELF's span from first to last segment with file content %d" % (hdr["fsbl_length"], fsbl_span))
    # PMU firmware
    pl = [s for s in pmufw["loads"] if s["filesz"] > 0]
    check("MicroBlaze" in pmufw["machine"] or "Xilinx" in pmufw["machine"], "PMU firmware ELF machine is MicroBlaze (%s)" % pmufw["machine"])
    check(min(s["paddr"] for s in pl) == PMU_RAM_BASE, "PMU firmware lowest segment is at PMU RAM base 0xffdc0000 (where the MicroBlaze reset vector sits)")
    pmu_span = max(s["paddr"] + s["filesz"] for s in pl) - min(s["paddr"] for s in pl)
    check(hdr["pmufw_length"] == pmu_span, "boot header PMU firmware length %d == the ELF's loadable span %d" % (hdr["pmufw_length"], pmu_span))
    check(PMU_RAM_BASE <= pmufw["entry"] < PMU_RAM_BASE + hdr["pmufw_length"], "PMU firmware ELF entry 0x%x lies inside PMU RAM (the reset vector branches to it)" % pmufw["entry"])
    # RPU
    rpu_parts = [p for p in parts[1:]]
    rpu_loads = [s for s in rpu["loads"] if s["filesz"] > 0]
    check("ARM" in rpu["machine"], "RPU ELF machine is ARM (%s)" % rpu["machine"])
    check(len(rpu_parts) == len(rpu_loads), "%d RPU partitions == %d RPU PT_LOAD segments with file content" % (len(rpu_parts), len(rpu_loads)))
    for p, s in zip(rpu_parts, sorted(rpu_loads, key=lambda s: s["offset"])):
        at = p["attrs"]
        check(at.get("core") == "r5-lockstep", "%s destination core is r5-lockstep (attribute word 0x%x)" % (p["name"], p["attributes"]))
        check(p["load"] == s["paddr"], "%s load 0x%x == segment paddr 0x%x" % (p["name"], p["load"], s["paddr"]))
        check(p["bytes"] == ((s["filesz"] + 3) // 4) * 4, "%s length %d == segment filesz %d (word-padded)" % (p["name"], p["bytes"], s["filesz"]))
        check(at.get("exec_state") == "aarch-32" and at.get("dest_device") == "PS", "%s is aarch-32, dest_device PS" % p["name"])
    if rpu_parts:
        check(rpu_parts[0]["exec"] == rpu["entry"], "first RPU partition exec 0x%x == RPU ELF entry 0x%x" % (rpu_parts[0]["exec"], rpu["entry"]))
    print("RESULT: %s" % ("PASS" if not problems else "FAIL (%d)" % len(problems)))
    return 0 if not problems else 1


if __name__ == "__main__":
    sys.exit(main())
