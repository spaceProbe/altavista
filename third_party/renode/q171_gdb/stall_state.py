"""gdb-python script: print the state of a Renode-emulated RTEMS/cFS guest that is held paused at a
STEP stall (question 171, q171-c). Run it with `gdb_attach.py` (same directory), against the stub
that `renode_bridge.py` starts when `AV_BRIDGE_GDB_PORT` is set (`hold_for_debugger`).

What it prints:
  - every classic-API task: name, `current_state`, and a symbolic backtrace of its saved context
    (the saved `Context_Control` -- r4..r10, fp, sp, lr -- is loaded into the live registers, `bt`
    is taken, and every register is restored before exit; the script refuses to leave the guest
    altered and says so);
  - for every task blocked in `rtems_termios_read_tty`: the termios `tty` it waits on (raw input
    ring Head/Tail/Size/`rawInBufDropped`) and the frames of `lockstep_read_frame`/`read_all`
    with their locals (`got`, `len`, `length`, `payload_len`);
  - the Cadence UART registers of uart0/uart1 (mask 0x10, status 0x14, rx trigger 0x20, channel
    status 0x2C) and the RPU GIC enable/pending/active words for IDs 53/54 and its CPU interface.
    The RX FIFO register (0x30) is NEVER read: reading it pops a byte.

Registers must only be written while frame 0 is selected: assigning `$pc`/`$sp` with an outer frame
selected writes the *saved-register slot on that thread's stack*, i.e. it corrupts guest memory
(learnt the hard way; hence `select_frame_zero()` before every restore).
"""
import struct

import gdb

REGS = ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12", "sp", "lr", "pc", "cpsr"]
STATES = {0x1: "DORMANT", 0x2: "SUSPENDED", 0x4: "TRANSIENT", 0x8: "DELAYING", 0x10: "WAITING_FOR_TIME",
          0x20: "WAITING_FOR_BUFFER", 0x40: "WAITING_FOR_SEGMENT", 0x80: "WAITING_FOR_MESSAGE",
          0x100: "WAITING_FOR_EVENT", 0x200: "WAITING_FOR_SEMAPHORE", 0x400: "WAITING_FOR_MUTEX",
          0x800: "WAITING_FOR_CONDVAR"}


def select_frame_zero():
    gdb.execute("frame 0", to_string=True)


def reg(name):
    return int(gdb.parse_and_eval("$" + name)) & 0xFFFFFFFF


def u32(addr):
    return int(gdb.parse_and_eval("*(unsigned int*)%d" % addr)) & 0xFFFFFFFF


def task_name(raw):
    return "".join(chr(c) if 32 <= c < 127 else "." for c in struct.pack(">I", raw))


def describe_state(state):
    return "|".join(v for k, v in STATES.items() if state & k) or "READY"


def load_context(tcb, saved_cpsr):
    r = tcb["Registers"]
    for n, f in [("r4", "register_r4"), ("r5", "register_r5"), ("r6", "register_r6"), ("r7", "register_r7"),
                 ("r8", "register_r8"), ("r9", "register_r9"), ("r10", "register_r10"), ("r11", "register_fp")]:
        gdb.execute("set $%s=%d" % (n, int(r[f])))
    gdb.execute("set $sp=%d" % (int(r["register_sp"]) & 0xFFFFFFFF))
    gdb.execute("set $pc=%d" % (int(r["register_lr"]) & ~1 & 0xFFFFFFFF))
    gdb.execute("set $cpsr=%d" % (saved_cpsr | 0x20))  # Thumb: RTEMS is built -mthumb


def show_read_all_frames(bt_text):
    """For a task blocked inside lockstep_read_frame: print the tty and read_all/lockstep locals."""
    frames = [ln for ln in bt_text.splitlines() if ln.startswith("#")]
    for ln in frames:
        idx = int(ln[1:].split()[0])
        if " fillBufferQueue " in ln or " read_all " in ln or " lockstep_read_frame " in ln:
            gdb.execute("frame %d" % idx, to_string=True)
            print("  -- frame %d: %s" % (idx, ln.split(" in ")[-1][:90]))
            if " fillBufferQueue " in ln:
                tty = gdb.parse_and_eval("tty")
                rb = tty["rawInBuf"]
                print("     tty=%s rawInBuf: Head=%d Tail=%d Size=%d  rawInBufDropped=%d  rawInBufSemaphoreWait=%s"
                      % (tty, int(rb["Head"]), int(rb["Tail"]), int(rb["Size"]), int(tty["rawInBufDropped"]),
                         tty["rawInBufSemaphoreWait"]))
            else:
                print(gdb.execute("info locals", to_string=True).replace("\n", "\n     ")[:600])
    select_frame_zero()


def main():
    select_frame_zero()
    saved = {r: reg(r) for r in REGS}
    print("LIVE CPU: pc=%#x cpsr=%#x  %s" % (saved["pc"], saved["cpsr"], gdb.execute("x/i $pc", to_string=True).strip()))
    info = gdb.parse_and_eval("_RTEMS_tasks_Information.Objects")
    local_table = int(info["local_table"])
    n_max = int(info["maximum_id"]) & 0xFFFF
    for i in range(1, n_max + 1):
        p = u32(local_table + 4 * i)
        if p == 0:
            continue
        tcb = gdb.parse_and_eval("(Thread_Control*)%d" % p)
        state = int(tcb["current_state"])
        print("=== task idx=%d tcb=%#x name=%r state=%#x (%s)" % (i, p, task_name(u32(p + 12)), state, describe_state(state)))
        load_context(tcb, saved["cpsr"])
        try:
            bt = gdb.execute("bt 30", to_string=True)
        except gdb.error as e:
            bt = "bt failed: %s" % e
        for ln in bt.splitlines():
            print("   " + ln[:200])
        if "rtems_termios_read_tty" in bt:
            show_read_all_frames(bt)
        select_frame_zero()
    for r in REGS:
        gdb.execute("set $%s=%d" % (r, saved[r]))
    print("REGISTERS RESTORED:", {r: reg(r) for r in REGS} == saved)

    print("=== Cadence UART (MMIO reads only; the RX FIFO register 0x30 is never touched)")
    for name, base in (("uart0", 0xFF000000), ("uart1", 0xFF010000)):
        print("  %s control=%#x irq_mask(0x10)=%#x irq_sts(0x14)=%#x rx_trg(0x20)=%#x channel_sts(0x2C)=%#x"
              % (name, u32(base), u32(base + 0x10), u32(base + 0x14), u32(base + 0x20), u32(base + 0x2C)))
    print("=== RPU GIC (v1): distributor CTLR=%#x ISENABLER1(IDs32-63)=%#x ISPENDR1=%#x ISACTIVER1=%#x; "
          "CPU IF CTLR=%#x PMR=%#x HPPIR=%#x"
          % (u32(0xF9000000), u32(0xF9000104), u32(0xF9000204), u32(0xF9000304), u32(0xF9001000), u32(0xF9001004), u32(0xF9001018)))
    print("    (uart0 = ID 53 = bit 21, uart1 = ID 54 = bit 22 of word 1)")


main()
