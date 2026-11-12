"""LLM tracing: spans with timing, status, model, token usage and payloads, sent to ``vigily up``.

.. code-block:: python

    import vigily

    @vigily.trace(kind="tool")
    def search(query: str) -> list[str]: ...

    with vigily.trace("answer", run=run) as root:
        docs = search("synthetic question")
        with vigily.trace("chat", kind="llm", model="example-model", provider="example") as llm:
            llm.set_input([{"role": "user", "content": "..."}])
            reply = client_call(...)
            llm.set_output(reply.text)
            llm.set_usage(input_tokens=812, output_tokens=95, cached_tokens=512)

Spans nest through :mod:`contextvars`, so nesting follows ``asyncio`` tasks and stays correct when
coroutines interleave. A span ends when its block exits; it is then queued and a background thread
sends queued spans to the server in batches (``POST /api/v1/traces/ingest``), so tracing never blocks
the traced code on the network. Queued spans are sent when the interpreter exits normally; call
:func:`flush` to send them earlier (for example before ``os._exit``).

The server is ``http://127.0.0.1:7600`` unless :func:`configure` or ``VIGILY_URL`` names another;
``VIGILY_TRACING=off`` turns tracing off. The server decides what is stored of inputs and outputs
(full, truncated to 4 KiB per field by default, or metadata only) and computes cost from its price
table; :func:`set_redactor` changes or drops spans before they leave the process.
"""

from __future__ import annotations

import atexit
import contextlib
import contextvars
import functools
import inspect
import json
import math
import os
import queue
import re
import secrets
import sys
import threading
import time
import urllib.error
import urllib.request
from collections.abc import Callable, Mapping
from typing import Any, TypeVar

__all__ = [
    "Span",
    "configure",
    "current_span",
    "flush",
    "set_redactor",
    "trace",
    "usage_from",
]

DEFAULT_URL = "http://127.0.0.1:7600"

F = TypeVar("F", bound=Callable[..., Any])
Redactor = Callable[[dict[str, Any]], "dict[str, Any] | None"]

_current: contextvars.ContextVar[Span | None] = contextvars.ContextVar("vigily_span", default=None)
#: Set by ``evaluate()`` around each target call: (run id, item id, trace ids started for it).
_eval_item: contextvars.ContextVar[tuple[str, str, list[str]] | None] = contextvars.ContextVar(
    "vigily_eval_item", default=None
)


def _jsonable(v: Any, depth: int = 0) -> Any:
    """Plain JSON values; anything else becomes its ``str``."""
    if depth > 32:
        return str(v)
    if v is None or isinstance(v, (bool, int, str)):
        return v
    if isinstance(v, float):
        return v if math.isfinite(v) else str(v)
    if isinstance(v, Mapping):
        return {str(k): _jsonable(x, depth + 1) for k, x in v.items()}
    if isinstance(v, (list, tuple, set, frozenset)):
        return [_jsonable(x, depth + 1) for x in v]
    for attr in ("model_dump", "to_dict"):
        f = getattr(v, attr, None)
        if callable(f):
            try:
                return _jsonable(f(), depth + 1)
            except Exception:  # noqa: BLE001 - fall back to str
                break
    return str(v)


def _now_us() -> int:
    return time.time_ns() // 1000


# ---------------------------------------------------------------------------------------------
# Export


class _Exporter:
    """A background thread that sends queued spans in batches."""

    def __init__(
        self,
        url: str,
        batch_size: int,
        flush_interval: float,
        timeout: float,
        max_queue: int,
    ) -> None:
        self.url = url.rstrip("/") + "/api/v1/traces/ingest"
        self.batch_size = batch_size
        self.flush_interval = flush_interval
        self.timeout = timeout
        self.queue: queue.Queue[dict[str, Any] | threading.Event] = queue.Queue(max_queue)
        self.dropped = 0
        self.failed = 0
        self.sent = 0
        self._warned = False
        self._thread = threading.Thread(target=self._loop, name="vigily-tracing", daemon=True)
        self._thread.start()

    def put(self, span: dict[str, Any]) -> None:
        try:
            self.queue.put_nowait(span)
        except queue.Full:
            self.dropped += 1
            self._warn(f"the span queue is full; {self.dropped} spans dropped")

    def flush(self, timeout: float | None = None) -> bool:
        """Sends everything queued so far; returns False when that took longer than ``timeout``."""
        done = threading.Event()
        try:
            self.queue.put(done, timeout=timeout)
        except queue.Full:
            return False
        return done.wait(timeout)

    def _warn(self, msg: str) -> None:
        if not self._warned:
            self._warned = True
            print(f"vigily tracing: {msg}", file=sys.stderr)

    def _send(self, batch: list[dict[str, Any]]) -> None:
        body = json.dumps({"spans": batch}, default=str).encode()
        req = urllib.request.Request(
            self.url, data=body, method="POST", headers={"content-type": "application/json"}
        )
        for attempt in range(3):
            try:
                with urllib.request.urlopen(req, timeout=self.timeout) as r:
                    r.read()
                self.sent += len(batch)
                return
            except urllib.error.HTTPError as e:
                if e.code < 500:
                    # A client error does not go away on retry. The response body is not shown:
                    # it could echo span content.
                    self.failed += len(batch)
                    self._warn(f"the server refused {len(batch)} spans (HTTP {e.code} {e.reason})")
                    return
            except (OSError, ValueError):
                pass
            time.sleep(0.2 * (attempt + 1))
        self.failed += len(batch)
        self._warn(f"could not reach {self.url}; {self.failed} spans not sent (is `vigily up` running?)")

    def _loop(self) -> None:
        batch: list[dict[str, Any]] = []
        deadline = time.monotonic() + self.flush_interval
        while True:
            try:
                item = self.queue.get(timeout=max(0.0, deadline - time.monotonic()))
            except queue.Empty:
                item = None
            if isinstance(item, threading.Event):
                if batch:
                    self._send(batch)
                    batch = []
                item.set()
                deadline = time.monotonic() + self.flush_interval
                continue
            if item is not None:
                batch.append(item)
            if len(batch) >= self.batch_size or (batch and time.monotonic() >= deadline):
                self._send(batch)
                batch = []
            if time.monotonic() >= deadline:
                deadline = time.monotonic() + self.flush_interval


_lock = threading.Lock()
_exporter: _Exporter | None = None
_settings: dict[str, Any] = {
    "url": None,
    "enabled": None,
    "batch_size": 256,
    "flush_interval": 1.0,
    "timeout": 5.0,
    "max_queue": 10_000,
}
_redactor: Redactor | None = None


def _enabled() -> bool:
    if _settings["enabled"] is not None:
        return bool(_settings["enabled"])
    return os.environ.get("VIGILY_TRACING", "on").strip().lower() not in ("0", "off", "false", "no")


def _get_exporter() -> _Exporter:
    global _exporter
    with _lock:
        if _exporter is None:
            _exporter = _Exporter(
                _settings["url"] or os.environ.get("VIGILY_URL") or DEFAULT_URL,
                _settings["batch_size"],
                _settings["flush_interval"],
                _settings["timeout"],
                _settings["max_queue"],
            )
        return _exporter


def configure(
    url: str | None = None,
    *,
    enabled: bool | None = None,
    batch_size: int | None = None,
    flush_interval: float | None = None,
    timeout: float | None = None,
    max_queue: int | None = None,
) -> None:
    """Sets where and how spans are sent. Spans already queued are sent to the old server first.

    ``url`` is the server (``VIGILY_URL``, else ``http://127.0.0.1:7600``); ``enabled=False``
    turns tracing off (``trace`` blocks then cost a few microseconds and send nothing);
    ``batch_size`` and ``flush_interval`` (seconds) bound how long a span waits in the queue;
    ``max_queue`` spans at most are queued, newer ones are dropped with a warning.
    """
    global _exporter
    flush()
    with _lock:
        for k, v in {
            "url": url,
            "enabled": enabled,
            "batch_size": batch_size,
            "flush_interval": flush_interval,
            "timeout": timeout,
            "max_queue": max_queue,
        }.items():
            if v is not None:
                _settings[k] = v
        _exporter = None


def set_redactor(fn: Redactor | None) -> None:
    """Calls ``fn`` on every finished span (a dict with ``name``, ``kind``, ``input``, ``output``,
    ``error``, ``attributes``, ...) before it is queued. Return the span, changed as needed, or
    ``None`` to drop it. If ``fn`` raises, the span is sent without its input, output, error
    message and string attributes, so a failing redactor never leaks a payload."""
    global _redactor
    _redactor = fn


def flush(timeout: float | None = 10.0) -> bool:
    """Sends every finished span now. Returns False if that took longer than ``timeout`` seconds."""
    ex = _exporter
    if ex is None:
        return True
    return ex.flush(timeout)


atexit.register(flush)


def _after_fork_in_child() -> None:
    """A forked child starts with no exporter: the parent's thread does not exist in it, and the
    spans queued in the parent are the parent's to send. Without this, the child's exit would wait
    on a queue nothing drains."""
    global _exporter, _lock
    _lock = threading.Lock()
    _exporter = None


if hasattr(os, "register_at_fork"):
    os.register_at_fork(after_in_child=_after_fork_in_child)


_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_.]{0,63}")


def _error_type(error: Any) -> str:
    """An error's type when its text starts with an identifier and a colon (``KeyError: ...``),
    else ``error``: free text never survives."""
    head = str(error).split(":", 1)[0].strip()
    return head if _IDENTIFIER.fullmatch(head) else "error"


# ---------------------------------------------------------------------------------------------
# Spans


def usage_from(response: Any) -> dict[str, int]:
    """Token usage from a model client's response object or dict, without importing any client.

    Understands ``usage.prompt_tokens`` / ``completion_tokens`` /
    ``prompt_tokens_details.cached_tokens`` and ``usage.input_tokens`` / ``output_tokens`` /
    ``cache_read_input_tokens``. Returns only the counts it found.
    """

    def get(o: Any, k: str) -> Any:
        if o is None:
            return None
        return o.get(k) if isinstance(o, Mapping) else getattr(o, k, None)

    u = get(response, "usage") or response
    out: dict[str, int] = {}
    for key, names in (
        ("input_tokens", ("input_tokens", "prompt_tokens")),
        ("output_tokens", ("output_tokens", "completion_tokens")),
    ):
        for n in names:
            v = get(u, n)
            if isinstance(v, int):
                out[key] = v
                break
    cached = get(get(u, "prompt_tokens_details"), "cached_tokens")
    if not isinstance(cached, int):
        cached = get(u, "cache_read_input_tokens")
    if isinstance(cached, int):
        out["cached_tokens"] = cached
        # Some APIs count cache reads apart from the input; vigily counts them as part of it.
        if get(u, "cache_read_input_tokens") is not None and "input_tokens" in out:
            out["input_tokens"] += cached
    return out


class Span:
    """A span being recorded. Get it from ``with vigily.trace(...) as span`` or
    :func:`current_span`."""

    __slots__ = (
        "_ended",
        "_started",
        "_token",
        "attributes",
        "end_time",
        "error",
        "eval_item",
        "input",
        "kind",
        "model",
        "name",
        "output",
        "parent_id",
        "project",
        "provider",
        "run_id",
        "span_id",
        "start_time",
        "status",
        "trace_id",
        "usage",
    )

    def __init__(
        self,
        name: str,
        kind: str,
        parent: Span | None,
        run_id: str | None,
        project: str | None,
        eval_item: str | None,
    ) -> None:
        self.name = name
        self.kind = kind
        self.trace_id = parent.trace_id if parent else secrets.token_hex(16)
        self.span_id = secrets.token_hex(8)
        self.parent_id = parent.span_id if parent else None
        self.run_id = run_id if run_id is not None else (parent.run_id if parent else None)
        self.project = project if project is not None else (parent.project if parent else None)
        self.eval_item = eval_item if eval_item is not None else (parent.eval_item if parent else None)
        self.start_time = _now_us()
        self._started = time.perf_counter()
        self.end_time: int | None = None
        self.status = "unset"
        self.error: str | None = None
        self.attributes: dict[str, Any] = {}
        self.model: str | None = None
        self.provider: str | None = None
        self.input: Any = None
        self.output: Any = None
        self.usage: dict[str, int] | None = None
        self._token: contextvars.Token[Span | None] | None = None
        self._ended = False

    def set_input(self, value: Any) -> None:
        self.input = _jsonable(value)

    def set_output(self, value: Any) -> None:
        self.output = _jsonable(value)

    def set_model(self, model: str, provider: str | None = None) -> None:
        self.model = model
        if provider is not None:
            self.provider = provider

    def set_usage(
        self,
        input_tokens: int | None = None,
        output_tokens: int | None = None,
        cached_tokens: int | None = None,
        *,
        response: Any = None,
    ) -> None:
        """Records token usage: counts given directly, or read from a client response
        (``response=``, see :func:`usage_from`). ``cached_tokens`` are the input tokens served from
        the provider's prompt cache, counted within ``input_tokens``."""
        u = dict(self.usage or {})
        if response is not None:
            u.update(usage_from(response))
        for k, v in (
            ("input_tokens", input_tokens),
            ("output_tokens", output_tokens),
            ("cached_tokens", cached_tokens),
        ):
            if v is not None:
                u[k] = int(v)
        self.usage = u or None

    def set_attribute(self, key: str, value: Any) -> None:
        self.attributes[key] = _jsonable(value)

    def set_attributes(self, attributes: Mapping[str, Any]) -> None:
        for k, v in attributes.items():
            self.set_attribute(k, v)

    def record_error(self, error: BaseException | str) -> None:
        """Marks the span failed. An exception leaving a ``trace`` block does this itself."""
        self.status = "error"
        if isinstance(error, BaseException):
            self.error = f"{type(error).__name__}: {error}"
        else:
            self.error = str(error)

    def _to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {
            "trace_id": self.trace_id,
            "span_id": self.span_id,
            "parent_id": self.parent_id,
            "name": self.name,
            "kind": self.kind,
            "start_time": self.start_time,
            "end_time": self.end_time,
            "status": self.status,
            "error": self.error,
            "attributes": self.attributes,
            "run_id": self.run_id,
            "eval_item": self.eval_item,
            "project": self.project,
            "model": self.model,
            "provider": self.provider,
            "input": self.input,
            "output": self.output,
            "usage": self.usage,
        }
        return {k: v for k, v in d.items() if v is not None and v != {}}

    def _end(self) -> None:
        if self._ended:
            return
        self._ended = True
        # Wall-clock start plus a monotonic duration: immune to clock steps during the span.
        self.end_time = self.start_time + int((time.perf_counter() - self._started) * 1_000_000)
        if self.status == "unset":
            self.status = "ok"
        if not _enabled():
            return
        d = self._to_dict()
        if _redactor is not None:
            try:
                d = _redactor(d)
            except Exception as e:  # noqa: BLE001 - never leak a payload because the hook failed
                print(
                    f"vigily tracing: the redactor raised {type(e).__name__}; payloads dropped",
                    file=sys.stderr,
                )
                d = self._to_dict()
                for k in ("input", "output"):
                    d.pop(k, None)
                if "error" in d:
                    d["error"] = _error_type(d["error"])
                # Only numbers and booleans: strings, lists and dicts can all carry text.
                d["attributes"] = {
                    k: v
                    for k, v in self.attributes.items()
                    if isinstance(v, (bool, int, float)) and _IDENTIFIER.fullmatch(str(k))
                }
            if d is None:
                return
        _get_exporter().put(d)

    def __repr__(self) -> str:
        return f"Span({self.name!r}, kind={self.kind!r}, trace={self.trace_id[:8]})"


def current_span() -> Span | None:
    """The innermost open span in this thread or task, or None."""
    return _current.get()


def _run_id(run: Any) -> str | None:
    if run is None:
        return None
    return run if isinstance(run, str) else str(run.id)


class _Trace:
    """What :func:`trace` returns: a context manager (sync and async) and a decorator."""

    def __init__(
        self,
        name: str | None,
        kind: str,
        run: Any,
        project: str | None,
        model: str | None,
        provider: str | None,
        attributes: Mapping[str, Any] | None,
        capture: bool,
    ) -> None:
        self.name = name
        self.kind = kind
        self.run_id = _run_id(run)
        self.project = project
        self.model = model
        self.provider = provider
        self.attributes = dict(attributes or {})
        self.capture = capture

    def _start(self, name: str | None = None) -> Span:
        span = self._make(name)
        span._token = _current.set(span)
        return span

    def _make(self, name: str | None = None) -> Span:
        """A new span, child of the current one, not made current."""
        parent = _current.get()
        run_id, eval_item = self.run_id, None
        ev = _eval_item.get()
        if parent is None and ev is not None:
            run_id = run_id or ev[0]
            eval_item = ev[1]
        span = Span(name or self.name or "span", self.kind, parent, run_id, self.project, eval_item)
        if parent is None and ev is not None:
            ev[2].append(span.trace_id)
        if self.model is not None:
            span.set_model(self.model, self.provider)
        elif self.provider is not None:
            span.provider = self.provider
        if self.attributes:
            span.set_attributes(self.attributes)
        return span

    @staticmethod
    def _finish(span: Span, exc: BaseException | None) -> None:
        if exc is not None and not isinstance(exc, (GeneratorExit, StopIteration, StopAsyncIteration)):
            span.record_error(exc)
        try:
            span._end()
        finally:
            if span._token is not None:
                # Exited in another context (a span handed between tasks): leave it be.
                with contextlib.suppress(ValueError):
                    _current.reset(span._token)

    # The context manager keeps no per-entry state on self (that would break reuse across
    # threads and tasks): the open span is the context variable's value.
    def __enter__(self) -> Span:
        return self._start()

    def __exit__(self, tp: Any, exc: BaseException | None, tb: Any) -> None:
        span = _current.get()
        if span is not None:
            self._finish(span, exc)

    async def __aenter__(self) -> Span:
        return self._start()

    async def __aexit__(self, tp: Any, exc: BaseException | None, tb: Any) -> None:
        self.__exit__(tp, exc, tb)

    def __call__(self, fn: F) -> F:
        name = self.name or getattr(fn, "__qualname__", None) or getattr(fn, "__name__", "span")

        def capture_input(span: Span, args: tuple[Any, ...], kwargs: dict[str, Any]) -> None:
            if not self.capture:
                return
            try:
                bound = inspect.signature(fn).bind_partial(*args, **kwargs)
                inputs = {k: v for k, v in bound.arguments.items() if k not in ("self", "cls")}
            except (TypeError, ValueError):
                inputs = {"args": list(args), "kwargs": kwargs}
            span.set_input(inputs)

        if inspect.isgeneratorfunction(fn):
            return self._wrap_generator(fn, name, capture_input)  # type: ignore[return-value]
        if inspect.isasyncgenfunction(fn):
            return self._wrap_async_generator(fn, name, capture_input)  # type: ignore[return-value]

        if inspect.iscoroutinefunction(fn):

            @functools.wraps(fn)
            async def awrapper(*args: Any, **kwargs: Any) -> Any:
                span = self._start(name)
                capture_input(span, args, kwargs)
                try:
                    out = await fn(*args, **kwargs)
                except BaseException as e:
                    self._finish(span, e)
                    raise
                if self.capture:
                    span.set_output(out)
                self._finish(span, None)
                return out

            return awrapper  # type: ignore[return-value]

        @functools.wraps(fn)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            span = self._start(name)
            capture_input(span, args, kwargs)
            try:
                out = fn(*args, **kwargs)
            except BaseException as e:
                self._finish(span, e)
                raise
            if self.capture:
                span.set_output(out)
            self._finish(span, None)
            return out

        return wrapper  # type: ignore[return-value]

    # A generator's span covers its whole iteration, from the first item to exhaustion (or to the
    # consumer closing it). The span is current only while the generator's own code runs, never
    # in the consumer's code between items.
    def _wrap_generator(self, fn: Callable[..., Any], name: str, capture_input: Callable[..., None]) -> Any:
        @functools.wraps(fn)
        def gwrapper(*args: Any, **kwargs: Any) -> Any:
            span = self._make(name)
            capture_input(span, args, kwargs)
            items, error = 0, None
            try:
                tok = _current.set(span)
                try:
                    gen = fn(*args, **kwargs)
                finally:
                    _current.reset(tok)
                sent: Any = None
                thrown: BaseException | None = None
                while True:
                    tok = _current.set(span)
                    try:
                        item = gen.throw(thrown) if thrown is not None else gen.send(sent)
                    except StopIteration as stop:
                        if self.capture and stop.value is not None:
                            span.set_output(stop.value)
                        return stop.value
                    finally:
                        _current.reset(tok)
                        thrown = None
                    items += 1
                    try:
                        sent = yield item
                    except GeneratorExit:
                        gen.close()
                        raise
                    except BaseException as e:  # noqa: BLE001 - passed into the generator
                        thrown = e
            except GeneratorExit:
                pass
            except BaseException as e:
                error = e
                raise
            finally:
                span.set_attribute("items", items)
                self._finish(span, error)

        return gwrapper

    def _wrap_async_generator(
        self, fn: Callable[..., Any], name: str, capture_input: Callable[..., None]
    ) -> Any:
        @functools.wraps(fn)
        async def agwrapper(*args: Any, **kwargs: Any) -> Any:
            span = self._make(name)
            capture_input(span, args, kwargs)
            items, error = 0, None
            try:
                tok = _current.set(span)
                try:
                    gen = fn(*args, **kwargs)
                finally:
                    _current.reset(tok)
                sent: Any = None
                thrown: BaseException | None = None
                while True:
                    tok = _current.set(span)
                    try:
                        item = await (gen.athrow(thrown) if thrown is not None else gen.asend(sent))
                    except StopAsyncIteration:
                        return
                    finally:
                        _current.reset(tok)
                        thrown = None
                    items += 1
                    try:
                        sent = yield item
                    except GeneratorExit:
                        await gen.aclose()
                        raise
                    except BaseException as e:  # noqa: BLE001 - passed into the generator
                        thrown = e
            except GeneratorExit:
                pass
            except BaseException as e:
                error = e
                raise
            finally:
                span.set_attribute("items", items)
                self._finish(span, error)

        return agwrapper


def trace(
    name: str | Callable[..., Any] | None = None,
    *,
    kind: str = "chain",
    run: Any = None,
    project: str | None = None,
    model: str | None = None,
    provider: str | None = None,
    attributes: Mapping[str, Any] | None = None,
    capture: bool = True,
) -> Any:
    """Records a span: ``with vigily.trace("name") as span:``, ``async with``, or as a decorator
    (``@vigily.trace``, ``@vigily.trace(kind="tool")``).

    A span opened inside another becomes its child; the outermost span starts a new trace. ``kind``
    is ``llm``, ``tool``, ``retrieval``, ``chain`` (the default), ``embedding``, ``agent`` or another
    short word. ``run`` (a :class:`~vigily.Run` or run id) links the trace to a run, so it is listed
    on the run's page and deleted with it; children inherit it. Inside ``evaluate()`` traces are
    linked to the eval run and item automatically. ``project`` picks the server's payload policy
    (the run's experiment by default). As a decorator, the arguments and return value are recorded
    as input and output unless ``capture=False``.
    """
    if callable(name):
        return _Trace(None, kind, run, project, model, provider, attributes, capture)(name)
    return _Trace(name, kind, run, project, model, provider, attributes, capture)


class _EvalItem:
    """Links traces started inside to an evaluation item (used by ``evaluate()``)."""

    def __init__(self, run_id: str, item_id: str) -> None:
        self.ids: list[str] = []
        self._value = (run_id, item_id, self.ids)
        self._token: contextvars.Token[Any] | None = None

    def __enter__(self) -> _EvalItem:
        self._token = _eval_item.set(self._value)
        return self

    def __exit__(self, *exc: Any) -> None:
        if self._token is not None:
            _eval_item.reset(self._token)
