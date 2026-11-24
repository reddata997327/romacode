"""rc_python.manifest — serialisable tool identity (Python port of ``rc_core::manifest``).

A :class:`Manifest` is the description of a tool independent of its runtime
:class:`~rc_python.core.Tool` impl. Sources produce manifests during
discovery; the registry holds live ``Tool`` impls. A manifest is everything
you need to *know* about a tool without loading it.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import List, Optional

from .core import Tool
from .schema import Schema


@dataclass
class Manifest:
    name: str
    description: str
    category: str
    source: str
    version: str
    schema: Schema = field(default_factory=Schema.new)
    permission: str = ""
    origin: Optional[str] = None

    @classmethod
    def from_tool(cls, t: Tool) -> "Manifest":
        """Build a manifest from a live tool (snapshots identity + schema)."""
        return cls(
            name=t.name(),
            description=t.description(),
            category=t.category(),
            source=t.source().as_str,
            version=t.version(),
            schema=t.schema(),
            permission="",
            origin=None,
        )

    def to_json(self) -> str:
        import json
        return json.dumps(
            {
                "name": self.name,
                "description": self.description,
                "category": self.category,
                "source": self.source,
                "version": self.version,
                "permission": self.permission,
                "origin": self.origin,
            },
            indent=2,
        )


@dataclass
class Catalog:
    """A directory of manifests — what a source advertises before loading."""

    manifests: List[Manifest] = field(default_factory=list)

    @classmethod
    def new(cls) -> "Catalog":
        return cls()

    def push(self, m: Manifest) -> None:
        self.manifests.append(m)

    def __len__(self) -> int:
        return len(self.manifests)

    def is_empty(self) -> bool:
        return len(self) == 0

    def names(self) -> List[str]:
        n = [m.name for m in self.manifests]
        n.sort()
        return n
