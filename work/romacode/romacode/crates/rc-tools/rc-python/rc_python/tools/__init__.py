"""rc_python.tools — the Python language pack + its ``ToolSource`` adapter."""

from __future__ import annotations

from typing import List

from ..core import Tool, ToolSourceKind
from ..sources.source import ToolSource
from .python import all as _python_all, COUNT, CATEGORY


def all_tools() -> List[Tool]:
    """Every ``python.*`` tool, ready to register."""
    return _python_all()


class PythonSource(ToolSource):
    """A tool source that yields the seven ``python.*`` developer-workflow tools.

    Mirrors the Rust ``rc_rust::RustSource``: reports
    :class:`~rc_python.core.ToolSourceKind.Builtin` because the tools ship
    with the framework — the ``python`` category is carried in each tool's
    name, not in the source kind.
    """

    def kind(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    def id(self) -> str:
        return "python"

    async def tools(self) -> List[Tool]:
        return _python_all()


__all__ = ["all_tools", "PythonSource", "COUNT", "CATEGORY"]
