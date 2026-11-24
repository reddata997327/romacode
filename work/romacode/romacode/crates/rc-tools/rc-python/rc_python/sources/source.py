"""rc_python.sources.source — the ToolSource contract (Python port of ``rc_sources::source``).

Every tool-source mechanism implements :meth:`ToolSource.tools`: discover and
load in one step, returning fully-built, ready-to-register tools. The
lifecycle manager iterates a :class:`SourceRegistry` of sources and registers
their results.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from typing import List

from ..core import Tool, ToolSourceKind


class SourceError(Exception):
    """Errors a source can surface during discovery or loading."""

    def __init__(self, src: str, detail: str) -> None:
        super().__init__(f"source `{src}`: {detail}")
        self.src = src
        self.detail = detail


class ToolSource(ABC):
    """The contract every tool source implements."""

    @abstractmethod
    def kind(self) -> ToolSourceKind:
        """Which of the mechanisms this source is."""

    @abstractmethod
    def id(self) -> str:
        """A short id for this source instance (e.g. ``builtin``, ``rest:github``)."""

    @abstractmethod
    async def tools(self) -> List[Tool]:
        """Produce every tool this source can, fully built and ready to register."""


class SourceRegistry:
    """A registry of sources — what the lifecycle manager iterates over."""

    def __init__(self) -> None:
        self._sources: List[ToolSource] = []

    def add(self, source: ToolSource) -> None:
        self._sources.append(source)

    def __len__(self) -> int:
        return len(self._sources)

    def is_empty(self) -> bool:
        return len(self) == 0

    def __iter__(self):
        return iter(self._sources)

    async def all_tools(self) -> List[Tool]:
        """Run every source's ``tools()`` and flatten the results.

        Sources are awaited concurrently so a slow HTTP source does not block
        a fast builtin one.
        """
        import asyncio
        if not self._sources:
            return []
        batches = await asyncio.gather(
            *(s.tools() for s in self._sources), return_exceptions=True
        )
        out: List[Tool] = []
        for s, batch in zip(self._sources, batches):
            if isinstance(batch, Exception):
                raise SourceError(s.id(), str(batch)) from batch
            out.extend(batch)
        return out
