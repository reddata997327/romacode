# The `browser` tool — a real browser engine under the agent's control

RomaCode embeds **Servo** (the Rust browser engine) as a first-class, fully
 drivable component. The agent launches the engine headlessly, keeps a
persistent WebDriver session, and gets **full frontend and backend access**
to any page:

| Side | Actions | What the agent sees |
|---|---|---|
| Frontend | `screenshot`, `dom`, `title`, `url` | what the user would see (real compositor PNG, element HTML) |
| Backend | `eval`, `network`, `cookies` | arbitrary JavaScript in the page, every resource load with timings/sizes, cookie jar |
| Navigation | `navigate`, `back`, `forward`, `refresh` | any URL — localhost, private ranges, public internet; no allowlists |
| Session | `start`, `restart`, `status`, `stop` | engine lifecycle control |

## Quick start

1. **Get an engine.** Any of these works:
   - download a nightly: <https://servo.org/download/> (Linux/macOS/Windows/Android)
   - build one: `./mach build` in a Servo checkout → `target/release/servo`
2. **Point RomaCode at it** (one of):
   - `config.json`: `"browser": { "binary": "/path/to/servoshell" }`
   - environment: `ROMACODE_SERVO=/path/to/servoshell`
   - vendored install: `<data_dir>/romacode/servo/bin/servo`
   - or just put `servo`/`servoshell` on `PATH`
3. **Use it.** The engine starts automatically on the agent's first
   `browser` action (headless by default) and stays up until `stop`.

## How it works

- The engine is spawned with `--headless --webdriver <free-port>` plus any
  `browser.extra_args`.
- RomaCode talks to the engine over the standard **W3C WebDriver** protocol
  (`/status`, `/session`, `/url`, `/execute/sync`, `/screenshot`,
  `/cookies`, …) using a dependency-free HTTP client
  (`crates/server/src/browser/webdriver.rs`) — no external binaries, no
  selenium, no chromedriver.
- Screenshots are decoded from the engine and written under
  `browser-screenshots/` in the project, so the agent can `Read` them.
- `eval` scripts run as a W3C function body: `return document.title` hands a
  value back; arguments travel as `arguments[0]`, never spliced into code.
- If a session dies with the engine, the next call relaunches automatically.

## Configuration (`config.json` → `browser`)

```jsonc
{
  "enabled": true,              // offer the tool to the agent
  "binary": "",                 // engine path; empty = auto-resolve (see above)
  "host": "127.0.0.1",          // WebDriver interface
  "port": 0,                    // 0 = free ephemeral port per launch
  "headless": true,             // no visible window
  "accept_insecure_certs": true,// best-effort; engines may decline
  "auto_start": true,           // launch on first action
  "start_timeout_secs": 90,     // engine boot budget
  "timeout_secs": 120,          // per-WebDriver-call budget
  "extra_args": [],             // passed to the engine verbatim
  "max_output_chars": 200000    // tool-output cap
}
```

No URL filtering, no domain lists, no private-host blocks — the policy is
fully open by design, like every other RomaCode tool.

## Verification

Two test layers ship in the repo:

- `cargo test -p romacode-server browser::` — the whole tool path against an
  in-process mock WebDriver server (fast, no engine needed).
- `cargo test -p romacode-server --test browser_live -- --ignored` —
  end-to-end against a **real** engine (set `ROMACODE_SERVO`). It starts a
  local HTTP server, navigates the engine to it, and asserts title, DOM,
  JS-executed page state (fetch + cookie), Performance-API network entries,
  and a real compositor PNG.

## Engine runtime notes (Linux headless)

A nightly `servoshell` needs its shared libraries. On a headless server the
usual recipe is:

```sh
apt-get download libgstreamer-plugins-bad1.0-0 libgstreamer-gl1.0-0 \
  libgstreamer1.0-0 libgstreamer-plugins-base1.0-0 libegl1 libegl-mesa0 \
  libgl1-mesa-dri libglx-mesa0 mesa-libgallium libosmesa6
for d in *.deb; do dpkg -x "$d" ~/servo-libs; done   # no root needed
export LD_LIBRARY_PATH=~/servo-libs/usr/lib/x86_64-linux-gnu:~/servo-libs/usr/lib/x86_64-linux-gnu/dri
export XDG_RUNTIME_DIR=/tmp/xdg LIBGL_ALWAYS_SOFTWARE=1
export __EGL_VENDOR_LIBRARY_FILENAMES=~/servo-libs/usr/share/glvnd/egl_vendor.d/50_mesa.json
./servoshell --headless --webdriver 7000   # smoke test
```

## Tool schema (what the model gets)

Action enum: `navigate · eval · dom · screenshot · network · cookies ·
title · url · back · forward · refresh · status · start · restart · stop`,
with `url`, `script` (+ optional `args`), and `selector` parameters.
