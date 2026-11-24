#!/usr/bin/env python3
"""Derive the CBC IV from known plaintext (block1 = {"code":1,"msg":),
then decrypt the captured REQUEST payload with the same key+IV."""
import base64
import json

from Crypto.Cipher import AES

from pkr_login import k1_key, TOKEN

KEY16 = k1_key(TOKEN).encode()[:16]
P1 = b'{"code":1,"msg":'  # 16 bytes, confirmed by block2 starting with '"'

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

ecb = AES.new(KEY16, AES.MODE_ECB)
iv = bytes(a ^ b for a, b in zip(ecb.decrypt(raw[:16]), P1))
print("derived IV:", iv.hex(), "ascii:", iv)

pt = AES.new(KEY16, AES.MODE_CBC, iv).decrypt(raw)
pt = pt[: -pt[-1]] if 0 < pt[-1] <= 16 else pt
print("\ndecrypted RESPONSE (fresh login through our tap):")
print(pt.decode("utf-8", "replace")[:800])
open("/home/z/my-project/scripts/dsd-live/pkr_iv.bin", "wb").write(iv)

cap = json.load(open("/home/z/my-project/scripts/dsd-live/captured_login.json"))
enc_req = base64.b64decode(json.loads(cap["body"])["encryptString"])
pt = AES.new(KEY16, AES.MODE_CBC, iv).decrypt(enc_req)
pt = pt[: -pt[-1]] if 0 < pt[-1] <= 16 else pt
print("\ndecrypted captured REQUEST payload:")
print(pt.decode("utf-8", "replace")[:700])
