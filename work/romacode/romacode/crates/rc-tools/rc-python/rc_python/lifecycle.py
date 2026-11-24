"""rc_python.lifecycle — L3: the full tool lifecycle (Python port of ``rc_lifecycle``).

The :class:`Lifecycle` manager ties the other layers together:

1. discover — walk every registered :class:`~rc_python.sources.ToolSource`,
   collect the tools each produces.
2. register — insert them into the :class:`~rc_python.registry.Registry`.
3. validate — check every tool's schema for duplicates / empty names.
4. load — sources load eagerly (tools are ready-to-register on discover).
5. execute — run a tool by name through the :class:`~rc_python.executor.Executor`,
   timing the call and recording metrics.
6. monitor — per-tool call/success/failure counts + latency, via
   :class:`MetricsStore`.
7. version — a version report of every registered tool.
8. retire — remove a tool from the registry.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Tuple

from .core import Tool, ToolArc, ToolError, ToolSourceKind
from .executor import Executor
from .permissions import Permission, PermissionPolicy
from .registry import Registry
from .sandbox import Sandbox
from .sources import SourceRegistry, ToolSource


@dataclass
class ToolMetrics:
    calls: int = 0
    successes: int = 0
    failures: int = 0
    total_us: int = 0
    last_error: Optional[str] = None

    @property
    def avg_us(self) -> float:
        return self.total_us / self.calls if self.calls else 0.0


class MetricsStore:
    def __init__(self) -> None:
        self._store: Dict[str, ToolMetrics] = {}

    def record(self, name: str, success: bool, elapsed_us: int, err: Optional[str]) -> None:
        m = self._store.setdefault(name, ToolMetrics())
        m.calls += 1
        m.total_us += int(elapsed_us)
        if success:
            m.successes += 1
        else:
            m.failures += 1
            m.last_error = err

    def get(self, name: str) -> Optional[ToolMetrics]:
        return self._store.get(name)

    def all(self) -> List[Tuple[str, ToolMetrics]]:
        out = list(self._store.items())
        out.sort(key=lambda kv: kv[0])
        return out


class Lifecycle:
    def __init__(self, policy: PermissionPolicy, sandbox: Sandbox) -> None:
        self.registry = Registry()
        self.policy = policy
        self.sandbox = sandbox
        self.executor = Executor(self.registry, policy, sandbox)
        self.sources = SourceRegistry()
        self.metrics = MetricsStore()

    @classmethod
    def permissive(cls) -> "Lifecycle":
        """A fully-permissive lifecycle (allow everything, default sandbox) —
        handy for examples and tests."""
        policy = PermissionPolicy.default_policy().with_default(Permission.ALLOW)
        return cls(policy, Sandbox())

    @classmethod
    def new(cls, policy: PermissionPolicy, sandbox: Sandbox) -> "Lifecycle":
        return cls(policy, sandbox)

    def add_source(self, source: ToolSource) -> None:
        """Register a source. Its tools are loaded on the next :meth:`discover`."""
        self.sources.add(source)

    def register(self, tool: Tool) -> None:
        self.registry.register(tool)

    def register_all(self, tools: List[Tool]) -> None:
        self.registry.register_all(tools)

    async def discover(self) -> int:
        """Run every source's ``tools()`` and register the results.

        Returns the number of tools registered.
        """
        tools = await self.sources.all_tools()
        n = len(tools)
        self.registry.register_all(tools)
        return n

    def validate_all(self) -> List[str]:
        """Validate every registered tool's schema. Returns a list of issue
        strings (empty = all good)."""
        issues: List[str] = []
        for info in self.registry.list():
            if not info.name:
                issues.append("tool with empty name")
                continue
            try:
                tool = self.registry.get(info.name)
            except ToolError:
                continue
            schema = tool.schema()
            seen = set()
            for p in schema.parameters:
                if not p.name:
                    issues.append(f"{info.name}: parameter with empty name")
                if p.name in seen:
                    issues.append(f"{info.name}: duplicate parameter `{p.name}`")
                seen.add(p.name)
        return issues

    async def execute(self, name: str, input: Dict[str, Any], approved: bool) -> Any:
        """Execute a tool by name, timing the call and recording metrics."""
        start = time.perf_counter()
        try:
            out = await self.executor.run(name, input, approved)
        except Exception as e:
            elapsed = int((time.perf_counter() - start) * 1_000_000)
            self.metrics.record(name, False, elapsed, str(e))
            raise
        elapsed = int((time.perf_counter() - start) * 1_000_000)
        self.metrics.record(name, True, elapsed, None)
        return out

    def retire(self, name: str) -> bool:
        """Retire (remove) a tool from the registry. Returns True if present."""
        return self.registry.remove(name) is not None

    def metrics_for(self, name: str) -> Optional[ToolMetrics]:
        return self.metrics.get(name)

    def all_metrics(self) -> List[Tuple[str, ToolMetrics]]:
        return self.metrics.all()

    def version_report(self) -> List[Tuple[str, str, str]]:
        """Every tool's name + version + source, sorted by name."""
        out = [(i.name, i.version, i.source) for i in self.registry.list()]
        out.sort(key=lambda t: t[0])
        return out
