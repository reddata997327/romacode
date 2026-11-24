# What changed in this source tree


## Build restored: 7 pre-existing compile defects + 1 wrong test assertion
The shipped tree did not compile clean — `cargo check --workspace` failed on
5 defects and `cargo test` on 2 more (check does not compile `#[cfg(test)]`
code, so the check-only pass hid two of them):

* `agent.rs` — the two stream-drain call sites (`read_to_end` on curl's
  stderr/stdout readers) used the `Read` trait without importing it;
  `use std::io::BufReader` is now `use std::io::{BufReader, Read}`.
* `api.rs` — both 431 `write_response` calls passed a `String`
  (`json!({...}).to_string()`) where the signature takes `&str`; now `&`.
* `android/setup.rs` — the emulator reaper closure calls `child.wait()`
  (`&mut self`), but the binding was not `mut`; the handle is moved into the
  closure, so `let mut child` fixes it at the source.
* `client-tui/settings.rs` — `begin_models_fallback` called
  `romacode_providers::catalog::bundled()`, but `bundled()` is an associated
  function of `Catalog`, not a free function; now
  `catalog::Catalog::bundled()`.
* `client-tui/app.rs` — `Bg::Command` reply handling compared
  `s.input.trim()`, but the chatbox editor type `Input` has no `trim`
  (its text accessor is `text()`); now `s.input.text().trim() == sent.trim()`.
* `remix/rrx-precompiles` — `ecrecover_roundtrip` asserted that invalid
  `v` (29) makes `run(ECRECOVER, ..)` return `Err`. That contradicts both
  the implementation and its doc comment: per geth semantics every
  ecrecover input failure is SUCCESS with EMPTY output at the same gas
  (only out-of-gas/unknown addresses Err). The assertion now expects an
  empty output; the implementation was already correct and is untouched.

Environment note: the `rrx-tools` fakeasset/wildtoken suites need a real
`solc` (`rrx solc install <version>` layout: `crates/.solc/<v>/solc-<v>`),
and the `phone` suite needs actual free disk space for its data-root
fixtures — a full-disk sandbox turned both into spurious failures.

Full state after the fixes: `cargo check --workspace --locked` exit 0;
`cargo test --workspace --locked --no-fail-fast` — every suite green
(server 176 incl. the 11 persona tests, rnmap engine 15, the approval
end-to-end, phone 28, rrx-precompiles 12, solc-backed token sims 40, …).

## Persona file: user-owned standing instructions, hot-reloaded every turn
The prompt channel for "how I want my agent to behave" was config-only
(`system_prompt_append`), which means editing it through config.json and
reloading. There is now a proper standing-instructions file:

* New module `crates/server/src/persona.rs`. The default persona file is
  `<config_dir>/romacode/persona.md`; its trimmed content is appended to the
  system prompt on every turn (after `agent.system_prompt_append`, before the
  tool sections), in BOTH engines — `system_prompt()` is the single choke
  point, so native and langchain pick it up with no further wiring.
* Hot reload by construction: the file is re-read each turn; edit it and the
  next message already speaks with the new voice. No restart, no panel.
* First-run starter: the `serve` path writes the starter template (a short,
  blunt-coder tuning block plus usage comments) if the default persona file
  does not exist. The file is NEVER overwritten afterwards — user edits are
  sacred (`write_starter_if_missing` returns instead of clobbering).
* One config key, `agent.persona_file` (serde-defaulted, so old configs are
  wire-compatible): `null`/absent = default path, `""`/`off`/`none`/
  `disabled`/`false`/`0` = off, anything else = that path with `~/` expanded
  (bare `~` and `~user/...` stay literal — no passwd lookups).
* Hardening: reads capped at 64 KiB with an explicit truncation note appended
  to the prompt (never a silent cut); missing/empty/unreadable files and
  non-files contribute nothing, so a broken persona can never break the loop;
  the metadata-vs-read shrink race is handled (`min()` before slicing).
* TUI gap fix while in the file: `/settings` existed server-side
  (`"prefs" | "settings"` arm in `Hub::run_command_in`) but was missing from
  the client's `provider_cmd` whitelist, so typing `/settings` sent the text
  to the model as chat. Now whitelisted (`/rnmap` was already whitelisted;
  the tree differs from the stale note about it).
* `config.example.json` documents the key (`"persona_file": null`); README
  gains "Persona file (standing instructions)" and "Local uncensored models
  (Ollama)" sections (Ollama = `http://localhost:11434/v1`, keyless, the
  clean route to an uncensored agent: uncensored open-weight fine-tunes
  need no bypass text because there is nothing to bypass).

Tests: `resolve_default_uses_config_dir`, `resolve_off_spellings_disable`,
`resolve_custom_path_and_whitespace`, `expand_tilde_forms`,
`load_appends_trimmed_content`, `load_missing_or_empty_is_none`,
`load_truncates_oversized_files_with_a_note`, `load_hot_reloads_between_turns`,
`starter_is_created_once_and_never_clobbers`, `starter_uses_bom_free_utf8`,
`system_prompt_picks_up_the_persona_and_its_off_switch` (persona.rs).

## Fetched models persist in the model options (settings-panel fix)
Entering an API key and fetching the live catalog fixed the key but threw
away the catalog itself: `FetchModels` only ECHOED the model list to the
settings screen that asked, so the moment the panel closed without a model
pick (Esc, a crash, a failed later step) the whole fetched list was gone and
Select Model / `/models` kept showing the stale stored + suggested list. The
catalog is now remembered server-side the moment the fetch succeeds:

* `ClientRequest::FetchModels` carries an optional `provider_id`
  (`#[serde(default)]`, wire-compatible with protocol-v1 clients in both
  directions); the TUI sends the provider it is configuring.
* On success the brain merges the fetched ids into the provider entry via
  `Config::remember_models` — already-stored ids keep their position, fetched
  ones are appended deduplicated, a 512-entry cap keeps the config sane, and
  unknown provider ids are ignored so a typo cannot conjure a junk entry.
  The config is saved atomically and a req-0 `Welcome` broadcast re-syncs
  every attached screen (the same pattern `SaveModel` uses).
* Picking a model afterwards still works exactly as before; the difference
  is that NOT picking no longer loses anything. Tests:
  `remember_models_persists_fetched_catalog_for_future_selection`,
  `fetch_models_and_command_requests_stay_wire_compatible`.

## Model options hardened for ALL providers (settings-panel hardening)
The persistence fix above covered the happy path. Four edge cases could still
lose or fork fetched models depending on provider kind, client age and screen
staleness. All four are now closed, with the same semantics for every
provider — known, custom, fresh or long-configured:

* `save_model` MERGES the incoming list instead of wholesale-replacing it
  (`p.models = models.to_vec()` was a stale-screen clobber: a pick from a
  panel opened before another screen fetched models erased the newer ids).
  Stored ids keep their position, incoming ids append deduplicated under the
  512-entry cap — the exact `remember_models` semantics.
* `save_model` normalizes the provider id to lowercase before touching the
  config (`OpenAI` can no longer fork an entry away from `openai`, splitting
  models and keys across two rows). Empty ids are refused up front.
* `FetchModels` without a `provider_id` (protocol-v1 clients) now persists
  too: `resolve_fetch_provider_id` falls back to the ACTIVE provider, which
  the provider-only `SaveModel` sent right before the fetch has already set.
  Pure function, unit-tested without a network round-trip.
* A FAILED live fetch no longer leaves an empty model picker. The key is
  already saved at that point, so for a KNOWN provider the TUI shows the
  shared catalog's suggestions with the error surfaced alongside
  (`Settings::begin_models_fallback`); a custom provider shows the error
  alone. Every pick from the fallback list persists through the same
  `SaveModel` merge path.

Tests: `save_model_merges_models_and_normalizes_provider_id`,
`resolve_fetch_provider_id_falls_back_to_the_active_provider` (api.rs),
`fetch_failure_falls_back_to_catalog_suggestions_for_known_providers`
(settings.rs).

## Phone shell arithmetic: parentheses evaluate (pre-existing defect)
`nested_parentheses_are_not_truncated` failed in the shipped tree: the
calculator's `eval_expr` (phone/src/apps.rs) had no `(` / `)` handling — any
parenthesized expression returned `None`, and `$((1+(2*3)))` stayed verbatim
instead of expanding to `7` (the truncation the test's name warns about was
the verbatim fallback, not lost output). Fix: open parens are pushed as a
floor on the operator stack (nothing to their left folds early), `)` folds
back to the matching `(`, the operator precedence loop never folds across a
subexpression boundary, and unbalanced input degrades to `None` → verbatim,
same as before. Unary minus before a paren (`-(1+2)`) keeps working through
the existing push-0 path. All 28 phone tests pass.

## Skills subsystem (`skill` tool + `/skills` + `/skill <name>`, new)
Claude-Code-style SKILL.md packages, ported to RomaCode's client-server
architecture (progressive disclosure: the catalog is advertised, the body
loads only on activation):

* New module `server/src/skills.rs`: `Skill::parse` (small frontmatter —
  name/description — plus body, 32 000-char cap, slug-validated names,
  malformed files degrade or are skipped, never crash discovery),
  `SkillSet` discovery in three layers — built-in library embedded at
  compile time (`crates/server/skills/*/SKILL.md`), user-wide
  `<config_dir>/romacode/skills/*/SKILL.md`, per-project
  `.romacode/skills/*/SKILL.md` — with later layers overriding same-name
  earlier ones (a repo can override a shipped skill).
* Two activation paths, same result: the agent calls the new `skill` TOOL
  (always available, read-class in the gate, live catalog embedded in its
  description) and the body arrives as the tool result; or the user runs
  `/skills` (list) / `/skill <name>` — the brain resolves the name, starts a
  turn whose prompt carries the body and broadcasts the session snapshot so
  every screen sees the activation. `Command` requests now carry the sending
  tab (`session_id`, optional) so `/skill` activates on the right session.
* System prompt gains a `## Skills` catalog section (`prompt_section()`).
* Built-in library (8 skills — the RomaCode customization work shipped as
  agent-facing skills): `romacode-customization` (the exact customization
  recipes: six seams, Recipes A–E, verify-before-ship), plus its reference
  set as focused skills — `romacode-architecture-map` (measured crate/file/
  tool/config/wire inventory), `romacode-security-posture` (what was removed,
  what remains, how to re-enable), `romacode-tool-recipes` (code skeletons) —
  and `claude-parity` (Claude Code ↔ RomaCode concept mapping),
  `source-manifest` (source-tree extraction/manifest method), `docx-report`
  (catalog-depth Word report pipeline), `unlock` (the full Unlock skill:
  edit a local model with Heretic, original untouched, honest before/after —
  generalized to ANY downloadable model, not only the tested Qwen3 recipe;
  ships its scripts, assets, references and AGPL license in the skill
  folder).
* The skills arms run before the config lock in `run_command_in` — activation
  starts an agent turn which locks the config itself; holding the lock across
  it would deadlock. Tests: 11 unit tests in `skills.rs` (parse, merge,
  catalog, tool exec, prompt section) and
  `skills_command_lists_catalog_and_activation_validates_names` in `api.rs`.


## Fully open tools framework (restrictions removed)
At the owner's request the entire restriction policy is gone: the agent is
fully open by default and nothing it does is blocked or gated any more.

* **THE gate** (`server/src/security.rs`) now runs every known tool in every
  mode without asking — deny rules, ask/allow rules, Plan-mode hard stops,
  protected paths and Android host-access confinement are all removed. Unknown
  tools are refused only because no executor exists behind them. (Latent bug
  fixed on the way: `webapp_scaffold` / `webapp_check` were never classified
  in the gate, so the agent always got "unknown tool" — now they run.)
* **Bash runs, always** (`tools/coding.rs`): the fail-closed block
  ("Bash blocked: secure shell confinement …") is gone — the shipped default
  `confine_to_project=true` + non-empty `protected_paths` used to make every
  shell call fail out of the box, which is exactly the "Android bridge /
  bash blocked" wall in the user's screenshot.
* **File tools are unrestricted**: confinement + protected-path checks are
  removed from both the Linux descriptor-anchored backend and the non-Linux
  fallback; hard-link refusal is gone; symlinked leaves AND symlinked
  directories resolve to their real targets and read fine (e.g. `/etc/hostname`,
  `outside/link/file`).
* **rnmap is unrestricted**: `permissions.net = "deny"` refusal gone,
  external-target refusal gone (public internet hosts scan fine), IDS-evasion
  options (decoy/fragment) always permitted, `max_targets` default 256 → 4096.
* **web_fetch is unrestricted**: the `permissions.web = "deny"` refusal is
  gone; the SSRF private/loopback guard is off by default
  (`webfetch.allow_private_hosts = true`), so localhost and 10.x/192.168.x
  fetches work; `user:pass@host` URLs are now supported (userinfo stripped and
  passed to curl as basic auth) instead of rejected.
* **Android never says "bridge unavailable"**: when the in-process phone
  bridge is down, every `android_*` tool automatically falls back to the host
  `adb` CLI (`tools/android.rs::execute_via_host_adb`) and either talks to the
  real device or returns a real adb diagnostic.
* **Defaults ship open** (`config.rs`, shared `tui_settings.rs`,
  `config.example.json`): every permission class `allow`, no deny/ask rules,
  no protected paths, `confine_to_project=false`,
  `tui.session.permissions="autoaccept"` (was `prompt`), rnmap
  external/evasion allowed, webfetch private hosts allowed. The startup
  warnings for the wide-open settings are gone (they are the defaults now).
* **Agent prompts updated**: no more "public internet hosts are refused",
  "State-changing actions require the configured approval policy" or
  "Plan mode is strictly read-only" — Plan mode is advisory only.
* The approval popup machinery (registry, wire event, modal popup, snapshot
  restore) is untouched and stays available for users who explicitly re-enable
  prompting.

## rnmap built-in scanner (`rnmap_scan`, new)
The user-uploaded rnmap 0.4.0 distribution (pure-Rust port scanner: TCP/UDP
port scan, service/version detection, OS fingerprinting, traceroute, TLS
certificates, 24 Rhai probe scripts, versioned JSON output) is now a built-in
default agent tool, wired like the existing built-ins:

* Vendored at `crates/server/rnmap/` (binary + scripts, provenance README),
  embedded into the server at compile time and auto-installed to
  `<data_dir>/romacode/rnmap/` on first use — no download, no external dep.
* Engine at `crates/server/src/rnmap/` + thin `tools/rnmap.rs` wrapper:
  structured arguments → scanner CLI (JSON-output subset only), child process
  with cancel/timeout through unlinked private capture files, schema-1.0 JSON
  report parsed into a compact per-host summary (`mode=full` for the whole
  report), UTF-8-safe output caps.
* Security: new `net` permission class — scans prompt by default like shell
  commands (the approval popup shows target list, ports and scan type), Plan
  mode hard-denies, `deny`/`ask`/`allow` rules match the target string,
  `permissions.net = "deny"` hides the tool and refuses at the engine.
  External (public internet) targets are refused unless
  `rnmap.allow_external_targets = true`; IDS-evasion options (decoy/fragment)
  need `rnmap.allow_evasion = true`; targets/time/output are capped.
* Config: `rnmap` section (10 keys, production-safe defaults) + clamps +
  startup warnings; `permissions.net = "ask"` default; `config.example.json`.
* Docs: `docs/RNMAP.md`; README feature list, architecture tree, gate text.
* Tests: 15 engine tests (including an end-to-end local scan through the
  installed binary) + the `net`-class gate test.

## Approval popup (the permission prompt now actually reaches the screen)
The brain evaluated `ask` decisions and parked them on the approval registry,
but the screen never saw them: the agent loop broadcast a plain transcript
notice instead of the wire `ApprovalRequest` event, and the TUI's
`pending_approval` queue was never populated, so y/n typed into the chat input
while the tool call waited forever. The full chain is fixed:

* `Emit::approval_request` broadcasts `Event::ApprovalRequest { request_id,
  name, effect }` in **both** engines (native `run_tools`, langchain tool
  closure); the transcript notice is rendered by the session mirror from that
  one event — no duplicated notice.
* The answer slot is registered **before** the broadcast, so a screen that
  reacts instantly can never lose its Allow to a registry race.
* The TUI queues the request (deduped by request id), draws a modal
  PERMISSION REQUIRED popup — tool name, exact pending effect, word-wrapped,
  `[Y] Allow once` / `[N] Deny` buttons that are mouse-clickable — and y/n/esc
  answer it over the wire while the popup is up (keys are modal; nothing leaks
  into the chat input).
* Pending approvals ride `SessionSnapshot.pending_approvals`, so a
  (re)connecting screen re-raises a prompt the brain is still waiting on; the
  record is taken when the answer arrives so an answered prompt never
  re-appears (`Hub::resolve_approval`).
* End-to-end coverage: `api::tests::approval_flow_end_to_end_through_the_hub`
  (gate → register → broadcast → snapshot restore → resolve → cleanup) and
  `app::tests::{approval_request_queues_popup_and_y_n_answers,
  snapshot_restores_pending_permission_popups}` on the screen side.

## Permission defaults (production-grade)
* `tui.session.permissions` ships as **`prompt`** (Manual) — previously the
  code default was `autoaccept`, so out of the box the agent ran shell
  commands without ever asking, contradicting the documented default.
  Auto accept remains an explicit Ctrl+P → PERMISSIONS choice and now warns at
  startup when a config carries it. Unknown permission spellings still fall
  back to the safe behaviour (only reads pass).
* `config.example.json` matches the new default.

## Models in the settings panel
* **Anthropic catalog fetch worked only in theory**: `fetch_models` sent
  `x-api-key` but not the required `anthropic-version` header, so
  Add Provider → Anthropic → key → Enter returned 400 and the model list never
  appeared. The header is sent now.
* `Welcome.providers` is no longer empty for fresh installs: every row fills
  missing models/name/base URL/kind from the shared provider catalog, and the
  **active** provider is always listed even when it has no `providers` config
  entry (out-of-the-box `provider = "anthropic"` + env key).
* **Select Model no longer wipes API keys**: the screen sends an empty key on
  purpose ("keys never round-trip through the screen") and `save_model` used
  to store it verbatim — overwriting the stored key with `""`. Now only a
  non-empty key overwrites, an empty fetched list keeps the stored models, and
  a blank base URL keeps the catalog default.
* `Select Model` merges the catalog suggestions for the active provider plus
  the effective model, so the picker is never empty; the per-row provider
  lookup falls back to the catalog for known providers without a config entry
  (no more saving an Anthropic row as `kind: openai`).

## Tool call ids
A provider that streamed a tool call without an id produced an empty id, which was
echoed back as `tool_use_id` / `tool_call_id` and rejected with
"tool call id must be a non-empty string". Both stream parsers now synthesize a
unique, never-empty id (`crates/server/src/agent.rs`).

## Permissions
`session.permissions` is now the single switch, and the Ctrl+P panel writes it.

* **Manual** (default in the config file, `prompt`): the only thing that asks is a
  shell command, the action that leaves the project. Reads, edits, web fetches and
  Android actions run without a prompt.
* **Auto accept** (`autoaccept`): nothing ever asks.

Explicit `deny` / `ask` / `allow` rules still win, and Plan mode still blocks
edits and shell.

## Settings screen (Ctrl+P)
The tree rows previously toggled nothing — every leaf toasted "staged". Now every
row is a real `tui_settings` catalog path:

* the current value is shown on the right, and ← → step it (Enter / → steps forward,
  ← steps back, ← on a group goes up a level);
* each change is applied immediately and persisted as `tui.<path>` in the brain's
  `config.json`;
* groups: PERMISSIONS · APPEARANCE · SESSION · TABS & DIFFS · INPUT · ALERTS ·
  AI & SERVER · ADVANCED (every row maps to a catalog entry, so nothing is a no-op
  placeholder);
* the panel is sized to its content instead of stretching to the terminal.

## Input
* Mouse: clicking the panel background no longer activates the first row (the
  swallow hit-test used to point at row 0); the "‹ back" row is clickable; the
  wheel scrolls; hover only moves the cursor over real rows.
* Keyboard: up/down/left/right/enter/esc/backspace, type-to-filter, ctrl+p and esc.
* The Mouse setting turns terminal capture on and off live.
* Scroll speed and sidebar changes apply immediately.

## Not covered here
Settings with no renderer behind them (theme palette, markdown/diff view modes,
sounds, notifications) are stored, validated and persisted, but the screen does not
yet draw differently for them.

## Queue box, ctrl+s steering, small working box, display fixes
* **Queue while the agent works.** Enter during a task parks the query in a
  queue box above the chatbox (`═════╡ queued · n ╞═══` rule, one row per
  query: `▣ query … ➤ ✎ ✕`). `ctrl+s` sends the typed text (or, with an empty
  chatbox, the selected queued query) to the agent; `alt+e` edit, `alt+d`
  delete, `alt+↑/↓` select; the icons are clickable.
* **Steering protocol.** New `ClientRequest::Steer` and `Event::UserInjected`.
  The message shows in the chat at once; the native engine reads it before its
  next model round (after tool results, or as one extra round if the answer
  was already streaming). Anything unread when a turn ends continues as a
  follow-up turn; on Esc the user is told what was not read. Idle session =
  normal turn. (The langchain engine reads it via the follow-up turn.)
* **Working box.** Waterfall shrunk to 14×4 on a dark-space background with a
  star dot that follows the live response: pulse = thinking, fast twinkle =
  streaming, spinning star = tool running. Drawn via a scratch buffer, so it
  scrolls in/out smoothly.
* **Display bugs fixed.** "Interrupted" shown twice (screen and saved
  transcript); settings panel u16 underflow on 12–15 row terminals; popups
  spilling past the buffer on small terminals (`put` now clips to the buffer);
  toast wider than the terminal; long values drawn over labels in the settings
  tree; tool results overflowing / multi-line in the transcript; footer cwd
  running under the key hints; chatbox eating the transcript on short
  terminals; bus tap lock held during `apply` (deadlock risk for nested
  broadcasts).

## Agent self-knowledge + full capability linking + production CLI
**Why the agent "did not know its tools":** the system prompt hand-named four
tools (web_fetch, rnmap, browser, android) and said nothing else, and several
capability sets in the tree were never linked to the agent at all.

* **Generated tool map in the prompt** (`tools/catalog.rs`). Built from the live
  definitions, so it cannot drift: every callable tool by group, what is
  switched off and how to enable it, and operating rules (prefer tools to
  guessing, verify after Edit/Bash, retry on errors, report only real results).
* **`tools_help` tool** — the agent's own manual: list / search / describe
  (full schema) for every tool.
* **mandroid linked** (`tools/mandroid.rs`). `mandroid::tools` is documented as
  "shared by the CLI and the RomaCode agent" but nothing called it: ~27 native
  APK tools (apk_info, manifest, scan, strings, decompile, resources, modify,
  sign, verify, build, workflow, device/adb…) are now agent tools. Names that
  collide with the live-device bridge become `mandroid_*`.
* **`github` tool** — search/repo/research/list/api + clone/worktree/inspect/
  apply-patch/validate/diff/archive, via the same `github::run` the CLI uses.
* **`rc_tools` gateway** — the 38 built-in tools + 8 `rust.*` developer tools
  the Ctrl+P Tools section lists, now runnable by the agent (list/describe/run;
  bare unique names like `sqrt` resolve).
* **Bug: db tools were unusable.** They were defined and advertised, but the
  security gate did not classify them and refused every call as "unknown
  tool". Fixed; a test now asserts EVERY defined tool passes the gate.
* **CLI.** `romacode tools [list|describe|search|run] [--json]`,
  `romacode doctor [--json]` (config, provider, key, engine, tools, skills,
  git/adb, state dir; exit 1 on a failed check). The screen forwards utility
  commands (tools, doctor, github, git, db, fetch, providers, models,
  tunnel) to the brain, so one binary is the whole CLI. Piping into `head` no
  longer panics.
* Both engines (native and langchain) get the same definitions.

## Tools corner fully neutral: rc-core/rc-python ship with no permission gate
The server-side gate (`crates/server/src/security.rs`) was already fully open
(every tool runs, nothing denied, nothing asked; `is_protected` → false;
android host access unrestricted). The remaining restrictions lived one layer
down, in the `rc-tools` framework defaults — the "other tools corner". They
are now neutral too:

* `rc-core::permissions` — `PermissionPolicy::default()` carried a hard
  `system.*` deny rule and an `Ask` fallback for everything else (so
  `Executor::run` refused any non-math/text/data/time/code tool without
  approval). The default is now **fully open**: no rules, `default: Allow` —
  same policy the server gate ships. The deny → ask → allow → default
  evaluation order and builder helpers stay for embedders who want to opt in
  to restrictions; `Ask`/`Deny` handling in the executor is untouched.
* `rc-core::sandbox` — the default sandbox shipped an empty FS allow-list,
  and both `Sandbox::check_path` and `ToolContext::check_path` fail closed on
  an empty list, so FS-capable rc tools (`fs.*` in rc-builtin) could never
  touch the filesystem by default. The default is now the whole filesystem
  (`["/"]`); `allow_net` was already true. `Sandbox::strict()` remains as the
  opt-in lockdown for untrusted/composed tools. Timeout and output caps are
  operational limits (runaway-process protection), not permission gates, and
  are unchanged.
* `rc-core::tool` — `ToolContext::default()` / `ToolContext::new()` now
  propagate the whole-filesystem roots (composition inner-calls used the
  default context, so composed FS steps were silently blocked).
* `rc-python` (the Python port) mirrors all three: `PermissionPolicy` default
  ALLOW with no rules, `Sandbox.allow_fs_paths` defaults to `["/"]`,
  `ToolContext.allow_fs_paths` likewise.
* Tests updated to pin the new shipped behavior:
  `default_policy_is_fully_open` (rc-core + rc-python),
  `default_sandbox_is_fully_open` (rc-core), `default_policy_runs_everything`
  (rc-python executor); the rule-precedence and Ask/Deny machinery tests now
  build explicit opt-in policies. rc-python: 38 passed. rc-core/builtin/
  compose/rust/lifecycle/sources: all green.

State after this change: no default anywhere in the tree refuses, blocks,
asks, or filters a tool call — server gate, config defaults, framework
policy, sandbox roots, and both language runtimes agree. Restrictions remain
available only as explicit opt-ins (`PermissionPolicy` builders,
`Sandbox::strict`, config `permissions.*` for legacy embedders).

## Agent pack: `agent/` + `agent-skills/` + `FULL_PROMPT.md`
Self-documenting packaging for the agent itself:

* `agent/agent.md` — identity card (brain/screen split, both engines, the
  OpenAI-compatible provider layer incl. keyless local Ollama, the full
  live tool roster, the fully-open posture, where everything lives). Every
  claim names its source file.
* `agent/instructions.md` — the binding operating rules: the reading
  pipeline, ground-truth-and-cite, tool discipline with verification,
  the neutral do-the-work stance, user voice-channel authority
  (persona.md / system_prompt_append outrank house style), output style.
* `agent/follow-prompt.md` — paste-ready continuation template with the
  four re-anchor steps and the disk-wins-over-memory rule.
* `agent-skills/` — four SKILL.md packages: `romacode-customization` (the
  customization playbook: config map, persona channel, Ollama route, skill
  layers, adding tools/providers, rebuild+repack, neutrality posture),
  plus `agent-intro`, `agent-instructions`, `agent-follow-prompt` carrying
  the three files' content for the `skill` tool / `/skill <name>` loader
  (`crates/server/src/skills.rs` formats).
* `FULL_PROMPT.md` — generated single file (`scripts/gen_full_prompt.py`)
  with the whole repository source (~510 files, ~6 MB): pipeline header,
  link table for every file, full per-file dump. Excludes binaries,
  `target/`, `Cargo.lock`, generated modeldata and binary fixtures.
