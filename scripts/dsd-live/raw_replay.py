#!/usr/bin/env python3
"""Raw-socket replay through the tap; saves the client-bound stream so the
chunk framing can be checked byte by byte."""
import json
import socket

cap = json.load(open("/home/z/my-project/scripts/dsd-live/captured_login.json"))
head = cap["body"].encode()
req = (
    f"POST /hall/api/member/login HTTP/1.1\r\n"
    f"Host: a898rkp.pkr898.com\r\n"
).encode()
for k, v in cap["h"].items():
    if k.lower() in ("content-length", "transfer-encoding", "host"):
        continue
    req += f"{k}: {v}\r\n".encode()
req += f"Content-Length: {len(head)}\r\n\r\n".encode() + head

s = socket.create_connection(("127.0.0.1", 8807), timeout=20)
s.sendall(req)
out = b""
try:
    while True:
        d = s.recv(65536)
        if not d:
            break
        out += d
        if len(out) > 300000:
            break
except socket.timeout:
    print("timeout with", len(out), "bytes")
s.close()

open("/home/z/my-project/scripts/dsd-live/client_view.bin", "wb").write(out)
i = out.find(b"\r\n\r\n")
print("resp head ends at", i)
print(out[: i + 4][:120])
body = out[i + 4 :]
print("body bytes:", len(body))
print("first 16 body bytes:", body[:16])
# walk chunk framing
pos = 0
n = 0
try:
    while n < 40:
        j = body.find(b"\r\n", pos)
        if j < 0:
            print(f"chunk {n}: no size-line terminator at pos {pos}; tail={body[pos:pos+24]!r}")
            break
        size = int(body[pos:j].split(b";")[0], 16)
        n += 1
        pos = j + 2 + size + 2
        if size == 0:
            print(f"chunk {n}: FINAL at {pos}, ok")
            break
        if pos > len(body):
            print(f"chunk {n}: size {size:x} but stream ended")
            break
except Exception as e:
    print("framing walk failed at chunk", n, "pos", pos, ":", e)
    print("bytes around failure:", body[max(0, pos - 24) : pos + 48])
print("walked", n, "chunks cleanly")
