---
name: romacode-customization
description: Customize RomaCode end to end - config.json keys, the persona.md standing-instructions channel, providers and local Ollama models, the three SKILL.md discovery layers, adding tools or providers in Rust, TUI slash commands, rebuild and install. Use when the user wants to tune behavior, add a model, add a tool or skill, change defaults, or rebuild and repackage the binaries.
---

# RomaCode Customization

Everything below is verified against this repository. Files are named so you
can go straight to the code.

## 1. The config map — `~/.config/romacode/config.json`

Template with every key and comment: `config.example.json` at the repo root.
Loader with serde defaults (old configs stay wire-compatible):
`crates/server/src/config.rs`.

* `agent.*` — `engine` (`native` | `langchain`), `system_prompt_append`
  (permanent prompt addition), `persona_file` (`null` = default path
  `~/.config/romacode/persona.md`; `""`/`off`/`none`/`disabled`/`false`/`0`
  = off; anything else = that path with `~/` expanded), `default_mode`.
* `providers.*` — endpoint + key + model per provider entry; any
  OpenAI-compatible endpoint works.
* `webfetch.*` — `allow_private_hosts` (default `true`), `allowed_domains` /
  `blocked_domains` (default empty = fetch anything), `render`, `browser`.
* `rnmap.*` — `enabled`, `binary`, `default_ports`, `timing`,
  `allow_external_targets` (default `true`), `allow_evasion` (default `true`).
* `remix.*` — `binary`, `allow_chain_writes` (default `false`: send/deploy/
  call/script are refused and `RRX_PRIVATE_KEY` is stripped from the child;
  the one shipped default that is deliberately not open — flip it in config
  to enable chain writes).
* `permissions.*`, `tui.session.permissions` — legacy knobs kept for old
  configs; the shipped defaults are allow/autoaccept and the gate ignores
  restrictive values (`security.rs::legacy_restrictive_config_still_runs_
  everything`).
* `agent.engine = "langchain"` — swap the loop to the vendored Rust
  LangChain engine; same prompt choke point, same tools.

## 2. The persona channel — user-owned standing instructions

`crates/server/src/persona.rs`. Default path
`~/.config/romacode/persona.md`; `serve` writes a blunt-coder starter on
first run and never overwrites user edits afterwards. Re-read every turn
(hot reload by construction), 64 KiB cap with an explicit truncation note,
BOM stripped. Injection order in `system_prompt()`
(`crates/server/src/agent.rs`): base prompt → `agent.system_prompt_append`
→ persona → generated tool map → tool sections. Both engines inherit it
automatically. To change the agent's voice permanently: edit persona.md.
To disable: set `agent.persona_file` to `"off"` (or delete the file).

## 3. Models — local uncensored route without any key

```sh
ollama serve &                       # or the desktop app
ollama pull dolphin3                 # open weights, no refusal training
# in the TUI:  /provider ollama  →  /models  →  /model dolphin3
```

Ollama endpoint: `http://localhost:11434/v1` (no API key). LM Studio:
`http://localhost:1234/v1`. vLLM: `http://<host>/v1`. The bundled catalog
lives in `crates/shared/modeldata/` (`_default.json`,
`<provider>/_provider.json`, `<provider>/<model>.json`; regenerate with
`scripts/gen_modeldata.py`). `/prefs` or `/settings` opens the TUI
settings; the command whitelist lives in
`crates/client-tui/src/app.rs` (`provider_cmd`).

## 4. Skills — the SKILL.md format

`crates/server/src/skills.rs`. A skill = directory with `SKILL.md`, optional
`---` frontmatter (`name:`, `description:` — name must be slug-like),
body ≤ 32 000 chars, progressive disclosure: the catalog is advertised in
the system prompt, the body loads only on activation (`skill` tool or
`/skill <name>`; `/skills` lists). Three layers, later overrides same-name
earlier: builtin `crates/server/skills/*/SKILL.md` (compile-time
`include_str!`) → user `<config_dir>/romacode/skills/*/SKILL.md` → project
`.romacode/skills/*/SKILL.md`. To add one: create the directory, write the
SKILL.md, done — no code, no rebuild (user/project layers).

## 5. Adding a tool (Rust)

Native tools live in `crates/server/src/tools/` and register in
`tools/mod.rs` + `tools/catalog.rs` (the generated prompt map and
`tools_help` pick them up automatically). A tool takes a JSON `Value`
input and returns JSON; classify it in `security.rs::class_and_target` —
or don't: unknown names pass the gate anyway (`Decision::Run`). The
rc-tools gateway path: implement `rc_core::Tool` (name/schema/execute) in
`crates/rc-tools/rc-builtin/` (or a `rust.*` tool in `rc-rust`), register
it, and it becomes runnable through `rc_tools run` immediately.

## 6. Adding a provider

`crates/providers/src/{client,adapter,catalog,schema}.rs`. If the upstream
speaks OpenAI chat completions, a catalog entry is enough: model JSON in
`crates/shared/modeldata/<provider>/` + `_provider.json` (endpoint shape),
then run `scripts/gen_modeldata.py` to refresh the bundled snapshot. The
adapter handles the wire format; no code changes for OpenAI-compatible
upstreams.

## 7. Rebuild, install, repackage

```sh
cargo test --workspace --locked --no-fail-fast   # must be green first
cargo build --workspace --locked --release
cp target/release/{romacode,romacode-server,rrx} bin/
bash scripts/install.sh                          # uses prebuilt bin/ when present
tar -czf romacode-uncensored.tar.gz romacode/    # from the parent dir
```

Test-profile note: `[profile.dev] debug = 0` keeps debug test binaries
small enough for a 10 GB disk; release uses thin LTO + strip.

## 8. Neutrality posture and opt-in restrictions

The shipped tree is fully neutral: server gate
(`crates/server/src/security.rs`) returns `Decision::Run` for everything,
`is_protected` is always false, rc-core `PermissionPolicy::default()` has
no rules and allows everything, `Sandbox::default()` allows `/` and
network, and the Python mirror in `crates/rc-tools/rc-python/` matches.
Restrictions remain available only as explicit opt-ins: build a
`PermissionPolicy` with `deny`/`ask`/`allow` rules, use `Sandbox::strict()`,
or set legacy `permissions.*` config keys. History of what was removed and
why: `FIXES.md`.
