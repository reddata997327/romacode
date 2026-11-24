"""rc_python.compose.branch — conditionally route to one of N tools.

Arms are evaluated in order; the first whose ``when`` template resolves to a
truthy value wins. An arm with ``when = None`` is the catch-all (placed last).
The matched arm's tool runs against a template resolved from the input (or the
input directly when no template is given). Mirrors the Rust ``rc_compose::branch``.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Dict, Optional

from ..core import Tool, ToolContext, ToolError, ToolSourceKind
from ..executor import Executor
from ..schema import Schema
from .templating import resolve


@dataclass
class BranchArm:
    tool: str
    when: Optional[Any] = None  # None = catch-all (always matches)
    template: Optional[Any] = None


class BranchTool(Tool):
    def __init__(
        self,
        name: str,
        description: str,
        arms: list[BranchArm],
        executor: Executor,
    ) -> None:
        self._name = name
        self._desc = description
        self._arms = list(arms)
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

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> Any:
        for arm in self._arms:
            if arm.when is None:
                matched = arm
                break
            val = resolve(arm.when, None, input)
            if val:
                matched = arm
                break
        else:
            raise ToolError(
                f"branch `{self._name}`: no arm matched and no catch-all", kind="other"
            )
        step_input = (
            resolve(matched.template, None, input) if matched.template is not None else input
        )
        if not isinstance(step_input, dict):
            step_input = {"value": step_input}
        return await self._executor.run_inner(matched.tool, step_input)
