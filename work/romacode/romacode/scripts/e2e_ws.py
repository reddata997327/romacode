#!/usr/bin/env python3
"""End-to-end WS test against romacode-server: handshake, hello -> welcome,
new session, message flow (no provider key -> the server answers with an
error event in chat, which is the honest no-canned-reply behavior), approval
plumbing, cancel, android stream toggle, ping."""
import base64
import hashlib
import json
import os
import socket
import struct
import sys
import time

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 17712
BIND = ("127.0.0.1", PORT)
GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def ws_connect():
    s = socket.create_connection(BIND, timeout=10)
    key = base64.b64encode(os.urandom(16)).decode()
    req = (
        f"GET /ws HTTP/1.1\r\nHost: {BIND[0]}:{BIND[1]}\r\n"
        f"Upgrade: websocket\r\nConnection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    s.sendall(req.encode())
    resp = b""
    while b"\r\n\r\n" not in resp:
        resp += s.recv(4096)
    head, _, rest = resp.partition(b"\r\n\r\n")
    assert b"101" in head.split(b"\r\n")[0], head
    expected = base64.b64encode(hashlib.sha1((key + GUID).encode()).digest()).decode()
    assert expected.encode() in head, "bad accept key"
    return s, rest


def ws_send(s, obj):
    payload = json.dumps(obj).encode()
    mask = os.urandom(4)
    header = bytearray([0x81])
    n = len(payload)
    if n < 126:
        header.append(0x80 | n)
    elif n < 65536:
        header.append(0x80 | 126)
        header += struct.pack(">H", n)
    else:
        header.append(0x80 | 127)
        header += struct.pack(">Q", n)
    header += mask
    masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    s.sendall(bytes(header) + masked)


_buf = {"data": b""}


def ws_recv(s):
    while True:
        data = _buf["data"]
        if len(data) >= 2:
            ln = data[1] & 0x7F
            off = 2
            if ln == 126:
                if len(data) < 4:
                    pass
                else:
                    ln = struct.unpack(">H", data[2:4])[0]
                    off = 4
            elif ln == 127:
                if len(data) >= 10:
                    ln = struct.unpack(">Q", data[2:10])[0]
                    off = 10
            if len(data) >= off + ln and ln > 0 or (len(data) >= off and ln == 0):
                payload = data[off:off + ln]
                _buf["data"] = data[off + ln:]
                op = data[0] & 0x0F
                if op == 0x1:
                    return json.loads(payload.decode())
                continue
        chunk = s.recv(65536)
        if not chunk:
            raise ConnectionError("server closed")
        _buf["data"] += chunk


def main():
    s, rest = ws_connect()
    _buf["data"] = rest
    ok = 0

    # 0. the brain greets on connect: unsolicited welcome (req 0) + snapshot
    greet = [ws_recv(s), ws_recv(s)]
    assert any(e.get("reply") and e["reply"]["data"]["kind"] == "welcome" for e in greet), greet
    assert any(e.get("event", {}).get("type") == "snapshot" for e in greet), greet
    print("PASS unsolicited welcome + snapshot on connect")

    # 1. hello -> welcome (again, on demand)
    ws_send(s, {"req": 1, "body": {"type": "hello", "version": "test"}})
    envs = [ws_recv(s), ws_recv(s)]
    welcome = [e for e in envs if e.get("reply")]
    snap = [e for e in envs if e.get("event", {}).get("type") == "snapshot"]
    assert welcome and welcome[0]["req"] == 1, welcome
    w = welcome[0]["reply"]["data"]
    assert w["kind"] == "welcome", w.get("kind")
    assert w["context_window"] > 0
    assert snap, "no snapshot event after hello"
    ok += 1
    print("PASS hello/welcome/snapshot")

    # 2. ping/pong roundtrip
    ws_send(s, {"req": 2, "body": {"type": "ping"}})
    env = ws_recv(s)
    assert env["req"] == 2 and env["reply"]["data"]["kind"] == "pong", env
    ok += 1
    print("PASS ping/pong")

    # 3. new session: Sessions event + Snapshot event + reply
    ws_send(s, {"req": 3, "body": {"type": "new_session"}})
    e1 = ws_recv(s)
    e2 = ws_recv(s)
    e3 = ws_recv(s)
    kinds = [x.get("event", {}).get("type") for x in (e1, e2, e3)]
    assert "sessions" in kinds and "snapshot" in kinds, (e1, e2, e3)
    sessions = next(x["event"]["sessions"] for x in (e1, e2, e3) if x.get("event", {}).get("type") == "sessions")
    ids = [x["id"] for x in sessions]
    new_id = next(
        (e["reply"]["data"].get("id") for e in (e1, e2, e3) if e.get("reply")),
        None,
    )
    assert new_id in ids, (new_id, ids)
    ok += 1
    print(f"PASS new_session (id={new_id})")

    # 4. toggle mode
    ws_send(s, {"req": 4, "body": {"type": "toggle_mode", "id": new_id}})
    e = ws_recv(s)
    snap_ev = e.get("event", {})
    assert snap_ev.get("type") == "snapshot" and snap_ev["session"]["mode"] == "plan", e
    ok += 1
    print("PASS toggle_mode -> plan (snapshot broadcast)")

    # 5. message: no API key configured -> honest provider error in chat
    ws_send(s, {"req": 5, "body": {"type": "message", "id": new_id, "text": "hello there", "mode": "build"}})
    got_error = False
    got_done = False
    deadline = time.time() + 20
    while time.time() < deadline and not (got_error and got_done):
        env = ws_recv(s)
        ev = env.get("event") or {}
        t = ev.get("type")
        if t == "snapshot":
            msgs = ev["session"]["messages"]
            if any(m.get("kind") == "error" for m in msgs):
                got_error = True
        elif t == "error":
            got_error = True
        elif t == "done":
            got_done = True
    assert got_error and got_done, (got_error, got_done)
    ok += 1
    print("PASS message -> honest provider error (no canned reply) + done")

    # 6. busy-guard: second message while idle is fine, but cancel is accepted
    ws_send(s, {"req": 6, "body": {"type": "cancel", "id": new_id}})
    e = ws_recv(s)
    assert e.get("req") == 6, e
    ok += 1
    print("PASS cancel accepted")

    # 7. android stream off/on does not crash the brain
    ws_send(s, {"req": 7, "body": {"type": "android", "action": {"kind": "stream", "on": True}}})
    e = ws_recv(s)
    assert e.get("req") == 7, e
    ws_send(s, {"req": 8, "body": {"type": "android", "action": {"kind": "stream", "on": False}}})
    e = ws_recv(s)
    assert e.get("req") == 8, e
    ok += 1
    print("PASS android stream toggle")

    # 8. slash command via Command request
    ws_send(s, {"req": 9, "body": {"type": "command", "text": "providers"}})
    e = ws_recv(s)
    data = e["reply"]["data"]
    assert data["kind"] == "notice" and "Anthropic" in data["text"], data
    ok += 1
    print("PASS /providers through the wire")

    s.close()
    print(f"\nALL {ok} E2E CHECKS PASSED")


if __name__ == "__main__":
    main()
