"""vigily: log runs, parameters and metrics from training code, then compare them with ``vigily up``.

.. code-block:: python

    from vigily import Run

    run = Run(experiment="mnist")
    run["hparams"] = {"lr": 3e-4, "batch_size": 64}
    for step in range(1000):
        run.track(loss, name="loss", step=step, context={"subset": "train"})
    run.finish()

Runs are written to ``./.vigily`` unless ``repo=`` or the ``VIGILY_REPO`` environment variable
names another directory or an object-store URI (``s3://bucket/prefix``).
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
import weakref
from collections.abc import Iterable, Mapping
from datetime import datetime
from typing import Any

from vigily import _native

__all__ = [
    "Dataset",
    "EvalResult",
    "Item",
    "Repo",
    "Run",
    "RunDeletedError",
    "__version__",
    "datasets",
    "evaluate",
    "evaluators",
    "log_features",
    "trace",
    "tracing",
]
__version__: str = _native.__version__

#: Raised when logging to a run that was deleted (in the web app or by another process).
RunDeletedError = _native.RunDeletedError

DEFAULT_REPO = ".vigily"
_SYSTEM_KEY = "__system__"
_RUN_KEY = "__run__"
_EVAL_KEY = "__eval__"
_RESERVED = (_SYSTEM_KEY, _RUN_KEY, _EVAL_KEY)

_repos: dict[str, _native.Repo] = {}
_repos_lock = threading.Lock()


def _repo_location(repo: str | os.PathLike[str] | None) -> str:
    loc = os.fspath(repo) if repo is not None else os.environ.get("VIGILY_REPO", DEFAULT_REPO)
    if "://" not in loc:
        loc = os.path.abspath(loc)
    return loc


def _open(repo: str | os.PathLike[str] | None) -> _native.Repo:
    """One native handle per location and process, shared by every run."""
    loc = _repo_location(repo)
    with _repos_lock:
        h = _repos.get(loc)
        if h is None:
            h = _repos[loc] = _native.Repo(loc)
        return h


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
    try:
        git = subprocess.run(
            ["git", "rev-parse", "--abbrev-ref", "HEAD", "HEAD"],
            capture_output=True,
            text=True,
            timeout=2,
            check=True,
        ).stdout.split()
        if len(git) == 2:
            info["git"] = {"branch": git[0], "commit": git[1]}
    except (OSError, subprocess.SubprocessError):
        pass
    return info


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


#: Seconds between heartbeats of open runs. The server shows a running run without a heartbeat,
#: metadata write or point for 10 minutes as stale (its process most likely died). Each heartbeat
#: is one small storage write, so it is kept rare.
HEARTBEAT_SECONDS = 120.0
_heartbeat_started = threading.Event()


def _heartbeat_loop() -> None:
    while True:
        time.sleep(HEARTBEAT_SECONDS)
        for run in list(_open_runs):
            # A failed heartbeat (storage briefly unavailable, run deleted) is not the
            # training script's problem; the next one may succeed.
            with contextlib.suppress(Exception):
                run._h.heartbeat()


def _start_heartbeats() -> None:
    if not _heartbeat_started.is_set():
        _heartbeat_started.set()
        threading.Thread(target=_heartbeat_loop, name="vigily-heartbeat", daemon=True).start()


@atexit.register
def _finish_open_runs() -> None:
    # The interpreter is shutting down: never raise here.
    for run in list(_open_runs):
        # A run deleted while the script ran has nothing left to finish.
        with contextlib.suppress(Exception):
            run.finish("failed" if _failed else "finished")
    for h in list(_repos.values()):
        with contextlib.suppress(Exception):
            h.flush()


class Run:
    """A run: parameters, metric series, metadata.

    ``kind`` says what the run is (``training`` by default, ``eval`` for evaluations) and ``target``
    what it trains or evaluates, as a dict such as ``{"model": "...", "prompt": "...", "git": "..."}``
    or a single string.

    ``Run()`` starts a new run; ``Run("<id>")`` reopens an existing one to log more. A run that is
    not finished explicitly is finished when the process exits: ``failed`` after an uncaught
    exception, ``finished`` otherwise. Used as a context manager, an exception inside the block
    marks it ``failed`` (``aborted`` for Ctrl-C).
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
        self._repo = _open(repo)
        self._contexts: dict[Any, str] = {}
        self._finished = False
        if run_id is not None:
            self._h = self._repo.open_run(run_id)
            self._params: dict[str, Any] = json.loads(self._repo.params(run_id))
            changes = {
                k: v
                for k, v in {"experiment": experiment, "name": name, "description": description}.items()
                if v is not None
            }
            if tags:
                changes["tags"] = list(dict.fromkeys([*self.tags, *tags]))
            if changes:
                self._h.update(json.dumps(changes))
        else:
            self._h = self._repo.create_run(
                name=name,
                experiment=experiment,
                description=description,
                tags=list(tags),
                created_at=_micros(created_at),
                kind=kind,
                target=None if target is None else json.dumps(_jsonable(target)),
            )
            self._params = {}
            if capture_system:
                self[_SYSTEM_KEY] = _system_info()
            schedule = _schedule_from_env()
            if schedule:
                self._h.set_param(f"{_RUN_KEY}.schedule", json.dumps(schedule))
                self._params.setdefault(_RUN_KEY, {})["schedule"] = schedule
        _open_runs.add(self)
        _start_heartbeats()

    # Identity and metadata

    @property
    def id(self) -> str:
        """The run's id, as shown in the web app and accepted by ``Run(id)``."""
        return self._h.id

    hash = id  # the name other trackers use

    def _info(self) -> dict[str, Any]:
        return json.loads(self._h.info())

    def _update(self, **changes: Any) -> None:
        self._h.update(json.dumps(changes))

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

    def __setitem__(self, key: str, value: Any) -> None:
        """Sets a parameter. Nested dicts are stored by dotted key, so ``run["hparams"] = {...}``
        followed by ``run["hparams"]["lr"]`` reads one value."""
        v = _jsonable(value)
        self._h.set_param(key, json.dumps(v))
        node = self._params
        parts = key.split(".")
        for p in parts[:-1]:
            node = node.setdefault(p, {})
        node[parts[-1]] = v

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

        ``step`` defaults to one past the series' last step. ``context`` tells series of the same
        metric apart, such as ``{"subset": "val"}``. ``timestamp`` (a datetime or Unix seconds)
        defaults to now; pass it when importing logs.
        """
        ctx = None
        if context:
            try:
                key: Any = tuple(sorted(context.items()))
                ctx = self._contexts.get(key)
            except TypeError:
                key = None
            if ctx is None:
                ctx = json.dumps(_jsonable(context), sort_keys=True)
                if key is not None:
                    self._contexts[key] = ctx
        self._h.track(name, float(value), step, epoch, ctx, _micros(timestamp))

    # Lifecycle

    def finish(self, status: str = "finished", ended_at: datetime | float | None = None) -> None:
        """Ends the run as ``finished``, ``failed`` or ``aborted`` and writes everything to storage."""
        if self._finished:
            return
        try:
            self._h.finish(status, _micros(ended_at))
        except RunDeletedError:
            # Deleted elsewhere while running: there is nothing left to finish, now or at exit.
            self._finished = True
            _open_runs.discard(self)
            raise
        self._finished = True
        _open_runs.discard(self)

    close = finish

    def flush(self) -> None:
        """Writes buffered points to storage now."""
        self._h.flush()

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


class Repo:
    """Read access to a repository: runs, parameters and metric values."""

    def __init__(self, path: str | os.PathLike[str] | None = None) -> None:
        self._repo = _open(path)

    @property
    def location(self) -> str:
        return self._repo.location

    def runs(self) -> list[dict[str, Any]]:
        """Every run's metadata, newest first. Times are microseconds since the Unix epoch."""
        return json.loads(self._repo.runs())

    def params(self, run_id: str) -> dict[str, Any]:
        return json.loads(self._repo.params(run_id))

    def series(self, run_id: str) -> list[dict[str, Any]]:
        """The metric series of a run: name and context."""
        return json.loads(self._repo.series(run_id))

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
        steps, values, epochs, timestamps = self._repo.points(
            run_id,
            name,
            json.dumps(_jsonable(context or {}), sort_keys=True),
            _micros(start_time),
            _micros(stop_time),
        )
        return {"steps": steps, "values": values, "epochs": epochs, "timestamps": timestamps}

    def delete_runs(self, run_ids: Iterable[str]) -> int:
        """Deletes runs with their parameters, metrics and evaluation records; returns how many
        existed. Running runs are refused (``ValueError``): finish or abort them first."""
        return self._repo.delete_runs(list(run_ids))

    def optimize(self) -> None:
        """Compacts storage and builds the metrics index. Run it when training is done."""
        self._repo.optimize()

    def flush(self) -> None:
        self._repo.flush()


# Evaluation and tracing API; imported last because these modules use the helpers above.
from vigily import evaluators, tracing  # noqa: E402, I001
from vigily.datasets import Dataset, Item, datasets  # noqa: E402
from vigily.evaluation import EvalResult, evaluate, log_features  # noqa: E402
from vigily.tracing import trace  # noqa: E402
