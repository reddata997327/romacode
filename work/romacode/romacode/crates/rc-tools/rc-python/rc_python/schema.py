"""rc_python.schema — tool input schemas (Python port of ``rc_core::schema``).

A :class:`Schema` is the contract a tool publishes about its input: the
parameters it accepts, their types, whether they are required, and their
defaults. :func:`rc_python.validator.validate` checks a Python dict against a
schema before the tool runs.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional


class ParamType:
    """The parameter types a schema can express.

    A Python ``bool`` is accepted for ``Boolean`` only (not ``Integer``),
    because in Python ``isinstance(True, int)`` is ``True`` — we must guard
    against that conflation explicitly. ``Array`` carries an inner type so
    ``["a", "b"]`` validates as ``Array(String)``.
    """

    STRING = "string"
    INTEGER = "integer"
    NUMBER = "number"
    BOOLEAN = "boolean"
    OBJECT = "object"
    ANY = "any"

    def __init__(self, kind: str, inner: Optional["ParamType"] = None) -> None:
        assert kind in (
            self.STRING, self.INTEGER, self.NUMBER, self.BOOLEAN,
            self.OBJECT, self.ANY,
        ) or (kind == "array" and inner is not None), f"bad ParamType kind: {kind!r}"
        self.kind = kind
        self.inner = inner

    # ── constructors ────────────────────────────────────────────────────────
    @classmethod
    def string(cls) -> "ParamType":
        return cls(cls.STRING)

    @classmethod
    def integer(cls) -> "ParamType":
        return cls(cls.INTEGER)

    @classmethod
    def number(cls) -> "ParamType":
        return cls(cls.NUMBER)

    @classmethod
    def boolean(cls) -> "ParamType":
        return cls(cls.BOOLEAN)

    @classmethod
    def object(cls) -> "ParamType":
        return cls(cls.OBJECT)

    @classmethod
    def any_(cls) -> "ParamType":
        return cls(cls.ANY)

    @classmethod
    def array(cls, inner: "ParamType") -> "ParamType":
        return cls("array", inner)

    # ── validation ──────────────────────────────────────────────────────────
    def check(self, v: Any) -> None:
        """Raise ``ValueError`` if ``v`` does not match this type."""
        ok, why = self._matches(v)
        if not ok:
            raise ValueError(f"expected {self}, got {type_name(v)}" + (f": {why}" if why else ""))

    def _matches(self, v: Any) -> tuple[bool, str]:
        k = self.kind
        if k == self.ANY:
            return True, ""
        if k == self.STRING:
            return isinstance(v, str), ""
        if k == self.INTEGER:
            # bool is a subclass of int in Python; exclude it explicitly.
            return isinstance(v, int) and not isinstance(v, bool), ""
        if k == self.NUMBER:
            return isinstance(v, (int, float)) and not isinstance(v, bool), ""
        if k == self.BOOLEAN:
            return isinstance(v, bool), ""
        if k == self.OBJECT:
            return isinstance(v, dict), ""
        if k == "array":
            if not isinstance(v, list):
                return False, ""
            for i, item in enumerate(v):
                try:
                    self.inner.check(item)  # type: ignore[union-attr]
                except ValueError as e:
                    return False, f"element {i}: {e}"
            return True, ""
        return False, "unknown type"

    def __str__(self) -> str:
        if self.kind == "array":
            return f"array<{self.inner}>"  # type: ignore[union-attr]
        return self.kind

    def __repr__(self) -> str:
        return f"ParamType({self})"

    def __eq__(self, other: object) -> bool:
        return isinstance(other, ParamType) and self.kind == other.kind and self.inner == other.inner

    def __hash__(self) -> int:
        return hash((self.kind, self.inner))


def type_name(v: Any) -> str:
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "boolean"
    if isinstance(v, int):
        return "integer"
    if isinstance(v, float):
        return "number"
    if isinstance(v, str):
        return "string"
    if isinstance(v, list):
        return "array"
    if isinstance(v, dict):
        return "object"
    return type(v).__name__


@dataclass
class Parameter:
    """One parameter of a tool's input schema."""

    name: str
    typ: ParamType
    description: str = ""
    required: bool = False
    default: Any = None
    enum_values: List[Any] = field(default_factory=list)

    @classmethod
    def required(cls, name: str, typ: ParamType) -> "Parameter":
        return cls(name=name, typ=typ, required=True)

    @classmethod
    def optional(cls, name: str, typ: ParamType) -> "Parameter":
        return cls(name=name, typ=typ, required=False)

    def with_desc(self, desc: str) -> "Parameter":
        self.description = desc
        return self

    def with_default(self, v: Any) -> "Parameter":
        self.default = v
        return self

    def with_enum(self, vs: List[Any]) -> "Parameter":
        self.enum_values = list(vs)
        return self


@dataclass
class Schema:
    """The full input schema of a tool: an ordered list of named parameters."""

    parameters: List[Parameter] = field(default_factory=list)

    @classmethod
    def new(cls) -> "Schema":
        return cls()

    def param(self, p: Parameter) -> "Schema":
        self.parameters.append(p)
        return self

    def to_json_schema(self) -> Dict[str, Any]:
        """Render as a JSON-Schema-ish dict, for OpenAI/Anthropic tool defs."""
        props: Dict[str, Any] = {}
        required: List[str] = []
        type_for = {
            ParamType.STRING: "string",
            ParamType.INTEGER: "integer",
            ParamType.NUMBER: "number",
            ParamType.BOOLEAN: "boolean",
            ParamType.OBJECT: "object",
            ParamType.ANY: None,
        }
        for p in self.parameters:
            entry: Dict[str, Any] = {}
            if p.typ.kind == "array":
                entry["type"] = "array"
            else:
                t = type_for.get(p.typ.kind)
                if t is not None:
                    entry["type"] = t
            if p.description:
                entry["description"] = p.description
            if p.enum_values:
                entry["enum"] = list(p.enum_values)
            if p.default is not None:
                entry["default"] = p.default
            props[p.name] = entry
            if p.required:
                required.append(p.name)
        return {"type": "object", "properties": props, "required": required}
