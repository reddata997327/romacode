"""Lifecycle layer tests."""
import asyncio

from rc_python import Lifecycle, BuiltinSource, PythonSource
from rc_python.core import ToolError


def test_discover_registers_builtin_catalog():
    life = Lifecycle.permissive()
    life.add_source(BuiltinSource())
    n = asyncio.run(life.discover())
    assert n >= 10
    assert life.registry.contains("math.add")
    assert life.registry.contains("time.now")


def test_execute_records_metrics_and_version_report():
    life = Lifecycle.permissive()
    life.add_source(BuiltinSource())
    asyncio.run(life.discover())
    out = asyncio.run(life.execute("math.add", {"a": 2, "b": 3}, False))
    assert out == 5.0
    m = life.metrics_for("math.add")
    assert m is not None and m.calls == 1 and m.successes == 1
    # version report is sorted and mentions the tool
    report = life.version_report()
    assert ("math.add", "0.1.0", "builtin") in report
    assert report == sorted(report, key=lambda t: t[0])


def test_validate_all_clean_for_builtins():
    life = Lifecycle.permissive()
    life.add_source(BuiltinSource())
    asyncio.run(life.discover())
    issues = life.validate_all()
    assert issues == []


def test_retire_removes_tool():
    life = Lifecycle.permissive()
    life.add_source(BuiltinSource())
    asyncio.run(life.discover())
    assert life.retire("math.add") is True
    assert not life.registry.contains("math.add")
    assert life.retire("math.add") is False
    # executing a retired tool raises not_found and records a failure
    try:
        asyncio.run(life.execute("math.add", {"a": 1, "b": 1}, False))
        assert False
    except ToolError as e:
        assert e.kind == "not_found"
    m = life.metrics_for("math.add")
    assert m is not None and m.failures == 1


def test_python_pack_loads_alongside_builtins():
    life = Lifecycle.permissive()
    life.add_source(PythonSource())
    life.add_source(BuiltinSource())
    n = asyncio.run(life.discover())
    assert n == 7 + len(asyncio.run(BuiltinSource().tools()))
    assert life.registry.contains("python.version")
    assert life.registry.contains("math.add")
