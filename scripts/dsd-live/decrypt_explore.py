#!/usr/bin/env python3
"""1) Decrypt the captured login REQUEST payload (exact field schema).
2) Crypto-mode sweep over the login RESPONSE encryptString to find the
   server->client decryption variant."""
import base64
import hashlib
import json
import sys

from Crypto.Cipher import AES

sys.path.insert(0, "/home/z/my-project/scripts/dsd-live")
from pkr_login import k1_key, evp_bytes_to_key, TOKEN


def pkcs7_strip(b: bytes) -> bytes:
    return b[: -b[-1]] if b and 0 < b[-1] <= 16 else b

KEY = k1_key(TOKEN)
print("k1 key:", KEY)

cap = json.load(open("/home/z/my-project/scripts/dsd-live/captured_login.json"))
req = json.loads(cap["body"])
enc_req = req["encryptString"]
print("\n=== 1) captured request payload decrypt (EvpKDF Salted, k1) ===")
try:
    raw = base64.b64decode(enc_req)
    assert raw.startswith(b"Salted__"), f"no Salted__ prefix: {raw[:16]!r}"
    salt = raw[8:16]
    key, iv = evp_bytes_to_key(KEY.encode(), salt)
    pt = pkcs7_strip(AES.new(key, AES.MODE_CBC, iv).decrypt(raw[16:]))
    payload = json.loads(pt.decode())
    print(json.dumps(payload, indent=1)[:800])
except Exception as e:
    print("request decrypt failed:", e)

# ── response sweep ───────────────────────────────────────────────────────────
# full response encryptString captured by raw_replay (client_view.bin)
cv = open("/home/z/my-project/scripts/dsd-live/client_view.bin", "rb").read()
i = cv.find(b"\r\n\r\n")
body = cv[i + 4 :]
# walk chunk framing to reassemble
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
joined = b"".join(chunks)
print(f"\n=== 2) response sweep: {len(chunks)} chunks, {len(joined)} bytes ===")
try:
    resp = json.loads(joined.decode())
    enc_resp = resp["encryptString"]
    print("response encryptString len:", len(enc_resp))
except Exception as e:
    print("response JSON parse failed:", e)
    enc_resp = None

if enc_resp:
    raw = base64.b64decode(enc_resp)
    print("first bytes:", raw[:12])

    def show(tag, data):
        s = data.decode("utf-8", "replace")
        if "code" in s or "uid" in s or "token" in s:
            print(f"*** HIT [{tag}]:", s[:220])
        else:
            print(f"    [{tag}]:", s[:60])

    # Salted variants
    if raw[:8] == b"Salted__":
        salt = raw[8:16]
        k, iv = evp_bytes_to_key(KEY.encode(), salt)
        show("evpkdf-salted-cbc", pkcs7_strip(AES.new(k, AES.MODE_CBC, iv).decrypt(raw[16:])))
    # unsalted evpkdf (salt = empty)
    k32, iv = evp_bytes_to_key(KEY.encode(), b"")
    show("evpkdf-unsalted-cbc", pkcs7_strip(AES.new(k32, AES.MODE_CBC, iv).decrypt(raw)))
    # raw key candidates
    md5k = hashlib.md5(KEY.encode()).digest()
    for tag, k, mode in [
        ("aes128-ecb-key=k1[:16]", KEY.encode()[:16], AES.MODE_ECB),
        ("aes128-ecb-key=md5(k1)", md5k, AES.MODE_ECB),
        ("aes256-ecb-key=md5(k1)+pad", (md5k * 2), AES.MODE_ECB),
        ("aes128-cbc-key=md5(k1)-iv0", md5k, AES.MODE_CBC),
        ("aes128-cbc-key=k1[:16]-iv0", KEY.encode()[:16], AES.MODE_CBC),
        ("aes256-cbc-key=k1padded32", (KEY.encode() + b"\x00" * 14), AES.MODE_CBC),
        ("aes192-cbc-key=k1padded24", (KEY.encode() + b"\x00" * 6), AES.MODE_CBC),
    ]:
        try:
            c = AES.new(k[:32].ljust(16, b"\x00")[: len(k) or 16] if False else k, mode)
            show(tag, pkcs7_strip(c.decrypt(raw)))
        except Exception as e:
            print(f"    [{tag}]: error {e}")
