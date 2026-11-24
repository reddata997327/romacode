"""rc_python.tools.python — the Python language pack.

Seven developer-workflow tools, each a first-class
:class:`~rc_python.core.Tool`, that wrap the Python toolchain (`python3`,
`pip`, `black`, `ruff`, `mypy`, `pytest`) behind the framework's sandbox +
permission gate. They are side-effecting (they spawn subprocesses), so the
default permission policy leaves them at `Ask`.

| tool | what it runs |
|------|--------------|
| `python.version`     | `python3 --version` |
| `python.pip_list`    | `python3 -m pip list --format=json` |
| `python.black`       | `black <path>` |
| `python.ruff`        | `ruff check <path>` |
| `python.mypy`        | `mypy <path>` |
| `python.pytest_run`  | `python3 -m pytest <path>` |
| `python.eval`        | evaluate a Python expression in-process |

Every subprocess tool returns the same shape the ``cli``/``declarative``
sources produce: ``{"exit_code": int, "stdout": str, "stderr": str}``.
"""

from __future__ import annotations

import asyncio
import json
from typing import Any, Dict, List, Optional

from ..core import Tool, ToolContext, ToolError, ToolSourceKind
from ..schema import ParamType, Parameter, Schema


async def _run(argv: List[str], ctx: ToolContext, cwd: Optional[str] = None) -> Dict[str, Any]:
    """Run ``argv`` and return ``{exit_code, stdout, stderr}``.

    Honours ``ctx.cancelled`` (returns early without spawning). Spawn failures
    (binary not on ``$PATH``) surface as :class:`ToolError`.
    """
    if ctx.cancelled:
        raise ToolError("cancelled before python tool", kind="other")
    try:
        proc = await asyncio.create_subprocess_exec(
            *argv,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=cwd,
        )
    except FileNotFoundError as e:
        raise ToolError.tool_failed(argv[0], f"binary not found: {e.filename or argv[0]}") from None
    stdout_b, stderr_b = await proc.communicate()
    return {
        "exit_code": proc.returncode if proc.returncode is not None else -1,
        "stdout": stdout_b.decode("utf-8", errors="replace"),
        "stderr": stderr_b.decode("utf-8", errors="replace"),
    }


def _opt_str(input: Dict[str, Any], key: str) -> Optional[str]:
    v = input.get(key)
    return v if isinstance(v, str) else None


def _opt_str_array(input: Dict[str, Any], key: str) -> List[str]:
    v = input.get(key)
    if isinstance(v, list):
        return [x if isinstance(x, str) else str(x) for x in v]
    return []


# ─── python.version ──────────────────────────────────────────────────────────

class Version(Tool):
    def name(self) -> str:
        return "python.version"

    def description(self) -> str:
        return "Report the installed Python interpreter version."

    def schema(self) -> Schema:
        return Schema.new()

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> Dict[str, Any]:
        res = await _run(["python3", "--version"], ctx)
        line = res["stdout"].strip().splitlines()[0] if res["stdout"].strip() else ""
        return {"python": line, "exit_code": res["exit_code"]}


# ─── python.pip_list ─────────────────────────────────────────────────────────

class PipList(Tool):
    def name(self) -> str:
        return "python.pip_list"

    def description(self) -> str:
        return "List installed pip packages as JSON."

    def schema(self) -> Schema:
        return Schema.new()

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> Dict[str, Any]:
        res = await _run(["python3", "-m", "pip", "list", "--format=json"], ctx)
        packages: Any
        try:
            packages = json.loads(res["stdout"])
        except ValueError:
            packages = []
        return {"packages": packages, "exit_code": res["exit_code"], "stderr": res["stderr"]}


# ─── the linter/test runner tools ────────────────────────────────────────────

def _path_schema(tool_desc_path: str) -> Schema:
    return (
        Schema.new()
        .param(Parameter.required("path", ParamType.string()).with_desc(tool_desc_path))
        .param(
            Parameter.optional("extra_args", ParamType.array(ParamType.string()))
            .with_desc("extra arguments forwarded after the path")
        )
    )


class _SubprocessTool(Tool):
    """Shared shape: one required ``path``, optional ``extra_args``, optional ``cwd``."""

    def __init__(self, name: str, desc: str, argv_prefix: List[str], path_desc: str) -> None:
        self._name = name
        self._desc = desc
        self._argv_prefix = list(argv_prefix)
        self._schema = _path_schema(path_desc)

    def name(self) -> str:
        return self._name

    def description(self) -> str:
        return self._desc

    def schema(self) -> Schema:
        return self._schema

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> Dict[str, Any]:
        path = input["path"]
        argv = self._argv_prefix + [path] + _opt_str_array(input, "extra_args")
        return await _run(argv, ctx, _opt_str(input, "cwd"))


def black() -> _SubprocessTool:
    return _SubprocessTool("python.black", "Run `black <path>` (format in place).", ["black"], "file or directory to format")


def ruff() -> _SubprocessTool:
    return _SubprocessTool("python.ruff", "Run `ruff check <path>` (lint).", ["ruff", "check"], "file or directory to lint")


def mypy() -> _SubprocessTool:
    return _SubprocessTool("python.mypy", "Run `mypy <path>` (type-check).", ["mypy"], "file or directory to type-check")


def pytest_run() -> _SubprocessTool:
    return _SubprocessTool("python.pytest_run", "Run `python -m pytest <path>`.", ["python3", "-m", "pytest"], "file or directory to test")


# ─── python.eval ─────────────────────────────────────────────────────────────

class Eval(Tool):
    def name(self) -> str:
        return "python.eval"

    def description(self) -> str:
        return "Evaluate a Python expression and return its value."

    def schema(self) -> Schema:
        return Schema.new().param(Parameter.required("code", ParamType.string()).with_desc("the Python expression to evaluate"))

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> Any:
        import builtins
        code = input["code"]
        try:
            value = eval(code, {"__builtins__": builtins}, {})  # noqa: S307 — dev tool, user's own machine
        except Exception as e:
            raise ToolError.tool_failed("python.eval", f"{type(e).__name__}: {e}") from None
        # normalise non-JSON-serialisable values to their repr for transport
        try:
            json.dumps(value)
            return value
        except (TypeError, ValueError):
            return repr(value)


def all() -> List[Tool]:  # type: ignore[override]  # shadows builtin all() inside this module
    """Every `python.*` tool, ready to register."""
    return [Version(), PipList(), black(), ruff(), mypy(), pytest_run(), Eval()]


COUNT: int = 7
CATEGORY: str = "python"
