"""Sources layer tests."""
import asyncio

from rc_python.sources import (
    BuiltinSource,
    CliSource,
    DeclarativeSource,
    SourceRegistry,
    builtin_all_tools,
)
from rc_python.sources.tools import ShellTool, one_string_param
from rc_python.core import ToolSourceKind
from rc_python.schema import ParamType, Parameter, Schema


def test_builtin_source_yields_catalog():
    src = BuiltinSource()
    assert src.kind() == ToolSourceKind.Builtin
    assert src.id() == "builtin"
    tools = asyncio.run(src.tools())
    assert len(tools) == len(builtin_all_tools())
    names = sorted(t.name() for t in tools)
    deduped = sorted(set(names))
    assert names == deduped, f"duplicate builtin names: {names}"
    # every category is one of the known set
    cats = {t.category() for t in tools}
    assert cats.issubset({"math", "text", "data", "code", "time"})


def test_declarative_source_builds_shell_tools():
    specs = [
        {
            "name": "git.status",
            "description": "git status",
            "command": "git -C {{repo}} status --porcelain",
            "params": [
                {"name": "repo", "type": "string", "required": True, "description": "repo path"},
            ],
        },
        {
            "name": "echo.msg",
            "description": "echo",
            "command": "printf %s {{msg}}",
            "params": [
                {"name": "msg", "type": "string", "required": True},
                {"name": "upper", "type": "boolean", "required": False, "default": False},
            ],
        },
    ]
    src = DeclarativeSource(specs, source_id="decl:test")
    assert src.kind() == ToolSourceKind.Declarative
    tools = asyncio.run(src.tools())
    assert [t.name() for t in tools] == ["git.status", "echo.msg"]
    assert all(t.source() == ToolSourceKind.Declarative for t in tools)
    # the shell tool renders its template
    echo = tools[1]
    assert isinstance(echo, ShellTool)
    # Values are data, not code: a safe word passes through, while any value
    # with shell metacharacters is quoted as ONE literal shell word.
    assert echo.render({"msg": "hi", "upper": False}) == "printf %s hi"
    # An injection payload must arrive as ONE quoted literal word: the text
    # is echoed back verbatim and no command is executed.
    from rc_python.core import ToolContext
    hostile = "x; rm -rf ~"
    rendered = echo.render({"msg": hostile, "upper": False})
    assert rendered == "printf %s 'x; rm -rf ~'"
    out = asyncio.run(echo.execute({"msg": hostile, "upper": False}, ToolContext()))
    assert out["exit_code"] == 0
    assert out["stdout"] == hostile


def test_source_registry_runs_all_concurrently():
    sr = SourceRegistry()
    assert sr.is_empty()
    sr.add(BuiltinSource())
    sr.add(DeclarativeSource([{"name": "x.y", "description": "", "command": "true", "params": []}]))
    assert len(sr) == 2
    tools = asyncio.run(sr.all_tools())
    names = sorted(t.name() for t in tools)
    assert "x.y" in names
    assert "math.add" in names


def test_cli_source_one_tool():
    src = CliSource(
        name="sys.whoami",
        description="whoami",
        schema=Schema.new(),
        command="whoami",
    )
    assert src.kind() == ToolSourceKind.Cli
    tools = asyncio.run(src.tools())
    assert len(tools) == 1 and tools[0].name() == "sys.whoami"


def test_shell_tool_runs_echo():
    t = ShellTool(
        name="test.echo",
        description="echo",
        schema=one_string_param("msg", "the message"),
        command="printf %s {{msg}}",
    )
    from rc_python.core import ToolContext
    out = asyncio.run(t.execute({"msg": "hi"}, ToolContext()))
    assert out["exit_code"] == 0
    assert out["stdout"] == "hi"
