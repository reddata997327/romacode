"""rc_python.compose.templating — template resolution shared by the composition primitives.

Handles ``{{prev}}``, ``{{prev.x}}``, ``{{input.y}}``, and ``{{nodes.NAME.z}}``
references, mirroring the Rust ``rc_compose::templating`` module. A reference
that cannot be resolved is left as-is (the caller will see the literal
``{{...}}``); a reference whose path does not exist resolves to ``None``.
"""

from __future__ import annotations

import re
from typing import Any, Dict

_PLACEHOLDER = re.compile(r"\{\{\s*([a-zA-Z0-9_.]+)\s*\}\}")


def lookup(value: Any, path: str) -> Any:
    """Walk a dotted path through ``value``. Returns ``None`` on any miss."""
    cur = value
    for part in path.split("."):
        if isinstance(cur, dict) and part in cur:
            cur = cur[part]
        elif isinstance(cur, list):
            try:
                cur = cur[int(part)]
            except (ValueError, IndexError):
                return None
        else:
            return None
    return cur


def resolve(tmpl: Any, prev: Any, input: Dict[str, Any], nodes: Dict[str, Any] = None) -> Any:
    """Resolve placeholders in ``tmpl``.

    Mirrors the Rust ``rc_compose::templating::resolve``:

    * a **whole-string** template ``"{{path}}"`` (no inner ``{`` and no spaces)
      is replaced by the referenced value **with its type preserved** — a
      number stays a number, a dict stays a dict. This is what lets a
      ``math.sqrt`` step receive ``{"n": "{{prev}}"}`` where ``prev`` is a
      float;
    * a template **embedded** in a larger string (``"count: {{prev}}"``) is
      string-substituted, and the result stays a string;
    * dicts and lists are recursed into, so templates can be structured.
    """
    nodes = nodes or {}
    if isinstance(tmpl, str):
        return _resolve_string(tmpl, prev, input, nodes)
    if isinstance(tmpl, dict):
        return {k: resolve(v, prev, input, nodes) for k, v in tmpl.items()}
    if isinstance(tmpl, list):
        return [resolve(v, prev, input, nodes) for v in tmpl]
    return tmpl


def _resolve_string(s: str, prev: Any, input: Dict[str, Any], nodes: Dict[str, Any]) -> Any:
    # whole-string "{{path}}" → the looked-up value, type preserved
    inner = _whole_placeholder(s)
    if inner is not None:
        return resolve_ref(inner, prev, input, nodes)
    if "{{" in s:
        return _PLACEHOLDER.sub(
            lambda m: _render(resolve_ref(m.group(1), prev, input, nodes)), s
        )
    return s


def _whole_placeholder(s: str) -> "str | None":
    """If ``s`` is exactly ``{{path}}`` with no inner ``{`` and no spaces,
    return the trimmed inner path; otherwise None."""
    if not s.startswith("{{") or not s.endswith("}}"):
        return None
    inner = s[2:-2]
    if "{" in inner or " " in inner:
        return None
    return inner.strip()


def resolve_ref(ref: str, prev: Any, input: Dict[str, Any], nodes: Dict[str, Any]) -> Any:
    head, _, rest = ref.partition(".")
    if head == "prev":
        return lookup(prev, rest) if rest else prev
    if head == "input":
        return lookup(input, rest) if rest else input
    if head == "nodes":
        return lookup(nodes, rest) if rest else nodes
    # unknown root → leave the literal placeholder value
    return None


def _render(v: Any) -> str:
    if v is None:
        return ""
    if isinstance(v, str):
        return v
    import json
    try:
        return json.dumps(v, ensure_ascii=False)
    except (TypeError, ValueError):
        return str(v)
