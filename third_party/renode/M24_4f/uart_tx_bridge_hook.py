#
# M24.4f -- UART TX-byte forwarding hook (third_party/renode/M24_4f_REPORT.md).
#
# Registers a handler on the given UART peripheral's own `CharReceived` event (the same event
# Renode's own bundled `scripts/monitor.py::mc_uart_connect` uses to mirror guest output to a
# terminal -- `uart.CharReceived += <fn(b)>`, read directly from that file, not guessed) that
# forwards every byte the GUEST transmits to a plain TCP socket this script connects OUT to,
# owned and listened-on by the Python bridge (`renode_bridge.py`), not by Renode. Delivery
# therefore does not depend on any Renode terminal/backend's own buffering, flush-on-dispose, or
# forwarding-pump behavior (the `CreateServerSocketTerminal` defect M24.4e root-caused and
# evidenced with a live GDB-remote attach plus an independent `CreateFileBackend` capture) -- the
# byte is handed straight from the peripheral's own C# event dispatch, synchronously, on the same
# call that fires it, directly into an outbound socket write.
#
# Included the same way `scripts/single-node/segger-rtt.py` already is in this Renode
# distribution (`include @<path>.py` from a `.resc`, confirmed by reading `ek-ra2e1.resc`) -- not
# a new pattern this task invented.
#
# Host->guest stays on `uart.WriteChar(...)` (called from the bridge's own Python process over
# the ordinary monitor connection, exactly as M24.4b already proved) -- this file does not touch
# that direction at all.

try:
    clr.AddReference("System")
except Exception:
    pass
try:
    clr.AddReference("System.Net.Sockets")
except Exception:
    pass

from System.Net.Sockets import TcpClient
from System import Array, Byte
from Antmicro import Renode

# Module-level list: keeps every (client, stream, handler, uart) tuple alive for the life of the
# Renode process. Without this, nothing outside this module holds a Python-side reference to the
# TcpClient/handler closure once `mc_setup_uart_tx_bridge` returns, and IronPython's own GC could
# collect them -- silently un-wiring the forwarding with no error anywhere, exactly the class of
# failure this task exists to get away from.
_active_bridges = []


def mc_setup_uart_tx_bridge(uart, port, host="127.0.0.1"):
    """Monitor macro: `setup_uart_tx_bridge sysbus.uart1 <port>`. Connects out, as a plain TCP
    client, to `host:port` -- a socket the CALLER (renode_bridge.py) already has bound and
    listening before this line executes -- and forwards every byte `uart`'s own `CharReceived`
    event reports, one `Write`+`Flush` per byte, from here on until the Renode process exits."""
    iuart = clr.Convert(uart, Renode.Peripherals.UART.IUART)

    client = TcpClient()
    client.NoDelay = True
    client.Connect(host, int(port))
    stream = client.GetStream()

    state = {"count": 0, "errors": 0}

    def _forward(b):
        try:
            buf = Array[Byte]([b & 0xFF])
            stream.Write(buf, 0, 1)
            stream.Flush()
            state["count"] = state["count"] + 1
        except Exception as e:
            state["errors"] = state["errors"] + 1
            uart.ErrorLog("uart_tx_bridge: forwarding byte failed: {0}".format(e))

    iuart.CharReceived += _forward
    _active_bridges.append((client, stream, _forward, iuart, state))
    uart.InfoLog("uart_tx_bridge: connected to {0}:{1}, forwarding CharReceived".format(host, port))
    return 0


def mc_uart_tx_bridge_stats():
    """Diagnostic monitor macro: prints how many bytes each registered hook has forwarded and how
    many forwarding errors it hit -- read live over the monitor connection during a run, not
    inferred from an absence of errors."""
    for i, (_client, _stream, _fn, iuart, state) in enumerate(_active_bridges):
        print("uart_tx_bridge[{0}]: forwarded={1} errors={2}".format(i, state["count"], state["errors"]))
    return 0
