"""rc_python.registry — the in-memory tool store (Python port of ``rc_core::registry``).

A thread-safe store of every loaded tool, indexed by name. The registry is
the spine of the framework: sources feed it, the executor reads it,
composition reads it to resolve steps, and the CLI lists it.
"""

from __future__ import annotations

import threading
from dataclasses import dataclass
from typing import Dict, List, Optional

from .core import Tool, ToolError


@dataclass
class ToolInfo:
    """A read-only snapshot of one registered tool's identity."""

    name: str
    description: str
    category: str
    source: str
    version: str


class Registry:
    """A thread-safe registry of tools, keyed by ``tool.name()``."""

    def __init__(self) -> None:
        self._tools: Dict[str, Tool] = {}
        self._lock = threading.RLock()

    def register(self, tool: Tool) -> Optional[Tool]:
        """Register a tool. If one with the same name exists it is replaced
        (the new version wins — see the lifecycle manager for upgrade policy).
        Returns the previous tool if one was displaced.
        """
        with self._lock:
            prev = self._tools.get(tool.name())
            self._tools[tool.name()] = tool
            return prev

    def register_all(self, tools: List[Tool]) -> None:
        """Register many tools at once (e.g. from a source's ``tools()``)."""
        with self._lock:
            for t in tools:
                self._tools[t.name()] = t

    def get(self, name: str) -> Tool:
        """Look up a tool by name. Raises :class:`ToolError` if absent."""
        with self._lock:
            t = self._tools.get(name)
        if t is None:
            raise ToolError.not_found(name)
        return t

    def contains(self, name: str) -> bool:
        with self._lock:
            return name in self._tools

    def remove(self, name: str) -> Optional[Tool]:
        """Retire a tool. Returns the removed tool if present."""
        with self._lock:
            return self._tools.pop(name, None)

    def __len__(self) -> int:
        with self._lock:
            return len(self._tools)

    def is_empty(self) -> bool:
        return len(self) == 0

    def list(self) -> List[ToolInfo]:
        """A sorted snapshot of every tool's identity."""
        with self._lock:
            infos = [
                ToolInfo(
                    name=t.name(),
                    description=t.description(),
                    category=t.category(),
                    source=t.source().as_str,
                    version=t.version(),
                )
                for t in self._tools.values()
            ]
        infos.sort(key=lambda i: i.name)
        return infos

    def names(self) -> List[str]:
        """All registered tool names, sorted."""
        with self._lock:
            n = list(self._tools.keys())
        n.sort()
        return n
