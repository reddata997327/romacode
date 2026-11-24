"""rc_python.executor — the single point a tool is actually run (Python port of ``rc_core::executor``).

For every call the executor:

1. looks the tool up in the registry,
2. asks the policy for a decision (``Allow`` / ``Ask`` / ``Deny``),
3. validates the input against the tool's schema (filling defaults),
4. enforces the sandbox (timeout, output cap),
5. runs the tool's ``execute``,
6. clamps the output to the sandbox's size limit.

The executor never prompts a human — ``Ask`` is surfaced as a decision the
caller (CLI, agent loop) must handle before calling :meth:`run` with
``approved = True``.
"""

from __future__ import annotations

import asyncio
from typing import Any, Dict

from .core import Tool, ToolContext, ToolError
from .permissions import Permission, PermissionPolicy
from .registry import Registry
from .sandbox import Sandbox
from .validator import validate


class Executor:
    def __init__(self, registry: Registry, policy: PermissionPolicy, sandbox: Sandbox) -> None:
        self.registry = registry
        self.policy = policy
        self.sandbox = sandbox

    def authorize(self, name: str) -> Permission:
        """What would the policy decide for this tool? Callers use this to
        decide whether to prompt the user (``Ask``) before calling ``run``."""
        tool = self.registry.get(name)  # raises ToolError if absent
        return self.policy.decide(tool)

    async def run(self, name: str, input: Dict[str, Any], approved: bool) -> Any:
        decision = self.authorize(name)
        if decision == Permission.DENY:
            raise ToolError.permission_denied(name, "denied by policy")
        if decision == Permission.ASK and not approved:
            raise ToolError.permission_denied(name, "requires approval (Ask)")
        return await self._run_inner(name, input)

    async def run_inner(self, name: str, input: Dict[str, Any]) -> Any:
        """Run without the permission gate — used internally by composition,
        where the outer composed tool has already been authorized and the
        inner steps are trusted parts of the same call."""
        return await self._run_inner(name, input)

    async def _run_inner(self, name: str, input: Dict[str, Any]) -> Any:
        tool = self.registry.get(name)
        validated = validate(tool.schema(), input)
        ctx = ToolContext(
            correlation_id=name,
            timeout_ms=self.sandbox.timeout_ms,
            cancelled=False,
            allow_net=self.sandbox.allow_net,
            allow_fs_paths=list(self.sandbox.allow_fs_paths),
        )
        if self.sandbox.timeout_ms > 0:
            try:
                out = await asyncio.wait_for(
                    tool.execute(validated, ctx),
                    timeout=self.sandbox.timeout_ms / 1000.0,
                )
            except asyncio.TimeoutError as e:
                raise ToolError.timeout(self.sandbox.timeout_ms) from None
        else:
            out = await tool.execute(validated, ctx)
        # clamp output (may raise ToolError.output_too_large for huge results)
        self.sandbox.clamp_output(out)
        return out
