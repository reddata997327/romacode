"""rc_python.permissions — the gate the executor consults (Python port of ``rc_core::permissions``).

Like the Rust policy (and the RomaCode server gate) this is **fully open by
default**: the default policy carries no rules and every tool call is
``ALLOW`` — nothing is denied, nothing asks. The rule machinery
(deny → ask → allow → default evaluation order) is kept so an embedder who
*wants* restrictions can opt in with a non-default policy.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import List

from .core import Tool


class Permission(Enum):
    ALLOW = "allow"
    ASK = "ask"
    DENY = "deny"


def glob_match(pattern: str, s: str) -> bool:
    """Glob match: ``*`` matches any run of chars, ``?`` matches one char."""
    return _glob_inner(pattern, 0, s, 0)


def _glob_inner(p: str, pi: int, t: str, ti: int) -> bool:
    while pi < len(p):
        c = p[pi]
        if c == "*":
            # trailing * eats everything
            if pi == len(p) - 1:
                return True
            # try to match the rest of the pattern at every position
            for skip in range(ti, len(t) + 1):
                if _glob_inner(p, pi + 1, t, skip):
                    return True
            return False
        if c == "?":
            if ti >= len(t):
                return False
            pi += 1
            ti += 1
            continue
        # literal
        if ti >= len(t) or t[ti] != c:
            return False
        pi += 1
        ti += 1
    return ti == len(t)


@dataclass
class PermissionRule:
    """A glob pattern matched against the tool name, bound to a decision."""

    pattern: str
    decision: Permission

    @classmethod
    def new(cls, pattern: str, decision: Permission) -> "PermissionRule":
        return cls(pattern=pattern, decision=decision)

    def matches(self, name: str) -> bool:
        return glob_match(self.pattern, name)


@dataclass
class PermissionPolicy:
    """The full permission policy.

    Evaluation order: explicit ``deny`` (first match wins) → ``ask`` →
    ``allow`` → ``default``.
    """

    deny: List[PermissionRule] = field(default_factory=list)
    ask: List[PermissionRule] = field(default_factory=list)
    allow: List[PermissionRule] = field(default_factory=list)
    default: Permission = Permission.ALLOW

    @classmethod
    def default_policy(cls) -> "PermissionPolicy":
        """Fully open, matching the Rust ``rc_core`` default and the server
        gate: no rules, every tool allowed. Restrictive policies are opt-in
        via the builder helpers (``deny_`` / ``ask_`` / ``allow_`` /
        ``with_default``); they are never the shipped default.
        """
        return cls(deny=[], ask=[], allow=[], default=Permission.ALLOW)

    # Python's dataclass default_factory makes `default_policy` the canonical
    # constructor; expose `__init__`-friendly `default()` too for ergonomics.
    @classmethod
    def default(cls) -> "PermissionPolicy":  # type: ignore[override]
        return cls.default_policy()

    def decide(self, tool: Tool) -> Permission:
        name = tool.name()
        for r in self.deny:
            if r.matches(name):
                return r.decision
        for r in self.ask:
            if r.matches(name):
                return r.decision
        for r in self.allow:
            if r.matches(name):
                return r.decision
        return self.default

    # ── builder helpers ────────────────────────────────────────────────────
    def deny_(self, pattern: str) -> "PermissionPolicy":
        self.deny.append(PermissionRule.new(pattern, Permission.DENY))
        return self

    def ask_(self, pattern: str) -> "PermissionPolicy":
        self.ask.append(PermissionRule.new(pattern, Permission.ASK))
        return self

    def allow_(self, pattern: str) -> "PermissionPolicy":
        self.allow.append(PermissionRule.new(pattern, Permission.ALLOW))
        return self

    def with_default(self, d: Permission) -> "PermissionPolicy":
        self.default = d
        return self
