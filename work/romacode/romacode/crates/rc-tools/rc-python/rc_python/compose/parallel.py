"""rc_python.compose.parallel — fan out N tools concurrently, join as a dict.

Each step runs against a template resolved from the pipeline input (or the
input directly when no template is given), all steps run concurrently via
:meth:`asyncio.gather`, and the results are joined into a dict keyed by step
name. Mirrors the Rust ``rc_compose::parallel``.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any, Dict, Optional

from ..core import Tool, ToolContext, ToolSourceKind, ToolError
from ..executor import Executor
from ..schema import Schema
from .templating import resolve


@dataclass
class ParallelStep:
    name: str
    tool: str
    template: Optional[Any] = None


class ParallelTool(Tool):
    def __init__(
        self,
        name: str,
        description: str,
        steps: list[ParallelStep],
        executor: Executor,
    ) -> None:
        self._name = name
        self._desc = description
        self._steps = list(steps)
        self._executor = executor
        self._schema = Schema.new()

    def name(self) -> str:
        return self._name

    def description(self) -> str:
        return self._desc

    def schema(self) -> Schema:
        return self._schema

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Compose

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> Dict[str, Any]:
        async def _run(step: ParallelStep) -> Any:
            step_input = (
                resolve(step.template, None, input)
                if step.template is not None
                else input
            )
            if not isinstance(step_input, dict):
                step_input = {"value": step_input}
            return await self._executor.run_inner(step.tool, step_input)

        results = await asyncio.gather(*(_run(s) for s in self._steps))
        return {s.name: r for s, r in zip(self._steps, results)}
