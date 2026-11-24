"""Executor layer tests (run via asyncio.run — no pytest-asyncio needed)."""
import asyncio

from rc_python.core import Tool, ToolContext, ToolError, ToolSourceKind
from rc_python.schema import ParamType, Parameter, Schema
from rc_python.registry import Registry
from rc_python.executor import Executor
from rc_python.permissions import Permission, PermissionPolicy, PermissionRule
from rc_python.sandbox import Sandbox


class _Add(Tool):
    def name(self): return "math.add"
    def description(self): return "add two ints"
    def schema(self):
        return Schema.new().param(Parameter.required("a", ParamType.number())).param(Parameter.required("b", ParamType.number()))
    def source(self): return ToolSourceKind.Builtin
    async def execute(self, input, ctx):
        return float(input["a"]) + float(input["b"])


class _Fetch(Tool):
    """A `web.*` tool — Ask under the default policy."""
    def name(self): return "web.fetch"
    def description(self): return "fetch"
    def schema(self):
        return Schema.new().param(Parameter.required("url", ParamType.string()))
    def source(self): return ToolSourceKind.Builtin
    async def execute(self, input, ctx):
        return {"url": input["url"]}


def _make(policy=None):
    r = Registry()
    r.register(_Add())
    r.register(_Fetch())
    return Executor(r, policy or PermissionPolicy.default_policy(), Sandbox())


def test_run_allow():
    ex = _make()
    out = asyncio.run(ex.run("math.add", {"a": 2, "b": 3}, False))
    assert out == 5.0


def test_run_ask_needs_approval():
    # The shipped default is fully open, so force Ask with an explicit
    # opt-in policy to cover the Ask machinery.
    policy = PermissionPolicy.default_policy().ask_("*")
    ex = _make(policy)
    try:
        asyncio.run(ex.run("web.fetch", {"url": "x"}, False))
        assert False
    except ToolError as e:
        assert e.kind == "permission_denied"
    # with approval it runs
    out = asyncio.run(ex.run("web.fetch", {"url": "x"}, True))
    assert out == {"url": "x"}


def test_default_policy_runs_everything():
    """The shipped default policy runs every tool without approval."""
    ex = _make()
    out = asyncio.run(ex.run("web.fetch", {"url": "x"}, False))
    assert out == {"url": "x"}


def test_run_deny_blocked():
    policy = PermissionPolicy.default_policy().deny_("web.*")
    ex = _make(policy)
    try:
        asyncio.run(ex.run("web.fetch", {"url": "x"}, True))
        assert False
    except ToolError as e:
        assert e.kind == "permission_denied"


def test_validation_fills_defaults():
    class _Echo(Tool):
        def name(self): return "text.echo"
        def description(self): return ""
        def schema(self):
            return (
                Schema.new()
                .param(Parameter.required("msg", ParamType.string()))
                .param(Parameter.optional("loud", ParamType.boolean()).with_default(False))
            )
        def source(self): return ToolSourceKind.Builtin
        async def execute(self, input, ctx):
            return input["msg"].upper() if input["loud"] else input["msg"]

    r = Registry()
    r.register(_Echo())
    ex = Executor(r, PermissionPolicy.default_policy(), Sandbox())
    out = asyncio.run(ex.run("text.echo", {"msg": "hi"}, False))
    assert out == "hi"
