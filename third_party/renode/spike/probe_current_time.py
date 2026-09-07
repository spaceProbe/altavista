#!/usr/bin/env python3
"""One-off probe: what does the `currentTime` monitor command print, once a machine with
a running CPU exists? Used only to decide the parsing format for measure_virtual_time.py;
not itself a spike measurement."""
import re
import socket
import sys
import time

HOST = "127.0.0.1"
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 9999
SETUP_RESC = "/Users/probe/code/AltaVista/third_party/renode/spike/setup.resc"

s = socket.create_connection((HOST, PORT), timeout=5)
s.settimeout(3)


def read_until_idle(gap=0.3):
    chunks = []
    while True:
        try:
            data = s.recv(65536)
            if not data:
                break
            chunks.append(data)
        except socket.timeout:
            break
    return b"".join(chunks)


read_until_idle()
s.sendall(f"include @{SETUP_RESC}\r\n".encode())
out = read_until_idle(1.0)
print("--- after include ---")
print(re.sub(rb"\x1b\[[0-9;]*m", b"", out).decode(errors="replace"))

s.sendall(b'emulation RunFor "0.1"; currentTime\r\n')
out = read_until_idle(1.0)
print("--- after RunFor+currentTime ---")
print(re.sub(rb"\x1b\[[0-9;]*m", b"", out).decode(errors="replace"))

s.close()
