"""rc_python.sources.tools — reusable Tool impls the source adapters build on.

:class:`ShellTool` runs a templated shell command; :class:`HttpTool` calls an
HTTP endpoint. Both are full :class:`~rc_python.core.Tool` impls, so once a
source produces one it is indistinguishable from a built-in. This mirrors the
Rust ``rc_sources::tools`` module (``ShellTool`` / ``HttpTool``).
"""

from __future__ import annotations

import asyncio
import base64
import json
import shlex
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Dict, List, Optional, Tuple

from ..core import Tool, ToolContext, ToolError, ToolSourceKind
from ..schema import ParamType, Parameter, Schema


def _stringify(v: Any) -> str:
    if isinstance(v, str):
        return v
    if v is None:
        return ""
    if isinstance(v, bool):
        return "true" if v else "false"
    return str(v)


def _render_once(tmpl: str, values: Dict[str, str]) -> str:
    """One-pass ``{{param}}`` substitution.

    The template is scanned left to right exactly once; substituted values
    are never rescanned, so a value that itself contains ``{{...}}`` arrives
    as inert text instead of being re-expanded (mirrors the Rust one-pass
    renderer in ``rc_sources::tools``).
    """
    out: List[str] = []
    rest = tmpl
    while True:
        start = rest.find("{{")
        if start < 0:
            out.append(rest)
            break
        out.append(rest[:start])
        after = rest[start + 2:]
        end = after.find("}}")
        if end < 0:
            out.append("{{")
            rest = after
            continue
        name = after[:end]
        if name in values:
            out.append(values[name])
        else:
            out.append(rest[start:start + end + 4])
        rest = after[end + 2:]
    return "".join(out)


def one_string_param(name: str, desc: str) -> Schema:
    """Helper: a schema with one required string parameter."""
    return Schema.new().param(Parameter.required(name, ParamType.string()).with_desc(desc))


class ShellTool(Tool):
    """A tool backed by a shell command template.

    ``{{param}}`` placeholders are substituted with each param's stringified
    value, then the command runs under ``sh -c``. This is the engine behind
    the ``declarative`` and ``cli`` sources.
    """

    def __init__(
        self,
        name: str,
        description: str,
        schema: Schema,
        command: str,
        source: ToolSourceKind = ToolSourceKind.Declarative,
        cwd: Optional[str] = None,
    ) -> None:
        self._name = name
        self._desc = description
        self._schema = schema
        self.command = command
        self._source = source
        self.cwd = cwd

    # ── Tool impl ───────────────────────────────────────────────────────────
    def name(self) -> str:
        return self._name

    def description(self) -> str:
        return self._desc

    def schema(self) -> Schema:
        return self._schema

    def source(self) -> ToolSourceKind:
        return self._source

    def render(self, input: Dict[str, Any]) -> str:
        # Values are data, not code: every substituted value is quoted as a
        # single literal shell word (mirrors the Rust `shell_quote` contract).
        values = {
            p.name: shlex.quote(_stringify(input[p.name]))
            for p in self._schema.parameters
            if p.name in input
        }
        return _render_once(self.command, values)

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> Dict[str, Any]:
        if ctx.cancelled:
            raise ToolError("cancelled before shell tool", kind="other")
        rendered = self.render(input)
        proc = await asyncio.create_subprocess_exec(
            "sh", "-c", rendered,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=self.cwd,
        )
        stdout_b, stderr_b = await proc.communicate()
        return {
            "exit_code": proc.returncode if proc.returncode is not None else -1,
            "stdout": stdout_b.decode("utf-8", errors="replace"),
            "stderr": stderr_b.decode("utf-8", errors="replace"),
        }


class HttpTool(Tool):
    """A tool backed by an HTTP call.

    The URL and optional JSON body are templates with ``{{param}}``
    placeholders, substituted from the input. Uses ``urllib`` (stdlib) so the
    framework stays dependency-free; the blocking call is run in a thread via
    :func:`asyncio.to_thread`.
    """

    def __init__(
        self,
        name: str,
        description: str,
        schema: Schema,
        method: str,
        url: str,
        source: ToolSourceKind = ToolSourceKind.Rest,
        body: Optional[str] = None,
        headers: Optional[List[Tuple[str, str]]] = None,
    ) -> None:
        self._name = name
        self._desc = description
        self._schema = schema
        self.method = method.upper()
        self.url = url
        self.body = body
        self.headers = headers or []
        self._source = source

    def name(self) -> str:
        return self._name

    def description(self) -> str:
        return self._desc

    def schema(self) -> Schema:
        return self._schema

    def source(self) -> ToolSourceKind:
        return self._source

    def render(self, input: Dict[str, Any], tmpl: str, in_url: bool = False) -> str:
        # One-pass substitution: substituted values are never rescanned. URL
        # placeholders get percent-encoding; body placeholders get JSON
        # escaping (the body template keeps its own surrounding quotes).
        values = {}
        for p in self._schema.parameters:
            if p.name in input:
                raw = _stringify(input[p.name])
                if in_url:
                    values[p.name] = urllib.parse.quote(raw, safe="")
                else:
                    values[p.name] = json.dumps(raw)[1:-1]
        return _render_once(tmpl, values)

    async def execute(self, input: Dict[str, Any], ctx: ToolContext) -> Dict[str, Any]:
        ctx.check_net()
        url = self.render(input, self.url, in_url=True)
        body_bytes: Optional[bytes] = None
        headers = list(self.headers)
        if self.body is not None:
            body_str = self.render(input, self.body)
            # try JSON; fall back to raw string
            try:
                parsed = json.loads(body_str)
                body_bytes = json.dumps(parsed).encode("utf-8")
                headers.append(("content-type", "application/json"))
            except ValueError:
                body_bytes = body_str.encode("utf-8")

        def _do() -> Tuple[int, str]:
            req = urllib.request.Request(url, data=body_bytes, method=self.method)
            for k, v in headers:
                req.add_header(k, v)
            try:
                with urllib.request.urlopen(req, timeout=20) as resp:
                    status = resp.getcode()
                    raw = resp.read().decode("utf-8", errors="replace")
                    return status, raw
            except urllib.error.HTTPError as e:
                raw = e.read().decode("utf-8", errors="replace")
                return e.code, raw

        status, text = await asyncio.to_thread(_do)
        try:
            parsed_body: Any = json.loads(text)
        except ValueError:
            parsed_body = text
        return {"status": status, "body": parsed_body}
