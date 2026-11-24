"""rc_python.sources.builtin — the shipped tool catalog (Python port of ``rc_builtin``).

A representative subset of the eight Rust categories, every one a
:class:`~rc_python.core.Tool`:

| category | tools |
|----------|-------|
| `math`   | add, subtract, multiply, divide, sqrt |
| `text`   | upper, lower, length, reverse |
| `data`   | json_pretty |
| `code`   | base64_encode, base64_decode |
| `time`   | now, iso_now |

:meth:`builtin_all_tools` returns every built-in, ready to register. This is
the "source 1: builtin" mechanism; the others live in the sibling modules.
"""

from __future__ import annotations

import base64
import datetime
import json
import math
from typing import Any, Dict, List

from ..core import Tool, ToolContext, ToolSourceKind, ToolError
from ..schema import ParamType, Parameter, Schema
from .source import SourceError, ToolSource


class _Binop(Tool):
    """A two-number operation. The ``op`` callable maps ``(a, b) -> result``."""

    def __init__(self, name: str, desc: str, op):
        self._name = name
        self._desc = desc
        self._op = op
        self._schema = (
            Schema.new()
            .param(Parameter.required("a", ParamType.number()))
            .param(Parameter.required("b", ParamType.number()))
        )

    def name(self) -> str:
        return self._name

    def description(self) -> str:
        return self._desc

    def schema(self) -> Schema:
        return self._schema

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> float:
        a = float(input["a"])
        b = float(input["b"])
        return self._op(a, b)


def _add() -> _Binop:
    return _Binop("math.add", "Add two numbers (a + b).", lambda a, b: a + b)


def _sub() -> _Binop:
    return _Binop("math.subtract", "Subtract b from a (a - b).", lambda a, b: a - b)


def _mul() -> _Binop:
    return _Binop("math.multiply", "Multiply two numbers (a × b).", lambda a, b: a * b)


def _div() -> _Binop:
    def _f(a: float, b: float) -> float:
        if b == 0.0:
            raise ToolError.tool_failed("math.divide", "division by zero")
        return a / b
    return _Binop("math.divide", "Divide a by b (a / b). Errors on zero.", _f)


class _Sqrt(Tool):
    def name(self) -> str:
        return "math.sqrt"

    def description(self) -> str:
        return "Square root of a number."

    def schema(self) -> Schema:
        return Schema.new().param(Parameter.required("n", ParamType.number()))

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> float:
        return math.sqrt(float(input["n"]))


class _TextOp(Tool):
    def __init__(self, name: str, desc: str, fn):
        self._name = name
        self._desc = desc
        self._fn = fn
        self._schema = Schema.new().param(Parameter.required("s", ParamType.string()))

    def name(self) -> str:
        return self._name

    def description(self) -> str:
        return self._desc

    def schema(self) -> Schema:
        return self._schema

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext):
        return self._fn(input["s"])


class _JsonPretty(Tool):
    def name(self) -> str:
        return "data.json_pretty"

    def description(self) -> str:
        return "Pretty-print a JSON value (indent=2)."

    def schema(self) -> Schema:
        return Schema.new().param(Parameter.required("value", ParamType.any_()))

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> str:
        return json.dumps(input["value"], indent=2, ensure_ascii=False)


class _B64Encode(Tool):
    def name(self) -> str:
        return "code.base64_encode"

    def description(self) -> str:
        return "Base64-encode a string."

    def schema(self) -> Schema:
        return Schema.new().param(Parameter.required("s", ParamType.string()))

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> str:
        return base64.b64encode(input["s"].encode("utf-8")).decode("ascii")


class _B64Decode(Tool):
    def name(self) -> str:
        return "code.base64_decode"

    def description(self) -> str:
        return "Base64-decode a string."

    def schema(self) -> Schema:
        return Schema.new().param(Parameter.required("s", ParamType.string()))

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> str:
        try:
            return base64.b64decode(input["s"]).decode("utf-8")
        except Exception as e:
            raise ToolError.tool_failed("code.base64_decode", str(e)) from None


class _Now(Tool):
    def name(self) -> str:
        return "time.now"

    def description(self) -> str:
        return "Current Unix timestamp (seconds)."

    def schema(self) -> Schema:
        return Schema.new()

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> float:
        return datetime.datetime.now(datetime.timezone.utc).timestamp()


class _IsoNow(Tool):
    def name(self) -> str:
        return "time.iso_now"

    def description(self) -> str:
        return "Current UTC time as an ISO-8601 string."

    def schema(self) -> Schema:
        return Schema.new()

    def source(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> str:
        return datetime.datetime.now(datetime.timezone.utc).isoformat()


def builtin_all_tools() -> List[Tool]:
    """Every built-in tool, across all categories, ready to register."""
    return [
        _add(), _sub(), _mul(), _div(), _Sqrt(),
        _TextOp("text.upper", "Uppercase a string.", str.upper),
        _TextOp("text.lower", "Lowercase a string.", str.lower),
        _TextOp("text.length", "Length of a string.", lambda s: len(s)),
        _TextOp("text.reverse", "Reverse a string.", lambda s: s[::-1]),
        _JsonPretty(),
        _B64Encode(), _B64Decode(),
        _Now(), _IsoNow(),
    ]


# exposed for parity with the Rust `rc_builtin::COUNT` / `CATEGORIES`.
COUNT: int = len(builtin_all_tools())
CATEGORIES = ("math", "text", "data", "code", "time")


class BuiltinSource(ToolSource):
    """Source 1 of the mechanisms — wraps :func:`builtin_all_tools`."""

    def kind(self) -> ToolSourceKind:
        return ToolSourceKind.Builtin

    def id(self) -> str:
        return "builtin"

    async def tools(self) -> List[Tool]:
        return builtin_all_tools()
