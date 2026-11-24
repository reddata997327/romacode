"""rc_python.validator — input validation (Python port of ``rc_core::validator``).

Check a Python dict against a tool's :class:`~rc_python.schema.Schema` before
the tool runs. Fills in defaults for missing optional parameters, so a tool's
``execute`` always sees a complete input.
"""

from __future__ import annotations

from typing import Any, Dict

from .core import ToolError
from .schema import Schema


def validate(schema: Schema, input: Any) -> Dict[str, Any]:
    """Validate ``input`` against ``schema``.

    On success, returns a new dict with caller-supplied values copied through
    (after type-checking) and defaults applied for any missing optional
    parameters. Raises :class:`ToolError` on a type mismatch, a missing
    required parameter, or an out-of-enum value.
    """
    if not isinstance(input, dict):
        raise ToolError.validation("input must be a JSON object (dict)")

    out: Dict[str, Any] = {k: v for k, v in input.items()}

    for p in schema.parameters:
        if p.name in out:
            v = out[p.name]
            # type-check
            try:
                p.typ.check(v)
            except ValueError as detail:
                raise ToolError.bad_param(p.name, str(p.typ), str(detail)) from None
            # enum constraint
            if p.enum_values and v not in p.enum_values:
                raise ToolError.bad_param(
                    p.name, str(p.typ), f"value {v!r} is not one of the allowed values"
                ) from None
        else:
            if p.required:
                raise ToolError.validation(f"missing required parameter `{p.name}`")
            if p.default is not None:
                out[p.name] = p.default

    return out


class Validator:
    """Public facade matching the docs (``Validator.validate``)."""

    @staticmethod
    def validate(schema: Schema, input: Any) -> Dict[str, Any]:
        return validate(schema, input)
