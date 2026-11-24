#!/usr/bin/env python3
"""Local test site for the RomaCode browser-tool live test.

Serves on 127.0.0.1:8642:
  /            -> index.html (loads /app.js, fetches /data.json, reads cookie)
  /app.js      -> sets window.ROMACODE_LIVE = {...} proof of JS execution
  /data.json   -> JSON payload the page fetches (backend -> frontend flow)
Every request is logged to <logpath> so the test can prove the engine really
reached the server. Set-Cookie: romacode=live is sent on every response.
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

LOG = sys.argv[2] if len(sys.argv) > 2 else "/tmp/browser-live-site.log"

INDEX = b"""<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>RomaCode Live Test Page</title>
<link rel="stylesheet" href="/style.css"></head>
<body>
<h1 id="headline">RomaCode + Servo live integration</h1>
<p id="counter">waiting for JS</p>
<script src="/app.js"></script>
</body>
</html>
"""

APP_JS = b"""fetch('/data.json').then(r => r.json()).then(d => {
  window.ROMACODE_LIVE = {
    js_executed: true,
    payload_received: d,
    cookie_seen: document.cookie,
    user_agent: navigator.userAgent
  };
  document.getElementById('counter').textContent =
    'JS ran; payload: ' + d.message;
});
"""

DATA = json.dumps({"message": "hello from the local backend", "n": 42}).encode()
STYLE = b"h1 { color: purple; }\n"


class Site(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        with open(LOG, "a") as f:
            f.write(fmt % args + "\n")

    def _send(self, code, body, ctype):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Set-Cookie", "romacode=live; Path=/")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        with open(LOG, "a") as f:
            f.write(f"GET {self.path} UA={self.headers.get('User-Agent','?')}\n")
        if self.path == "/":
            self._send(200, INDEX, "text/html; charset=utf-8")
        elif self.path == "/app.js":
            self._send(200, APP_JS, "application/javascript")
        elif self.path == "/style.css":
            self._send(200, STYLE, "text/css")
        elif self.path == "/data.json":
            self._send(200, DATA, "application/json")
        else:
            self._send(404, b"not found", "text/plain")


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8642
    open(LOG, "w").close()
    HTTPServer(("127.0.0.1", port), Site).serve_forever()
