"""rc_python — a full Python implementation of the RomaCode tools builder.

A faithful port of the Rust ``rc-*`` framework: the same layer cake
(core → schema → registry → permissions → sandbox → validator → executor →
manifest → sources → compose → lifecycle), the same ``Tool`` contract, the
same eight tool-source mechanisms, the same composition primitives — plus a
``python.*`` language pack mirroring the Rust ``rc-rust`` pack.

The framework is **stdlib-only** at runtime (no third-party dependencies);
``pytest`` is a dev convenience, not a runtime requirement.

Quick start::

    import asyncio
    from rc_python import Lifecycle, BuiltinSource, PythonSource

    async def main():
        life = Lifecycle.permissive()
        life.add_source(BuiltinSource())      # the 15-tool builtin catalog
        life.add_source(PythonSource())       # the 7 python.* tools
        n = await life.discover()             # registers 15 + 7 tools
        print(f"loaded {n} tools")
        out = await life.execute("python.version", {}, True)
        print(out)                            # {'python': 'Python 3.12.x', 'exit_code': 0}

    asyncio.run(main())
"""

from __future__ import annotations

# ── core layer ───────────────────────────────────────────────────────────────
from .core import (
    VERSION,
    Tool,
    ToolArc,
    ToolContext,
    ToolError,
    ToolResult,
    ToolSourceKind,
    arc,
)
from .schema import ParamType, Parameter, Schema
from .registry import Registry, ToolInfo
from .permissions import Permission, PermissionPolicy, PermissionRule, glob_match
from .sandbox import Sandbox
from .validator import Validator, validate
from .executor import Executor
from .manifest import Catalog, Manifest

# ── sources layer ────────────────────────────────────────────────────────────
from .sources import (
    BuiltinSource,
    CliSource,
    DeclarativeSource,
    HttpTool,
    RestSource,
    ShellTool,
    SourceError,
    SourceRegistry,
    ToolSource,
    builtin_all_tools,
    builtin_only,
)

# ── compose layer ────────────────────────────────────────────────────────────
from .compose import (
    BranchArm,
    BranchTool,
    ParallelStep,
    ParallelTool,
    PipelineStep,
    PipelineTool,
    lookup,
    resolve,
)

# ── lifecycle layer ──────────────────────────────────────────────────────────
from .lifecycle import Lifecycle, MetricsStore, ToolMetrics

# ── the Python language pack ─────────────────────────────────────────────────
from .tools import COUNT as PYTHON_COUNT, CATEGORY as PYTHON_CATEGORY, PythonSource, all_tools

__all__ = [
    # core
    "VERSION", "Tool", "ToolArc", "ToolContext", "ToolError", "ToolResult",
    "ToolSourceKind", "arc",
    # schema
    "ParamType", "Parameter", "Schema",
    # registry
    "Registry", "ToolInfo",
    # permissions
    "Permission", "PermissionPolicy", "PermissionRule", "glob_match",
    # sandbox / validator / executor / manifest
    "Sandbox", "Validator", "validate", "Executor", "Catalog", "Manifest",
    # sources
    "BuiltinSource", "CliSource", "DeclarativeSource", "HttpTool", "RestSource",
    "ShellTool", "SourceError", "SourceRegistry", "ToolSource",
    "builtin_all_tools", "builtin_only",
    # compose
    "BranchArm", "BranchTool", "ParallelStep", "ParallelTool",
    "PipelineStep", "PipelineTool", "lookup", "resolve",
    # lifecycle
    "Lifecycle", "MetricsStore", "ToolMetrics",
    # python pack
    "PythonSource", "all_tools", "PYTHON_COUNT", "PYTHON_CATEGORY",
]


def python_lifecycle() -> Lifecycle:
    """A convenience: a permissive :class:`Lifecycle` pre-loaded with the
    Python language pack **and** the builtin catalog. Add more sources
    (declarative, rest, …) on the returned lifecycle before calling
    :meth:`Lifecycle.discover`."""
    life = Lifecycle.permissive()
    life.add_source(PythonSource())
    life.add_source(BuiltinSource())
    return life
