#!/usr/bin/env python3
"""M24.4e -- raw probe of Renode's GDB-remote stub on port 15900, to see exactly what bytes it
sends on connect and how it replies to a manually-crafted qSupported packet, since lldb's own
gdb-remote handshake failed with 'timeout of 0.0 seconds' -- checking whether that is an lldb-side
setting problem or a real protocol mismatch on Renode's side."""
import socket
import time
import sys

port = int(sys.argv[1]) if len(sys.argv) > 1 else 15900
s = socket.create_connection(("127.0.0.1", port), timeout=5)
s.settimeout(3)
try:
    data = s.recv(4096)
    print("initial recv:", data)
except Exception as e:
    print("no initial data:", e)

pkt = b"$qSupported#37"
s.sendall(pkt)
time.sleep(0.5)
try:
    data = s.recv(4096)
    print("after qSupported:", data)
except Exception as e:
    print("no reply to qSupported:", e)

# Try the classic ack byte then a '?' status query
s.sendall(b"+")
s.sendall(b"$?#3f")
time.sleep(0.5)
try:
    data = s.recv(4096)
    print("after ?:", data)
except Exception as e:
    print("no reply to ?:", e)

s.close()
