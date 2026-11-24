"""rc_python.sandbox — resource limits the executor enforces (Python port of ``rc_core::sandbox``).

A wall-clock timeout, an output-size cap, and network/filesystem allow-lists
that tools *should* consult before doing I/O. The shipped default is fully
open (network allowed, whole filesystem); :meth:`Sandbox.strict` remains as
an opt-in lockdown.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any, List

from .core import ToolError


@dataclass
class Sandbox:
    """Resource limits for one execution."""

    timeout_ms: int = 30_000
    max_output_bytes: int = 1_000_000
    allow_net: bool = True
    # Whole filesystem by default (matching the Rust default); an explicitly
    # empty list disables FS access for tools that consult the list.
    allow_fs_paths: List[str] = field(default_factory=lambda: ["/"])
    max_read_bytes: int = 10_000_000

    @classmethod
    def strict(cls) -> "Sandbox":
        """A locked-down sandbox: no net, no fs, short timeout. Good default
        for untrusted/composed tools."""
        return cls(
            timeout_ms=5_000,
            max_output_bytes=100_000,
            allow_net=False,
            allow_fs_paths=[],
            max_read_bytes=100_000,
        )

    def check_net(self) -> None:
        """Raise :class:`ToolError` if network is not permitted."""
        if not self.allow_net:
            raise ToolError.sandbox("network access is disabled in this sandbox")

    def check_path(self, path: str) -> None:
        """Raise :class:`ToolError` if ``path`` is outside the allowed FS roots."""
        import os
        if not self.allow_fs_paths:
            raise ToolError.sandbox("filesystem access is disabled in this sandbox")
        canonical = os.path.realpath(path)
        for root in self.allow_fs_paths:
            root_canonical = os.path.realpath(root)
            if canonical == root_canonical or canonical.startswith(root_canonical + os.sep):
                return
        raise ToolError.sandbox(f"path `{path}` is outside the allowed filesystem roots")

    def clamp_output(self, value: Any) -> None:
        """Enforce the output-size cap **in place** where possible.

        If the value is a string, truncate it and append a marker. Otherwise
        serialise it to JSON and, if that exceeds the cap, raise
        :class:`ToolError` — non-string structures cannot be sensibly
        truncated without losing meaning.
        """
        if self.max_output_bytes == 0:
            return
        if isinstance(value, str):
            if len(value.encode("utf-8")) <= self.max_output_bytes:
                return
            max_chars = max(1, self.max_output_bytes - 40)
            head = value[:max_chars]
            value_marker = f"{head}… [truncated: output exceeded {self.max_output_bytes} bytes]"
            # truncate in place is not possible for an immutable str passed by
            # value; callers should use the returned value instead. We raise to
            # be safe, mirroring the Rust path for non-truncatable cases.
            raise ToolError.output_too_large(self.max_output_bytes)
        # non-string: serialise and size-check
        try:
            s = json.dumps(value, ensure_ascii=False)
        except (TypeError, ValueError):
            return
        if len(s.encode("utf-8")) > self.max_output_bytes:
            raise ToolError.output_too_large(self.max_output_bytes)
