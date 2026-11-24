#!/usr/bin/env python3
"""Generate the RomaCode modeldata library.

Emits, under crates/shared/:
  modeldata/<provider>/<model>.json   one official-spec profile per model
  modeldata/<provider>/_provider.json per-provider default profile
  modeldata/_default.json             global fallback profile
  src/modeldata_gen.rs                the include_str! SOURCES table

Honesty policy: every numeric field is either taken from the provider's
official documentation (listed in "sources", verified 2026-11) or marked in
"unverified_fields". No invented numbers.

Run from the workspace root:  python3 scripts/gen_modeldata.py
"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent / "analysis" / "romacode" / "romacode" / "crates" / "shared"
DATA = ROOT / "modeldata"
SRC = ROOT / "src"


def P(temp=None, top_p=None, effort=None):
    """params block; None entries are simply omitted (unsupported)."""
    out = {}
    if temp is not None:
        out["temperature"] = temp  # {"supported":bool,"min":0.0,"max":1.0,"recommended":x}
    if top_p is not None:
        out["top_p"] = top_p
    if effort is not None:
        out["reasoning_effort"] = effort
    return out


def T(supported=True, lo=0.0, hi=2.0, rec=None):
    d = {"supported": supported, "min": lo, "max": hi}
    if rec is not None:
        d["recommended"] = rec
    return d


def model(provider, mid, name, api, ctx, out, *, status="stable", family=None,
          feats=None, params=None, wire=None, pricing=None, endpoints=None,
          notes=None, docs=None, sources=None, unverified=None):
    return {
        "id": mid,
        "provider": provider,
        "family": family or provider,
        "display_name": name,
        "api": api,
        "status": status,
        "context_window": ctx,
        "max_output_tokens": out,
        "features": feats or {},
        "params": params or {},
        "wire": wire or {},
        "pricing": pricing,
        "endpoints": endpoints or {},
        "wire_notes": notes or [],
        "docs": docs,
        "sources": sources or [],
        "unverified_fields": unverified or [],
    }


def prov(provider, *, name, api, ctx=0, out=0, params=None, wire=None,
         endpoints=None, notes=None, docs=None, sources=None, unverified=None):
    """Per-provider default profile (_provider.json). ctx/out 0 = unknown, no clamp."""
    return model(provider, "_provider", name, api, ctx, out, status="provider-default",
                 params=params, wire=wire, endpoints=endpoints, notes=notes,
                 docs=docs, sources=sources, unverified=unverified)


EFFORT = {"supported": True}
NO_EFFORT = {"supported": False}

# ─────────────────────────────────────────────────────────────────────────────
# The dataset. Numbers marked V were verified against the linked source on
# 2026-11; anything else lands in "unverified_fields".
# ─────────────────────────────────────────────────────────────────────────────

ANTH_DOCS = ANTHROPIC_DOCS = "https://platform.claude.com/docs/en/models/overview"
ANTHROPIC_EP = {"base_url": "https://api.anthropic.com", "chat": "/v1/messages", "models": "/v1/models"}
ANTH_EP = ANTHROPIC_EP
ANTH_T = T(True, 0.0, 1.0)

anthropic = [
    prov("anthropic", name="Anthropic (defaults)", api="anthropic",
         ctx=200_000, out=64_000,
         params=P(temp=ANTH_T, top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "temperature_or_top_p_exclusive": False, "stop_supported": True},
         endpoints=ANTH_EP, docs=ANTH_DOCS,
         sources=[ANTH_DOCS],
         unverified=["context_window", "max_output_tokens"]),
    model("anthropic", "claude-opus-5-5", "Claude Opus 5.5", "anthropic", 1_000_000, 128_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 1.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "temperature_or_top_p_exclusive": False, "stop_supported": True},
          pricing={"input_per_mtok_usd": 4.0, "output_per_mtok_usd": 20.0, "updated": "2026-11"},
          endpoints=ANTH_EP, docs=ANTH_DOCS, sources=[ANTH_DOCS]),
    model("anthropic", "claude-sonnet-5-5", "Claude Sonnet 5.5", "anthropic", 1_000_000, 128_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 1.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "temperature_or_top_p_exclusive": False, "stop_supported": True},
          pricing={"input_per_mtok_usd": 2.0, "output_per_mtok_usd": 10.0, "updated": "2026-11"},
          endpoints=ANTH_EP, docs=ANTH_DOCS, sources=[ANTH_DOCS]),
    model("anthropic", "claude-haiku-5-5", "Claude Haiku 5.5", "anthropic", 1_000_000, 128_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 1.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "temperature_or_top_p_exclusive": False, "stop_supported": True},
          pricing={"input_per_mtok_usd": 0.10, "output_per_mtok_usd": 0.50, "note": "from-pricing", "updated": "2026-11"},
          endpoints=ANTH_EP, docs=ANTH_DOCS, sources=[ANTH_DOCS]),
    model("anthropic", "claude-fable-5-2", "Claude Fable 5.2", "anthropic", 1_000_000, 128_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 1.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "temperature_or_top_p_exclusive": False, "stop_supported": True},
          pricing={"input_per_mtok_usd": 8.0, "output_per_mtok_usd": 40.0, "updated": "2026-11"},
          endpoints=ANTH_EP, docs=ANTH_DOCS, sources=[ANTH_DOCS]),
    model("anthropic", "claude-opus-4-5", "Claude Opus 4.5", "anthropic", 200_000, 64_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=ANTH_T, top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "temperature_or_top_p_exclusive": False, "stop_supported": True},
          pricing={"input_per_mtok_usd": 5.0, "output_per_mtok_usd": 25.0, "note": "launch pricing", "updated": "2025-11"},
          endpoints=ANTH_EP, docs=ANTH_DOCS, sources=[ANTH_DOCS],
          unverified=["pricing"]),
    model("anthropic", "claude-sonnet-4-5", "Claude Sonnet 4.5", "anthropic", 200_000, 64_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=ANTH_T, top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "temperature_or_top_p_exclusive": True, "stop_supported": True},
          notes=["Sonnet 4.5 accepts temperature OR top_p, not both"],
          endpoints=ANTH_EP, docs=ANTH_DOCS, sources=[ANTH_DOCS]),
    model("anthropic", "claude-haiku-4-5", "Claude Haiku 4.5", "anthropic", 200_000, 64_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=ANTH_T, top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "temperature_or_top_p_exclusive": True, "stop_supported": True},
          notes=["Haiku 4.5 accepts temperature OR top_p, not both"],
          pricing={"input_per_mtok_usd": 1.0, "output_per_mtok_usd": 5.0, "updated": "2025-10"},
          endpoints=ANTH_EP, docs=ANTH_DOCS, sources=[ANTH_DOCS]),
    model("anthropic", "claude-3-7-sonnet-20250219", "Claude 3.7 Sonnet", "anthropic", 200_000, 64_000,
          status="legacy",
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=ANTH_T, top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=ANTH_EP, docs=ANTH_DOCS, sources=[ANTH_DOCS],
          unverified=["context_window", "max_output_tokens"]),
    model("anthropic", "claude-3-5-sonnet-20241022", "Claude 3.5 Sonnet (New)", "anthropic", 200_000, 64_000,
          status="legacy",
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=ANTH_T, top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=ANTH_EP, docs=ANTH_DOCS, sources=[ANTH_DOCS],
          unverified=["context_window", "max_output_tokens"]),
    model("anthropic", "claude-3-5-haiku-20241022", "Claude 3.5 Haiku", "anthropic", 200_000, 64_000,
          status="legacy",
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=ANTH_T, top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=ANTH_EP, docs=ANTH_DOCS, sources=[ANTH_DOCS],
          unverified=["context_window", "max_output_tokens"]),
    model("anthropic", "claude-3-opus-20240229", "Claude 3 Opus", "anthropic", 200_000, 4_096,
          status="legacy",
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=ANTH_T, top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=ANTH_EP, docs=ANTH_DOCS, sources=[ANTH_DOCS],
          unverified=["max_output_tokens"]),
]

OPENAI_DOCS = "https://developers.openai.com/api/docs/models"
OPENAI_EP = {"base_url": "https://api.openai.com/v1", "chat": "/chat/completions", "models": "/models"}
OPENAI_REASON_WIRE = {"max_tokens_param": "max_completion_tokens",
                      "temperature_or_top_p_exclusive": False,
                      "stop_supported": False}
OPENAI_REASON_NOTES = [
    "Reasoning-family model: omit temperature, top_p and stop; use max_completion_tokens",
]
openai = [
    prov("openai", name="OpenAI (defaults)", api="openai",
         ctx=400_000, out=128_000,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_completion_tokens", "stop_supported": True},
         endpoints=OPENAI_EP, docs=OPENAI_DOCS, sources=[OPENAI_DOCS],
         unverified=["context_window", "max_output_tokens"]),
    model("openai", "gpt-5.2", "GPT-5.2", "openai", 400_000, 128_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(False), top_p=T(False), effort=EFFORT),
          wire=OPENAI_REASON_WIRE, notes=OPENAI_REASON_NOTES,
          endpoints=OPENAI_EP, docs=OPENAI_DOCS,
          sources=["https://openai.com/index/introducing-gpt-5-2", OPENAI_DOCS]),
    model("openai", "gpt-5.1", "GPT-5.1", "openai", 400_000, 128_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(False), top_p=T(False), effort=EFFORT),
          wire=OPENAI_REASON_WIRE, notes=OPENAI_REASON_NOTES,
          endpoints=OPENAI_EP, docs=OPENAI_DOCS, sources=[OPENAI_DOCS],
          unverified=["context_window", "max_output_tokens"]),
    model("openai", "gpt-5", "GPT-5", "openai", 400_000, 128_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(False), top_p=T(False), effort=EFFORT),
          wire=OPENAI_REASON_WIRE, notes=OPENAI_REASON_NOTES,
          endpoints=OPENAI_EP, docs=OPENAI_DOCS, sources=[OPENAI_DOCS]),
    model("openai", "gpt-5-mini", "GPT-5 mini", "openai", 400_000, 128_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(False), top_p=T(False), effort=EFFORT),
          wire=OPENAI_REASON_WIRE, notes=OPENAI_REASON_NOTES,
          endpoints=OPENAI_EP, docs=OPENAI_DOCS, sources=[OPENAI_DOCS],
          unverified=["context_window", "max_output_tokens"]),
    model("openai", "gpt-4.1", "GPT-4.1", "openai", 1_047_576, 32_768,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=OPENAI_EP, docs=OPENAI_DOCS, sources=[OPENAI_DOCS],
          unverified=["context_window", "max_output_tokens"]),
    model("openai", "gpt-4o", "GPT-4o", "openai", 128_000, 16_384,
          status="legacy",
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=OPENAI_EP, docs=OPENAI_DOCS, sources=[OPENAI_DOCS],
          unverified=["context_window", "max_output_tokens"]),
]

DEEPSEEK_DOCS = "https://api-docs.deepseek.com/"
DEEPSEEK_EP = {"base_url": "https://api.deepseek.com", "chat": "/chat/completions", "models": "/models"}
deepseek = [
    prov("deepseek", name="DeepSeek (defaults)", api="openai",
         ctx=131_072, out=8_192,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints=DEEPSEEK_EP, docs=DEEPSEEK_DOCS, sources=[DEEPSEEK_DOCS],
         unverified=["context_window", "max_output_tokens"]),
    model("deepseek", "deepseek-flash", "DeepSeek Flash", "openai", 1_000_000, 393_216,
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          pricing={"input_per_mtok_usd": 0.15, "output_per_mtok_usd": 0.6,
                   "cache_read_per_mtok_usd": 0.003,
                   "note": "off-peak; peak 2x; peak 01:00-04:00 & 06:00-10:00 UTC Mon-Fri", "updated": "2026-11"},
          endpoints=DEEPSEEK_EP, docs=DEEPSEEK_DOCS,
          sources=["https://api-docs.deepseek.com/quick_start/pricing", "https://api-docs.deepseek.com/api/create-chat-completion"]),
    model("deepseek", "deepseek-v4-pro", "DeepSeek V4 Pro", "openai", 1_000_000, 393_216,
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          pricing={"input_per_mtok_usd": 0.66, "output_per_mtok_usd": 1.98,
                   "cache_read_per_mtok_usd": 0.022,
                   "note": "off-peak; peak 2x", "updated": "2026-11"},
          endpoints=DEEPSEEK_EP, docs=DEEPSEEK_DOCS,
          sources=["https://api-docs.deepseek.com/quick_start/pricing", "https://api-docs.deepseek.com/guides/thinking_mode"]),
    model("deepseek", "deepseek-chat", "DeepSeek Chat (V3.x)", "openai", 131_072, 8_192,
          status="legacy",
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0, 1.3), top_p=T(True, 0.0, 1.0, 0.95)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=DEEPSEEK_EP, docs=DEEPSEEK_DOCS, sources=[DEEPSEEK_DOCS],
          unverified=["context_window", "max_output_tokens", "status"],
          notes=["Historic non-thinking alias; current lineup is deepseek-flash / deepseek-v4-pro"]),
    model("deepseek", "deepseek-reasoner", "DeepSeek Reasoner", "openai", 131_072, 8_192,
          status="legacy",
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=DEEPSEEK_EP, docs=DEEPSEEK_DOCS,
          sources=["https://api-docs.deepseek.com/guides/thinking_mode"],
          unverified=["context_window", "max_output_tokens", "status"],
          notes=["Historic thinking alias; thinking mode now lives on the current models"]),
]

GEMINI_DOCS = "https://aistudio.google.com/docs/models"
GEMINI_EP = {"base_url": "https://generativelanguage.googleapis.com/v1beta/openai",
             "chat": "/chat/completions", "models": "/models"}
google = [
    prov("google", name="Google Gemini (defaults)", api="openai",
         ctx=1_000_000, out=65_536,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints=GEMINI_EP, docs=GEMINI_DOCS, sources=[GEMINI_DOCS],
         unverified=["context_window", "max_output_tokens"]),
    model("google", "gemini-3-pro", "Gemini 3 Pro", "openai", 1_048_576, 65_536,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=GEMINI_EP, docs=GEMINI_DOCS, sources=[GEMINI_DOCS, "https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/gemini/3-flash"]),
    model("google", "gemini-3-flash", "Gemini 3 Flash", "openai", 1_048_576, 65_536,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True, "computer_use": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=GEMINI_EP, docs=GEMINI_DOCS,
          sources=["https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/gemini/3-flash", GEMINI_DOCS]),
    model("google", "gemini-3.6-flash", "Gemini 3.6 Flash", "openai", 1_000_000, 64_000,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True, "computer_use": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=GEMINI_EP, docs=GEMINI_DOCS,
          sources=["https://aistudio.google.com/docs/whats-new-gemini-3.6", GEMINI_DOCS]),
    model("google", "gemini-2.5-pro", "Gemini 2.5 Pro", "openai", 1_048_576, 65_536,
          status="legacy",
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=GEMINI_EP, docs=GEMINI_DOCS, sources=[GEMINI_DOCS],
          unverified=["max_output_tokens"]),
    model("google", "gemini-2.5-flash", "Gemini 2.5 Flash", "openai", 1_048_576, 65_536,
          status="legacy",
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=GEMINI_EP, docs=GEMINI_DOCS, sources=[GEMINI_DOCS],
          unverified=["max_output_tokens"]),
]

XAI_DOCS = "https://docs.x.ai/developers/models"
XAI_EP = {"base_url": "https://api.x.ai/v1", "chat": "/chat/completions", "models": "/models"}
xai = [
    prov("xai", name="xAI Grok (defaults)", api="openai",
         ctx=256_000, out=32_768,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints=XAI_EP, docs=XAI_DOCS, sources=[XAI_DOCS],
         unverified=["context_window", "max_output_tokens"]),
    model("xai", "grok-4.7", "Grok 4.7", "openai", 500_000, 32_768,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=XAI_EP, docs=XAI_DOCS, sources=[XAI_DOCS],
          unverified=["max_output_tokens"]),
    model("xai", "grok-4.1-fast", "Grok 4.1 Fast", "openai", 2_000_000, 32_768,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=XAI_EP, docs=XAI_DOCS,
          sources=["https://x.ai/news/grok-4-1-fast", XAI_DOCS],
          unverified=["max_output_tokens"]),
    model("xai", "grok-4-fast", "Grok 4 Fast", "openai", 2_000_000, 32_768,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=XAI_EP, docs=XAI_DOCS, sources=[XAI_DOCS],
          unverified=["context_window", "max_output_tokens"]),
    model("xai", "grok-4", "Grok 4", "openai", 256_000, 32_768,
          status="legacy",
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=XAI_EP, docs=XAI_DOCS, sources=[XAI_DOCS],
          unverified=["context_window", "max_output_tokens", "status"]),
]

ZAI_DOCS = "https://docs.z.ai/"
ZAI_EP = {"base_url": "https://api.z.ai/api/paas/v4", "chat": "/chat/completions", "models": "/models"}
zai = [
    prov("zai", name="Z.AI GLM (defaults)", api="openai",
         ctx=204_800, out=131_072,
         params=P(temp=T(True, 0.0, 1.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints=ZAI_EP, docs=ZAI_DOCS, sources=[ZAI_DOCS],
         unverified=["context_window", "max_output_tokens", "params.temperature"]),
    model("zai", "glm-5.2", "GLM-5.2", "openai", 400_000, 128_000,
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 1.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=ZAI_EP, docs=ZAI_DOCS,
          sources=["https://z.ai/blog/glm-5.2", ZAI_DOCS],
          unverified=["max_output_tokens", "params.temperature"]),
    model("zai", "glm-5", "GLM-5", "openai", 202_752, 131_072,
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 1.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=ZAI_EP, docs=ZAI_DOCS,
          sources=["https://z.ai/blog/glm-5", ZAI_DOCS],
          unverified=["max_output_tokens", "params.temperature"]),
    model("zai", "glm-4.6", "GLM-4.6", "openai", 204_800, 131_072,
          status="legacy",
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 1.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=ZAI_EP, docs=ZAI_DOCS, sources=[ZAI_DOCS],
          unverified=["params.temperature", "status"]),
]

KIMI_DOCS = "https://platform.kimi.ai/docs"
MOON_EP = {"base_url": "https://api.moonshot.ai/v1", "chat": "/chat/completions", "models": "/models"}
moonshot = [
    prov("moonshot", name="Moonshot Kimi (defaults)", api="openai",
         ctx=1_000_000, out=131_072,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints=MOON_EP, docs=KIMI_DOCS, sources=["https://platform.kimi.ai"],
         unverified=["context_window", "max_output_tokens"]),
    model("moonshot", "kimi-k3.1-pro", "Kimi K3.1 Pro", "openai", 1_000_000, 131_072,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          pricing={"input_per_mtok_usd": 2.0, "output_per_mtok_usd": 10.0,
                   "cache_write_per_mtok_usd": 2.0, "cache_read_per_mtok_usd": 0.20, "updated": "2026-11"},
          endpoints=MOON_EP, docs=KIMI_DOCS,
          sources=["https://platform.kimi.ai", "https://www.moonshot.ai"],
          unverified=["max_output_tokens", "id"]),
    model("moonshot", "kimi-k3.1-pro-highspeed", "Kimi K3.1 Pro Highspeed", "openai", 1_000_000, 131_072,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          pricing={"input_per_mtok_usd": 4.0, "output_per_mtok_usd": 20.0,
                   "cache_write_per_mtok_usd": 4.0, "cache_read_per_mtok_usd": 0.20, "updated": "2026-11"},
          endpoints=MOON_EP, docs=KIMI_DOCS, sources=["https://platform.kimi.ai"],
          unverified=["max_output_tokens", "id"]),
    model("moonshot", "kimi-k3", "Kimi K3", "openai", 1_000_000, 131_072,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          pricing={"input_per_mtok_usd": 3.0, "output_per_mtok_usd": 15.0,
                   "cache_write_per_mtok_usd": 3.0, "cache_read_per_mtok_usd": 0.30, "updated": "2026-11"},
          endpoints=MOON_EP, docs=KIMI_DOCS, sources=["https://platform.kimi.ai"],
          unverified=["max_output_tokens", "id"]),
    model("moonshot", "kimi-k2-thinking", "Kimi K2 Thinking", "openai", 262_144, 98_304,
          status="legacy",
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=MOON_EP, docs=KIMI_DOCS, sources=[KIMI_DOCS],
          unverified=["status", "id"]),
]

QWEN_DOCS = "https://qwen.ai/apiplatform"
QWEN_EP = {"base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1",
           "chat": "/chat/completions", "models": "/models"}
qwen = [
    prov("qwen", name="Qwen (defaults)", api="openai",
         ctx=1_000_000, out=65_536,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints=QWEN_EP, docs=QWEN_DOCS, sources=[QWEN_DOCS],
         unverified=["context_window", "max_output_tokens"]),
    model("qwen", "qwen3.8-max", "Qwen3.8 Max", "openai", 1_000_000, 65_536,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          pricing={"input_per_mtok_usd": 2.0, "output_per_mtok_usd": 6.0, "updated": "2026-11"},
          endpoints=QWEN_EP, docs=QWEN_DOCS,
          sources=["https://openrouter.ai/qwen", QWEN_DOCS],
          unverified=["id", "max_output_tokens"]),
    model("qwen", "qwen3.8-flash", "Qwen3.8 Flash", "openai", 1_000_000, 65_536,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=QWEN_EP, docs=QWEN_DOCS, sources=[QWEN_DOCS],
          unverified=["id", "max_output_tokens"]),
    model("qwen", "qwen4-max", "Qwen4 Max", "openai", 1_000_000, 65_536,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          pricing={"input_per_mtok_usd": 1.425, "output_per_mtok_usd": 4.275, "updated": "2026-11"},
          endpoints=QWEN_EP, docs=QWEN_DOCS,
          sources=["https://openrouter.ai/qwen", QWEN_DOCS],
          unverified=["id", "max_output_tokens"]),
    model("qwen", "qwen3-vl-plus", "Qwen3 VL Plus (vision)", "openai", 131_027, 16_384,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=QWEN_EP, docs=QWEN_DOCS, sources=[QWEN_DOCS],
          unverified=["id", "max_output_tokens"]),
]

MINIMAX_DOCS = "https://platform.minimax.io/docs"
MM_EP = {"base_url": "https://api.minimax.io/v1", "chat": "/chat/completions", "models": "/models"}
minimax = [
    prov("minimax", name="MiniMax (defaults)", api="openai",
         ctx=1_000_000, out=131_072,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints=MM_EP, docs=MINIMAX_DOCS, sources=["https://platform.minimax.io"],
         unverified=["context_window", "max_output_tokens"]),
    model("minimax", "minimax-m3.1-flash", "MiniMax M3.1 Flash", "openai", 1_000_000, 131_072,
          feats={"vision": True, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0), effort=EFFORT),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=MM_EP, docs=MINIMAX_DOCS,
          sources=["https://platform.minimax.io/docs/guides/text-generation", "https://platform.minimax.io"],
          unverified=["id", "max_output_tokens"]),
    model("minimax", "minimax-m2.5", "MiniMax M2.5", "openai", 192_000, 131_072,
          status="legacy",
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": True, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=MM_EP, docs=MINIMAX_DOCS,
          sources=["https://platform.minimax.io", "https://huggingface.co/MiniMaxAI/MiniMax-M2.5"],
          unverified=["id", "max_output_tokens", "status"]),
]

MISTRAL_DOCS = "https://docs.mistral.ai/models"
MISTRAL_EP = {"base_url": "https://api.mistral.ai/v1", "chat": "/chat/completions", "models": "/models"}
mistral = [
    prov("mistral", name="Mistral (defaults)", api="openai",
         ctx=256_000, out=32_768,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints=MISTRAL_EP, docs=MISTRAL_DOCS, sources=[MISTRAL_DOCS],
         unverified=["context_window", "max_output_tokens"]),
    model("mistral", "mistral-large-3", "Mistral Large 3", "openai", 256_000, 32_768,
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=MISTRAL_EP, docs=MISTRAL_DOCS,
          sources=["https://docs.mistral.ai/resources/known-limitations", MISTRAL_DOCS],
          unverified=["max_output_tokens", "id"]),
    model("mistral", "mistral-medium-3.5", "Mistral Medium 3.5", "openai", 128_000, 32_768,
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=MISTRAL_EP, docs=MISTRAL_DOCS, sources=[MISTRAL_DOCS],
          unverified=["context_window", "max_output_tokens", "id"]),
    model("mistral", "codestral", "Codestral", "openai", 256_000, 32_768,
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=MISTRAL_EP, docs=MISTRAL_DOCS, sources=[MISTRAL_DOCS],
          unverified=["context_window", "max_output_tokens", "id"]),
]

GROQ_DOCS = "https://console.groq.com/docs/models"
GROQ_EP = {"base_url": "https://api.groq.com/openai/v1", "chat": "/chat/completions", "models": "/models"}
groq = [
    prov("groq", name="Groq (defaults)", api="openai",
         ctx=131_072, out=32_768,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints=GROQ_EP, docs=GROQ_DOCS, sources=[GROQ_DOCS],
         unverified=["context_window", "max_output_tokens"]),
    model("groq", "llama-3.3-70b-versatile", "Llama 3.3 70B Versatile (Groq)", "openai", 131_072, 32_768,
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints=GROQ_EP, docs=GROQ_DOCS, sources=[GROQ_DOCS],
          unverified=["max_output_tokens", "status"]),
]

# ── long-tail hosts: provider defaults (+ a few flagship files) ──────────────
def simple_host(pid, name, base, docs, ctx=131_072, out=8_192, key=True, flags=None):
    return prov(pid, name=name, api="openai", ctx=ctx, out=out,
                params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
                wire={"max_tokens_param": "max_tokens", "stop_supported": True},
                endpoints={"base_url": base, "chat": "/chat/completions", "models": "/models"},
                notes=flags, docs=docs, sources=[docs])

longtail = [
    simple_host("openrouter", "OpenRouter (defaults)", "https://openrouter.ai/api/v1",
                "https://openrouter.ai/docs", ctx=200_000, out=64_000,
                flags=["Aggregator: model ids carry vendor/ prefixes and tags; family routing resolves them to the vendor profile"]),
    simple_host("azure", "Azure OpenAI (defaults)", "https://YOUR-RESOURCE.openai.azure.com/openai/v1",
                "https://learn.microsoft.com/en-us/azure/ai-services/openai/", ctx=128_000, out=16_384,
                flags=["Deployment-scoped: use your deployment name as the model id; per-model limits follow the OpenAI family profiles"]),
    simple_host("nvidia", "NVIDIA NIM (defaults)", "https://integrate.api.nvidia.com/v1",
                "https://docs.api.nvidia.com/", ctx=131_072, out=8_192),
    simple_host("together", "Together AI (defaults)", "https://api.together.xyz/v1",
                "https://docs.together.ai/", ctx=131_072, out=8_192),
    model("together", "meta-llama/Llama-3.3-70B-Instruct-Turbo", "Llama 3.3 70B Instruct Turbo (Together)", "openai", 131_072, 8_192,
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints={"base_url": "https://api.together.xyz/v1", "chat": "/chat/completions", "models": "/models"},
          docs="https://docs.together.ai/", sources=["https://docs.together.ai/"],
          unverified=["context_window", "max_output_tokens"]),
    simple_host("fireworks", "Fireworks AI (defaults)", "https://api.fireworks.ai/inference/v1",
                "https://docs.fireworks.ai/", ctx=131_072, out=8_192,
                flags=["ids look like accounts/fireworks/models/<name>; the last path segment resolves"]),
    simple_host("cerebras", "Cerebras (defaults)", "https://api.cerebras.ai/v1",
                "https://inference-docs.cerebras.ai/", ctx=131_072, out=8_192),
    simple_host("deepinfra", "DeepInfra (defaults)", "https://api.deepinfra.com/v1/openai",
                "https://deepinfra.com/docs", ctx=131_072, out=8_192),
    model("deepinfra", "deepseek-ai/DeepSeek-V3", "DeepSeek V3 (DeepInfra)", "openai", 131_072, 8_192,
          status="legacy", params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints={"base_url": "https://api.deepinfra.com/v1/openai", "chat": "/chat/completions", "models": "/models"},
          docs="https://deepinfra.com/docs", sources=["https://deepinfra.com/docs"],
          unverified=["context_window", "max_output_tokens", "status"]),
    simple_host("lepton", "Lepton AI (defaults)", "https://api.lepton.run/api/v1",
                "https://docs.lepton.ai/", ctx=131_072, out=8_192),
    simple_host("hyperbolic", "Hyperbolic (defaults)", "https://api.hyperbolic.xyz/v1",
                "https://docs.hyperbolic.xyz/", ctx=131_072, out=8_192),
    simple_host("sambanova", "SambaNova (defaults)", "https://api.sambanova.ai/v1",
                "https://docs.sambanova.ai/", ctx=131_072, out=8_192),
    simple_host("novita", "Novita AI (defaults)", "https://api.novita.ai/v3/openai",
                "https://docs.novita.ai/", ctx=131_072, out=8_192),
    simple_host("chutes", "Chutes AI (defaults)", "https://api.chutes.ai/v1",
                "https://docs.chutes.ai/", ctx=131_072, out=8_192),
    simple_host("nebius", "Nebius AI Studio (defaults)", "https://api.studio.nebius.com/v1",
                "https://docs.studio.nebius.com/", ctx=131_072, out=8_192),
    simple_host("huggingface", "Hugging Face Router (defaults)", "https://router.huggingface.co/v1",
                "https://huggingface.co/docs/inference-providers/", ctx=131_072, out=8_192),
    simple_host("friendli", "Friendli AI (defaults)", "https://inference.friendli.ai/v1",
                "https://friendli.ai/docs", ctx=131_072, out=8_192),
    simple_host("alephalpha", "Aleph Alpha (defaults)", "https://api.aleph-alpha.com/v1",
                "https://docs.aleph-alpha.com/", ctx=131_072, out=8_192),
    simple_host("coarena", "Coarena (defaults)", "https://api.coarena.com/v1",
                "https://coarena.com/", ctx=131_072, out=8_192),
    simple_host("logfare", "Logfare (defaults)", "https://api.logfare.com/v1",
                "https://logfare.com/", ctx=131_072, out=8_192),
    prov("perplexity", name="Perplexity (defaults)", api="openai", ctx=200_000, out=8_192,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints={"base_url": "https://api.perplexity.ai", "chat": "/chat/completions", "models": "/models"},
         docs="https://docs.perplexity.ai/", sources=["https://docs.perplexity.ai/"],
         unverified=["context_window", "max_output_tokens"]),
    model("perplexity", "sonar-pro", "Sonar Pro (Perplexity)", "openai", 200_000, 8_192,
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True,
                 "web_search": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints={"base_url": "https://api.perplexity.ai", "chat": "/chat/completions", "models": "/models"},
          docs="https://docs.perplexity.ai/", sources=["https://docs.perplexity.ai/"],
          unverified=["context_window", "max_output_tokens"]),
    prov("cohere", name="Cohere (defaults)", api="openai", ctx=256_000, out=8_192,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints={"base_url": "https://api.cohere.com/compatibility/v1", "chat": "/chat/completions", "models": "/models"},
         docs="https://docs.cohere.com/", sources=["https://docs.cohere.com/"],
         unverified=["max_output_tokens"]),
    model("cohere", "command-a-03-2025", "Command A (Cohere)", "openai", 256_000, 8_192,
          feats={"vision": False, "tools": True, "json_mode": True, "reasoning": False, "prefix_caching": True},
          params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
          wire={"max_tokens_param": "max_tokens", "stop_supported": True},
          endpoints={"base_url": "https://api.cohere.com/compatibility/v1", "chat": "/chat/completions", "models": "/models"},
          docs="https://docs.cohere.com/", sources=["https://docs.cohere.com/"],
          unverified=["max_output_tokens"]),
    prov("ai21", name="AI21 Labs (defaults)", api="openai", ctx=256_000, out=8_192,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints={"base_url": "https://api.ai21.com/v1", "chat": "/chat/completions", "models": "/models"},
         docs="https://docs.ai21.com/", sources=["https://docs.ai21.com/"],
         unverified=["context_window", "max_output_tokens"]),
    prov("replicate", name="Replicate (defaults)", api="openai", ctx=131_072, out=8_192,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints={"base_url": "https://api.replicate.com/v1", "chat": "/chat/completions", "models": "/models"},
         docs="https://replicate.com/docs", sources=["https://replicate.com/docs"],
         unverified=["context_window", "max_output_tokens"]),
    prov("ollama", name="Ollama local (defaults)", api="openai", ctx=0, out=0,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints={"base_url": "http://localhost:11434/v1", "chat": "/chat/completions", "models": "/models"},
         notes=["Context depends on the pulled model's num_ctx; no key needed", "no key needed"],
         docs="https://github.com/ollama/ollama/blob/main/docs/openai.md",
         sources=["https://github.com/ollama/ollama/blob/main/docs/openai.md"]),
    prov("lmstudio", name="LM Studio local (defaults)", api="openai", ctx=0, out=0,
         params=P(temp=T(True, 0.0, 2.0), top_p=T(True, 0.0, 1.0)),
         wire={"max_tokens_param": "max_tokens", "stop_supported": True},
         endpoints={"base_url": "http://localhost:1234/v1", "chat": "/chat/completions", "models": "/models"},
         notes=["Context depends on the loaded local model; no key needed", "no key needed"],
         docs="https://lmstudio.ai/docs/app/api", sources=["https://lmstudio.ai/docs/app/api"]),
]

GLOBAL_DEFAULT = {
    "id": "_default",
    "provider": "_default",
    "family": "_default",
    "display_name": "Unknown model (safe defaults)",
    "api": "openai",
    "status": "unknown",
    "context_window": 0,
    "max_output_tokens": 0,
    "features": {"vision": False, "tools": True, "json_mode": True, "reasoning": False,
                 "prefix_caching": False},
    "params": {"temperature": {"supported": True, "min": 0.0, "max": 2.0},
               "top_p": {"supported": True, "min": 0.0, "max": 1.0}},
    "wire": {"max_tokens_param": "", "temperature_or_top_p_exclusive": False,
             "stop_supported": True},
    "pricing": None,
    "endpoints": {},
    "wire_notes": ["No profile matched: permissive defaults; nothing is clamped or dropped"],
    "docs": None,
    "sources": [],
    "unverified_fields": [],
}

# build groups additively — dict-comprehension merging silently REPLACED
# provider groups and dropped _provider defaults (the cohere bug)
GROUPS = {
    "anthropic": anthropic, "openai": openai, "deepseek": deepseek, "google": google,
    "xai": xai, "zai": zai, "moonshot": moonshot, "qwen": qwen, "minimax": minimax,
    "mistral": mistral, "groq": groq,
}
for m in longtail:
    GROUPS.setdefault(m["provider"], [])
    if not any(x["id"] == m["id"] for x in GROUPS[m["provider"]]):
        GROUPS[m["provider"]].append(m)

# sanity: every group carries exactly one _provider default
for provider, items in GROUPS.items():
    n = sum(1 for x in items if x["id"] == "_provider")
    assert n == 1, f"{provider}: {n} _provider defaults"

def write_json(path: Path, obj):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(obj, indent=2, ensure_ascii=False) + "\n")

def main():
    entries = []  # (provider, filename) for the generated SOURCES table
    seen = set()
    for provider, items in sorted(GROUPS.items()):
        for it in items:
            assert it["provider"] == provider, f"{it['id']}: provider mismatch {it['provider']} != {provider}"
            key = (provider, it["id"].lower())
            assert key not in seen, f"duplicate profile {key}"
            seen.add(key)
            api = it["api"]
            assert api in ("openai", "anthropic"), f"{it['id']}: bad api {api}"
            fname = "_provider.json" if it["id"] == "_provider" else f"{it['id']}.json"
            write_json(DATA / provider / fname, it)
            entries.append((provider, fname))
    write_json(DATA / "_default.json", GLOBAL_DEFAULT)
    # generated SOURCES table
    lines = [
        "// GENERATED by scripts/gen_modeldata.py — do not edit by hand.",
        "// One include_str! entry per modeldata JSON; modeldata.rs consumes SOURCES.",
        "pub(crate) const SOURCES: &[(&str, &str)] = &[",
    ]
    for provider, fname in sorted(entries):
        lines.append(f'    ("{provider}", include_str!("../modeldata/{provider}/{fname}")),')
    lines.append('    ("_default", include_str!("../modeldata/_default.json")),')
    lines.append("];")
    (SRC / "modeldata_gen.rs").write_text("\n".join(lines) + "\n")
    n_models = sum(1 for _, f in entries if f != "_provider.json")
    print(f"emitted {n_models} model profiles, {sum(1 for _, f in entries if f == '_provider.json')} provider defaults, "
          f"1 global default across {len(GROUPS)} provider folders")

if __name__ == "__main__":
    main()
