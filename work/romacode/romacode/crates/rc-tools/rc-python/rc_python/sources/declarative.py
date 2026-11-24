"""rc_python.sources.declarative — source 2: YAML/JSON spec → shell tools.

A declarative spec is a list of tool definitions, each with a name, a
description, a command template (``{{param}}`` placeholders), and a parameter
list. No code is written — the source turns each entry into a
:class:`~rc_python.sources.tools.ShellTool`.
"""

from __future__ import annotations

import json
import os
from typing import Any, Dict, List, Union

from ..core import Tool, ToolSourceKind
from ..schema import ParamType, Parameter, Schema
from .source import ToolSource
from .tools import ShellTool

_TYPE_MAP = {
    "string": ParamType.string,
    "integer": ParamType.integer,
    "number": ParamType.number,
    "boolean": ParamType.boolean,
    "object": ParamType.object,
    "any": ParamType.any_,
}


def _param_from_spec(p: Dict[str, Any]) -> Parameter:
    typ_name = p.get("type", "string")
    if typ_name.startswith("array<") and typ_name.endswith(">"):
        inner_name = typ_name[len("array<"):-1]
        inner_factory = _TYPE_MAP.get(inner_name, ParamType.string)
        typ = ParamType.array(inner_factory())
    else:
        factory = _TYPE_MAP.get(typ_name, ParamType.string)
        typ = factory()
    required = bool(p.get("required", False))
    par = Parameter(name=p["name"], typ=typ, description=p.get("description", ""), required=required)
    if "default" in p:
        par.with_default(p["default"])
    if "enum" in p:
        par.with_enum(list(p["enum"]))
    return par


def _schema_from_spec(params: List[Dict[str, Any]]) -> Schema:
    s = Schema.new()
    for p in params or []:
        s.param(_param_from_spec(p))
    return s


def _tool_from_spec(spec: Dict[str, Any]) -> ShellTool:
    schema = _schema_from_spec(spec.get("params", []))
    return ShellTool(
        name=spec["name"],
        description=spec.get("description", ""),
        schema=schema,
        command=spec["command"],
        source=ToolSourceKind.Declarative,
        cwd=spec.get("cwd"),
    )


class DeclarativeSource(ToolSource):
    """Build shell tools from a declarative spec.

    Pass either an in-memory list of spec dicts, or a path to a JSON file
    containing ``{"tools": [...]}``.
    """

    def __init__(self, specs: Union[List[Dict[str, Any]], str], source_id: str = "declarative") -> None:
        self._id = source_id
        if isinstance(specs, str):
            with open(specs, "r", encoding="utf-8") as f:
                data = json.load(f)
            self._specs = data["tools"] if isinstance(data, dict) else data
        else:
            self._specs = list(specs)

    def kind(self) -> ToolSourceKind:
        return ToolSourceKind.Declarative

    def id(self) -> str:
        return self._id

    async def tools(self) -> List[Tool]:
        return [_tool_from_spec(s) for s in self._specs]
