"""rc_python.sources.rest — source 5: one HTTP endpoint → one tool.

Wraps a single HTTP call behind a :class:`~rc_python.sources.tools.HttpTool`
so it can be registered and run through the framework like any other tool.
"""

from __future__ import annotations

from typing import List, Optional, Tuple

from ..core import Tool, ToolSourceKind
from ..schema import Schema
from .source import ToolSource
from .tools import HttpTool


class RestSource(ToolSource):
    def __init__(
        self,
        name: str,
        description: str,
        schema: Schema,
        method: str,
        url: str,
        body: Optional[str] = None,
        headers: Optional[List[Tuple[str, str]]] = None,
        source_id: str = "rest",
    ) -> None:
        self._id = source_id
        self._tool = HttpTool(
            name=name,
            description=description,
            schema=schema,
            method=method,
            url=url,
            source=ToolSourceKind.Rest,
            body=body,
            headers=headers,
        )

    def kind(self) -> ToolSourceKind:
        return ToolSourceKind.Rest

    def id(self) -> str:
        return self._id

    async def tools(self) -> List[Tool]:
        return [self._tool]
