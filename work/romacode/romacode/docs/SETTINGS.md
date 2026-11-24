# RomaCode settings (schema v2)

File: `~/.config/romacode/config.json` (`romacode --config-path`). Inspect the effective result with `romacode --print-config`.

* Every section and key is optional — put only what you want to change in the file. `config.example.json` lists every option at its default.
* Existing v1 files keep working unchanged.
* Out-of-range values are clamped on load and reported as warnings; an unparsable file is copied to `config.json.bak` and defaults are used (your file is never overwritten with defaults).
* **Status:** *Active* = already read by the app. *Reserved* = part of the schema, validated and saved, but its feature is not wired up yet.

## Environment overrides (highest priority)

`ROMACODE_PROVIDER` · `ROMACODE_MODEL` · `ROMACODE_API_BASE` · `ROMACODE_MAX_TOKENS` · `ROMACODE_MODE` (build|plan) · `ROMACODE_THEME` · `ROMACODE_PROXY` · `ROMACODE_LOG` · `ROMACODE_NO_ANIMATIONS=1` · `ROMACODE_NO_ANDROID=1`

## Core (top level)

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `version` | number | `2` | Schema version. Older files are upgraded automatically. | Active |
| `provider` | string | `anthropic` | Active provider id: a known one (see [Providers](#providers)) or a custom id from `providers`. Switching with `set_provider` resets `model`, `api_base`, `api_key_env`. | Active |
| `model` | string | `""` | Model id. Empty = the provider's default model. | Active |
| `api_key_env` | string | `""` | Override: env var holding the active provider's key. Empty = the provider's own. | Active |
| `api_base` | string | `""` | Override: base URL of the active provider. Empty = the provider's own. | Active |
| `max_tokens` | number | `4096` | Max output tokens per response (1–128000). | Active |
| `system_prompt` | string|null | `null` | Replaces the built-in system prompt. | Active |
| `context_window` | number | `200000` | Context size used by the sidebar meter and auto-compaction. | Active |
| `price_in_per_mtok / price_out_per_mtok` | number | `3.0 / 15.0` | USD per million tokens, for the cost estimate. | Active |
| `chat_width_pct` | number | `36` | Chat share of the width when a device panel is open (20–70). | Active |
| `sidebar` | bool | `true` | Show the right info panel. | Active |

## Providers

`provider` picks the active one. Known ids: `anthropic` · `openai` · `openrouter` · `google` · `groq` · `deepseek` · `mistral` · `xai` · `together` · `fireworks` · `cerebras` · `ollama` · `lmstudio`. List them with `romacode providers`. Define your own (or override a known one) under `providers.<id>`; every field is optional for a known provider:

| Key (`providers.<id>.`) | Type | Default | Description |
|---|---|---|---|
| `name` | string | known name | Display name. |
| `kind` | `anthropic`\|`openai` | known kind / `openai` | Wire protocol: Anthropic Messages, or OpenAI-compatible Chat Completions. |
| `base_url` | string | known URL | Required for custom providers. Trailing `/` ignored. |
| `api_key` | string | `""` | Key stored in the file (written owner-only). Prefer `api_key_env`. |
| `api_key_env` | string | known env var | Env var holding the key. |
| `default_model` | string | known default | Used when `model` is empty. |
| `models` | string[] | known list | Suggestions for pickers; any model id works. |
| `headers` | object | `{}` | Extra headers (e.g. OpenRouter `HTTP-Referer`). |
| `stream_usage` | bool | `true` | Ask for token usage in the stream (OpenAI-compatible). Turn off if the server rejects `stream_options`. |
| `tools` | bool | `true` | Send tool definitions. Turn off for models without function calling. |
| `no_key` | bool | `false` | Custom provider needs no key (local server). |

Key lookup: `ROMACODE_API_KEY` → `providers.<id>.api_key` → the env var. In the app: `/provider`, `/model`, `/baseurl`, `/apikey`, `/customprovider`; on the command line: `romacode provider|model|set|add-provider` (see README).

Example — local vLLM plus an OpenRouter override:

```json
{
  "provider": "vllm",
  "model": "qwen2.5-coder-32b",
  "providers": {
    "vllm": { "kind": "openai", "base_url": "http://10.0.0.5:8000/v1", "no_key": true },
    "openrouter": { "headers": { "HTTP-Referer": "https://example.com", "X-Title": "RomaCode" } }
  }
}
```

## model_params

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `temperature` | number|null | `null` | 0–1. Omitted from the request when null. | Active |
| `top_p` | number|null | `null` | 0–1 nucleus sampling. | Active |
| `top_k` | number|null | `null` | Top-k sampling. | Active |
| `stop_sequences` | string[] | `[]` | Extra stop sequences. | Active |
| `thinking` | bool | `false` | Extended thinking. | Reserved |
| `thinking_budget_tokens` | number | `8000` | Thinking budget; auto-corrected to stay below max_tokens. | Reserved |
| `prompt_cache` | bool | `true` | Mark the system prompt for caching. | Reserved |
| `stream` | bool | `true` | Stream tokens as they arrive. | Reserved |
| `fallback_models` | string[] | `[]` | Models tried in order when the primary is unavailable. | Reserved |

## agent

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `default_mode` | build|plan | `build` | Mode a new session starts in. | Reserved |
| `max_tool_rounds` | number | `25` | Max model↔tool round trips per message (1–200). Was a fixed 12. | Active |
| `parallel_tools` | bool | `true` | Run independent tool calls concurrently. | Reserved |
| `tool_timeout_secs` | number | `120` | Per tool call timeout; 0 = none. | Reserved |
| `max_tool_output_bytes` | number | `65536` | Truncate tool output beyond this size. | Reserved |
| `auto_compact` | bool | `true` | Summarise old turns when context fills. | Reserved |
| `compact_threshold_pct` | number | `85` | Context % that triggers compaction (30–98). | Reserved |
| `auto_title` | bool | `true` | Generate short session titles. | Reserved |
| `instruction_files` | string[] | `ROMACODE.md, AGENTS.md, CLAUDE.md` | Project instruction files read into the prompt. | Reserved |
| `system_prompt_append` | string|null | `null` | Text appended to the system prompt. | Active |

## permissions

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `read / edit / bash / web / android / net` | allow|ask|deny | `allow` for every class | Per-class policy. The shipped policy is fully open; set `ask`/`deny` yourself if you want a gate (note: the gate currently runs everything, restrictions were removed at the owner's request). | Inert (kept for config compatibility) |
| `allow / ask / deny` | string[] | `[] / [] / []` | Rules like `bash(git *)`, `edit(src/**)`. Kept for compatibility; the restriction policy was removed, so rules are inert. | Inert |
| `protected_paths` | string[] | `[]` | Former protected-path policy; removed — nothing is protected. | Inert |
| `confine_to_project` | bool | `false` | Former confinement policy; removed — the agent may read and edit anywhere on the host. | Inert |

## ui

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `theme` | auto|dark|light|bronze | `auto` | Colour theme. | Reserved |
| `accent` | string | `""` | `#rrggbb` accent override; invalid values are ignored. | Reserved |
| `animations / reduce_motion` | bool | `true / false` | Waterfall and logo sheen; reduce_motion slows redraws. | Reserved |
| `mouse` | bool | `true` | Mouse support. | Reserved |
| `syntax_highlight / line_numbers` | bool | `true / true` | Code block rendering. | Reserved |
| `diff_style` | unified|split | `unified` | Diff layout. | Reserved |
| `tab_width` | number | `4` | 1–16. | Reserved |
| `wrap_lines / compact` | bool | `true / false` | Wrapping and dense layout. | Reserved |
| `show_timestamps / show_tokens / show_cost` | bool | `false / true / true` | Transcript and sidebar details. | Reserved |
| `bell_on_done` | bool | `false` | Terminal bell when a long task finishes. | Reserved |
| `scrollback_lines` | number | `5000` | 100–1,000,000. | Reserved |
| `max_fps` | number | `30` | 5–120. | Reserved |

## keys

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `send / newline / interrupt / toggle_mode` | string | `enter / alt+enter / esc / tab` | Core input keys. | Reserved |
| `new_session / close_session / palette / side_panel / android / quit` | string | `ctrl+n / ctrl+w / ctrl+p / ctrl+b / alt+a / ctrl+q` | Global shortcuts. | Reserved |
| `vim_mode` | bool | `false` | Modal editing in the input box. | Reserved |
| `overrides` | object | `{}` | Map any palette command id to a key. | Reserved |

## network

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `proxy` | string | `""` | `http://host:port` or `socks5://host:port`. | Active |
| `connect_timeout_secs` | number | `15` | 1–300. | Active |
| `request_timeout_secs` | number | `0` | Whole-request cap; 0 = none (streams can be long). | Active |
| `max_retries / retry_backoff_ms` | number | `2 / 800` | Retry policy for transient errors. | Reserved |
| `ca_bundle` | string | `""` | PEM file for TLS-intercepting proxies. | Active |
| `user_agent` | string | `""` | Overrides the User-Agent header. | Active |
| `extra_headers` | object | `{}` | Extra headers on every API request. | Active |

## tui — OpenCode settings (`/settings`)

The OpenCode settings dialog, reproduced row for row. Open it from `Ctrl+P` → *Settings — appearance, session, tabs…*, or type `/settings` in the chat box. Same categories, titles, values and ranges as OpenCode:

* **↑ ↓** move · **← →** change the value (choices and toggles wrap, numbers step and clamp) · **Enter** next value · **type** to search (title, category, path, keywords) · **Esc** clears the search, then goes back · mouse: hover selects, click advances.
* Every change is validated and saved to `config.json` immediately, under the `tui` key (dotted paths below are relative to it). Out-of-range hand-edited values are clamped on load and reported as warnings.
* **Status:** *Active* = already changes behaviour. *Reserved* = stored, validated, shown and persisted, but RomaCode's screen does not act on it yet.

| Category | Setting | Path | Default | Values | Status |
|---|---|---|---|---|---|
| Appearance | Theme | `theme.name` | `opencode` | picker (34 themes) | Reserved |
| Appearance | Color mode | `theme.mode` | `system` | system / dark / light | Reserved |
| Appearance | Animations | `animations` | `on` | off / on (mirrors `ui.animations`) | Reserved |
| Session | Sidebar | `session.sidebar` | `auto` | hide / auto (same switch as `sidebar`, Ctrl+B) | Active |
| Session | Scrollbar | `session.scrollbar` | `off` | off / on | Reserved |
| Session | Scroll on send | `session.submit_scroll` | `jump to latest` | jump to latest (`bottom`) / keep position (`keep`) | Reserved |
| Session | Thinking | `session.thinking` | `hide` | hide / show | Reserved |
| Session | Markdown | `session.markdown` | `rendered` | source / rendered | Reserved |
| Session | Tool grouping | `session.grouping` | `auto` | none / auto | Reserved |
| Session | Verbosity | `session.verbosity` | `low` | low / medium / high | Reserved |
| Session | Transcript images | `session.image_preview` | `off` | off / on | Reserved |
| Session | TPS | `session.tps` | `on` | off / on | Reserved |
| Session | New session location | `session.new_location` | `launch directory` | launch directory (`launch`) / active session (`inherit`) | Reserved |
| Session | Permissions | `session.permissions` | `autoaccept` | prompt / auto accept (`autoaccept`). The default is fully open (the agent runs everything without asking); `prompt` remains selectable for users who want the approval popup. | Active |
| Tabs | Mode | `tabs.mode` | `auto` | off / on / auto | Reserved |
| Tabs | Scope | `tabs.scope` | `current directory` | current directory (`cwd`) / global | Reserved |
| Tabs | Layout | `tabs.layout` | `horizontal` | horizontal / vertical | Reserved |
| Tabs | Indicators | `tabs.indicators` | `status icons` | status icons (`status`) / always show numbers (`numbers`) | Reserved |
| Diffs | Layout | `diffs.view` | `auto` | auto / split / unified | Reserved |
| Diffs | Wrapping | `diffs.wrap` | `word` | none / word | Reserved |
| Diffs | File tree | `diffs.tree` | `on` | off / on | Reserved |
| Diffs | Single patch | `diffs.single` | `off` | off / on | Reserved |
| Input | Scroll speed | `scroll.speed` | `3.00` | 0.25–10, step 0.25 | Reserved |
| Input | Acceleration | `scroll.acceleration` | `off` | off / on | Reserved |
| Input | Mouse | `mouse` | `on` | off / on (mirrors `ui.mouse`) | Reserved |
| Input | Editor context | `prompt.editor` | `on` | off / on | Reserved |
| Input | Large pastes | `prompt.paste` | `compact` | compact / full | Reserved |
| Input | Image previews | `prompt.image_preview` | `off` | off / on | Reserved |
| Input | Leader timeout | `leader.timeout` | `2000 ms` | 250–10000 ms, step 250 | Reserved |
| Alerts | Notifications | `attention.notifications` | `off` | off / on | Reserved |
| Alerts | Sounds | `attention.sound` | `off` | off / on | Reserved |
| Alerts | Volume | `attention.volume` | `40%` | 0–100%, step 10% | Reserved |
| Alerts | Release highlights | `highlights` | `on` | off / on | Reserved |
| Terminal | Window title | `terminal.title` | `on` | off / on | Reserved |
| Terminal | Copy behavior | `terminal.copy` | `select` (`manual` on Windows) | manual / select | Reserved |
| Debug | Developer tools | `debug.devtools` | `off` | off / on | Reserved |

Rows that mirror a legacy key stay in step with it: `session.sidebar` ⇄ `sidebar`, `animations` ⇄ `ui.animations`, `mouse` ⇄ `ui.mouse`. For `session.sidebar`, `hide` from either side wins on load.

The `tui` section also carries the rest of OpenCode's config schema (no dialog row, edit in the file): `keybinds`, `plugins`, `attention.sound_pack`, `attention.sounds`, `diffs.source`, `diffs.fullscreen`, `mini.*` (`thinking`, `tools`, `shell_output`, `turn_summary`, `footer`, `splash`, `work_spinner`, `mono`, `replay`, `replay_limit`), `debug.timing`, `debug.turn_tokens`, `experimental`, `highlights`, `cursor.style` (`block|underline|line|default`) and `cursor.blinking`. See `config.example.json` for every default.

On the wire the screen sends `SetUiPref { key: "tui.<path>", value }`; the brain answers `Welcome.tui` with the effective settings.

## webfetch

The built-in `web_fetch` tool, on by default and unrestricted — any URL works, including localhost and private-network addresses. Turn it off with `enabled = false`.

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `enabled` | bool | `true` | Give the agent the `web_fetch` tool. | Active |
| `default_mode` | string | `text` | text · markdown · raw · json · links · headers. | Active |
| `max_body_bytes` | number | `4194304` | Response bytes kept per fetch (4 KiB–64 MiB). | Active |
| `max_output_chars` | number | `20000` | Characters returned to the model per fetch (hard cap, 1000–500000). | Active |
| `timeout_secs` | number | `30` | Per-request timeout (1–300). | Active |
| `max_redirects` | number | `5` | Hops followed; each hop is re-checked (0–10). | Active |
| `user_agent` | string | `""` | Empty = `Mozilla/5.0 (compatible; RomaCode/<version>; +web_fetch)`. | Active |
| `allow_private_hosts` | bool | `false` | Permit localhost / RFC 1918 / link-local / cloud-metadata targets. | Active |
| `allowed_domains` | string[] | `[]` | If non-empty, only these domains (and subdomains). | Active |
| `blocked_domains` | string[] | `[]` | Never fetched (and subdomains). | Active |
| `render` | bool | `false` | Enable `render` mode (needs Chromium/Chrome). | Active |
| `browser` | string | `""` | Browser executable; empty = search PATH. | Active |

Network settings (`network.proxy`, `connect_timeout_secs`, `ca_bundle`) also apply to `web_fetch`. Fetches never ask and are never blocked by policy; URL userinfo (`https://user:pass@host/`) is passed to curl as basic auth.

## sessions

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `restore_on_start / autosave` | bool | `true / true` | Reopen and save sessions. | Reserved |
| `max_saved` | number | `100` | 1–10000. | Reserved |
| `dir` | string | `""` | Session directory; empty = default data dir. | Reserved |
| `history_limit` | number | `1000` | Input history size. | Reserved |

## privacy

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `telemetry` | bool | `false` | RomaCode sends no telemetry today; this stays off unless you opt in. | Reserved |
| `redact_secrets` | bool | `true` | Mask keys/tokens in tool output. | Reserved |
| `save_transcripts` | bool | `true` | Write transcripts to disk. | Reserved |

## android

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `backend` | string | `adb` | Use Android Platform-Tools to attach to a real SDK emulator or device. | Active |
| `adb_path` | string | `adb` | Platform-Tools executable path; `adb` resolves via `PATH`. | Active |
| `serial` | string | `""` | Exact serial shown by `adb devices -l`; empty requires exactly one authorized online target. | Active |
| `auto_start` | bool | `true` | Connect to the selected real target with the app. No simulated target is booted. | Active |
| `port` | number | `5555` | Legacy simulator setting; unused by the ADB backend. | Reserved |
| `display` | [w,h] | `[720, 1560]` | Fallback panel size if the real target does not report its display. | Active |
| `density` | number | `420` | Legacy simulator setting; real devices keep their own density. | Reserved |
| `bind` | string | `127.0.0.1` | Legacy simulator setting; unused by the ADB backend. | Reserved |
| `open_window / window_scale / window_on_top` | bool/number/bool | `false / 0.5 / false` | Desktop window behaviour (scale 0.25–2.0). | Reserved |
| `system_theme` | dark|light | `dark` | Legacy simulator setting. | Reserved |
| `wallpaper` | number | `0` | Legacy simulator setting. | Reserved |
| `boot_animation / persist / network` | bool | `true / true / true` | Legacy simulator settings. | Reserved |
| `fps` | number | `30` | 1–60. | Reserved |
| `data_dir` | string | `""` | Legacy simulator data directory. | Reserved |

To connect a real target, install Android SDK Platform-Tools and ensure `adb` is on `PATH`, or set `android.adb_path` to its executable. Start an SDK emulator or connect a physical device with USB debugging enabled, unlock it, and approve the RSA debugging prompt. Check `adb devices -l`; if multiple targets are online, set `android.serial` to the exact target serial. RomaCode never silently chooses among multiple devices and never requests root. For wireless debugging, use `adb pair`/`adb connect` first; RomaCode then uses the official ADB client/server connection.

## advanced

| Key | Type | Default | Description | Status |
|---|---|---|---|---|
| `log_level` | off|error|warn|info|debug|trace | `info` | Verbosity. | Reserved |
| `log_file` | string | `""` | Empty = no file logging. | Reserved |
| `env` | object | `{}` | Env vars for every agent shell command. | Reserved |
| `experimental` | object | `{}` | Feature flags. | Reserved |

## Example

```json
{
  "model": "claude-opus-5-5",
  "model_params": { "temperature": 0.2 },
  "agent": { "max_tool_rounds": 40, "system_prompt_append": "Prefer small, reviewable diffs." },
  "network": { "proxy": "http://proxy.corp:3128" },
  "android": { "auto_start": false }
}
```

## Interactive provider settings

`Ctrl+P` opens the keyboard-only Settings overlay. Use `↑`/`↓` to choose **Add Provider** or **Select Model**, `Enter` to confirm, and `Esc` to go back or close. The previous command palette is still available with `Ctrl+Shift+P`.

**Add Provider** presents the single built-in provider registry (367 entries, including the primary OpenAI-compatible and Anthropic providers) plus **Add Custom Provider**. Known providers ask for an API key; custom providers ask for a base URL and API key. RomaCode fetches the provider's `/models` catalog asynchronously, shows a loading state, reports invalid keys, unreachable URLs, malformed responses, and empty catalogs as retryable errors, then lets you select and activate a model.

Fetched models, provider ids, custom base URLs, API keys, and the active model are saved to the owner-only config file. **Select Model** reuses those saved catalogs across restarts and switches both the active provider and model without asking for the key again.

## GitHub research CLI

RomaCode includes a pure-Rust GitHub REST client that uses the existing curl transport and does not require `gh`, Python, Node, or a browser. Set `GITHUB_TOKEN` (or `GH_TOKEN`) for authenticated access and higher rate limits:

```text
romacode github search "embedded rust" --type repos --sort stars --order desc --out repos.json
romacode github search "repo:owner/name memory safety" --type code --json
romacode github repo owner/name
romacode github research owner/name --code --out owner-name-research.json
romacode github rate-limit
```

Every paginated endpoint follows GitHub's `Link: rel="next"` header until it disappears. Repository research collects metadata, issues, pull requests, commits, releases, contributors, languages, topics, and optional code matches. Results can be emitted as JSON or saved with `--out`. GitHub Search itself imposes a documented 1,000-result cap per query; broad research must partition queries by owner, language, topic, or date, and RomaCode reports this limitation instead of claiming impossible completeness.

## Local repository engineering

The GitHub framework also exposes local source-engineering primitives. These commands operate on a checkout and do not require a GitHub token:

```text
romacode github clone owner/repository --branch main --dest ./workspace/repository
romacode github worktree ./workspace/repository feature/my-change --dest ./workspace/repository-worktree
romacode github inspect ./workspace/repository-worktree
romacode github apply-patch ./workspace/repository-worktree changes.patch
romacode github validate ./workspace/repository-worktree --kind rust --out validation.json
romacode github diff ./workspace/repository-worktree
romacode github archive ./workspace/repository-worktree deliverable.zip
```

`inspect` writes a repository fingerprint including remote, branch, commit, dirty state, detected project kind, file count, and test-file count. `clone` and `worktree` write `.romacode-manifest.json` with the selected revision and workspace provenance. `validate` runs project-native format, build, and test checks and emits a machine-readable report. `apply-patch` runs `git apply --check` before changing files, and `archive` packages the selected `HEAD` reproducibly through Git.
