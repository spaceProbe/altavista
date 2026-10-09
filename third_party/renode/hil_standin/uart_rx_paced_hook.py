#
# hilprep-6 -- host-to-guest bytes into a Renode UART at a BAUD RATE IN VIRTUAL TIME.
#
# Why: bytes delivered to UART1 in wall-clock time arrive faster, relative to the emulated CPU,
# than a real line would deliver them whenever Renode runs slower than real time (it does: the
# virtual-to-wall ratio is 0.1 to 0.6 on this host, depending on load). The guest's UART driver
# moves every received byte into termios' 256-byte raw input ring from the interrupt handler, and
# a ring that is not drained fast enough drops bytes (question 238: `rawInBufDropped`), after which
# the application blocks in `read_all` forever. Measured (README.md): with the wall-clock pty path
# about half of all runs stalled within a few steps at 115200 baud, and none stalled at 19200.
# Delivering a byte every 10 / baud seconds of VIRTUAL time restores the line the guest was
# written against, whatever the host's speed.
#
# How: the macro connects out, as a plain TCP client, to a socket the helper
# (renode_realtime_pty.py) already listens on, and schedules a recurring machine action
# (`Machine.ScheduleAction`, virtual time) every TICK_US microseconds. Each tick reads what the
# socket has (non-blocking, never waits) into a Python-side buffer and writes as many bytes to the
# UART (`IUART.WriteChar`) as the line could have carried since the last tick, banking at most
# MAX_CREDIT bytes so an idle line does not accumulate a burst. The action runs on the emulation's
# own thread, in virtual time, so pausing or throttling the machine slows the line with it.
#
# Python 2.7 / IronPython syntax on purpose (the Renode in this repository embeds it).

import clr

try:
    clr.AddReference("System")
except Exception:
    pass
try:
    clr.AddReference("System.Net.Sockets")
except Exception:
    pass

from System.Net.Sockets import TcpClient
from System import Action, Array, Byte
from Antmicro import Renode
from Antmicro.Renode.Time import TimeInterval

TICK_US = 1000
MAX_CREDIT = 4.0

_active = []


def mc_setup_uart_rx_paced(uart, port, baud, host="127.0.0.1"):
    """Monitor macro: `setup_uart_rx_paced sysbus.uart1 <port> <baud>`."""
    iuart = clr.Convert(uart, Renode.Peripherals.UART.IUART)
    machine = uart.GetMachine()
    client = TcpClient()
    client.NoDelay = True
    client.Connect(host, int(port))
    stream = client.GetStream()
    bytes_per_tick = (float(baud) / 10.0) * (TICK_US / 1000000.0)
    state = {"pending": [], "credit": 0.0, "injected": 0, "ticks": 0, "max_pending": 0, "errors": 0}
    buf = Array.CreateInstance(Byte, 4096)

    def _tick(_now):
        try:
            state["ticks"] = state["ticks"] + 1
            while stream.DataAvailable:
                n = stream.Read(buf, 0, 4096)
                if n <= 0:
                    break
                state["pending"].extend([int(buf[i]) for i in range(n)])
            if len(state["pending"]) > state["max_pending"]:
                state["max_pending"] = len(state["pending"])
            state["credit"] = state["credit"] + bytes_per_tick
            if not state["pending"] and state["credit"] > MAX_CREDIT:
                state["credit"] = MAX_CREDIT
            n_out = int(state["credit"])
            if n_out > len(state["pending"]):
                n_out = len(state["pending"])
            if n_out > 0:
                out = state["pending"][:n_out]
                del state["pending"][:n_out]
                state["credit"] = state["credit"] - n_out
                for b in out:
                    iuart.WriteChar(b)
                state["injected"] = state["injected"] + n_out
        except Exception as e:
            state["errors"] = state["errors"] + 1
            uart.ErrorLog("uart_rx_paced: {0}".format(e))
        machine.ScheduleAction(TimeInterval.FromMicroseconds(TICK_US), Action[TimeInterval](_tick), "uart_rx_paced")

    machine.ScheduleAction(TimeInterval.FromMicroseconds(TICK_US), Action[TimeInterval](_tick), "uart_rx_paced")
    _active.append((client, stream, _tick, iuart, state, buf))
    uart.InfoLog("uart_rx_paced: connected to {0}:{1}, {2} baud in virtual time".format(host, port, baud))
    return 0


def mc_uart_rx_paced_stats():
    for i, entry in enumerate(_active):
        s = entry[4]
        print("uart_rx_paced[{0}]: injected={1} pending={2} max_pending={3} ticks={4} errors={5}".format(i, s["injected"], len(s["pending"]), s["max_pending"], s["ticks"], s["errors"]))
    return 0
