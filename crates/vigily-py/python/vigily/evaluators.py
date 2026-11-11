"""Evaluators: functions that score one output against an item.

Built-in: :func:`exact_match`, :func:`contains`, :func:`regex`, :func:`numeric`, :func:`json_schema`.
:func:`custom` wraps any function, including an LLM-as-judge that calls your own model client;
vigily itself never calls a model or holds an API key.

Every evaluator has a name and a version. Both are stored with each score, so a change of
evaluator shows in the audit trail. Built-ins are versioned by vigily; a custom evaluator's
version is the one you give, or a hash of its source code.
"""

from __future__ import annotations

import hashlib
import inspect
import json
import math
import re
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class Score:
    """A verdict: a value (usually 0 to 1), whether it passes, and why."""

    value: float | None
    passed: bool | None = None
    reason: str | None = None


class Evaluator:
    def __init__(self, name: str, version: str, kind: str, fn: Callable[..., Any]) -> None:
        self.name, self.version, self.kind, self._fn = name, version, kind, fn

    def __call__(self, *, input: Any, output: Any, expected: Any, item: Any) -> Score:
        return as_score(self._fn(input=input, output=output, expected=expected, item=item))

    def describe(self) -> dict[str, str]:
        return {"name": self.name, "version": self.version, "kind": self.kind}

    def __repr__(self) -> str:
        return f"<Evaluator {self.name} {self.version}>"


def as_score(r: Any) -> Score:
    """Turns a bool, number, Score, (value, passed) or (value, passed, reason) tuple, or dict into a Score."""
    if isinstance(r, Score):
        return r
    if isinstance(r, bool):
        return Score(1.0 if r else 0.0, r)
    if isinstance(r, (int, float)):
        return Score(float(r))
    if isinstance(r, tuple) and 1 <= len(r) <= 3:
        return Score(*r)
    if isinstance(r, Mapping):
        return Score(r.get("value"), r.get("passed"), r.get("reason"))
    if r is None:
        return Score(None)
    raise TypeError(f"an evaluator returned {type(r).__name__}; return a bool, a number or a Score")


def _text(x: Any) -> str:
    return x if isinstance(x, str) else json.dumps(x, sort_keys=True, ensure_ascii=False)


def exact_match(*, case_sensitive: bool = True, strip: bool = True, name: str = "exact_match") -> Evaluator:
    """Passes when the output equals the expected output (as text)."""

    def f(*, output, expected, **_):
        a, b = _text(output), _text(expected)
        if strip:
            a, b = a.strip(), b.strip()
        if not case_sensitive:
            a, b = a.lower(), b.lower()
        return a == b

    return Evaluator(name, "vigily/exact_match@1", "exact_match", f)


def contains(
    substring: str | None = None, *, case_sensitive: bool = False, name: str = "contains"
) -> Evaluator:
    """Passes when the output contains ``substring``, or the expected output when none is given."""

    def f(*, output, expected, **_):
        needle = _text(substring if substring is not None else expected)
        hay = _text(output)
        if not case_sensitive:
            needle, hay = needle.lower(), hay.lower()
        return needle in hay

    return Evaluator(name, "vigily/contains@1", "contains", f)


def regex(pattern: str, *, flags: int = 0, name: str = "regex") -> Evaluator:
    """Passes when ``pattern`` matches anywhere in the output."""
    compiled = re.compile(pattern, flags)

    def f(*, output, **_):
        return compiled.search(_text(output)) is not None

    return Evaluator(name, f"vigily/regex@1:{pattern}", "regex", f)


def numeric(*, tolerance: float = 1e-6, relative: bool = False, name: str = "numeric") -> Evaluator:
    """Passes when the output is a number within ``tolerance`` of the expected number (relative to
    it when ``relative``). The value is 1 or 0; the reason holds the absolute error."""

    def f(*, output, expected, **_):
        try:
            a, b = float(output), float(expected)
        except (TypeError, ValueError):
            return Score(None, False, "not a number")
        err = abs(a - b)
        limit = tolerance * abs(b) if relative else tolerance
        return Score(1.0 if err <= limit else 0.0, err <= limit, f"error {err:.6g}")

    return Evaluator(name, f"vigily/numeric@1:{tolerance}{'r' if relative else ''}", "numeric", f)


def json_schema(schema: Mapping[str, Any], *, name: str = "json_schema") -> Evaluator:
    """Passes when the output (a JSON string or a value) is valid against ``schema``.

    Uses the ``jsonschema`` package when it is installed; otherwise a built-in subset: ``type``,
    ``properties``, ``required``, ``additionalProperties: false``, ``items``, ``enum``, ``const``,
    ``minimum``, ``maximum``, ``minLength``, ``maxLength``, ``minItems``, ``maxItems``.
    """
    digest = hashlib.sha256(json.dumps(schema, sort_keys=True).encode()).hexdigest()[:12]
    try:
        import jsonschema  # type: ignore[import-not-found]

        validator = jsonschema.Draft202012Validator(dict(schema))

        def errors(v):
            return [e.message for e in validator.iter_errors(v)]
    except ImportError:

        def errors(v):
            return _validate(v, schema, "$")

    def f(*, output, **_):
        value = output
        if isinstance(output, str):
            try:
                value = json.loads(output)
            except json.JSONDecodeError as e:
                return Score(0.0, False, f"not JSON: {e.msg}")
        errs = errors(value)
        return Score(0.0 if errs else 1.0, not errs, "; ".join(errs[:3]) or None)

    return Evaluator(name, f"vigily/json_schema@1:{digest}", "json_schema", f)


_TYPES = {
    "object": dict,
    "array": list,
    "string": str,
    "boolean": bool,
    "null": type(None),
}


def _type_ok(v: Any, t: str) -> bool:
    if t == "integer":
        return isinstance(v, int) and not isinstance(v, bool)
    if t == "number":
        return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)
    return isinstance(v, _TYPES.get(t, object))


def _validate(v: Any, s: Mapping[str, Any], path: str) -> list[str]:
    errs: list[str] = []
    t = s.get("type")
    if t is not None:
        types = t if isinstance(t, list) else [t]
        if not any(_type_ok(v, x) for x in types):
            return [f"{path}: expected {' or '.join(types)}"]
    if "enum" in s and v not in s["enum"]:
        errs.append(f"{path}: not one of {s['enum']}")
    if "const" in s and v != s["const"]:
        errs.append(f"{path}: must be {s['const']!r}")
    if isinstance(v, (int, float)) and not isinstance(v, bool):
        if "minimum" in s and v < s["minimum"]:
            errs.append(f"{path}: below {s['minimum']}")
        if "maximum" in s and v > s["maximum"]:
            errs.append(f"{path}: above {s['maximum']}")
    if isinstance(v, str):
        if "minLength" in s and len(v) < s["minLength"]:
            errs.append(f"{path}: shorter than {s['minLength']}")
        if "maxLength" in s and len(v) > s["maxLength"]:
            errs.append(f"{path}: longer than {s['maxLength']}")
    if isinstance(v, dict):
        props = s.get("properties", {})
        for k in s.get("required", []):
            if k not in v:
                errs.append(f"{path}: missing {k!r}")
        for k, x in v.items():
            if k in props:
                errs += _validate(x, props[k], f"{path}.{k}")
            elif s.get("additionalProperties") is False:
                errs.append(f"{path}: unexpected {k!r}")
    if isinstance(v, list):
        if "minItems" in s and len(v) < s["minItems"]:
            errs.append(f"{path}: fewer than {s['minItems']} items")
        if "maxItems" in s and len(v) > s["maxItems"]:
            errs.append(f"{path}: more than {s['maxItems']} items")
        if isinstance(s.get("items"), Mapping):
            for i, x in enumerate(v):
                errs += _validate(x, s["items"], f"{path}[{i}]")
    return errs


def custom(name: str, fn: Callable[..., Any], *, version: str | None = None) -> Evaluator:
    """Wraps your own function as an evaluator.

    ``fn`` is called with keyword arguments it accepts out of ``input``, ``output``, ``expected``
    and ``item``, and returns a bool, a number, a :class:`Score`, or ``(value, passed, reason)``.
    An LLM-as-judge is a custom evaluator that calls your model client inside ``fn``.

    The version defaults to a hash of the function's source, so editing the judge prompt in the
    function changes the recorded evaluator version.
    """
    if version is None:
        try:
            src = inspect.getsource(fn)
        except (OSError, TypeError):
            src = getattr(fn, "__qualname__", repr(fn))
        version = "sha256:" + hashlib.sha256(src.encode()).hexdigest()[:12]
    try:
        accepted = set(inspect.signature(fn).parameters)
        takes_all = any(
            p.kind is inspect.Parameter.VAR_KEYWORD for p in inspect.signature(fn).parameters.values()
        )
    except (TypeError, ValueError):
        accepted, takes_all = set(), True

    def call(**kw):
        return fn(**(kw if takes_all else {k: v for k, v in kw.items() if k in accepted}))

    return Evaluator(name, version, "custom", call)
