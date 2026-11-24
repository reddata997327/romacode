#!/usr/bin/env python3
"""Find the exact AES-128-CBC IV used by pkr98's crypto layer:
- sweep IV candidates against the response (block 1 should become valid JSON)
- then decrypt the captured REQUEST payload with the winning scheme
- print the exact login field schema for the wrong-password rebuild."""
import base64
import hashlib
import json

from Crypto.Cipher import AES

from pkr_login import k1_key, TOKEN

KEY16 = k1_key(TOKEN).encode()[:16]
IVS = {
    "zero": b"\x00" * 16,
    "md5(k1)[:16]": hashlib.md5(k1_key(TOKEN).encode()).digest()[:16],
    "md5(k1)hex[:16]": hashlib.md5(k1_key(TOKEN).encode()).hexdigest()[:16].encode(),
    "k1hex[2:18]": k1_key(TOKEN).encode()[2:18],
    "key-self": KEY16,
    "TOKEN[:16]": TOKEN.encode()[:16],
    "md5(TOKEN)[:16]": hashlib.md5(TOKEN.encode()).digest()[:16],
}

cv = open("/home/z/my-project/scripts/dsd-live/client_view.bin", "rb").read()
i = cv.find(b"\r\n\r\n")
body = cv[i + 4 :]
pos, chunks = 0, []
while True:
    j = body.find(b"\r\n", pos)
    if j < 0:
        break
    try:
        size = int(body[pos:j].split(b";")[0], 16)
    except ValueError:
        break
    if size == 0:
        break
    chunks.append(body[j + 2 : j + 2 + size])
    pos = j + 2 + size + 2
resp = json.loads(b"".join(chunks).decode())
raw = base64.b64decode(resp["encryptString"])

print("=== IV sweep on response (want block1 = {\"code\"...) ===")
win = None
for name, iv in IVS.items():
    pt = AES.new(KEY16, AES.MODE_CBC, iv).decrypt(raw)
    head = pt[:24]
    if head.startswith(b"{") or b"code" in head:
        print(f"*** HIT iv={name}: {pt[:80]!r}")
        win = (name, iv, pt)
    else:
        print(f"    iv={name}: {head!r}")

if win:
    name, iv, pt = win
    pt = pt[: -pt[-1]] if 0 < pt[-1] <= 16 else pt
    print(f"\nWINNER: iv={name}")
    print("full decrypted response:")
    print(pt.decode("utf-8", "replace")[:900])
    open("/home/z/my-project/scripts/dsd-live/winner_iv.txt", "w").write(name)

cap = json.load(open("/home/z/my-project/scripts/dsd-live/captured_login.json"))
enc_req = base64.b64decode(json.loads(cap["body"])["encryptString"])
print("\n=== captured REQUEST decrypt with same scheme ===")
for name, iv in IVS.items():
    try:
        pt = AES.new(KEY16, AES.MODE_CBC, iv).decrypt(enc_req)
        if 0 < pt[-1] <= 16:
            pt = pt[: -pt[-1]]
        s = pt.decode("utf-8", "replace")
        if s.startswith("{"):
            print(f"*** HIT iv={name}:")
            print(json.dumps(json.loads(s), indent=1)[:700])
            break
    except Exception:
        pass
else:
    print("no iv hit for request — raw tail:", enc_req[:16])
