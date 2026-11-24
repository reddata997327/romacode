"""Schema layer tests."""
from rc_python.schema import ParamType, Parameter, Schema


def test_paramtype_checks():
    assert ParamType.string().check.__self__  # bound method exists
    ParamType.string().check("hi")
    ParamType.integer().check(42)
    ParamType.number().check(3.14)
    ParamType.boolean().check(True)

    # bool is NOT an integer here (Python conflates them; we guard)
    try:
        ParamType.integer().check(True)
        assert False, "bool should not pass Integer"
    except ValueError:
        pass

    # array inner-type check
    ParamType.array(ParamType.string()).check(["a", "b"])
    try:
        ParamType.array(ParamType.integer()).check(["a"])
        assert False
    except ValueError:
        pass


def test_parameter_builders():
    p = Parameter.required("n", ParamType.integer()).with_desc("count").with_default(0)
    assert p.required and p.name == "n" and p.description == "count" and p.default == 0
    o = Parameter.optional("g", ParamType.string())
    assert not o.required


def test_schema_to_json_schema():
    s = (
        Schema.new()
        .param(Parameter.required("n", ParamType.integer()).with_desc("count"))
        .param(Parameter.optional("greeting", ParamType.string()).with_default("hi"))
    )
    j = s.to_json_schema()
    assert j["type"] == "object"
    assert j["required"] == ["n"]
    assert j["properties"]["n"]["type"] == "integer"
    assert j["properties"]["n"]["description"] == "count"
    assert j["properties"]["greeting"]["default"] == "hi"
