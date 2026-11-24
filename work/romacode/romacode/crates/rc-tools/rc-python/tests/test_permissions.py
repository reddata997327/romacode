"""Permissions layer tests."""
from rc_python.core import Tool, ToolSourceKind
from rc_python.schema import Schema
from rc_python.permissions import Permission, PermissionPolicy, PermissionRule, glob_match


class _Named(Tool):
    def __init__(self, n):
        self._n = n

    def name(self):
        return self._n

    def description(self):
        return ""

    def schema(self):
        return Schema.new()

    def source(self):
        return ToolSourceKind.Builtin

    async def execute(self, input, ctx):
        return {}


def test_glob():
    assert glob_match("math.*", "math.add")
    assert glob_match("math.*", "math.deep.nested")
    assert not glob_match("math.*", "text.upper")
    assert glob_match("*", "anything.at.all")
    assert glob_match("a?c", "abc")
    assert not glob_match("a?c", "ac")


def test_default_policy_is_fully_open():
    """The shipped default carries no rules: everything is ALLOW — including
    the system.* tools older defaults hard-denied."""
    p = PermissionPolicy.default_policy()
    for name in ("math.add", "text.upper", "system.env", "unknown.thing", "anything.at.all"):
        assert p.decide(_Named(name)) == Permission.ALLOW, name


def test_deny_wins_over_allow():
    """Rule precedence on an explicit opt-in policy (default has no rules)."""
    p = (
        PermissionPolicy.default_policy()
        .allow_("dangerous.*")
        .deny_("dangerous.rm")
        .with_default(Permission.ASK)
    )
    assert p.decide(_Named("dangerous.rm")) == Permission.DENY
    assert p.decide(_Named("dangerous.other")) == Permission.ALLOW
    assert p.decide(_Named("math.add")) == Permission.ASK
    assert p.decide(_Named("web.fetch")) == Permission.ASK
