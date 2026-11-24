"""rc_python.compose — L4 composition: new tools from old tools.

Four composition primitives, each producing a registered
:class:`~rc_python.core.Tool`:

| primitive | what it does |
|-----------|--------------|
| :class:`PipelineTool` | chain tools — each step's output feeds the next |
| :class:`ParallelTool` | fan out N tools concurrently, join as a dict |
| :class:`BranchTool`   | conditionally route to one of N tools (first match wins) |

Because every composition is itself a ``Tool``, compositions nest — a pipeline
step can be a branch, etc. Template references (``{{prev.x}}``,
``{{input.y}}``) are resolved by :mod:`rc_python.compose.templating`.
"""

from __future__ import annotations

from .branch import BranchArm, BranchTool
from .parallel import ParallelStep, ParallelTool
from .pipeline import PipelineStep, PipelineTool
from .templating import lookup, resolve, resolve_ref

__all__ = [
    "BranchArm",
    "BranchTool",
    "ParallelStep",
    "ParallelTool",
    "PipelineStep",
    "PipelineTool",
    "lookup",
    "resolve",
    "resolve_ref",
]
