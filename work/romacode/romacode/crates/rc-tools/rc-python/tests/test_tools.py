"""Python language pack tests."""
import asyncio

from rc_python.tools import COUNT, all_tools
from rc_python.tools import PythonSource
from rc_python.core import ToolContext, ToolError, ToolSourceKind


def test_all_tools_unique_and_complete():
    tools = all_tools()
    assert len(tools) == COUNT == 7
    names = sorted(t.name() for t in tools)
    assert names == sorted(set(names)), f"duplicate python tool names: {names}"
    for t in tools:
        assert t.name().startswith("python."), t.name()
        assert t.category() == "python"
        assert t.source() == ToolSourceKind.Builtin


def test_python_source_yields_seven():
    src = PythonSource()
    assert src.kind() == ToolSourceKind.Builtin
    assert src.id() == "python"
    tools = asyncio.run(src.tools())
    assert len(tools) == COUNT


def test_python_version_runs():
    from rc_python.tools.python import Version
    out = asyncio.run(Version().execute({}, ToolContext()))
    # python3 is present in every reasonable environment
    assert out["exit_code"] == 0
    assert out["python"].startswith("Python")


def test_python_eval_returns_value():
    from rc_python.tools.python import Eval
    out = asyncio.run(Eval().execute({"code": "2 + 2"}, ToolContext()))
    assert out == 4


def test_python_eval_errors_on_bad_code():
    from rc_python.tools.python import Eval
    try:
        asyncio.run(Eval().execute({"code": "def"}, ToolContext()))
        assert False
    except ToolError as e:
        assert e.kind == "tool_failed"
        assert "python.eval" in str(e)


def test_python_eval_repr_for_non_serialisable():
    from rc_python.tools.python import Eval
    # a set is not JSON-serialisable; eval should fall back to repr
    out = asyncio.run(Eval().execute({"code": "{1, 2, 3}"}, ToolContext()))
    assert isinstance(out, str)
    assert "1" in out and "2" in out
