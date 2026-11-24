# rc-python

A full **Python** implementation of the RomaCode tools builder — a faithful
port of the Rust `rc-*` framework, plus a `python.*` language pack that
mirrors the Rust `rc-rust` pack.

## What's here

The same layer cake as the Rust workspace, in one stdlib-only package:

| layer | module | what it is |
|-------|--------|------------|
| core | `rc_python.core` | the `Tool` ABC, `ToolContext`, `ToolError`, `ToolSourceKind` |
| schema | `rc_python.schema` | `ParamType`, `Parameter`, `Schema` (+ JSON-Schema export) |
| registry | `rc_python.registry` | thread-safe `Registry` + `ToolInfo` |
| permissions | `rc_python.permissions` | `Permission`, `PermissionPolicy`, glob rules |
| sandbox | `rc_python.sandbox` | timeout / output-cap / net+fs allow-lists |
| validator | `rc_python.validator` | type-check + default-filling `validate()` |
| executor | `rc_python.executor` | the single run gate (policy → validate → sandbox → run) |
| manifest | `rc_python.manifest` | serialisable `Manifest` + `Catalog` |
| sources | `rc_python.sources` | `ToolSource` ABC + builtin/declarative/cli/rest + `ShellTool`/`HttpTool` |
| compose | `rc_python.compose` | `Pipeline` / `Parallel` / `Branch` + templating |
| lifecycle | `rc_python.lifecycle` | discover → register → validate → execute → monitor → retire |
| tools | `rc_python.tools` | the 7 `python.*` dev tools + `PythonSource` |

Runtime dependencies: **none** (stdlib only). Dev: `pytest`.

## Quick start

```python
import asyncio
from rc_python import Lifecycle, BuiltinSource, PythonSource

async def main():
    life = Lifecycle.permissive()
    life.add_source(BuiltinSource())      # the 14-tool builtin catalog
    life.add_source(PythonSource())       # the 7 python.* tools
    n = await life.discover()             # registers 21 tools
    print(f"loaded {n} tools")
    out = await life.execute("python.version", {}, True)
    print(out)                            # {'python': 'Python 3.12.x', 'exit_code': 0}
    print(await life.execute("math.add", {"a": 2, "b": 3}, False))  # 5.0

asyncio.run(main())
```

## Install (dev)

```sh
cd crates/rc-tools/rc-python
pip install -e .[dev]
pytest -q
```

Or run the tests with no install at all (the package is importable from the
source tree):

```sh
python3 -m pytest -q
```

## Parity with the Rust framework

| Rust (`rc-*`) | Python (`rc_python`) |
|---------------|----------------------|
| `rc_core::Tool` | `rc_python.core.Tool` |
| `rc_core::Registry` | `rc_python.registry.Registry` |
| `rc_core::Schema` / `ParamType` | `rc_python.schema.Schema` / `ParamType` |
| `rc_core::Executor` + `Sandbox` + `PermissionPolicy` | `rc_python.executor.Executor` + `sandbox.Sandbox` + `permissions.PermissionPolicy` |
| `rc_sources::{ToolSource, ShellTool, HttpTool}` | `rc_python.sources.{ToolSource, ShellTool, HttpTool}` |
| `rc_compose::{PipelineTool, ParallelTool, BranchTool}` | `rc_python.compose.{PipelineTool, ParallelTool, BranchTool}` |
| `rc_lifecycle::Lifecycle` + `MetricsStore` | `rc_python.lifecycle.Lifecycle` + `MetricsStore` |
| `rc_rust::{RustSource, all_tools}` | `rc_python.tools.{PythonSource, all_tools}` |

The only deliberate divergence: Python tool input/output is native Python
data (`dict` / `list` / `str` / …) rather than `serde_json::Value`, so there
is no separate JSON layer to bridge.
