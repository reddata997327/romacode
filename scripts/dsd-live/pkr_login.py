#!/usr/bin/env python3
"""pkr98.com login via reversed app-layer crypto.

Scheme (recovered from the site's JS bundle):
  token  = static site token (from the site's index.html config; redacted
           here — supply it via the DSD_SITE_TOKEN env var)
  key(k1)= md5(token + md5(token)).hex()[2:20]              # wA() in the bundle
  body   = CryptoJS.AES.encrypt(JSON(payload), key).toString()
         = base64("Salted__" + 8-byte salt + AES-256-CBC(PKCS7, EvpKDF-MD5-1-iter key+iv))
  POST   = https://pkr98.com/api/member/login  {"encryptString": body}
"""
import hashlib, json, os, sys, base64, time
import requests

TOKEN = os.environ.get("DSD_SITE_TOKEN", "REDACTED")
LOGIN_URL = "https://pkr98.com/api/member/login"

def k1_key(token: str) -> str:
    inner = hashlib.md5(token.encode()).hexdigest()
    return hashlib.md5((token + inner).encode()).hexdigest()[2:20]

def evp_bytes_to_key(password: bytes, salt: bytes, key_len=32, iv_len=16):
    """CryptoJS EvpKDF default: MD5, 1 iteration."""
    d = b""
    prev = b""
    while len(d) < key_len + iv_len:
        prev = hashlib.md5(prev + password + salt).digest()
        d += prev
    return d[:key_len], d[key_len:key_len + iv_len]

def pkcs7_pad(data: bytes) -> bytes:
    n = 16 - len(data) % 16
    return data + bytes([n]) * n

def cryptojs_encrypt(plaintext: str, passphrase: str) -> str:
    salt = os.urandom(8)
    key, iv = evp_bytes_to_key(passphrase.encode(), salt)
    from Crypto.Cipher import AES
    ct = AES.new(key, AES.MODE_CBC, iv).encrypt(pkcs7_pad(plaintext.encode()))
    return base64.b64encode(b"Salted__" + salt + ct).decode()

def build_headers(extra=None):
    h = {
        "Content-Type": "application/json",
        "Origin": "https://pkr98.com",
        "Referer": "https://pkr98.com/",
        "User-Agent": "Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Mobile Safari/537.36",
        "token": TOKEN,
        "version": "9",
        "language": "en",
        "platformType": "5",
        "deviceType": "2",
        "Accept": "application/json, text/plain, */*",
    }
    if extra:
        h.update(extra)
    return h

def attempt(username, password, url=LOGIN_URL, payload_extra=None, verbose=True):
    payload = {
        "username": username,
        "password": password,
        "language": "en",
        "platformType": 5,
        "deviceType": 2,
        "version": 9,
        "domain": "pkr98.com",
        "networkType": "wifi",
        "deviceId": "dsd-live-test-01",
    }
    if payload_extra:
        payload.update(payload_extra)
    key = k1_key(TOKEN)
    enc = cryptojs_encrypt(json.dumps(payload, separators=(",", ":"), ensure_ascii=False), key)
    body = {"encryptString": enc}
    t0 = time.time()
    r = requests.post(url, json=body, headers=build_headers(), timeout=20)
    ms = (time.time() - t0) * 1000
    if verbose:
        print(f"-> POST {url}")
        print(f"   key(k1)={key}  enc[:48]={enc[:48]}...")
        print(f"<- HTTP {r.status_code}  {ms:.0f}ms  body[:400]={r.text[:400]}")
    return r.status_code, r.text, ms

if __name__ == "__main__":
    u = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("DSD_ACCOUNT", "REDACTED-ACCOUNT")
    p = sys.argv[2] if len(sys.argv) > 2 else os.environ.get("DSD_PASSWORD", "REDACTED-PASSWORD")
    attempt(u, p)
