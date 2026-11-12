"""The sender: one background thread per engine that ships runs and spans in batches.

Everything a process logs goes into an ordered operation log per engine. Points of the same series
collect in columns (a *segment*: steps, values, epochs, timestamps), so a point costs about 24 bytes
on the wire before compression. The log is cut into batches every second, at 20 000 points or at
4 MiB, and each batch is sent gzip-compressed to ``POST /api/v1/ingest`` with the writer's id and a
sequence number (``Idempotency-Key: <writer>:<seq>``): a batch sent twice is applied once, and
because the client assigns every step, even a batch the engine forgot (after a restart) stores
nothing twice.

Failures are retried with exponential backoff and full jitter, honouring ``Retry-After``. After
two minutes without reaching the engine (``VIGILY_SPOOL_AFTER``), batches go to the spool on disk
(:mod:`vigily._spool`), in order, and are sent from there once the engine answers again, or by
``vigily sync``. When batches pile up faster than the engine takes them, ``track()`` waits up to
five seconds, then the waiting batches go to the spool too. Nothing is dropped without a message.

Spans share the thread and the spool but keep their own bounded queue: when it is full, new spans
are dropped with a warning, so tracing never slows the traced code. The engine's payload policy
for the span's project is fetched and applied before a span is sent or spooled.
"""

from __future__ import annotations

import atexit
import contextlib
import gzip
import http.client
import itertools
import json
import os
import re
import sys
import threading
import time
import weakref
from collections import deque
from typing import Any

from vigily import _config, _engine, _spool
from vigily._http import (
    ApiError,
    EngineUnreachable,
    Http,
    RunDeletedError,
    backoff,
    error_from,
    to_exception,
)
from vigily._ids import uuid7_hex

#: Points per batch, at most.
MAX_POINTS = 20_000
#: Bytes per batch before compression, about.
MAX_BYTES = 4 * 1024 * 1024
#: Seconds a batch collects operations before it is sent.
SEAL_SECONDS = 1.0
#: Seconds ``track()`` waits for room before the waiting batches go to the spool.
BLOCK_SECONDS = 5.0
#: Estimated bytes per point in a segment (step, value, timestamp as JSON).
POINT_BYTES = 24

#: Span export settings, changed by :func:`vigily.tracing.configure`.
SPAN_SETTINGS: dict[str, Any] = {
    "batch_size": 256,
    "flush_interval": 1.0,
    "timeout": 5.0,
    "max_queue": 10_000,
}

#: The policy used for a span while the engine's is not known.
DEFAULT_POLICY = {"payloads": "truncated", "max_field_bytes": 4096}
POLICY_TTL = 300.0

_fork_gen = 0
_registry_lock = threading.Lock()
_connections: dict[tuple[Any, ...], Connection] = {}
_senders: dict[tuple[Any, ...], Sender] = {}
#: Runs of this process: id -> (experiment, connection), for routing and the payload policy of spans.
RUNS: dict[str, tuple[str | None, Connection]] = {}


def _log(msg: str) -> None:
    with contextlib.suppress(Exception):
        print(f"vigily: {msg}", file=sys.stderr, flush=True)


# ---------------------------------------------------------------------------------------------
# Connections


class Connection:
    """An engine to talk to: an address given (``url``), or a repository's local engine, found
    or started when first needed (and found again when it stopped answering)."""

    def __init__(self, url: str | None, token: str | None, repo: str | None) -> None:
        self.url = url
        self.token = token
        self.repo = repo
        self.key: tuple[Any, ...] = ("url", url, token) if url else ("repo", repo, token)
        self.target: dict[str, Any] = {"url": url, "repo": None if url else repo}
        self._http: Http | None = None
        self._lock = threading.Lock()
        self._gen = _fork_gen

    def __repr__(self) -> str:
        return f"<vigily engine {self.describe()}>"

    def describe(self) -> str:
        if self.url:
            return self.url
        h = self._http
        return f"{h.base} ({self.repo})" if h is not None else f"the engine of {self.repo}"

    def http(self, start: bool = True) -> Http:
        """The engine's client; raises :class:`vigily._engine.EngineNotFound` when there is none."""
        with self._lock:
            if self._http is not None and self._gen == _fork_gen:
                return self._http
            if self.url:
                h = Http(self.url, self.token)
            else:
                f = _engine.resolve(self.repo or _config.repo(), self.token, start=start)
                h = Http(f.url, f.token)
            self._http = h
            self._gen = _fork_gen
            return h

    def forget(self) -> None:
        """A local engine stopped answering: look it up (or start it) again next time."""
        if not self.url:
            with self._lock:
                self._http = None

    def call(self, method: str, path: str, payload: Any = None, **kw: Any) -> Any:
        """A request whose engine errors are raised as Python exceptions (``FileNotFoundError``,
        ``ValueError``, ``PermissionError``, :class:`RunDeletedError`)."""
        try:
            return self.http().call(method, path, payload, **kw)
        except ApiError as e:
            raise to_exception(e) from None
        except EngineUnreachable:
            self.forget()
            raise

    def _after_fork(self) -> None:
        self._lock = threading.Lock()
        self._http = None


def connection(
    repo: str | os.PathLike[str] | None = None, url: str | None = None, token: str | None = None
) -> Connection:
    """The connection for a repository (or an engine address), shared by the whole process."""
    url = url or _config.url()
    if url:
        url = url.rstrip("/")
    tok = token if token is not None else _config.token()
    if url:
        key: tuple[Any, ...] = ("url", url, tok)
        canonical = None
    else:
        canonical = _engine.canonical_location(os.fspath(repo) if repo is not None else _config.repo())
        key = ("repo", canonical, tok)
    with _registry_lock:
        c = _connections.get(key)
        if c is None:
            c = _connections[key] = Connection(url, tok, canonical)
        return c


def sender_for(conn: Connection) -> Sender:
    with _registry_lock:
        s = _senders.get(conn.key)
        if s is None:
            s = _senders[conn.key] = Sender(conn)
        return s


def existing_sender(conn: Connection) -> Sender | None:
    return _senders.get(conn.key)


def flush_all(timeout: float | None = None) -> bool:
    """Sends everything queued so far by every sender (or spools it); False on timeout."""
    deadline = None if timeout is None else time.monotonic() + timeout
    ok = True
    for s in list(_senders.values()):
        left = None if deadline is None else max(0.0, deadline - time.monotonic())
        ok = s.flush(left) and ok
    return ok


def flush_connection(conn: Connection, timeout: float | None = None) -> bool:
    s = existing_sender(conn)
    return True if s is None else s.flush(timeout)


# ---------------------------------------------------------------------------------------------
# The operation log


class _Segment:
    """Points of one series, in columns."""

    __slots__ = ("context", "epochs", "name", "run_id", "steps", "timestamps", "values")

    def __init__(self, run_id: str, name: str, context: Any) -> None:
        self.run_id = run_id
        self.name = name
        self.context = context
        self.steps: list[int] = []
        self.values: list[Any] = []
        self.epochs: list[int | None] = []
        self.timestamps: list[int] = []

    def encode(self) -> str:
        d: dict[str, Any] = {"op": "points", "run_id": self.run_id, "name": self.name}
        if self.context:
            d["context"] = self.context
        d["steps"] = self.steps
        d["values"] = self.values
        if self.epochs:
            d["epochs"] = self.epochs
        d["timestamps"] = self.timestamps
        return json.dumps(d, separators=(",", ":"))


class _Batch:
    __slots__ = ("bytes", "first", "gen", "ops", "points", "seq")

    def __init__(self, ops: list[tuple[str, Any]], points: int, nbytes: int, first: int, gen: int) -> None:
        self.ops = ops
        self.points = points
        self.bytes = nbytes
        # Operations are counted as they are queued: this batch holds those after `first` up to
        # and including `gen`.
        self.first = first
        self.gen = gen
        self.seq: int | None = None


class _RunState:
    __slots__ = ("deleted", "__weakref__")

    def __init__(self) -> None:
        self.deleted: str | None = None


_OP_INDEX = re.compile(r"^operation (\d+) \(")
_DELETED_RUN = re.compile(r"run (\S+) was deleted")
_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_.]{0,63}")
_ATTRIBUTE_KEY = re.compile(r"[A-Za-z_][A-Za-z0-9_.]{0,127}")


def _dumps(v: Any) -> str:
    return json.dumps(v, separators=(",", ":"), default=str)


class Sender:
    """Ships one process's operations and spans to one engine (see the module documentation)."""

    def __init__(self, conn: Connection) -> None:
        self.conn = conn
        self.writer = uuid7_hex()
        self._pid = os.getpid()
        self._cond = threading.Condition(threading.Lock())
        self._pump_lock = threading.Lock()
        self._thread: threading.Thread | None = None
        # The batch being collected.
        self._ops: list[tuple[str, Any]] = []
        self._segs: dict[str, dict[tuple[str, str], _Segment]] = {}
        self._points = 0
        self._bytes = 0
        self._opened = 0.0
        # Sealed batches, oldest first; the head is the one being sent.
        self._sealed: deque[_Batch] = deque()
        self._sealed_bytes = 0
        self._seq = 0
        self._gen_in = 0
        self._gen_sealed = 0
        self._gen_done = 0
        # Spans.
        self._spans: deque[dict[str, Any]] = deque()
        self._span_in = 0
        self._span_done = 0
        self._span_since = 0.0
        self.spans_dropped = 0
        # Flow control.
        self._flush_req = 0
        self._exiting = False
        self._spill = False
        self._fail_since: float | None = None
        self._attempt = 0
        self._next_try = 0.0
        self._spooling = False
        self._spool: _spool.Spool | None = None
        self._spool_bytes: int | None = None
        self._adopted = False
        self._warned: set[str] = set()
        self._states: weakref.WeakValueDictionary[str, _RunState] = weakref.WeakValueDictionary()
        self._deleted: dict[str, str] = {}
        self._policies: dict[str | None, tuple[float, dict[str, Any]]] = {}
        # What this process could not send (reported at exit).
        self.spooled_batches = 0
        self.spooled_points = 0
        self.spooled_spans = 0
        self.lost_points = 0
        self.lost_spans = 0

    # --- producers ---------------------------------------------------------------------------

    def register(self, run_id: str, state: _RunState) -> None:
        self._states[run_id] = state

    def add(self, run_id: str, op: dict[str, Any] | str) -> None:
        """Appends a non-point operation; it ends the run's open segments, so later points of
        the run follow it."""
        s = op if isinstance(op, str) else _dumps(op)
        with self._cond:
            self._segs.pop(run_id, None)
            self._ops.append((run_id, s))
            self._bytes += len(s)
            self._gen_in += 1
            if not self._opened:
                self._opened = time.monotonic()
            if self._bytes >= MAX_BYTES:
                self._seal_locked(True)
            self._ensure_thread_locked()

    def track(
        self,
        run_id: str,
        name: str,
        ctx_key: str,
        context: Any,
        value: Any,
        step: int,
        epoch: int | None,
        timestamp: int,
    ) -> None:
        with self._cond:
            runsegs = self._segs.get(run_id)
            if runsegs is None:
                runsegs = self._segs[run_id] = {}
            seg = runsegs.get((name, ctx_key))
            if seg is None:
                seg = runsegs[(name, ctx_key)] = _Segment(run_id, name, context)
                self._ops.append((run_id, seg))
                self._bytes += 80 + len(name) + len(ctx_key)
                if not self._opened:
                    self._opened = time.monotonic()
                self._ensure_thread_locked()
            if epoch is not None:
                n = len(seg.steps)
                if len(seg.epochs) < n:
                    seg.epochs.extend([None] * (n - len(seg.epochs)))
                seg.epochs.append(epoch)
            elif seg.epochs:
                seg.epochs.append(None)
            seg.steps.append(step)
            seg.values.append(value)
            seg.timestamps.append(timestamp)
            self._points += 1
            self._bytes += POINT_BYTES
            self._gen_in += 1
            if self._points >= MAX_POINTS or self._bytes >= MAX_BYTES:
                self._seal_locked(True)

    def put_span(self, span: dict[str, Any]) -> None:
        with self._cond:
            if len(self._spans) >= SPAN_SETTINGS["max_queue"]:
                self.spans_dropped += 1
                self._warn_once("span-queue", f"the span queue is full; {self.spans_dropped} spans dropped")
                return
            if not self._spans:
                self._span_since = time.monotonic()
            self._spans.append(span)
            self._span_in += 1
            if len(self._spans) >= SPAN_SETTINGS["batch_size"]:
                self._cond.notify_all()
            self._ensure_thread_locked()

    def deleted(self, run_id: str) -> str | None:
        return self._deleted.get(run_id)

    def flush(self, timeout: float | None = None) -> bool:
        """Returns once everything queued before the call was sent (or spooled); False when that
        took longer than ``timeout`` seconds."""
        deadline = None if timeout is None else time.monotonic() + timeout
        with self._cond:
            gen, spans = self._gen_in, self._span_in
            if self._gen_done >= gen and self._span_done >= spans:
                return True
            self._flush_req += 1
            self._ensure_thread_locked()
            self._cond.notify_all()
        try:
            while True:
                with self._cond:
                    if self._gen_done >= gen and self._span_done >= spans:
                        return True
                    left = None if deadline is None else deadline - time.monotonic()
                    if left is not None and left <= 0:
                        return False
                    alive = self._thread is not None and self._thread.is_alive()
                    if alive:
                        self._cond.wait(1.0 if left is None else min(1.0, left))
                        continue
                # No thread (interpreter shutdown, or none could be started): send from here.
                self._pump_inline(deadline)
        finally:
            with self._cond:
                self._flush_req -= 1

    # --- the thread ----------------------------------------------------------------------------

    def _ensure_thread_locked(self) -> None:
        if self._thread is not None and self._thread.is_alive():
            return
        if self._pid != os.getpid():
            return
        t = threading.Thread(target=self._run, name="vigily-sender", daemon=True)
        try:
            t.start()
        except RuntimeError:  # interpreter shutdown: flush() sends from the calling thread
            return
        self._thread = t

    def _seal_locked(self, producer: bool = False) -> None:
        if not self._ops:
            return
        b = _Batch(self._ops, self._points, self._bytes, self._gen_sealed, self._gen_in)
        self._gen_sealed = self._gen_in
        self._ops = []
        self._segs = {}
        self._points = 0
        self._bytes = 0
        self._opened = 0.0
        self._sealed.append(b)
        self._sealed_bytes += b.bytes
        self._cond.notify_all()
        if producer:
            self._wait_for_room_locked(b)

    def _wait_for_room_locked(self, newest: _Batch) -> None:
        """Backpressure: the producer waits while too much is in memory; after a while the
        waiting batches go to the spool."""
        limit = _config.buffer_max_bytes()
        if self._sealed_bytes <= limit or threading.current_thread() is self._thread:
            return
        if self._thread is None or not self._thread.is_alive():
            return
        deadline = time.monotonic() + BLOCK_SECONDS
        spilled = False
        while self._sealed_bytes > limit:
            left = deadline - time.monotonic()
            if left > 0:
                self._cond.wait(left)
                continue
            if not spilled:
                spilled = True
                self._spill = True
                self._cond.notify_all()
                deadline = time.monotonic() + BLOCK_SECONDS
                continue
            # The spool cannot take them either: say so, loudly, and let training go on.
            if len(self._sealed) > 1 and self._sealed[-1] is newest:
                self._sealed.pop()
                self._sealed_bytes -= newest.bytes
                self.lost_points += newest.points
                _log(
                    f"dropped {newest.points} points: the engine at {self.conn.describe()} does not "
                    f"keep up and the spool is full or unavailable ({self.lost_points} points lost so far)"
                )
            return

    def _spans_due_locked(self, now: float) -> bool:
        return bool(self._spans) and (
            len(self._spans) >= SPAN_SETTINGS["batch_size"]
            or self._flush_req > 0
            or self._exiting
            or now - self._span_since >= SPAN_SETTINGS["flush_interval"]
        )

    def _work_locked(self, now: float) -> bool:
        if self._spill:
            return True
        due = bool(self._sealed) or self._spans_due_locked(now)
        if due and (self._spooling or _config.offline()):
            return True  # to the spool, without waiting
        if now < self._next_try and not self._exiting:
            return False
        return due or self._spooling

    def _wake_in_locked(self, now: float) -> float:
        t = 1.0
        if self._ops:
            t = min(t, self._opened + SEAL_SECONDS - now)
        if self._spans:
            t = min(t, self._span_since + SPAN_SETTINGS["flush_interval"] - now)
        if self._next_try > now:
            t = max(t, min(self._next_try - now, 1.0))
        return max(0.005, t)

    def _run(self) -> None:
        while True:
            with self._cond:
                while True:
                    if self._pid != os.getpid():
                        return
                    now = time.monotonic()
                    if self._ops and (self._flush_req or self._exiting or now - self._opened >= SEAL_SECONDS):
                        self._seal_locked()
                    if self._work_locked(now):
                        break
                    self._cond.wait(self._wake_in_locked(now))
            with self._pump_lock:
                try:
                    self._pump()
                except Exception as e:  # noqa: BLE001 - the thread must survive any surprise
                    self._warn_once(f"internal-{type(e).__name__}", f"sender error: {type(e).__name__}: {e}")
                    time.sleep(0.5)

    def _pump_inline(self, deadline: float | None) -> None:
        if not self._pump_lock.acquire(timeout=0.5):
            return
        try:
            with self._cond:
                self._seal_locked()
                now = time.monotonic()
                work = self._work_locked(now)
            if work:
                self._pump()
            else:
                wait = max(0.0, self._next_try - time.monotonic())
                if deadline is not None:
                    wait = min(wait, max(0.0, deadline - time.monotonic()))
                time.sleep(min(wait, 1.0) or 0.01)
        finally:
            self._pump_lock.release()

    # --- sending -------------------------------------------------------------------------------

    def _pump(self) -> None:
        if _config.offline():
            if not self._spooling:
                self._spooling = True
                self._fail_since = time.monotonic()
            self._spool_ready("offline")
            self._next_try = time.monotonic() + 5.0
            return
        if self._spill:
            self._spill = False
            self._enter_spooling("batches arrive faster than the engine takes them", quiet=True)
            return
        if self._spooling:
            self._spool_ready(None)
            if self._exiting and self._fail_since is not None:
                return  # the engine was not reachable: no more waiting at exit
            if time.monotonic() < self._next_try and not self._exiting:
                return
            if not self._replay_own():
                return
        with self._cond:
            head = self._sealed[0] if self._sealed else None
        if head is not None:
            self._send_batch(head)
        with self._cond:
            now = time.monotonic()
            spans_due = self._spans_due_locked(now) and now >= self._next_try
        if spans_due and not self._spooling:
            self._send_spans()

    def _assign_seq_locked(self, b: _Batch) -> int:
        if b.seq is None:
            self._seq += 1
            b.seq = self._seq
        return b.seq

    def _encode(self, b: _Batch) -> tuple[bytes, list[int]]:
        with self._cond:
            seq = self._assign_seq_locked(b)
        parts = []
        index = []
        for i, (rid, op) in enumerate(b.ops):
            if rid in self._deleted:
                continue
            parts.append(op if isinstance(op, str) else op.encode())
            index.append(i)
        head = f'{{"writer":"{self.writer}","seq":{seq},"ops":['
        return (head + ",".join(parts) + "]}").encode(), index

    def _done_batch(self, b: _Batch) -> None:
        with self._cond:
            if self._sealed and self._sealed[0] is b:
                self._sealed.popleft()
                self._sealed_bytes -= b.bytes
            elif b in self._sealed:
                self._sealed.remove(b)
                self._sealed_bytes -= b.bytes
            else:
                return
            self._gen_done = self._sealed[0].first if self._sealed else self._gen_sealed
            self._cond.notify_all()

    def _ok(self) -> None:
        if self._fail_since is not None and self._attempt > 1:
            _log(f"reached {self.conn.describe()} again")
        self._fail_since = None
        self._attempt = 0
        self._next_try = 0.0
        self._warned.discard("unreachable")
        if not self._adopted:
            self._adopted = True
            self._adopt_orphans()

    def _failed(self, why: str, retry_after: float | None = None, final: bool = False) -> None:
        now = time.monotonic()
        if self._fail_since is None:
            self._fail_since = now
        self._attempt += 1
        self.conn.forget()
        if final or self._exiting or now - self._fail_since >= _config.spool_after():
            self._enter_spooling(why)
            return
        if self._attempt == 3:
            self._warn_once("unreachable", f"cannot reach {self.conn.describe()} ({why}); retrying")
        self._next_try = now + (retry_after if retry_after is not None else backoff(self._attempt))

    def _enter_spooling(self, why: str, quiet: bool = False) -> None:
        first = not self._spooling
        self._spooling = True
        if self._spool_ready(why) and first and not quiet and not self._exiting:
            _log(
                f"cannot reach {self.conn.describe()} ({why}); batches go to the spool in "
                f"{self._spool.dir if self._spool else 'the state directory'} and are sent once it "
                "answers again (or with `vigily sync`)"
            )
        self._next_try = time.monotonic() + backoff(max(self._attempt, 3), 1.0, 30.0)

    def _spool_file(self, kind: str, lines: list[bytes]) -> bool:
        if self._spool is None:
            try:
                self._spool = _spool.Spool(self.writer, self.conn.target)
            except OSError as e:
                self._warn_once("spool", f"cannot spool: {e}")
                return False
        if self._spool_bytes is None:
            self._spool_bytes = _spool.total_bytes()
        size = sum(len(x) for x in lines) // 4
        if self._spool_bytes + size > _config.spool_max_bytes():
            self._warn_once(
                "spool-full",
                f"the spool is full ({_config.spool_max_bytes() // (1024 * 1024)} MiB, VIGILY_SPOOL_MAX_MB); "
                "batches stay in memory until the engine answers",
            )
            return False
        try:
            self._spool_bytes += self._spool.write(kind, lines)
        except OSError as e:
            self._warn_once("spool", f"cannot write to the spool: {e}")
            return False
        return True

    def _spool_ready(self, why: str | None) -> bool:
        """Moves every sealed batch and queued span to the spool, in order."""
        while True:
            with self._cond:
                b = self._sealed[0] if self._sealed else None
            if b is None:
                break
            body, _ = self._encode(b)
            if not self._spool_file("b", [body]):
                return False
            self.spooled_batches += 1
            self.spooled_points += b.points
            self._done_batch(b)
        while True:
            with self._cond:
                spans = list(itertools.islice(self._spans, SPAN_SETTINGS["batch_size"]))
            if not spans:
                break
            body = _dumps({"spans": [self._apply_policy(s) for s in spans]}).encode()
            if not self._spool_file("s", [body]):
                return False
            self.spooled_spans += len(spans)
            self._done_spans(len(spans))
        return True

    def _post_batch(self, h: Http, body: bytes, key: str) -> tuple[int, dict[str, str], bytes]:
        return h.request(
            "POST",
            "/api/v1/ingest",
            body=gzip.compress(body, compresslevel=3),
            content_type="application/json",
            gzipped=True,
            headers={"idempotency-key": key},
            timeout=60.0,
        )

    def _http(self) -> Http | None:
        try:
            return self.conn.http()
        except _engine.EngineNotFound as e:
            self._warn_once("engine", str(e))
            self._failed("no engine", final=True)
        except (OSError, ValueError) as e:
            self._failed(str(e))
        return None

    def _send_batch(self, b: _Batch) -> None:
        h = self._http()
        if h is None:
            return
        body, index = self._encode(b)
        try:
            status, headers, data = self._post_batch(h, body, f"{self.writer}:{b.seq}")
        except EngineUnreachable as e:
            self._failed(str(e))
            return
        if status < 400:
            self._done_batch(b)
            self._ok()
            return
        err = error_from(status, headers, data)
        if self._handle_refusal(err, b.ops, index):
            return
        if err.retryable or status in (401, 403) or status >= 500:
            self._auth_hint(status)
            self._failed(f"HTTP {status}: {err.message}", err.retry_after)
            return
        self.lost_points += b.points
        _log(f"the engine refused a batch of {b.points} points ({err.message}); it was not stored")
        self._done_batch(b)

    def _handle_refusal(self, err: ApiError, ops: list[Any], index: list[int]) -> bool:
        """A deleted run or a refused operation: drop what cannot be stored, keep the rest for
        the next attempt. True when the batch should be sent again."""
        m = _OP_INDEX.match(err.message)
        n = int(m.group(1)) - 1 if m else None
        if err.code == "run_deleted" or err.status == 410:
            d = _DELETED_RUN.search(err.message)
            rid = d.group(1) if d else None
            if rid is None and n is not None and 0 <= n < len(index):
                rid = ops[index[n]][0]
            if rid is not None and rid not in self._deleted:
                msg = (
                    f"run {rid} was deleted (in the web app or by another process); "
                    "nothing more is recorded for it"
                )
                self._deleted[rid] = msg
                st = self._states.get(rid)
                if st is not None:
                    st.deleted = msg
                return True
        if err.status in (400, 409, 410, 413, 422) and n is not None and 0 <= n < len(index):
            i = index[n]
            rid, op = ops[i]
            what = "points" if isinstance(op, _Segment) else _op_kind(op)
            _log(f"the engine refused a {what} operation of run {rid} ({err.message}); it was not stored")
            if isinstance(op, _Segment):
                self.lost_points += len(op.steps)
            del ops[i]
            return True
        return False

    def _auth_hint(self, status: int) -> None:
        if status in (401, 403):
            self._warn_once(
                "auth",
                f"{self.conn.describe()} refused the token (HTTP {status}); set VIGILY_TOKEN to a token "
                "with the read and ingest scopes",
            )

    # --- spans ---------------------------------------------------------------------------------

    def _done_spans(self, n: int) -> None:
        with self._cond:
            for _ in range(min(n, len(self._spans))):
                self._spans.popleft()
            self._span_done += n
            self._span_since = time.monotonic()
            self._cond.notify_all()

    def _send_spans(self) -> None:
        with self._cond:
            spans = list(itertools.islice(self._spans, SPAN_SETTINGS["batch_size"]))
        if not spans:
            return
        h = self._http()
        if h is None:
            return
        body = _dumps({"spans": [self._apply_policy(s) for s in spans]}).encode()
        try:
            status, headers, _ = h.request(
                "POST",
                "/api/v1/traces/ingest",
                body=body,
                content_type="application/json",
                timeout=float(SPAN_SETTINGS["timeout"]),
            )
        except EngineUnreachable as e:
            self._failed(str(e))
            return
        if status < 400:
            self._done_spans(len(spans))
            self._ok()
            return
        if status in (408, 429) or status >= 500:
            self._failed(f"HTTP {status}", error_from(status, headers, b"").retry_after)
            return
        # A client error does not go away on retry. The response body is not shown: it could
        # echo span content.
        reason = http.client.responses.get(status, "")
        hint = " (set VIGILY_TOKEN to the server's token)" if status in (401, 403) else ""
        _log_tracing(f"the server refused {len(spans)} spans (HTTP {status} {reason}){hint}")
        self.lost_spans += len(spans)
        self._done_spans(len(spans))

    def _policy_for(self, project: str | None) -> dict[str, Any]:
        now = time.monotonic()
        hit = self._policies.get(project)
        if hit is not None and hit[0] > now:
            return hit[1]
        doc: dict[str, Any] | None = None
        h = None
        with contextlib.suppress(Exception):
            h = self.conn.http(start=False) if not self.conn.url else self.conn.http()
        if h is not None:
            try:
                status, _, data = h.request(
                    "GET",
                    "/api/v1/tracing/policy",
                    query={"project": project} if project else None,
                    timeout=2.0,
                )
                if status == 200:
                    doc = json.loads(data)
            except (EngineUnreachable, ValueError):
                doc = None
        if not isinstance(doc, dict) or "payloads" not in doc:
            fallback = hit[1] if hit is not None else {**DEFAULT_POLICY, "unknown_project": DEFAULT_POLICY}
            self._policies[project] = (now + 30.0, fallback)
            return fallback
        self._policies[project] = (now + POLICY_TTL, doc)
        return doc

    def _apply_policy(self, span: dict[str, Any]) -> dict[str, Any]:
        """The span as the engine's payload policy lets it leave the process."""
        project = span.get("project")
        unknown = False
        if not project and span.get("run_id"):
            known = RUNS.get(span["run_id"])
            if known is not None:
                project = known[0]
            else:
                unknown = True
        doc = self._policy_for(project)
        policy = doc.get("unknown_project", doc) if unknown else doc
        mode = str(policy.get("payloads", "truncated"))
        return apply_policy(span, mode, int(policy.get("max_field_bytes", 4096)))

    # --- the spool -----------------------------------------------------------------------------

    def _replay_own(self) -> bool:
        """Sends this writer's spool in order; True once it is empty."""
        if self._spool is None:
            self._spooling = False
            return True
        sent = self._replay_files(self._spool.pending())
        if sent is None:
            return False
        if sent:
            _log(f"sent {sent} spooled files to {self.conn.describe()}")
        self._spooling = False
        return True

    def _replay_files(self, files: list[tuple[int, str, str]]) -> int | None:
        """Sends spool files in order and removes each once accepted; None when the engine could
        not be reached (the rest stay)."""
        if not files:
            return 0
        h = self._http()
        if h is None:
            return None
        n = 0
        for _, kind, path in files:
            try:
                lines = _spool.read(path)
            except (OSError, EOFError) as e:
                self._warn_once(f"spool-read-{path}", f"cannot read {path}: {e}; left in place")
                continue
            for line in lines:
                if not self._replay_line(h, kind, line):
                    return None
            with contextlib.suppress(OSError):
                os.unlink(path)
            n += 1
        self._ok()
        return n

    def _replay_line(self, h: Http, kind: str, line: bytes) -> bool:
        if kind == "s":
            try:
                status, headers, _ = h.request(
                    "POST", "/api/v1/traces/ingest", body=line, content_type="application/json", timeout=60.0
                )
            except EngineUnreachable as e:
                self._failed(str(e))
                return False
            if status < 400 or (status < 500 and status not in (401, 403, 408, 429)):
                if status >= 400:
                    _log_tracing(f"the server refused spooled spans (HTTP {status})")
                return True
            self._failed(f"HTTP {status}", error_from(status, headers, b"").retry_after)
            return False
        doc = json.loads(line)
        ops = [(_op_run(op), _dumps(op)) for op in doc.get("ops", [])]
        while True:
            index = [i for i, (rid, _) in enumerate(ops) if rid not in self._deleted]
            body = (
                f'{{"writer":{json.dumps(doc["writer"])},"seq":{int(doc["seq"])},"ops":['
                + ",".join(ops[i][1] for i in index)
                + "]}"
            ).encode()
            try:
                status, headers, data = self._post_batch(h, body, f"{doc['writer']}:{doc['seq']}")
            except EngineUnreachable as e:
                self._failed(str(e))
                return False
            if status < 400:
                return True
            err = error_from(status, headers, data)
            if self._handle_refusal(err, ops, index):
                continue
            if err.retryable or status in (401, 403) or status >= 500:
                self._auth_hint(status)
                self._failed(f"HTTP {status}: {err.message}", err.retry_after)
                return False
            _log(f"the engine refused a spooled batch ({err.message}); it was not stored")
            return True

    def _adopt_orphans(self) -> None:
        """Spool directories of earlier processes for this engine: sent now, in order."""
        own = self._spool.dir if self._spool is not None else None
        for d in _spool.orphans(self.conn.target):
            if d == own:
                continue
            fd = _spool.lock(d, wait=False)
            if fd is None:
                continue
            try:
                sent = self._replay_files(list(_spool.files(d)))
                if sent is None:
                    return
                if sent:
                    _log(f"sent {sent} files left in the spool by an earlier process ({d})")
                if not any(True for _ in _spool.files(d)):
                    _spool.remove_dir(d)
            finally:
                os.close(fd)

    # --- exit and fork -------------------------------------------------------------------------

    def begin_exit(self) -> None:
        with self._cond:
            self._exiting = True
            self._next_try = 0.0
            self._cond.notify_all()

    def finish_exit(self, deadline: float) -> None:
        """Sends what is left until ``deadline``; then spools the rest and says so."""
        if self._pid != os.getpid():
            return
        done = self.flush(max(0.0, deadline - time.monotonic()))
        if not done:
            with self._cond:
                self._seal_locked()
                left = list(self._sealed)
            for b in left:
                body, _ = self._encode(b)
                if self._spool_file("b", [body]):
                    self.spooled_batches += 1
                    self.spooled_points += b.points
                else:
                    self.lost_points += b.points
                self._done_batch(b)
            with self._cond:
                spans = list(self._spans)
            if spans:
                body = _dumps({"spans": [self._apply_policy(s) for s in spans]}).encode()
                if self._spool_file("s", [body]):
                    self.spooled_spans += len(spans)
                else:
                    self.lost_spans += len(spans)
                self._done_spans(len(spans))
        if self._spool is not None and self._spool.pending():
            what = []
            if self.spooled_batches:
                what.append(f"{self.spooled_batches} batches ({self.spooled_points} points)")
            if self.spooled_spans:
                what.append(f"{self.spooled_spans} spans")
            _log(
                f"could not send {' and '.join(what) or 'everything'} to {self.conn.describe()}; they are "
                f"in the spool at {self._spool.dir} and are sent by the next process that reaches it, "
                "or with `vigily sync`"
            )
        lost = [f"{self.lost_points} points"] if self.lost_points else []
        lost += [f"{self.lost_spans} spans"] if self.lost_spans else []
        if lost:
            _log(f"{' and '.join(lost)} sent to {self.conn.describe()} could not be stored or spooled")
        if self._spool is not None:
            with contextlib.suppress(OSError):
                self._spool.close()

    def _after_fork(self) -> None:
        if self._spool is not None:
            self._spool.forget_lock()

    def _warn_once(self, key: str, msg: str) -> None:
        if key not in self._warned:
            self._warned.add(key)
            _log(msg)


def _log_tracing(msg: str) -> None:
    with contextlib.suppress(Exception):
        print(f"vigily tracing: {msg}", file=sys.stderr, flush=True)


def _op_kind(op: str) -> str:
    m = re.match(r'\{"op":"(\w+)"', op)
    return m.group(1) if m else "an"


def _op_run(op: dict[str, Any]) -> str:
    if "run_id" in op:
        return str(op["run_id"])
    if op.get("op") == "create":
        return str(op.get("id"))
    for k in ("results", "samples"):
        rows = op.get(k)
        if rows:
            return str(rows[0].get("run_id"))
    return ""


# ---------------------------------------------------------------------------------------------
# Payload policy


def _cut(s: str, limit: int) -> str:
    b = s.encode()
    if len(b) <= limit:
        return s
    return b[:limit].decode("utf-8", "ignore")


def _cut_value(v: Any, limit: int) -> Any:
    if isinstance(v, str):
        return _cut(v, limit)
    if v is None or isinstance(v, (bool, int, float)):
        return v
    text = json.dumps(v, separators=(",", ":"), ensure_ascii=False, default=str)
    return _cut(text, limit) if len(text.encode()) > limit else v


def error_type(e: str) -> str:
    head = str(e).split(":", 1)[0].strip()
    return head if _IDENTIFIER.fullmatch(head) else "error"


def apply_policy(span: dict[str, Any], mode: str, limit: int) -> dict[str, Any]:
    """Applies a payload mode the way the engine does, so what it would not store never leaves
    the process: ``full`` keeps everything, ``truncated`` cuts each field to ``limit`` bytes,
    ``metadata`` keeps no inputs, outputs or free text."""
    if mode == "full":
        return span
    out = dict(span)
    attrs = out.get("attributes") or {}
    if mode == "metadata":
        out.pop("input", None)
        out.pop("output", None)
        if out.get("error"):
            out["error"] = error_type(out["error"])
        kept = {
            k: v
            for k, v in attrs.items()
            if isinstance(v, (bool, int, float)) and _ATTRIBUTE_KEY.fullmatch(str(k))
        }
        if kept:
            out["attributes"] = kept
        else:
            out.pop("attributes", None)
        return out
    for k in ("input", "output"):
        if k in out:
            out[k] = _cut_value(out[k], limit)
    if out.get("error"):
        out["error"] = _cut(str(out["error"]), limit)
    if attrs:
        out["attributes"] = {k: _cut_value(v, limit) for k, v in attrs.items()}
    return out


# ---------------------------------------------------------------------------------------------
# Runs


class RunHandle:
    """The writing side of one run: operations go to the sender of the run's engine."""

    __slots__ = ("_create", "_gen", "_next", "_sender", "conn", "id", "state")

    def __init__(self, conn: Connection, run_id: str, create: dict[str, Any] | None) -> None:
        self.conn = conn
        self.id = run_id
        self.state = _RunState()
        self._create = None if create is None else _dumps(create)
        self._next: dict[tuple[str, str], int] = {}
        self._gen = _fork_gen
        self._sender = sender_for(conn)
        self._sender.register(run_id, self.state)
        if self._create is not None:
            self._sender.add(run_id, self._create)

    def sender(self) -> Sender:
        if self._gen != _fork_gen:
            # A forked child: its own sender and writer. The run's create goes first, in case the
            # parent's had not reached the engine yet (a create of an existing run changes nothing).
            self._gen = _fork_gen
            self._sender = sender_for(self.conn)
            self._sender.register(self.id, self.state)
            if self._create is not None:
                self._sender.add(self.id, self._create)
        return self._sender

    def check(self) -> None:
        if self.state.deleted:
            raise RunDeletedError(self.state.deleted)

    def continue_steps(self, series: list[dict[str, Any]]) -> None:
        for s in series:
            if s.get("last_step") is not None:
                ctx = s.get("context") or {}
                key = json.dumps(ctx, sort_keys=True) if ctx else ""
                self._next[(s["name"], key)] = int(s["last_step"]) + 1

    def track(
        self,
        name: str,
        value: float,
        step: int | None,
        epoch: int | None,
        ctx_key: str,
        context: Any,
        timestamp: int | None,
    ) -> None:
        if self.state.deleted:
            raise RunDeletedError(self.state.deleted)
        key = (name, ctx_key)
        n = self._next.get(key, 0)
        if step is None:
            step = n
        if step >= n:
            self._next[key] = step + 1
        v: Any = float(value)
        if v - v != 0.0:  # NaN or an infinity: JSON has none, the wire spells them out
            v = "NaN" if v != v else ("Infinity" if v > 0 else "-Infinity")
        ts = time.time_ns() // 1000 if timestamp is None else timestamp
        self.sender().track(self.id, name, ctx_key, context, v, int(step), epoch, ts)

    def op(self, op: dict[str, Any]) -> None:
        self.check()
        self.sender().add(self.id, op)

    def heartbeat(self) -> None:
        self.op({"op": "heartbeat", "run_id": self.id})

    def put_results(self, results: list[dict[str, Any]]) -> None:
        self.op({"op": "results", "results": results})

    def put_features(self, samples: list[dict[str, Any]]) -> None:
        self.op({"op": "features", "samples": samples})

    def flush(self, timeout: float | None = None) -> bool:
        ok = self.sender().flush(timeout)
        self.check()
        return ok


# ---------------------------------------------------------------------------------------------
# Process exit and fork


def _exit_all() -> None:
    deadline = time.monotonic() + _config.exit_timeout()
    senders = [s for s in list(_senders.values()) if s._pid == os.getpid()]
    for s in senders:
        with contextlib.suppress(Exception):
            s.begin_exit()
    for s in senders:
        with contextlib.suppress(Exception):
            s.finish_exit(deadline)


atexit.register(_exit_all)


def _after_fork_in_child() -> None:
    """A forked child starts with no senders: the parent's threads do not exist in it, and what
    the parent queued is the parent's to send. Connections are kept (their locks and sockets are
    renewed); runs the child logs to get a sender of their own on first use."""
    global _fork_gen, _registry_lock, _senders
    _fork_gen += 1
    _registry_lock = threading.Lock()
    for s in list(_senders.values()):
        with contextlib.suppress(Exception):
            s._after_fork()
    _senders = {}
    for c in list(_connections.values()):
        c._after_fork()


class _Anchor:
    """What multiprocessing's after-fork registry holds on to (it keeps weak references)."""


_mp_anchor = _Anchor()
_mp_registered = False


def _before_fork() -> None:
    # multiprocessing ends its children with os._exit, which skips atexit; its finalizers run
    # instead. A child clears the finalizers it inherited, then calls the after-fork hooks: one of
    # them registers the exit flush as a finalizer of the child.
    global _mp_registered
    mp = sys.modules.get("multiprocessing.util")
    if mp is not None and not _mp_registered:
        _mp_registered = True
        with contextlib.suppress(Exception):
            mp.register_after_fork(_mp_anchor, _flush_at_mp_exit)


def _flush_at_mp_exit(_: Any) -> None:
    mp = sys.modules.get("multiprocessing.util")
    if mp is not None:
        mp.Finalize(None, _exit_all, exitpriority=0)


if hasattr(os, "register_at_fork"):
    os.register_at_fork(before=_before_fork, after_in_child=_after_fork_in_child)
