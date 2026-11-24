"""Validator layer tests."""
from rc_python.schema import ParamType, Parameter, Schema
from rc_python.validator import validate
from rc_python.core import ToolError


def test_fills_defaults_and_type_checks():
    s = (
        Schema.new()
        .param(Parameter.required("n", ParamType.integer()))
        .param(Parameter.optional("label", ParamType.string()).with_default("hi"))
    )
    v = validate(s, {"n": 5})
    assert v["n"] == 5
    assert v["label"] == "hi"


def test_rejects_bad_type():
    s = Schema.new().param(Parameter.required("n", ParamType.integer()))
    try:
        validate(s, {"n": "five"})
        assert False
    except ToolError as e:
        assert e.kind == "bad_param"


def test_rejects_missing_required():
    s = Schema.new().param(Parameter.required("n", ParamType.integer()))
    try:
        validate(s, {})
        assert False
    except ToolError as e:
        assert e.kind == "validation"


def test_enforces_enum():
    s = Schema.new().param(
        Parameter.required("mode", ParamType.string()).with_enum(["a", "b"])
    )
    assert validate(s, {"mode": "a"})["mode"] == "a"
    try:
        validate(s, {"mode": "z"})
        assert False
    except ToolError as e:
        assert e.kind == "bad_param"


def test_rejects_non_dict_input():
    try:
        validate(Schema.new(), [1, 2, 3])
        assert False
    except ToolError as e:
        assert e.kind == "validation"
