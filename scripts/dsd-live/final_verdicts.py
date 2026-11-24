#!/usr/bin/env python3
"""Final verdict pair through the custom dsd-live tap:
YES = captured login replayed verbatim (correct password)
NO  = identical payload, userpass replaced with a wrong password,
      encrypted with the fully reversed scheme (AES-128-CBC, key=k1[:16], iv=5421698523412578)
Both responses are decrypted with the same scheme."""
import base64
import json
import sys
import time
import uuid

import requests
from Crypto.Cipher import AES

sys.path.insert(0, "/home/z/my-project/scripts/dsd-live")
from pkr_login import k1_key, TOKEN

KEY16 = k1_key(TOKEN).encode()[:16]
IV = open("/home/z/my-project/scripts/dsd-live/pkr_iv.bin", "rb").read()
TAP = "http://127.0.0.1:8807"


def enc(plaintext: str) -> str:
    b = plaintext.encode()
    b += bytes([16 - len(b) % 16]) * (16 - len(b) % 16)
    return base64.b64encode(AES.new(KEY16, AES.MODE_CBC, IV).encrypt(b)).decode()


def dec(resp_text: str) -> str:
    try:
        obj = json.loads(resp_text)
    except Exception:
        return resp_text[:250]
    if isinstance(obj, dict) and "encryptString" in obj:
        raw = base64.b64decode(obj["encryptString"])
        pt = AES.new(KEY16, AES.MODE_CBC, IV).decrypt(raw)
        pt = pt[: -pt[-1]] if 0 < pt[-1] <= 16 else pt
        return pt.decode("utf-8", "replace")
    return json.dumps(obj)[:250]


cap = json.load(open("/home/z/my-project/scripts/dsd-live/captured_login.json"))
path = "/hall/api/member/login"
base_headers = {k: v for k, v in cap["h"].items()}
base_headers["Host"] = "a898rkp.pkr898.com"

raw_pt = AES.new(KEY16, AES.MODE_CBC, IV).decrypt(
    base64.b64decode(json.loads(cap["body"])["encryptString"])
)
if 0 < raw_pt[-1] <= 16:
    raw_pt = raw_pt[: -raw_pt[-1]]
payload = json.JSONDecoder().raw_decode(raw_pt.decode().strip())[0]

# ── YES: correct password, fresh request ─────────────────────────────────────
h = dict(base_headers)
h["x-request-id"] = str(uuid.uuid4())
h["timestamp"] = str(int(time.time()))
yes_payload = dict(payload)
yes_payload["time"] = int(time.time())
r = requests.post(TAP + path, data=json.dumps({"encryptString": enc(json.dumps(yes_payload, separators=(",", ":")))}).encode(), headers=h, timeout=25)
print(f"YES-ATTEMPT (correct password): HTTP {r.status_code}")
print("  verdict:", dec(r.text)[:300])

# ── NO: wrong password, everything else identical ────────────────────────────
h2 = dict(base_headers)
h2["x-request-id"] = str(uuid.uuid4())
h2["timestamp"] = str(int(time.time()))
no_payload = dict(payload)
no_payload["time"] = int(time.time())
no_payload["userpass"] = "Nope12345!"
r2 = requests.post(TAP + path, data=json.dumps({"encryptString": enc(json.dumps(no_payload, separators=(",", ":")))}).encode(), headers=h2, timeout=25)
print(f"NO-ATTEMPT (wrong password):  HTTP {r2.status_code}")
print("  verdict:", dec(r2.text)[:300])
