"""rc_python.compose.pipeline — chain tools so each step's output feeds the next.

A step may take the previous output directly (no template) or take a template
that references ``{{prev}}`` / ``{{prev.x}}`` / ``{{input.y}}``. The pipeline
is itself a :class:`~rc_python.core.Tool`, so pipelines can be steps in other
pipelines. Mirrors the Rust ``rc_compose::pipeline``.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Dict, Optional

from ..core import Tool, ToolContext, ToolSourceKind
from ..executor import Executor
from ..schema import Schema
from .templating import resolve


@dataclass
class PipelineStep:
    """One step of a pipeline.

    ``template`` is optional: if ``None``, the step receives the previous
    step's output directly (the first step receives the pipeline input). If
    given, the template is resolved against ``prev`` + ``input``.
    """

    tool: str
    template: Optional[Any] = None


class PipelineTool(Tool):
    def __init__(
        self,
        name: str,
        description: str,
        steps: list[PipelineStep],
        executor: Executor,
    ) -> None:
        self._name = name
        self._desc = description
        self._steps = list(steps)
        self._executor = executor
        # a pipeline's own input schema is an empty object; each step's input
        # is shaped by its template. The first step receives the pipeline input
        # verbatim, so callers should pass a dict the first tool will accept.
        self._schema = Schema.new()

    def name(self) -> str:
        return self._name

    def description(self) -> str:
        return self._desc

    def schema(self) -> Schema:
        return self._schema

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Compose

    def category(self) -> str:
        # dot-namespaced: compose.pipeline.<name>
        return self._name.split(".", 1)[0] if "." in self._name else "compose"

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> Any:
        prev: Any = input
        for i, step in enumerate(self._steps):
            step_input = (
                resolve(step.template, prev, input)
                if step.template is not None
                else prev
            )
            # The inner tool expects a dict; coerce a non-dict prev into
            # {"value": prev} so a pure-value pipeline still threads through.
            if not isinstance(step_input, dict):
                step_input = {"value": step_input}
            prev = await self._executor.run_inner(step.tool, step_input)
        return prev
