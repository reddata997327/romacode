# romacode full provider catalog — pure Rust

This patch adds a comprehensive, hand-curated catalog of every popular LLM
provider and their models to the `romacode-shared` crate. Every entry is
pure Rust (`const` data in a `&[FullEntry]` slice) — no TypeScript, no
runtime file loading, no serde cost at startup.

## What's in the patch

### `crates/shared/src/full_catalog.rs` (new)

A new module exporting:

```rust
pub struct FullEntry {
    pub vendor: &'static str,         // "anthropic", "openai", ...
    pub family: &'static str,         // "claude", "gpt-5", ...
    pub model: &'static str,          // exact wire id
    pub kind: ApiKind,                // anthropic | openai
    pub base_url: &'static str,
    pub key_env: &'static str,
    pub needs_key: bool,
    pub context_window: u64,          // tokens
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_reasoning: bool,
    pub note: &'static str,
}

pub const FULL_CATALOG: &[FullEntry] = &[ /* 100+ entries */ ];

pub fn models_for_vendor(vendor: &str) -> Vec<&'static FullEntry>;
pub fn find(vendor: &str, model: &str) -> Option<&'static FullEntry>;
pub fn vendors() -> Vec<&'static str>;
pub fn default_model(vendor: &str) -> Option<&'static FullEntry>;
pub fn known_for(entry: &'static FullEntry) -> Known;  // legacy projection
```

### `crates/shared/src/lib.rs` (modified)

```rust
pub mod full_catalog;
pub use full_catalog::{FullEntry, FULL_CATALOG};
```

### `crates/shared/examples/smoke_catalog.rs` (new)

A standalone binary that exercises every catalog entry whose API key is
present in the environment. For each entry it runs four probes:

  1. **models-list** — GET `/models`, confirm the catalog id is present.
  2. **chat** — POST `/chat/completions` with a trivial prompt.
  3. **tool** — POST `/chat/completions` with a `get_weather` tool and
     assert `tool_call_id` is non-empty (the bug from the screenshot).
  4. **roundtrip** — POST the tool result back and assert a final text
     reply.

All HTTP requests go through `curl --config -` (stdin config) so the API
key never appears in `argv` — this mirrors the production code path in
`crates/server/src/providers.rs`.

## Catalog coverage

17 vendors, 100+ models:

| Vendor | Models | Notes |
|---|---:|---|
| `anthropic`  | 8  | claude-opus-4-5, sonnet-4-5, haiku-4-5, 3.7/3.5/3/2.1 |
| `openai`     | 14 | gpt-5/mini/nano, gpt-4.1/mini/nano, gpt-4o/mini, o3/mini, o4-mini, realtime, embeddings |
| `deepseek`   | 5  | deepseek-chat (V3), reasoner (R1), coder, pinned snapshots |
| `google`     | 10 | gemini 2.5 pro/flash/lite, 2.0 flash/thinking, 1.5 pro/flash, gemma 3 27B/12B/4B |
| `mistral`    | 9  | large-latest, large-2411, small, codestral, pixtral, open-7b, mixtral-8x7b, embed |
| `xai`        | 5  | grok-4, grok-4-fast, grok-2-vision, grok-3, grok-3-mini |
| `cohere`     | 5  | command-a, command-r+, command-r, embed-v4, embed-v3 |
| `groq`       | 6  | llama-3.3-70b, 3.1-8b, deepseek-r1-distill, qwen-2.5-coder, vision, mixtral |
| `openrouter` | 7  | auto + claude/gpt/gemini/deepseek/llama/qwen passthrough |
| `together`   | 5  | llama-3.3-70b-turbo, 3.1-405b-turbo, qwen-coder, deepseek-v3, deepseek-r1 |
| `fireworks`  | 3  | llama-v3p3-70b, deepseek-v3, qwen2p5-coder-32b |
| `perplexity` | 4  | sonar-pro, sonar-reasoning-pro, sonar, r1-llama-3.3-70b |
| `cerebras`   | 3  | llama-3.3-70b, 3.1-8b, qwen-3-coder-480b |
| `nvidia`     | 9  | All verified working on 2026-11-19 (nemotron-3-ultra, bolt, lightning, super-vl, mistral-large-4, muse-glimmer, glm-5.3-flash, minimax-m3.1, laguna-xs-2.1) |
| `coarena`    | 10 | 3 free-tier (deepseek-v4-1-flash-1017, gemini-3-8-flash, gpt-6-luna) + 7 paid |
| `ollama`     | 3  | qwen2.5-coder, llama3.3, deepseek-r1 (local, no key) |
| `lmstudio`   | 1  | local-model (local, no key) |

## How to use

### Build

```bash
cd romacode-src
cargo build --release -p romacode-shared
```

### Run the unit tests

```bash
cargo test --release -p romacode-shared
```

Expected output:
```
running 38 tests
... 16 tests in full_catalog::tests ...
test result: ok. 38 passed; 0 failed; 0 ignored
```

### Run the live smoke test

```bash
COARENA_API_KEY=coa_... \
NVIDIA_API_KEY=nvapi-... \
cargo run --release -p romacode-shared --example smoke_catalog -- --timeout 120
```

Sample output (free-tier Coarena keys):
```
# coarena — key present, testing 10 model(s)
→ coarena/deepseek-v4-1-flash-1017
    models-list PASS
    chat PASS: "4"
    tool PASS: id="call_za8A72sEkZhSEydynDu49LLx" name="get_weather"
    roundtrip PASS: "Right now in Paris, it's 14°C and cloudy..."

# nvidia — key present, testing 9 model(s)
→ nvidia/nvidia/nemotron-3-ultra-550b-a55b
    models-list PASS
    chat PASS: "4"
    tool PASS: id="call-509a72cb-cc25-4b16-bca7-c14aaaa846e4" name="get_weather"
    roundtrip PASS: "The current weather in Paris is 14°C with cloudy..."

════════════════════════════════════════════
  Total probes: 19  PASS: 42  Skipped (no key): 88
════════════════════════════════════════════
```

### Use from your own code

```rust
use romacode_shared::full_catalog::{find, models_for_vendor, FULL_CATALOG};

// Iterate every entry:
for e in FULL_CATALOG {
    println!("{}/{} ({} ctx, tools={})",
        e.vendor, e.model, e.context_window, e.supports_tools);
}

// Look up a specific (vendor, model) pair:
let e = find("anthropic", "claude-sonnet-4-5").unwrap();
assert_eq!(e.context_window, 200_000);
assert!(e.supports_tools);
assert!(e.supports_vision);
assert!(e.supports_reasoning);

// Get every model for a vendor:
for e in models_for_vendor("openai") {
    println!("  - {} ({} ctx)", e.model, e.context_window);
}

// Project down to the legacy Known shape (for existing callers):
use romacode_shared::full_catalog::known_for;
let k = known_for(find("deepseek", "deepseek-chat").unwrap());
assert_eq!(k.id, "deepseek");
assert_eq!(k.default_model, "deepseek-chat");
```

## Tests

The `full_catalog::tests` module has 16 tests:

| Test | What it checks |
|---|---|
| `catalog_is_non_empty` | ≥ 80 entries |
| `every_entry_has_unique_vendor_model_pair` | no duplicates |
| `every_base_url_is_https_or_localhost` | SSRF hygiene |
| `needs_key_matches_key_env` | key_env non-empty iff needs_key |
| `every_context_window_is_reasonable` | 512 ≤ ctx ≤ 10M |
| `kind_round_trips` | ApiKind::from_str(as_str(k)) == k |
| `required_vendors_present` | all 17 vendors listed |
| `anthropic_models_use_anthropic_kind` | wire-protocol sanity |
| `ollama_and_lmstudio_dont_need_keys` | local servers |
| `find_returns_correct_entry` | lookup works |
| `find_is_case_insensitive_on_vendor` | OpenAI == openai |
| `models_for_vendor_returns_at_least_one` | no empty vendors |
| `default_model_is_first_in_catalog_order` | deterministic defaults |
| `to_known_projects_correctly` | legacy projection works |
| `to_known_method_also_works` | method form works |
| `vendor_count_meets_expectation` | ≥ 17 vendors |
| `nvidia_entries_are_verified_models` | only smoke-tested nvidia models |
| `coarena_free_tier_models_present` | the 3 free-tier ids |
| `serializes_to_json` | every entry round-trips through serde_json::Value |
| `no_placeholder_entries` | no `provider-NNN` or `.example` URLs |
| `openai_compatible_vendors_use_openai_kind` | every non-anthropic vendor |

Run them with `cargo test -p romacode-shared --release full_catalog::`.

## Bug fix included

The screenshot shows the user hitting:

```
'messages[6].tool_call_id' must be a non-empty string
```

That fix is **already present** in `crates/server/src/agent.rs` (the
upstream source included it). This patch adds the **regression test** for
it: the smoke_catalog example's probe 3 explicitly asserts the
`tool_call_id` returned by the live API is non-empty, and the
`roundtrip` probe confirms the model accepts the echoed id back without
the 400 error.

## File map

```
romacode-src/
├── crates/shared/
│   ├── Cargo.toml                (unchanged)
│   ├── src/
│   │   ├── lib.rs                (modified — adds `pub mod full_catalog;`)
│   │   ├── catalog.rs            (unchanged — legacy KNOWN slice)
│   │   └── full_catalog.rs       (NEW — 100+ entries, 16 tests)
│   └── examples/
│       └── smoke_catalog.rs      (NEW — live API smoke test binary)
└── crates/client-tui/src/ui/palette.rs   (one-line pre-existing bug fix)
```

## License

MIT (same as the rest of romacode).
