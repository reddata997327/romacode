"""rc_python.sources — the universal tool-source mechanisms (Python port of ``rc_sources``).

A tool source is anything that implements :class:`ToolSource` — discover and
produce ready-to-register :class:`~rc_python.core.Tool` handles. Sources feed
a :class:`SourceRegistry`, which the lifecycle manager walks to discover and
load every tool. Adding a new mechanism is one ``ToolSource`` subclass away.
"""

from __future__ import annotations

from .source import SourceError, SourceRegistry, ToolSource
from .tools import HttpTool, ShellTool, one_string_param
from .builtin import BuiltinSource, builtin_all_tools
from .declarative import DeclarativeSource
from .cli import CliSource
from .rest import RestSource

__all__ = [
    "SourceError",
    "SourceRegistry",
    "ToolSource",
    "HttpTool",
    "ShellTool",
    "one_string_param",
    "BuiltinSource",
    "builtin_all_tools",
    "DeclarativeSource",
    "CliSource",
    "RestSource",
    "builtin_only",
]


def builtin_only() -> SourceRegistry:
    """Build a :class:`SourceRegistry` pre-loaded with just the builtin source."""
    sr = SourceRegistry()
    sr.add(BuiltinSource())
    return sr
