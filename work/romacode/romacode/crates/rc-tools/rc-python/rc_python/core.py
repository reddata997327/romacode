"""rc_python.core — the foundation of the RomaCode tools builder (Python port).

The single contract every tool implements (:class:`Tool`), the per-call
context (:class:`ToolContext`), the source-kind enum
(:class:`ToolSourceKind`), and the error type (:class:`ToolError`). Every
other module in the package builds on this one.

This is a faithful Python port of the Rust crate ``rc-core``: the same trait
shape (``name`` / ``description`` / ``schema`` / ``source`` / ``category`` /
``version`` / ``execute``), the same context fields, the same source kinds.
The only divergence is that Python tool input/output is native Python data
(``dict`` / ``list`` / ``str`` / ``int`` / ``float`` / ``bool`` / ``None``)
instead of ``serde_json::Value`` — there is no separate JSON layer to bridge.
"""

from __future__ import annotations

import os
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Dict, List, Optional, Tuple


class ToolSourceKind(Enum):
    """Which of the universal tool-source mechanisms produced a tool.

    Mirrors ``rc_core::tool::ToolSourceKind``. Once loaded, every tool is just
    a :class:`Tool` — the source is metadata, not a special case.
    """

    Builtin = "builtin"
    Declarative = "declarative"
    LlmGenerated = "llm_gen"
    OpenApi = "openapi"
    Mcp = "mcp"
    Rest = "rest"
    GraphQL = "graphql"
    Cli = "cli"
    Compose = "compose"
    Marketplace = "marketplace"

    @property
    def as_str(self) -> str:
        return self.value


class ToolError(Exception):
    """Every tool-execution failure flows through ``ToolError``.

    Carries a ``kind`` (one of ``not_found`` / ``validation`` / ``bad_param``
    / ``permission_denied`` / ``sandbox`` / ``timeout`` / ``output_too_large``
    / ``tool_failed`` / ``io`` / ``json`` / ``other``) and a ``detail``. The
    string form is ``[kind] detail``, which composes cleanly in pipelines.
    """

    def __init__(self, detail: str, *, kind: str = "other") -> None:
        super().__init__(f"[{kind}] {detail}")
        self.kind = kind
        self.detail = detail

    # ── constructors mirroring the Rust enum variants ───────────────────────
    @classmethod
    def not_found(cls, name: str) -> "ToolError":
        return cls(f"tool not found: {name}", kind="not_found")

    @classmethod
    def validation(cls, msg: str) -> "ToolError":
        return cls(msg, kind="validation")

    @classmethod
    def bad_param(cls, name: str, expected: str, detail: str) -> "ToolError":
        return cls(f"parameter `{name}` (expected {expected}) — {detail}", kind="bad_param")

    @classmethod
    def permission_denied(cls, tool: str, reason: str) -> "ToolError":
        return cls(f"permission denied for tool `{tool}`: {reason}", kind="permission_denied")

    @classmethod
    def sandbox(cls, msg: str) -> "ToolError":
        return cls(msg, kind="sandbox")

    @classmethod
    def timeout(cls, timeout_ms: int) -> "ToolError":
        return cls(f"execution timed out after {timeout_ms} ms", kind="timeout")

    @classmethod
    def output_too_large(cls, limit: int) -> "ToolError":
        return cls(f"output exceeded sandbox limit ({limit} bytes)", kind="output_too_large")

    @classmethod
    def tool_failed(cls, tool: str, inner: str) -> "ToolError":
        return cls(f"tool `{tool}` returned an error: {inner}", kind="tool_failed")


ToolResult = Tuple[Any, Optional[ToolError]]  # convenience alias; we raise in practice


@dataclass
class ToolContext:
    """Per-call execution context.

    The executor populates ``allow_net`` / ``allow_fs_paths`` from its
    :class:`~rc_python.sandbox.Sandbox` so tools can self-check I/O policy.
    Composition passes a child context derived from this one.
    """

    correlation_id: str = "default"
    timeout_ms: int = 0
    cancelled: bool = False
    allow_net: bool = True
    # Whole filesystem by default (matching the Rust default); an explicitly
    # empty list disables FS access for tools that consult the list.
    allow_fs_paths: List[str] = field(default_factory=lambda: ["/"])

    @classmethod
    def new(cls, correlation_id: str) -> "ToolContext":
        return cls(correlation_id=correlation_id)

    def with_timeout(self, ms: int) -> "ToolContext":
        self.timeout_ms = ms
        return self

    def check_net(self) -> None:
        """Raise :class:`ToolError` if outbound network is not permitted."""
        if not self.allow_net:
            raise ToolError.sandbox("network access is disabled in this context")

    def check_path(self, path: str) -> None:
        """Raise :class:`ToolError` if ``path`` is outside the allowed FS roots.

        An empty allow-list means no FS access at all. Paths are canonicalised
        before the prefix check so ``..`` traversal cannot escape a root.
        """
        if not self.allow_fs_paths:
            raise ToolError.sandbox("filesystem access is disabled in this context")
        canonical = os.path.realpath(path)
        for root in self.allow_fs_paths:
            root_canonical = os.path.realpath(root)
            if canonical == root_canonical or canonical.startswith(
                root_canonical + os.sep
            ):
                return
        raise ToolError.sandbox(f"path `{path}` is outside the allowed filesystem roots")


class Tool(ABC):
    """The single trait every tool implements.

    ``execute`` is a coroutine so tools that do network or subprocess I/O can
    run without blocking the executor's event loop.
    """

    @abstractmethod
    def name(self) -> str:
        """Stable, unique, dot-namespaced id (e.g. ``math.add``)."""

    @abstractmethod
    def description(self) -> str:
        """Human-readable summary."""

    @abstractmethod
    def schema(self) -> "Schema":  # noqa: F821 — forward ref to rc_python.schema.Schema
        """The input schema; the validator runs against this before ``execute``."""

    def source(self) -> ToolSourceKind:
        """Which source produced this tool (default: builtin)."""
        return ToolSourceKind.Builtin

    def category(self) -> str:
        """Category label for grouping. Default: first segment of the name."""
        return self.name().split(".", 1)[0] if "." in self.name() else "misc"

    def version(self) -> str:
        """Semver version (default ``0.1.0``)."""
        return "0.1.0"

    @abstractmethod
    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> Any:
        """Run the tool. ``input`` matches :meth:`schema`."""


def arc(tool: Tool) -> Tool:
    """Return ``tool`` unchanged. In Python tools are plain objects shared by
    reference, so there is no ``Arc`` wrapper — this helper exists only for
    API parity with the Rust ``rc_core::tool::arc``."""
    return tool


# In Python tools are plain objects shared by reference; `ToolArc` is just an
# alias for `Tool`, kept for API parity with the Rust `ToolArc = Arc<dyn Tool>`.
ToolArc = Tool


# The framework version (matches pyproject.toml).
VERSION = "0.1.0"
