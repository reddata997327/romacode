# RomaCode

RomaCode is a Rust terminal coding assistant, rebuilt as a **client–server
workspace**: a headless *brain* (`romacode-server`) that owns the agent loop,
sessions, tools and security gate, and a *screen* (`romacode`, the TUI) that
connects to it over HTTP + WebSocket. Providers stream Anthropic- and
OpenAI-compatible APIs, the agent can fetch web pages, and an Android control
panel attaches to a real Android SDK emulator or an authorized physical device
through the official Android Platform-Tools `adb` client — RomaCode does not
boot or pretend an in-process mock is Android.

New in this restructuring: **LangChain in pure Rust is built in as a native
engine** (`crates/server/langchain`, vendored and compiled into the brain).
`agent.engine = "langchain"` runs the loop on langchain's `AgentExecutor` with
langchain memory, and ships `rag_index` / `rag_search` tools backed by
langchain's text splitter, embeddings and in-memory vector store. Every tool
— in either engine — still passes the same security gate.

Also built in by default: the **rnmap network scanner** (`rnmap_scan`) —
rnmap 0.4.0, vendored and embedded in the brain, auto-installed on first use.
Port scans, service/version detection, OS fingerprinting, traceroute and TLS
certificate probing return structured JSON. The scanner runs unrestricted:
localhost, private ranges and public internet hosts are all scannable. See
`docs/RNMAP.md`.

Also built in by default: the **Remix / Ethereum toolchain** (`remix`,
`remix_status`) — the `remix-rust` project ("rrx"), folded into this
workspace as first-party crates under `crates/remix/`: Remix IDE tools
rebuilt in pure Rust with a real EVM interpreter (Istanbul→Cancun), real MPT
state, real solc, real secp256k1 signing, and live Ethereum mainnet reads
with endpoint failover. There is no separate vendored tree and no second
build: `cargo build --release` at the repo root produces both the server and
the `rrx` binary side by side in `target/release/`, where the engine finds
it automatically (or copy it next to the shipped binary / set
`remix.binary` in the config):

```bash
cargo build --release        # builds romacode-server AND rrx together
# target/release/rrx is discovered automatically; for a packaged layout:
cp target/release/rrx bin/rrx   # next to the romacode binary
```

The agent gets the `remix` tool (52 curated commands: Solidity
compile/test/fuzz/invariant, gas, debugger, mainnet `ethcall`/`trace`/
`debugtx`/MPT `proof`, ENS, tokens, fake-asset detection) plus the
`remix_status` doctor, three builtin skills (`remix-ethereum`,
`remix-contract-dev`, `remix-chain-forensics`), and a `/remix`
slash command (`on|off|status|writes on|writes off|help`). Chain-writing
commands (`send`/`deploy`/`call`/`script`) are refused unless
`remix.allow_chain_writes=true` — while off, `RRX_PRIVATE_KEY` is stripped
from the child process.

And the **browser** (`browser`) — a real **Servo** engine the agent drives as
its own component. Headless launch, persistent WebDriver session, full
frontend access (real compositor screenshots, DOM/HTML) and full backend
access (arbitrary JS `eval`, Performance-API network log, cookies), with no
URL restrictions. Any URL — localhost, private ranges, public internet. See
`docs/BROWSER.md`.

Also vendored as a builtin native engine: **rustango**, a Django-style
batteries-included web framework rewritten in pure Rust
(`crates/server/rustango`: ORM with migrations, template engine, forms,
admin, auth, sessions, CSRF, i18n, cache, plus the `rustango` CLI). The
brain exposes it to the agent as `webapp_scaffold` (writes a complete,
runnable web app into the project) and
`webapp_check` (structural PASS/WARN/MISS review of a rustango app).

## Install (Kali Linux)

One command from the repo root — installs apt packages, Rust (rustup), builds
in release mode and puts the binaries on `PATH`:

```sh
bash scripts/install.sh                 # add --no-adb to skip Android platform-tools
```

If the archive ships prebuilt Linux x86_64 binaries in `bin/`, the installer
uses them directly and finishes in seconds (no compile); pass `--build` to
compile from source anyway.

Or, fully manual:

```sh
sudo apt update && sudo apt install -y \
  build-essential pkg-config git curl ca-certificates file \
  libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev adb

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
source "$HOME/.cargo/env"

cd romacode
cargo build --release
# (first build refreshes Cargo.lock with the rrx workspace members;
#  subsequent builds can use --locked for reproducibility)
sudo install -m 0755 target/release/romacode-server /usr/local/bin/romacode-server
sudo install -m 0755 target/release/romacode       /usr/local/bin/romacode
sudo install -m 0755 target/release/rrx            /usr/local/bin/rrx
```

The X11/XKB packages are runtime deps of the device-desktop window (`minifb`);
the TUI itself is pure crossterm. `adb` is only needed for the Android panel.

## Architecture

```
romacode/
├── crates/
│   ├── remix/                  # rrx 0.8.0 first-party: util→bls381→crypto/trie/
│   │                           #   precompiles→evm→chain→solc/rpc→tools + rrx CLI
│   ├── server/                 ← THE BRAIN (romacode-server)
│   │   ├── src/
│   │   │   ├── main.rs         # starts the HTTP server (+ CLI utilities)
│   │   │   ├── api.rs          # HTTP/WS endpoints, event bus, device streamer
│   │   │   ├── agent.rs        # think→act→observe loop (native + langchain engines)
│   │   │   ├── security.rs     # THE gate (every tool call passes here)
│   │   │   ├── session.rs      # history, state (server-owned)
│   │   │   ├── langchain_bridge.rs  # langchain engine + RAG tools (native)
│   │   │   ├── rustango_bridge.rs   # rustango webapp tools (scaffold/check)
│   │   │   ├── rnmap/           # engine: built-in rnmap network scanner (vendored)
│   │   │   ├── remix/           # engine: rrx Ethereum toolchain (52-command allowlist)
│   │   │   ├── browser/         # engine: agent-driven Servo browser (WebDriver)
│   │   │   ├── config.rs, providers.rs, settings, github …
│   │   │   └── tools/
│   │   │       ├── android.rs
│   │   │       ├── browser.rs
│   │   │       ├── coding.rs
│   │   │       ├── rnmap.rs
│   │   │       └── webfetch.rs
│   │   ├── rnmap/               # vendored rnmap 0.4.0 (binary + Rhai scripts, auto-installed)
│   │   ├── langchain/          # LangChain rebuilt in pure Rust (builtin native)
│   │   ├── rustango/           # Django-style web framework in pure Rust (builtin native)
│   │   ├── mandroid/           # engine: ADB wire protocol + APK analysis
│   │   ├── roxterm/            # engine: VT/PTY terminal core (client screen uses it)
│   │   └── phone/              # engine: legacy handset simulator (tests)
│   │
│   ├── client-tui/             ← THE SCREEN (romacode)
│   │   └── src/
│   │       ├── main.rs         # connects to the server
│   │       ├── net.rs          # pure-std WebSocket client
│   │       ├── app.rs          # UI state machine (mirror of server state)
│   │       ├── session.rs      # per-tab mirror + input box
│   │       ├── device.rs       # frame hub + device input
│   │       ├── display.rs      # scrcpy-style desktop window
│   │       └── ui/
│   │
│   └── shared/                 ← TYPES BOTH SIDES USE (romacode-shared)
│       └── src/
│           ├── protocol.rs     # request/response types + session model
│           ├── events.rs       # streamed event types
│           └── catalog.rs      # provider catalog (pure data)
│
└── Cargo.toml                  # workspace
```

One wire contract, two sides: the server owns every session and its history,
applies the same `Event` stream it broadcasts, and any client can resync from
a full `SessionSnapshot` at any time (reconnects are lossless). The screen
holds only pixels, keyboard state and the input boxes.

### The brain (`romacode-server`)

```sh
cd /your/project          # the project directory the agent works in
romacode-server serve     # binds 127.0.0.1:7712 (or --bind host:port, --cwd path)
```

REST surface (read-only): `GET /api/health`, `GET /api/sessions`,
`GET /api/config`, `GET /api/rag`. The interactive surface is WebSocket at
`/ws` using the `romacode-shared` envelope protocol — `hello`, `new_session`,
`select_session`, `message`, `approval`, `cancel`, `toggle_mode`, `command`
(provider/model/key/skills slash-commands, tagged with the sending tab),
`skill` (activate a skill on a session), `fetch_models` (carries the provider
id so the fetched catalog is remembered), `save_model`,
`set_ui_pref`, `android` (stream/tap/swipe/key/text). Events stream back as
`text` deltas, `tool_start`/`tool_end`, `usage`, `approval_request`, `notice`,
`error`, `done`, `snapshot`, `sessions`, `frame` (PNG device frames) and
`device` status.

The brain also hosts the CLI utilities that belong to it: `fetch` (webfetch),
`providers` / `models` / `provider` / `model`, `github …`, `--verify-android`,
`--print-config`, `--config-path`.

### The security gate

`server/src/security.rs` is THE gate, and the policy is **fully open**: every
known tool runs in every mode, without asking — shell commands, file edits,
web fetches (localhost and private networks included), network scans on any
target, Android actions, RAG and webapp tools. Both engines route every tool
call through it; unknown tools are refused only because no executor exists
behind them. The approval machinery is kept for opt-in use: an `ask` decision
(possible only when a user explicitly re-enables prompting in a custom
policy) broadcasts an `ApprovalRequest` event, every attached screen raises a
modal allow/deny popup (y = allow once, n/esc = deny, the buttons are
clickable), and the answer resolves the parked registry entry over the wire.
Pending approvals ride the session snapshot, so a reconnecting screen
re-raises a prompt the brain is still waiting on. File tools are
_descriptor-anchored_ on Linux (O_NOFOLLOW directory walk) with an
unrestricted fallback that resolves symlinks and hard links to their real
targets, so reads never fail on path shape.

### Two agent engines

| | `agent.engine = "native"` (default) | `agent.engine = "langchain"` |
|---|---|---|
| Loop | audited in-house SSE loop | langchain `AgentExecutor` |
| Protocols | Anthropic + OpenAI-compatible | OpenAI-compatible (Anthropic falls back to native with a notice) |
| Streaming | token-level | tool activity live, text per model step |
| Memory | session history → prompt | langchain `ConversationBufferMemory` (preloaded) |
| Extras | — | `rag_index` / `rag_search` tools (splitter + embeddings + vector store) |

Both engines: same tool registry, same security gate, same approval flow, no
canned replies (provider errors surface in chat).

RAG indexing reads project files directly (the permission policy ships fully
open; there are no protected paths or confinement to honor). Embeddings go to
the resolved provider's `/embeddings` endpoint (`ROMACODE_EMBEDDINGS_MODEL`
overrides the model).

### The screen (`romacode`)

```sh
romacode                  # auto-starts the brain if needed, then connects
romacode --server host:port
romacode --no-autostart   # require the brain to already be running
```

When the screen cannot reach `romacode-server` on the configured `--server`
address (default `127.0.0.1:7712`), it spawns the brain itself as a detached
background daemon — `romacode-server serve --bind <addr> --cwd <launch dir>`
— so a single `romacode` invocation is enough. There is no need to open a
second terminal and run `romacode-server serve` first. The brain is detached
via `setsid(2)`, so it survives the screen's exit; subsequent `romacode`
launches just reconnect to the existing instance. The brain's stdout/stderr
land in `~/.local/share/romacode/server.log` and its PID is recorded in
`~/.local/share/romacode/server.pid`. Stop a background brain with
`pkill -f romacode-server` (or `kill $(cat ~/.local/share/romacode/server.pid)`).
Pass `--no-autostart` to keep the legacy two-terminal launch behaviour.

Sessions, tabs, sidebars, the Android panel (PNG frames streamed from the
brain; taps/swipes/keys/text go back over the wire) and the scrcpy-style
desktop window all work against a remote brain. The native terminal screen
(`Alt+T`) runs its PTY where the TUI runs — the machine you type on — using
the roxterm engines. Config lives in the brain: the settings panel (`Ctrl+P`)
sends `fetch_models` / `save_model` requests instead of touching files; API
keys never round-trip through the screen.

### Settings, keyboard and paste

The `Ctrl+P` panel is the single settings surface. Under **Settings →
Preferences** every server-backed option is editable in place: agent engine
(native ⇄ langchain), default mode, web-fetch on/off, max tool rounds,
request timeout, context window, chat width and the side panel. Enum values
toggle on `Enter`; numeric ones open a small input. The same options work as
slash commands in the chat box: `/engine`, `/mode`, `/rounds`, `/webfetch`,
`/timeout`, `/context`, `/prefs` or `/settings` (a one-glance overview — both
names are whitelisted in the TUI and handled by the brain), `/skills` (list the
skill catalog) and `/skill <name>` (activate a skill on this tab).

### Skills

Skills are Claude-Code-style instruction packages (`SKILL.md` with a small
`name`/`description` frontmatter and an instruction body) that load on demand
instead of sitting in every prompt. The catalog (names + descriptions only) is
advertised in the system prompt and as the `skill` tool; the full body loads
only when activated — through the `skill` tool by the agent itself, or by you
with `/skills` (list) and `/skill <name>` (activate on the current tab).

Discovery layers, later ones override same-name earlier ones:

1. built-in library (embedded in the binary, currently `romacode-customization`,
   `romacode-architecture-map`, `romacode-security-posture`,
   `romacode-tool-recipes`, `claude-parity`, `source-manifest`, `docx-report`);
2. user-wide `<config_dir>/romacode/skills/<name>/SKILL.md`;
3. per-project `<project>/.romacode/skills/<name>/SKILL.md`.

The fetched-model persistence rule: a live catalog fetched after entering an
API key is merged into that provider's stored model list
(`Config::remember_models`) the moment the fetch succeeds — it survives
closed panels and restarts and shows up in Select Model, `/models` and
`romacode models` from then on.

### Persona file (standing instructions)

`~/.config/romacode/persona.md` is the user-owned standing-instructions
channel: its content is appended to the system prompt on every turn, in both
engines. It hot-reloads — edit the file and the next message already uses the
new text; no restart, no panel, no config reload. On first `serve` start a
starter template is written there (never overwritten afterwards).

Order in the prompt: base prompt → `agent.system_prompt_append` (config) →
persona file → tool sections. Reads are capped at 64 KiB with an explicit
truncation note; a missing, empty or unreadable file contributes nothing, so
a broken persona can never break the loop.

Switch or disable it with one config key:

```json
{
  "agent": {
    "persona_file": "~/prompts/my-agent-voice.md"
  }
}
```

`null`/absent keeps the default path; `""`, `"off"`, `"none"`, `"disabled"`,
`"false"`, `"0"` switch the feature off; anything else is that path (`~/`
expanded). For one-off prompt tuning without a file, `agent.system_prompt_append`
or a full `system_prompt` override in the same config remain available.

### Local uncensored models (Ollama)

The brain speaks to any OpenAI-compatible endpoint, and the built-in
`ollama` provider row points at `http://localhost:11434/v1` with no API key
required (plain HTTP on loopback is allowed for exactly this reason; LM
Studio on `:1234` and vLLM/llama.cpp `/v1` endpoints work the same way).
This is the clean route to an uncensored coding agent: run an uncensored
open-weight fine-tune locally — the model itself has no refusal training,
so there is nothing to bypass and no jailbreak text to maintain. The persona
file above then tunes style on top of the base prompt.

```sh
# 1. install Ollama (https://ollama.com) and pull an uncensored fine-tune,
#    e.g. the dolphin family (check the registry for current tags/sizes):
ollama pull dolphin3

# 2. point the brain at it (inside the TUI, or romacode-server CLI):
#    /provider ollama
#    /models            # lists everything Ollama has pulled
#    /model dolphin3
```

Optional: RAG needs embeddings — `ollama pull nomic-embed-text` and set
`ROMACODE_EMBEDDINGS_MODEL=nomic-embed-text` if you use the langchain
engine's RAG tools.

Reminder: `ollama serve` must be running (the desktop app or `ollama serve`)
and the model must fit in your VRAM/RAM; a 7–8B instruct model is the usual
coding sweet spot on a 8–12 GB GPU.

Keyboard/paste fixes in this build: bracketed paste is routed to the focused
overlay — an API key pasted into a provider field stays in that field (it
can no longer land in the chat input); control characters are stripped from
pasted keys/URLs; settings inputs have a real cursor (`←`/`→`/`Home`/`End`/
`Delete`/`Backspace`); `Ctrl+P` also closes the panel from inside; keys are
never swallowed by the settings list screens.

## Real Android setup

Install Android SDK Platform-Tools and make `adb` available on `PATH`. Start an Android Studio SDK emulator, or connect a physical Android device with USB debugging enabled. Unlock the device and approve its RSA debugging prompt. Confirm the target is listed as `device` by:

```sh
adb devices -l
```

If exactly one target is online, the brain can use it automatically. With multiple targets, set the exact serial shown by ADB in `~/.config/romacode/config.json`:

```json
{
  "android": {
    "backend": "adb",
    "adb_path": "adb",
    "serial": "emulator-5554",
    "auto_start": true
  }
}
```

RomaCode fails closed if `adb` is missing, no target is authorized, or the selection is ambiguous. It does not silently choose a target or request root. For wireless debugging, pair and connect using the official `adb pair` and `adb connect` commands; the brain then addresses that target through the ADB server. See [Android settings](docs/SETTINGS.md#android).

## What connects to the target

The TUI's Android panel and Android agent tools use the selected target for shell commands, touch/text/key input, installed-package listing, launching, and display screenshots. Device identity and status fields are queried from that target. Agent APK installation requires a v2-valid signed APK. Cellular/eSIM management, signal simulation, Termux automation, rooted operations, and compatibility validation are not implemented features.

The old `phone` crate (under `crates/server/phone/`) contains an in-process handset simulator retained for legacy development and tests. It is not a substitute for Android, does not execute Android APKs, and is not the default backend. Do not use it for Android compatibility, modem, security, or physical-device verification claims.

## Built-in native terminal

Press `Alt+T` in the TUI to open the Roxterm-backed terminal screen. It runs the current user's `$SHELL` (or `sh` if unset) in the current working directory, using a local PTY and Roxterm's VT parser/grid in the client process. `Alt+T` returns to the previous screen; the shell session remains alive while hidden and is terminated when the TUI exits. `Ctrl+C` is sent to the foreground shell process. Typed commands run locally as the user and are not subject to the AI agent's tool-permission rules.

This is the local VT/PTY integration, not Roxterm's separate daemon, RPC server, runbook engine, or GPU window. The view renders the text grid, colors, common attributes, alternate screen, and scrollback. Use `Shift+PageUp` / `Shift+PageDown` or the mouse wheel to browse history. Kitty graphics rendering and terminal mouse-reporting are not enabled.

## Build and run

```sh
cargo build --workspace
cargo test --workspace
cargo run -p romacode                        # the screen — auto-starts the brain
# (optional) drive the brain yourself:
cargo run -p romacode-server -- serve        # the brain, in your project dir
cargo run -p romacode -- --no-autostart      # the screen, no auto-spawn
```

A plain `cargo run -p romacode` is sufficient: if no brain is listening on
`127.0.0.1:7712`, the screen spawns `romacode-server serve` in the background
and reconnects. Configure a provider and API key through [docs/SETTINGS.md](docs/SETTINGS.md) or environment variables on the server. Network calls are real; RomaCode does not return mock AI answers.

## Keyboard shortcuts

`Enter` sends · `Alt+Enter` inserts a newline · `Tab` switches Build/Plan · `Esc` interrupts · `Ctrl+N` new session · `Ctrl+W` close session · `Ctrl+B` side panel · `Alt+A` Android panel · `Alt+T` native terminal · `Ctrl+P` command palette · `Ctrl+Q` quit.

## Agent pack: `agent/`, `agent-skills/`, `FULL_PROMPT.md`

The repository ships its own agent documentation pack:

- `agent/agent.md` — the agent's identity card: what the brain+screen split
  is, the two engines, any OpenAI-compatible model (keyless local Ollama),
  the live tool roster, and the fully-open neutral posture of the gate.
- `agent/instructions.md` — the binding operating rules: ground every claim
  in the source and cite it, verify after edits, do the work without
  lecturing, respect the user's `persona.md` / `system_prompt_append`
  authority, terminal-first output.
- `agent/follow-prompt.md` — the paste-ready continuation template for
  resumed sessions and compacted contexts.
- `agent-skills/` — the same content as loadable SKILL.md packages
  (`romacode-customization`, `agent-intro`, `agent-instructions`,
  `agent-follow-prompt`); copy any directory into
  `<config_dir>/romacode/skills/` or `.romacode/skills/` and the `skill`
  tool / `/skill <name>` can load it.
- `FULL_PROMPT.md` — the complete source of this repository in one file
  (~510 files, ~6 MB): header states the reading pipeline (see this file →
  the three `agent/` files → pass through them → answer queries), then a
  link table for every file, then the full dump with per-file anchors.

## Workspace

- `crates/server/src/` — the brain: agent loop, sessions, security gate, tools, api/bus, config.
- `crates/server/langchain/` — LangChain rebuilt in pure Rust: LCEL runnables, prompts, chat models, tool-calling agents, memory, RAG (splitter, embeddings, vector store) and a REPL CLI (`langchain-cli`). Vendored, not a git dependency.
- `crates/server/mandroid/` — ADB protocol/client and APK analysis/signing helpers.
- `crates/server/roxterm/` — Roxterm terminal core subset and integration/license-provenance notes (used by the client's native terminal).
- `crates/server/phone/` — legacy simulator; separate from the real-target ADB backend.
- `crates/client-tui/` — the screen: WS client, UI mirror, pixel renderer, desktop window.
- `crates/shared/` — the wire contract: protocol, events, provider catalog.

This source tree is under active integration work. A green unit suite does not replace testing against both an SDK emulator and an explicitly authorized physical Android device.
