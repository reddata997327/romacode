"""rc_python.sources.cli — source 6: a binary + arg template → one tool.

Wraps a single executable behind a :class:`~rc_python.sources.tools.ShellTool`
so it can be registered and run through the framework like any other tool.
"""

from __future__ import annotations

from typing import List

from ..core import Tool, ToolSourceKind
from ..schema import Schema
from .source import ToolSource
from .tools import ShellTool


class CliSource(ToolSource):
    def __init__(
        self,
        name: str,
        description: str,
        schema: Schema,
        command: str,
        source_id: str = "cli",
    ) -> None:
        self._id = source_id
        self._tool = ShellTool(
            name=name,
            description=description,
            schema=schema,
            command=command,
            source=ToolSourceKind.Cli,
        )

    def kind(self) -> ToolSourceKind:
        return ToolSourceKind.Cli

    def id(self) -> str:
        return self._id

    async def tools(self) -> List[Tool]:
        return [self._tool]
