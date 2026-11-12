"""vigily: log runs, parameters and metrics from training code, then compare them in the web app.

.. code-block:: python

    from vigily import Run

    run = Run(experiment="mnist")
    run["hparams"] = {"lr": 3e-4, "batch_size": 64}
    for step in range(1000):
        run.track(loss, name="loss", step=step, context={"subset": "train"})
    run.finish()

Runs go to the engine that serves the repository: ``./.vigily`` unless ``repo=`` or
``VIGILY_REPO`` names another directory or an object-store URI (``s3://bucket/prefix``). The
engine on this machine is found, or started in the background (``pip install 'vigily[local]'``
installs it); ``VIGILY_URL`` (or :func:`configure`) names an engine elsewhere, such as a team
server, with ``VIGILY_TOKEN``. Logging never waits for the network: a background thread sends
batches, and keeps them in a spool on disk while the engine cannot be reached.
"""

from __future__ import annotations

import atexit
import contextlib
import json
import math
import os
import platform
import subprocess
import sys
import threading
import time
import urllib.parse
import weakref
from collections.abc import Iterable, Mapping
from datetime import datetime
from typing import Any

from vigily import _config, _sender
from vigily._config import DEFAULT_REPO
from vigily._http import RunDeletedError
from vigily._ids import uuid7_hex

__all__ = [
    "DEFAULT_REPO",
    "HEARTBEAT_SECONDS",
    "Dataset",
    "EvalResult",
    "Item",
    "Repo",
    "Run",
    "RunDeletedError",
    "__version__",
    "configure",
    "datasets",
    "evaluate",
    "evaluators",
    "flush",
    "log_features",
    "trace",
    "tracing",
]


def _version() -> str:
    try:
        from importlib.metadata import PackageNotFoundError, version

        return version("vigily")
    except (ImportError, PackageNotFoundError):  # pragma: no cover - running from a source tree
        return "0.0.0+unknown"


__version__: str = _version()

_SYSTEM_KEY = "__system__"
_RUN_KEY = "__run__"
_EVAL_KEY = "__eval__"
_RESERVED = (_SYSTEM_KEY, _RUN_KEY, _EVAL_KEY)


def configure(
    url: str | None = None,
    token: str | None = None,
    project: str | None = None,
    offline: bool | None = None,
) -> None:
    """Sets where runs and spans go. Arguments left out keep their current value.

    ``url`` is an engine's address, such as a team server (``VIGILY_URL``); without one, the
    repository's local engine is found or started. ``token`` is sent with every request
    (``VIGILY_TOKEN``; a local engine's own token otherwise) and needs the ``read`` and ``ingest``
    scopes. ``project`` is the default project of traces, whose payload policy applies
    (``VIGILY_PROJECT``; the run's experiment otherwise). ``offline=True`` (``VIGILY_OFFLINE=1``)
    never contacts an engine: batches go to the spool, for ``vigily sync`` to send later.

    Runs already started keep the engine they started with.
    """
    _config.configure(url=url, token=token, project=project, offline=offline)


def flush(timeout: float | None = None) -> bool:
    """Sends everything logged so far (runs, metrics, results, spans) and waits until the engine
    has it, or it went to the spool. Returns False when that took longer than ``timeout`` seconds."""
    return _sender.flush_all(timeout)


def _micros(t: datetime | float | int | None) -> int | None:
    """A datetime or Unix seconds as microseconds since the epoch."""
    if t is None:
        return None
    if isinstance(t, datetime):
        return int(t.timestamp() * 1_000_000)
    return int(float(t) * 1_000_000)


def _jsonable(v: Any) -> Any:
    """Converts numpy and torch scalars, tuples and sets into plain JSON values."""
    if isinstance(v, Mapping):
        return {str(k): _jsonable(x) for k, x in v.items()}
    if isinstance(v, (list, tuple, set, frozenset)):
        return [_jsonable(x) for x in v]
    if v is None or isinstance(v, (bool, int, str)):
        return v
    if isinstance(v, float):
        return v if math.isfinite(v) else str(v)
    item = getattr(v, "item", None)
    if callable(item):
        try:
            return _jsonable(item())
        except (TypeError, ValueError):
            pass
    tolist = getattr(v, "tolist", None)
    if callable(tolist):
        return _jsonable(tolist())
    return str(v)


def _system_info() -> dict[str, Any]:
    """The environment of the training process; no user names, host names or paths."""
    info: dict[str, Any] = {
        "python": platform.python_version(),
        "platform": platform.platform(terse=True),
        "machine": platform.machine(),
        "cpu_count": os.cpu_count(),
        "vigily": __version__,
    }
    git = _git_info()
    if git:
        info["git"] = git
    return info


def _git_info(cwd: str | None = None) -> dict[str, str] | None:
    """The checkout's branch and commit hash, or None outside a git repository.

    ``--abbrev-ref`` applies to every revision after it, so the commit comes first: the hash, then
    the branch name (``HEAD`` when detached).
    """
    try:
        out = subprocess.run(
            ["git", "rev-parse", "HEAD", "--abbrev-ref", "HEAD"],
            capture_output=True,
            text=True,
            timeout=2,
            check=True,
            cwd=cwd,
        ).stdout.split()
    except (OSError, subprocess.SubprocessError):
        return None
    if len(out) != 2:
        return None
    return {"branch": out[1], "commit": out[0]}


def _schedule_from_env() -> dict[str, Any] | None:
    """When a vigily schedule started this process, the schedule it belongs to."""
    name = os.environ.get("VIGILY_SCHEDULE")
    if not name:
        return None
    out: dict[str, Any] = {"name": name}
    if os.environ.get("VIGILY_SCHEDULE_RUN"):
        out["run"] = os.environ["VIGILY_SCHEDULE_RUN"]
    if os.environ.get("VIGILY_SCHEDULED_FOR"):
        out["scheduled_for"] = os.environ["VIGILY_SCHEDULED_FOR"]
    return out


_open_runs: weakref.WeakSet[Run] = weakref.WeakSet()
_failed = False


def _excepthook(tp: type[BaseException], value: BaseException, tb: Any) -> None:
    global _failed
    _failed = True
    _previous_hook(tp, value, tb)


_previous_hook = sys.excepthook
sys.excepthook = _excepthook


#: Seconds between heartbeats of open runs. The engine shows a running run without a heartbeat,
#: metadata write or point for 10 minutes as stale (its process most likely died). Each heartbeat
#: is one small storage write, so it is kept rare.
HEARTBEAT_SECONDS = 120.0
_heartbeat_started = threading.Event()


def _heartbeat_loop() -> None:
    while True:
        time.sleep(HEARTBEAT_SECONDS)
        for run in list(_open_runs):
            if run._pid != os.getpid():
                continue
            # A failed heartbeat (run deleted) is not the training script's problem.
            with contextlib.suppress(Exception):
                run._h.heartbeat()


def _start_heartbeats() -> None:
    if not _heartbeat_started.is_set():
        _heartbeat_started.set()
        threading.Thread(target=_heartbeat_loop, name="vigily-heartbeat", daemon=True).start()


def _after_fork_in_child() -> None:
    # The heartbeat thread does not exist in the child; runs the child creates start it again.
    global _heartbeat_started
    _heartbeat_started = threading.Event()


if hasattr(os, "register_at_fork"):
    os.register_at_fork(after_in_child=_after_fork_in_child)


@atexit.register
def _finish_open_runs() -> None:
    # Runs atexit before the sender's exit hook (registered earlier), which then sends what is
    # queued, for at most VIGILY_EXIT_TIMEOUT seconds. The interpreter is shutting down: never raise.
    for run in list(_open_runs):
        if run._pid != os.getpid():
            continue  # a forked child leaves its parent's runs alone
        # A run deleted while the script ran has nothing left to finish.
        with contextlib.suppress(Exception):
            run.finish("failed" if _failed else "finished")


def _target(target: str | Mapping[str, Any] | None) -> dict[str, Any] | None:
    if target is None:
        return None
    if isinstance(target, str):
        return {"ref": target}
    return _jsonable(dict(target))


def _nested(key: str, value: Any) -> dict[str, Any]:
    """``{"a": {"b": value}}`` for the dotted key ``a.b``."""
    parts = key.split(".")
    out: Any = value
    for p in reversed(parts):
        out = {p: out}
    return out


def _now_us() -> int:
    return time.time_ns() // 1000


class Run:
    """A run: parameters, metric series, metadata.

    ``kind`` says what the run is (``training`` by default, ``eval`` for evaluations) and ``target``
    what it trains or evaluates, as a dict such as ``{"model": "...", "prompt": "...", "git": "..."}``
    or a single string.

    ``Run()`` starts a new run without waiting for the engine; ``Run("<id>")`` reopens an existing
    one to log more (that asks the engine for the run). A run that is not finished explicitly is
    finished when the process exits: ``failed`` after an uncaught exception, ``finished`` otherwise.
    Used as a context manager, an exception inside the block marks it ``failed`` (``aborted`` for
    Ctrl-C).

    Writes are sent in the background. When the run is deleted elsewhere (in the web app),
    :class:`RunDeletedError` is raised by the next call after the engine said so.
    """

    def __init__(
        self,
        run_id: str | None = None,
        *,
        repo: str | os.PathLike[str] | None = None,
        experiment: str | None = None,
        name: str | None = None,
        description: str | None = None,
        tags: Iterable[str] = (),
        created_at: datetime | float | None = None,
        kind: str | None = None,
        target: str | Mapping[str, Any] | None = None,
        capture_system: bool = True,
    ) -> None:
        self._conn = _sender.connection(repo)
        self._contexts: dict[Any, tuple[str, Any]] = {}
        self._finished = False
        self._pid = os.getpid()
        if run_id is not None:
            # The run may have been created by this process a moment ago.
            _sender.flush_connection(self._conn, _config.exit_timeout())
            try:
                resumed = self._conn.call("POST", f"/api/v1/runs/{_quote(run_id)}/resume")
            except (RunDeletedError, FileNotFoundError):
                raise FileNotFoundError(f"no run with id {run_id}") from None
            self._h = _sender.RunHandle(self._conn, run_id, None)
            self._h.continue_steps(resumed.get("series") or [])
            self._meta: dict[str, Any] = dict(resumed["run"])
            self._params: dict[str, Any] = resumed.get("params") or {}
            changes: dict[str, Any] = {
                k: v
                for k, v in {"experiment": experiment, "name": name, "description": description}.items()
                if v is not None
            }
            if tags:
                changes["tags"] = list(dict.fromkeys([*self.tags, *tags]))
            if changes:
                self._update(**changes)
        else:
            run_id = uuid7_hex()
            tags = list(tags)
            tgt = _target(target)
            created = _micros(created_at)
            if created is None:
                created = _now_us()
            create: dict[str, Any] = {"op": "create", "id": run_id, "created_at": created}
            for k, v in (
                ("name", name),
                ("experiment", experiment),
                ("description", description),
                ("kind", kind),
                ("target", tgt),
            ):
                if v is not None:
                    create[k] = v
            if tags:
                create["tags"] = tags
            self._meta = {
                "id": run_id,
                "name": name,
                "experiment": experiment,
                "description": description,
                "tags": tags,
                "status": "running",
                "archived": False,
                "created_at": created,
                "ended_at": None,
                "kind": kind,
                "target": tgt,
            }
            self._h = _sender.RunHandle(self._conn, run_id, create)
            self._params = {}
            if capture_system:
                self[_SYSTEM_KEY] = _system_info()
            schedule = _schedule_from_env()
            if schedule:
                self._set_param(f"{_RUN_KEY}.schedule", schedule)
        _sender.RUNS[run_id] = (self._meta.get("experiment"), self._conn)
        _open_runs.add(self)
        _start_heartbeats()

    # Identity and metadata

    @property
    def id(self) -> str:
        """The run's id (a UUIDv7 as 32 hex digits), as shown in the web app and accepted by ``Run(id)``."""
        return self._h.id

    hash = id  # the name other trackers use

    def _info(self) -> dict[str, Any]:
        return self._meta

    def _update(self, **changes: Any) -> None:
        self._h.op({"op": "update", "run_id": self.id, "patch": changes})
        self._meta.update(changes)
        if "experiment" in changes:
            _sender.RUNS[self.id] = (changes["experiment"], self._conn)

    @property
    def name(self) -> str | None:
        return self._info()["name"]

    @name.setter
    def name(self, value: str) -> None:
        self._update(name=value)

    @property
    def experiment(self) -> str | None:
        return self._info()["experiment"]

    @experiment.setter
    def experiment(self, value: str) -> None:
        self._update(experiment=value)

    @property
    def description(self) -> str | None:
        return self._info()["description"]

    @description.setter
    def description(self, value: str) -> None:
        self._update(description=value)

    @property
    def tags(self) -> list[str]:
        return list(self._info()["tags"])

    def add_tag(self, tag: str) -> None:
        self._update(tags=[*self.tags, tag])

    def remove_tag(self, tag: str) -> None:
        self._update(tags=[t for t in self.tags if t != tag])

    @property
    def kind(self) -> str:
        """``training`` (the default), ``eval`` or the kind the run was created with."""
        return self._info().get("kind") or "training"

    @property
    def target(self) -> dict[str, Any] | None:
        """What the run trains or evaluates: a model, prompt, endpoint or git commit.
        A string given at creation is stored as ``{"ref": "<string>"}``."""
        return self._info().get("target")

    @property
    def status(self) -> str:
        return self._info()["status"]

    # Parameters

    def _set_param(self, key: str, v: Any) -> None:
        self._h.op({"op": "params", "run_id": self.id, "params": _nested(key, v)})
        node = self._params
        parts = key.split(".")
        for p in parts[:-1]:
            node = node.setdefault(p, {})
        node[parts[-1]] = v

    def __setitem__(self, key: str, value: Any) -> None:
        """Sets a parameter. Nested dicts are stored by dotted key, so ``run["hparams"] = {...}``
        followed by ``run["hparams"]["lr"]`` reads one value."""
        self._set_param(key, _jsonable(value))

    def __getitem__(self, key: str) -> Any:
        node: Any = self._params
        for p in key.split("."):
            node = node[p]
        return node

    def set_params(self, params: Mapping[str, Any]) -> None:
        """Merges a dict of parameters into the run's parameters."""
        for k, v in params.items():
            self[k] = v

    @property
    def params(self) -> dict[str, Any]:
        """The parameters set so far, without the captured system information."""
        return {k: v for k, v in self._params.items() if k not in _RESERVED}

    def log_features(self, rows: Iterable[Mapping[str, Any]]) -> None:
        """Records input features (one dict per example: numbers or categories) for data drift."""
        from vigily.evaluation import log_features

        log_features(self, rows)

    # Metrics

    def track(
        self,
        value: float,
        name: str,
        step: int | None = None,
        epoch: int | None = None,
        context: Mapping[str, Any] | None = None,
        timestamp: datetime | float | None = None,
    ) -> None:
        """Records one value of the metric ``name``.

        ``step`` defaults to one past the series' highest step. ``context`` tells series of the
        same metric apart, such as ``{"subset": "val"}``. ``timestamp`` (a datetime or Unix
        seconds) defaults to now; pass it when importing logs. The value is queued and sent in
        the background.
        """
        ctx_key = ""
        ctx = None
        if context:
            try:
                key: Any = tuple(sorted(context.items()))
                hit = self._contexts.get(key)
            except TypeError:
                key, hit = None, None
            if hit is None:
                ctx = _jsonable(context)
                hit = (json.dumps(ctx, sort_keys=True), ctx)
                if key is not None:
                    self._contexts[key] = hit
            ctx_key, ctx = hit
        if not name or not str(name).strip():
            raise ValueError("a metric needs a name")
        self._h.track(name, value, step, epoch, ctx_key, ctx, _micros(timestamp))

    # Lifecycle

    def finish(self, status: str = "finished", ended_at: datetime | float | None = None) -> None:
        """Ends the run as ``finished``, ``failed`` or ``aborted``. The engine stores everything
        logged before, then marks the run; this call does not wait for that."""
        if self._finished:
            return
        if status not in ("finished", "failed", "aborted"):
            raise ValueError(f"unknown status {status!r}; use finished, failed or aborted")
        ended = _micros(ended_at)
        if ended is None:
            ended = _now_us()
        try:
            self._h.op({"op": "finish", "run_id": self.id, "status": status, "ended_at": ended})
        except RunDeletedError:
            # Deleted elsewhere while running: there is nothing left to finish, now or at exit.
            self._finished = True
            _open_runs.discard(self)
            raise
        self._finished = True
        self._meta.update(status=status, ended_at=ended)
        _open_runs.discard(self)

    close = finish

    def flush(self, timeout: float | None = None) -> None:
        """Waits until everything logged so far reached the engine (or the spool). Raises
        :class:`RunDeletedError` when the engine said the run was deleted."""
        self._h.flush(timeout)

    def __enter__(self) -> Run:
        return self

    def __exit__(self, tp: type[BaseException] | None, value: BaseException | None, tb: Any) -> None:
        if tp is None:
            self.finish("finished")
        elif issubclass(tp, KeyboardInterrupt):
            self.finish("aborted")
        else:
            self.finish("failed")

    def __repr__(self) -> str:
        return f"<vigily.Run {self.id}>"


def _quote(s: str) -> str:
    return urllib.parse.quote(str(s), safe="")


def _number(v: Any) -> float:
    if isinstance(v, str):
        return float(v.replace("Infinity", "inf"))
    return float(v)


class Repo:
    """Read access to a repository: runs, parameters and metric values.

    ``Repo(path)`` reads through the engine that serves the repository at ``path`` (started when
    none runs); ``Repo(url="http://host:port")`` through the engine at that address. Reads wait
    for what this process logged to reach the engine first, so they see it.
    """

    def __init__(self, path: str | os.PathLike[str] | None = None, *, url: str | None = None) -> None:
        self._conn = _sender.connection(path, url=url)

    @property
    def location(self) -> str:
        """The repository (its canonical path or URI), or the engine's address."""
        return self._conn.url or self._conn.repo or ""

    def _sync(self) -> None:
        _sender.flush_connection(self._conn, _config.exit_timeout())
        with contextlib.suppress(PermissionError):
            self._conn.call("POST", "/api/v1/flush")

    def runs(self) -> list[dict[str, Any]]:
        """Every run's metadata, newest first. Times are microseconds since the Unix epoch."""
        self._sync()
        doc = self._conn.call("GET", "/api/v1/runs", query={"fields": "summary", "archived": "true"})
        rows = doc["rows"]
        for r in rows:
            if not r.pop("has_name", True):
                r["name"] = None
        rows.sort(key=lambda r: r["created_at"], reverse=True)
        return rows

    def params(self, run_id: str) -> dict[str, Any]:
        self._sync()
        return self._conn.call("GET", f"/api/v1/runs/{_quote(run_id)}/params")["params"]

    def series(self, run_id: str) -> list[dict[str, Any]]:
        """The metric series of a run: name, context and statistics."""
        self._sync()
        return self._conn.call("GET", f"/api/v1/runs/{_quote(run_id)}/series")["series"]

    def metric(
        self,
        run_id: str,
        name: str,
        context: Mapping[str, Any] | None = None,
        *,
        start_time: datetime | float | None = None,
        stop_time: datetime | float | None = None,
    ) -> dict[str, list[Any]]:
        """One series' points: ``steps``, ``values``, ``epochs`` and ``timestamps`` (microseconds).

        ``start_time`` and ``stop_time`` (a ``datetime`` or Unix seconds) keep only the points logged
        in ``[start_time, stop_time)``; storage reads only that range.
        """
        self._sync()
        query = {
            "name": name,
            "context": json.dumps(_jsonable(context or {}), sort_keys=True),
            "start_time": _micros(start_time),
            "stop_time": _micros(stop_time),
        }
        try:
            p = self._conn.call("GET", f"/api/v1/runs/{_quote(run_id)}/points", query=query)
        except FileNotFoundError as e:
            if "has no series" in str(e):
                return {"steps": [], "values": [], "epochs": [], "timestamps": []}
            raise
        n = len(p["steps"])
        epochs = p.get("epochs") or [None] * n
        return {
            "steps": p["steps"],
            "values": [_number(v) for v in p["values"]],
            "epochs": epochs,
            "timestamps": p.get("timestamps") or [],
        }

    def results(self, run_id: str) -> list[dict[str, Any]]:
        """An eval run's per-item results: output, error, latency and scores."""
        self._sync()
        return self._conn.call("GET", f"/api/v1/runs/{_quote(run_id)}/results")

    def delete_runs(self, run_ids: Iterable[str]) -> int:
        """Deletes runs with their parameters, metrics and evaluation records; returns how many
        existed. Running runs are refused (``ValueError``): finish or abort them first. Needs the
        ``admin`` scope (a local engine's own token has it)."""
        ids = list(run_ids)
        if not ids:
            return 0
        self._sync()
        try:
            return int(self._conn.call("POST", "/api/v1/runs/delete", {"ids": ids})["deleted"])
        except FileNotFoundError:
            known = {r["id"] for r in self.runs()}
            ids = [i for i in ids if i in known]
            if not ids:
                return 0
            return int(self._conn.call("POST", "/api/v1/runs/delete", {"ids": ids})["deleted"])

    def optimize(self) -> None:
        """Compacts storage and builds the metrics index. Run it when training is done. Needs the
        ``admin`` scope."""
        self._sync()
        self._conn.call("POST", "/api/v1/admin/optimize", timeout=3600.0)

    def flush(self) -> None:
        """Waits until everything this process logged is in storage and visible to reads."""
        self._sync()

    def __repr__(self) -> str:
        return f"<vigily.Repo {self.location}>"


# Evaluation and tracing API; imported last because these modules use the helpers above.
from vigily import evaluators, tracing  # noqa: E402, I001
from vigily.datasets import Dataset, Item, datasets  # noqa: E402
from vigily.evaluation import EvalResult, evaluate, log_features  # noqa: E402
from vigily.tracing import trace  # noqa: E402
