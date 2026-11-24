#!/usr/bin/env python3
"""Replay the pkr98.com login through the custom dsd-live tap.

Request A: the browser-captured login (headers + encryptString body verbatim)
           -> expected: DB verdict YES.
Request B: same request, freshly rebuilt encryptString for a WRONG password
           -> expected: DB verdict NO.
Response bodies are decrypted with the reversed k1 crypto (same key both ways).
The tap prints [ENTRY]/[EXIT] lines with live timing for every hop.
"""
import base64
import hashlib
import json
import os
import sys
import time
import uuid

import requests
from Crypto.Cipher import AES

sys.path.insert(0, "/home/z/my-project/scripts/dsd-live")
from pkr_login import k1_key, evp_bytes_to_key, pkcs7_pad, cryptojs_encrypt, TOKEN

TAP = "http://127.0.0.1:8807"
UPSTREAM = "https://a898rkp.pkr898.com"
KEY = k1_key(TOKEN)

def evp_decrypt(b64: str, passphrase: str) -> str | None:
    try:
        raw = base64.b64decode(b64)
        if not raw.startswith(b"Salted__"):
            return None
        salt = raw[8:16]
        ct = raw[16:]
        key, iv = evp_bytes_to_key(passphrase.encode(), salt)
        pt = AES.new(key, AES.MODE_CBC, iv).decrypt(ct)
        return pkcs7_strip(pt).decode("utf-8", "replace")
    except Exception as e:
        return None

def pkcs7_strip(b: bytes) -> bytes:
    return b[: -b[-1]] if b and 0 < b[-1] <= 16 else b

def decrypt_response(resp_text: str) -> str:
    try:
        obj = json.loads(resp_text)
    except Exception:
        return "(not json) " + resp_text[:200]
    if isinstance(obj, dict) and "encryptString" in obj:
        pt = evp_decrypt(obj["encryptString"], KEY)
        if pt:
            return "decrypted: " + pt[:500]
        return "(decrypt failed) " + resp_text[:200]
    return json.dumps(obj)[:300]

def send(label, url_path, headers, body_bytes):
    t0 = time.time()
    r = requests.post(TAP + url_path, data=body_bytes, headers=headers, timeout=25)
    ms = (time.time() - t0) * 1000
    print(f"\n=== {label}: HTTP {r.status_code} in {ms:.0f}ms (via custom tap) ===")
    print("response:", decrypt_response(r.text)[:600])
    return r

# ── Request A: captured login verbatim (YES expected) ────────────────────────
cap = json.load(open("/home/z/my-project/scripts/dsd-live/captured_login.json"))
path = cap["u"].replace(UPSTREAM, "")
hdrs = {k: v for k, v in cap["h"].items()}
hdrs["Host"] = UPSTREAM.replace("https://", "")
body = cap["body"].encode()
print("A: replaying captured login (correct password, headers verbatim)")
send("YES-ATTEMPT", path, hdrs, body)

# ── Request B: wrong password, freshly encrypted (NO expected) ───────────────
payload = {
    "username": "REDACTED-ACCOUNT",
    "password": "DefinitelyWrong999!",
    "language": "en",
    "platformType": 5,
    "deviceType": 2,
    "version": 9,
    "domain": "pkr98.com",
    "networkType": "wifi",
    "deviceId": "dsd-live-test-01",
}
enc = cryptojs_encrypt(json.dumps(payload, separators=(",", ":")), KEY)
hdrs2 = dict(hdrs)
hdrs2["x-request-id"] = str(uuid.uuid4())
hdrs2["timestamp"] = str(int(time.time()))
print("\nB: wrong-password login, freshly encrypted with reversed k1 crypto")
send("NO-ATTEMPT", path, hdrs2, json.dumps({"encryptString": enc}).encode())
print("\n(replay finished; tap timeline lines are above in the tap output)")
