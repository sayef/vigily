"""Run a target over a dataset version and score every output.

.. code-block:: python

    from vigily import Dataset, evaluate, evaluators as ev

    result = evaluate(
        Dataset("intents"),
        target=classify,                                   # input -> output
        target_ref={"model": "intent-clf-v2", "git": "4f2c1ab"},
        evaluators=[ev.exact_match(), ev.custom("judge", my_llm_judge)],
        experiment="intents-nightly",
        features=lambda item: {"length": len(item.input), "language": item.metadata["lang"]},
    )
    print(result.scores, result.failures)

An eval run is an ordinary vigily run with ``kind="eval"``. Each item's output, error, latency and
scores are stored as records; the mean of each score is tracked as the metric ``eval/<score>``, so
the runs table, the metric explorer and drift detection all see it.
"""

from __future__ import annotations

import json
import math
import random
import time
import traceback
from collections.abc import Callable, Iterable, Mapping
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from typing import Any

from vigily import Run, __version__, _jsonable, tracing
from vigily.datasets import Dataset, Item
from vigily.evaluators import Evaluator, Score

#: Feature values kept per feature and run for data drift.
FEATURE_SAMPLE = 5000


@dataclass
class EvalResult:
    """The outcome of :func:`evaluate`."""

    run_id: str
    dataset: str
    dataset_version: int
    items: int
    errors: int
    #: Mean of each score over items that have it.
    scores: dict[str, float] = field(default_factory=dict)
    #: Ids of items that failed at least one evaluator.
    failures: list[str] = field(default_factory=list)


def _now_us() -> int:
    return int(time.time() * 1_000_000)


def evaluate(
    dataset: Dataset,
    target: Callable[[Any], Any],
    evaluators: Iterable[Evaluator],
    *,
    target_ref: str | Mapping[str, Any] | None = None,
    experiment: str | None = None,
    name: str | None = None,
    tags: Iterable[str] = (),
    features: Callable[[Item], Mapping[str, Any]] | Iterable[Mapping[str, Any]] | None = None,
    concurrency: int = 4,
    pass_item: bool = False,
    created_at: float | None = None,
    repo: str | None = None,
) -> EvalResult:
    """Calls ``target`` on every item of ``dataset``, scores each output with every evaluator, and
    records everything in a new eval run.

    ``target`` gets the item's input (or the whole :class:`Item` with ``pass_item=True``); an
    exception counts as an error for that item and fails its scores. ``features`` records input
    features for data-drift monitoring: a function of an item, or an iterable of feature dicts
    (for example, a sample of the day's production requests). ``created_at`` (Unix
    seconds) backdates the run when importing historical results.
    """
    evaluators = list(evaluators)
    if not evaluators:
        raise ValueError("evaluate needs at least one evaluator")
    names = [e.name for e in evaluators]
    if len(set(names)) != len(names):
        raise ValueError(f"evaluator names must be unique: {names}")
    if name is None:
        # "<model> <date>": readable in lists, unique enough for nightly runs.
        label = target_ref if isinstance(target_ref, str) else None
        if isinstance(target_ref, Mapping):
            label = next((str(target_ref[k]) for k in ("model", "name", "ref") if k in target_ref), None)
        when = created_at if created_at is not None else time.time()
        day = time.strftime("%Y-%m-%d %H:%M", time.gmtime(when))
        name = f"{label or dataset.name} {day}"
    run = Run(
        repo=repo,
        experiment=experiment,
        name=name,
        tags=tags,
        kind="eval",
        target=target_ref,
        created_at=created_at,
    )
    run["__eval__"] = {
        "dataset": {
            "name": dataset.name,
            "version": dataset.version,
            "digest": dataset.digest,
            "items": len(dataset),
        },
        "evaluators": [e.describe() for e in evaluators],
        "vigily": __version__,
    }
    items = dataset.items
    base_time = created_at

    def one(item: Item) -> dict[str, Any]:
        started = time.perf_counter()
        error = None
        link = tracing._EvalItem(run.id, item.id)
        try:
            with link:
                output = target(item if pass_item else item.input)
        except Exception as e:  # noqa: BLE001 - a failing target is a result, not a crash
            output = None
            error = f"{type(e).__name__}: {e}"[:2000] + "\n" + traceback.format_exc(limit=3)[-2000:]
        latency = (time.perf_counter() - started) * 1000
        scores = []
        for ev in evaluators:
            if error is not None:
                s = Score(0.0, False, "the target raised an error")
            else:
                try:
                    s = ev(input=item.input, output=output, expected=item.expected, item=item)
                except Exception as e:  # noqa: BLE001
                    s = Score(None, None, f"evaluator error: {type(e).__name__}: {e}"[:500])
            value = s.value
            if value is not None and not math.isfinite(float(value)):
                value = None
            scores.append(
                {
                    "name": ev.name,
                    "value": None if value is None else float(value),
                    "passed": s.passed,
                    "reason": s.reason,
                    "evaluator": ev.kind,
                    "evaluator_version": ev.version,
                }
            )
        return {
            "run_id": run.id,
            "item_id": item.id,
            "item_hash": item.hash,
            "dataset": dataset.name,
            "dataset_version": dataset.version,
            "output": _jsonable(output),
            "error": error,
            "latency_ms": latency,
            "scores": scores,
            "created_at": _now_us() if base_time is None else int(base_time * 1_000_000),
            "trace_id": link.ids[0] if link.ids else None,
        }

    try:
        with ThreadPoolExecutor(max_workers=max(1, concurrency)) as pool:
            results = list(pool.map(one, items))
        native = run._repo
        for i in range(0, len(results), 500):
            native.put_results(json.dumps(results[i : i + 500]))

        sums: dict[str, list[float]] = {n: [] for n in names}
        failures: list[str] = []
        for r in results:
            failed = False
            for s in r["scores"]:
                if s["value"] is not None:
                    sums[s["name"]].append(s["value"])
                elif s["passed"] is not None:
                    sums[s["name"]].append(1.0 if s["passed"] else 0.0)
                if s["passed"] is False:
                    failed = True
            if failed:
                failures.append(r["item_id"])
        n = len(results)
        stamp = None if base_time is None else base_time + 60
        means = {k: sum(v) / len(v) for k, v in sums.items() if v}
        for k, v in means.items():
            run.track(v, name=f"eval/{k}", step=n, timestamp=stamp)
        lat = sorted(r["latency_ms"] for r in results)
        if lat:
            run.track(lat[len(lat) // 2], name="latency_ms", step=n, context={"stat": "p50"}, timestamp=stamp)
            run.track(
                lat[min(len(lat) - 1, int(len(lat) * 0.95))],
                name="latency_ms",
                step=n,
                context={"stat": "p95"},
                timestamp=stamp,
            )
        errors = sum(1 for r in results if r["error"])
        run.track(errors, name="errors", step=n, timestamp=stamp)

        if callable(features):
            log_features(run, (features(it) for it in items))
        elif features is not None:
            log_features(run, features)
        # Results and features must be in storage before the run is marked finished: monitors and
        # other processes treat a finished eval run as complete.
        native.flush()
        run.finish("finished", ended_at=stamp)
    except BaseException:
        run.finish("failed")
        raise
    return EvalResult(run.id, dataset.name, dataset.version, n, errors, means, failures)


def log_features(run: Run, rows: Iterable[Mapping[str, Any]]) -> None:
    """Records input features (a dict per item: numbers or categories) of ``run`` for data drift.
    At most 5000 values per feature are kept, sampled uniformly."""
    cols: dict[str, list[Any]] = {}
    for row in rows:
        for k, v in row.items():
            cols.setdefault(str(k), []).append(_jsonable(v))
    rng = random.Random(0)
    samples = []
    for k, values in cols.items():
        if len(values) > FEATURE_SAMPLE:
            values = rng.sample(values, FEATURE_SAMPLE)
        samples.append({"run_id": run.id, "feature": k, "values": values})
    if samples:
        run._repo.put_features(json.dumps(samples))
        run._repo.flush()
