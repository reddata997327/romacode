"""Registry layer tests."""
from rc_python.core import Tool, ToolContext, ToolError, ToolSourceKind
from rc_python.schema import ParamType, Parameter, Schema
from rc_python.registry import Registry


class _Stub(Tool):
    def __init__(self, name, desc="stub"):
        self._n = name
        self._d = desc

    def name(self):
        return self._n

    def description(self):
        return self._d

    def schema(self):
        return Schema.new().param(Parameter.required("x", ParamType.integer()))

    def source(self):
        return ToolSourceKind.Builtin

    async def execute(self, input, ctx):
        return input["x"]


def test_register_get_remove():
    r = Registry()
    assert r.is_empty()
    t = _Stub("test.dummy")
    r.register(t)
    assert len(r) == 1
    assert r.contains("test.dummy")
    assert r.get("test.dummy") is t
    try:
        r.get("nope")
        assert False
    except ToolError as e:
        assert e.kind == "not_found"
    assert r.remove("test.dummy") is t
    assert r.is_empty()


def test_register_replaces_and_returns_prev():
    r = Registry()
    a = _Stub("t.x", "a")
    b = _Stub("t.x", "b")
    r.register(a)
    prev = r.register(b)
    assert prev is a
    assert r.get("t.x").description() == "b"


def test_list_and_names_sorted():
    r = Registry()
    r.register_all([_Stub("b.x"), _Stub("a.x")])
    assert r.names() == ["a.x", "b.x"]
    infos = r.list()
    assert [i.name for i in infos] == ["a.x", "b.x"]
    assert infos[0].source == "builtin"
