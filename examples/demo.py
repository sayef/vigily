"""Log a synthetic set of runs to try vigily without training anything.

    uv run --no-sync python examples/demo.py          # writes ./.vigily through its engine
    vigily open                                     # the web app, signed in

Every value is generated: four experiments with hyperparameter sweeps, three evaluation runs,
noisy train and validation curves, system metrics, a 200 000-point run, runs that diverge and
fail, runs that were stopped, two that are still running, and synthetic LLM traces (a support
assistant's retrieval, tool and model calls, with token usage). The engine prices token usage from
its configuration: with `vigily up --config examples/vigily.toml` the traces show costs from the
example price table there.
"""

from __future__ import annotations

import argparse
import math
import os
import random
import time

import vigily
from vigily import Dataset, Repo, Run, evaluate
from vigily import evaluators as ev

DAY = 86_400.0


def noisy(rng: random.Random, v: float, scale: float) -> float:
    return v + rng.gauss(0.0, scale)


def log_system(run: Run, rng: random.Random, step: int, t: float, gpu: float) -> None:
    run.track(
        min(100.0, max(0.0, noisy(rng, 62.0, 9.0))), name="__system__/cpu_percent", step=step, timestamp=t
    )
    run.track(min(100.0, max(0.0, noisy(rng, gpu, 4.0))), name="__system__/gpu_util", step=step, timestamp=t)
    run.track(
        max(0.0, noisy(rng, gpu / 100 * 21.5, 0.3)), name="__system__/gpu_memory_gb", step=step, timestamp=t
    )


def cifar(repo: str, rng: random.Random, now: float) -> None:
    """ResNet-18 on CIFAR-10: a learning-rate by batch-size sweep, two optimizers."""
    start = now - 9 * DAY
    i = 0
    for opt in ("sgd", "adamw"):
        for lr in (1e-3, 3e-3, 1e-2, 3e-2):
            for bs in (64, 128):
                i += 1
                created = start + i * 2400
                steps_per_epoch = 50_000 // bs
                epochs = 30
                diverge = opt == "adamw" and lr >= 3e-2 and bs == 64
                aborted = opt == "sgd" and lr == 1e-3 and bs == 64
                run = Run(
                    repo=repo,
                    experiment="cifar10-resnet18",
                    name=f"resnet18-{opt}-lr{lr:g}-bs{bs}",
                    tags=["sweep"] + (["baseline"] if lr == 1e-2 and opt == "sgd" and bs == 128 else []),
                    created_at=created,
                )
                run["hparams"] = {"lr": lr, "batch_size": bs, "epochs": epochs, "weight_decay": 5e-4}
                run["optimizer"] = {
                    "name": opt,
                    "momentum": 0.9 if opt == "sgd" else None,
                    "betas": [0.9, 0.999],
                }
                run["model"] = {"arch": "resnet18", "width": 64, "dropout": 0.0}
                run["data"] = {"dataset": "cifar10", "augment": ["crop", "flip"]}
                # The best learning rate depends on the optimizer: about 1e-2 for SGD, 1e-3 for AdamW.
                best = 1e-2 if opt == "sgd" else 2e-3
                quality = math.exp(-((math.log10(lr) - math.log10(best)) ** 2) / 0.5)
                floor = 0.18 + 0.35 * (1 - quality) + (0.03 if bs == 128 else 0.0)
                tau = 2500 + 6000 * (1 - quality)
                sec_per_step = 0.045 * bs / 64
                last_epoch = epochs if not aborted else 11
                step = 0
                stop = False
                for epoch in range(last_epoch):
                    for _ in range(steps_per_epoch):
                        t = created + step * sec_per_step
                        if step % 10 == 0:
                            loss = floor + 2.1 * math.exp(-step / tau)
                            if diverge and step > 2600:
                                loss *= math.exp((step - 2600) / 180)
                            value = noisy(rng, loss, 0.05 * loss)
                            if diverge and step > 3200:
                                value = float("nan")
                                stop = True
                            run.track(
                                value,
                                name="loss",
                                step=step,
                                epoch=epoch,
                                context={"subset": "train"},
                                timestamp=t,
                            )
                            run.track(
                                lr * 0.5 * (1 + math.cos(math.pi * step / (steps_per_epoch * epochs))),
                                name="lr",
                                step=step,
                                timestamp=t,
                            )
                        if step % 100 == 0:
                            log_system(run, rng, step, t, 93.0)
                        step += 1
                        if stop:
                            break
                    if stop:
                        break
                    t = created + step * sec_per_step
                    vloss = floor * 1.12 + 2.1 * math.exp(-step / tau) + 0.002 * max(0, epoch - 20) ** 2
                    run.track(
                        noisy(rng, vloss, 0.012),
                        name="loss",
                        step=step,
                        epoch=epoch,
                        context={"subset": "val"},
                        timestamp=t,
                    )
                    acc = 1 - 0.9 * math.exp(-step / (tau * 0.9)) - floor * 0.32
                    run.track(
                        min(1.0, noisy(rng, acc, 0.006)),
                        name="accuracy",
                        step=step,
                        epoch=epoch,
                        context={"subset": "val"},
                        timestamp=t,
                    )
                end = created + step * sec_per_step
                if diverge:
                    run.description = (
                        "Loss blew up after warmup; learning rate too high for AdamW at this batch size."
                    )
                    run.finish("failed", ended_at=end)
                elif aborted:
                    run.description = "Stopped by hand: too slow to converge."
                    run.finish("aborted", ended_at=end)
                else:
                    run.finish("finished", ended_at=end)


def mnist(repo: str, rng: random.Random, now: float) -> None:
    """A small MLP on MNIST: hidden size by dropout."""
    start = now - 6 * DAY
    i = 0
    for hidden in (128, 256, 512, 1024):
        for dropout in (0.0, 0.3):
            i += 1
            created = start + i * 900
            run = Run(
                repo=repo,
                experiment="mnist-mlp",
                name=f"mlp-h{hidden}-d{dropout:g}",
                created_at=created,
                tags=["quick"],
            )
            run["hparams"] = {
                "hidden": hidden,
                "dropout": dropout,
                "lr": 1e-3,
                "batch_size": 256,
                "epochs": 12,
            }
            run["optimizer"] = {"name": "adamw"}
            cap = 0.06 - 0.008 * math.log2(hidden / 128) - (0.01 if dropout else 0)
            t = created
            for step in range(0, 2820, 5):
                t = created + step * 0.02
                loss = cap + 2.3 * math.exp(-step / 260)
                run.track(
                    noisy(rng, loss, 0.04 * loss),
                    name="loss",
                    step=step,
                    epoch=step // 235,
                    context={"subset": "train"},
                    timestamp=t,
                )
                if step % 235 == 0 and step:
                    over = 0.0 if dropout else 0.004 * (step / 235) ** 1.4 * hidden / 512
                    run.track(
                        noisy(rng, cap * 1.3 + over + 2.3 * math.exp(-step / 260), 0.004),
                        name="loss",
                        step=step,
                        epoch=step // 235,
                        context={"subset": "val"},
                        timestamp=t,
                    )
                    run.track(
                        noisy(rng, 0.995 - cap * 0.4 - over * 0.5 - 0.6 * math.exp(-step / 200), 0.002),
                        name="accuracy",
                        step=step,
                        epoch=step // 235,
                        context={"subset": "val"},
                        timestamp=t,
                    )
            run.finish(ended_at=t)


def gpt(repo: str, rng: random.Random, now: float, long_points: int) -> None:
    """A small GPT pretraining sweep: width and warmup, one long run, one divergence, two live runs."""
    configs = [
        ("gpt-s-w512-wu1k", 512, 1000, "finished", 4.5 * DAY),
        ("gpt-s-w512-wu4k", 512, 4000, "finished", 4.4 * DAY),
        ("gpt-s-w768-wu1k", 768, 1000, "failed", 3.9 * DAY),
        ("gpt-s-w768-wu4k", 768, 4000, "finished", 3.8 * DAY),
        ("gpt-s-w1024-wu4k-long", 1024, 4000, "finished", 3.0 * DAY),
        ("gpt-s-w1024-wu2k", 1024, 2000, "running", 2.2 * 3600),
        ("gpt-s-w1024-wu8k", 1024, 8000, "running", 0.7 * 3600),
    ]
    for name, width, warmup, status, ago in configs:
        created = now - ago
        long = name.endswith("long")
        total = long_points if long else 30_000
        every = 1 if long else 10
        run = Run(
            repo=repo,
            experiment="gpt-small-pretrain",
            name=name,
            created_at=created,
            tags=["pretrain"] + (["long"] if long else []),
        )
        run["hparams"] = {"lr": 6e-4, "warmup_steps": warmup, "batch_tokens": 524_288, "max_steps": total}
        run["model"] = {"d_model": width, "n_layers": 12, "n_heads": width // 64, "vocab": 50_304}
        run["optimizer"] = {"name": "adamw", "betas": [0.9, 0.95], "weight_decay": 0.1}
        floor = 3.35 - 0.12 * math.log2(width / 512)
        sec_per_step = 0.38 * width / 512
        if status == "running":
            total = min(total, int(ago / sec_per_step))
        t = created
        for step in range(0, total, every):
            t = created + step * sec_per_step
            frac = min(1.0, step / warmup)
            lr = (
                6e-4 * frac
                if step < warmup
                else 6e-4 * 0.5 * (1 + math.cos(math.pi * (step - warmup) / max(1, total - warmup)))
            )
            loss = floor + 7.6 * math.exp(-step / 1800) + 0.9 * math.exp(-step / 12_000)
            if status == "failed" and step > 9000:
                loss += 0.004 * (step - 9000) ** 1.02 / 10
                if step > 11_000:
                    loss = float("inf")
            run.track(
                noisy(rng, loss, 0.035), name="loss", step=step, context={"subset": "train"}, timestamp=t
            )
            if step % (100 if long else 50) == 0:
                run.track(lr, name="lr", step=step, timestamp=t)
                run.track(
                    abs(noisy(rng, 0.9 + 2.5 * math.exp(-step / 3000), 0.15)),
                    name="grad_norm",
                    step=step,
                    timestamp=t,
                )
                run.track(
                    noisy(rng, 168_000 * 512 / width, 3000), name="tokens_per_sec", step=step, timestamp=t
                )
            if step % 1000 == 0 and step:
                run.track(
                    noisy(rng, loss + 0.04, 0.01),
                    name="loss",
                    step=step,
                    context={"subset": "val"},
                    timestamp=t,
                )
            if step % 500 == 0:
                log_system(run, rng, step, t, 97.0)
            if math.isinf(loss):
                break
        if status == "failed":
            run.description = (
                "Loss went to infinity at step 11k. Warmup of 1k steps is too short at d_model 768."
            )
            run.finish("failed", ended_at=t)
        elif status == "finished":
            run.finish(ended_at=t)
        else:
            run.flush()


def bert(repo: str, rng: random.Random, now: float) -> None:
    """BERT-base fine-tuning on SST-2: learning rate by seed."""
    start = now - 2 * DAY
    i = 0
    for lr in (1e-5, 2e-5, 5e-5):
        for seed in (1, 2, 3):
            i += 1
            created = start + i * 1500
            run = Run(
                repo=repo, experiment="sst2-bert-base", name=f"bert-lr{lr:g}-seed{seed}", created_at=created
            )
            run["hparams"] = {"lr": lr, "seed": seed, "batch_size": 32, "epochs": 3, "max_len": 128}
            run["model"] = {"name": "bert-base-uncased", "frozen_layers": 0}
            peak = {1e-5: 0.915, 2e-5: 0.928, 5e-5: 0.905}[lr] + rng.gauss(0, 0.004)
            t = created
            last = 6315 if not (lr == 5e-5 and seed == 3) else 2100
            for step in range(0, last, 15):
                t = created + step * 0.11
                loss = 0.19 + 0.5 * math.exp(-step / (700 * 2e-5 / lr))
                run.track(
                    noisy(rng, loss, 0.06),
                    name="loss",
                    step=step,
                    epoch=step // 2105,
                    context={"subset": "train"},
                    timestamp=t,
                )
                if step % 525 == 0 and step:
                    acc = peak - 0.25 * math.exp(-step / 900)
                    run.track(
                        noisy(rng, acc, 0.003),
                        name="accuracy",
                        step=step,
                        epoch=step // 2105,
                        context={"subset": "val"},
                        timestamp=t,
                    )
                    run.track(
                        noisy(rng, acc - 0.01, 0.004),
                        name="f1",
                        step=step,
                        epoch=step // 2105,
                        context={"subset": "val"},
                        timestamp=t,
                    )
            if last < 6315:
                run.description = "Stopped: the node was preempted."
                run.finish("aborted", ended_at=t)
            else:
                run.finish(ended_at=t)


SST2_OPENERS = ["The film", "This movie", "The script", "The cast", "The ending", "The soundtrack"]
SST2_POSITIVE = ["is a quiet delight", "earns every minute", "lands with real warmth", "is sharp and funny"]
SST2_NEGATIVE = ["drags badly", "never finds its footing", "is flat and forgettable", "wastes a good premise"]
SST2_TAILS = ["", " from start to finish", " despite a slow start", " in its second half"]


def sst2_items() -> list[dict]:
    """A small synthetic sentiment test split in the shape of SST-2: a sentence and its label."""
    items = []
    for label, phrases in (("positive", SST2_POSITIVE), ("negative", SST2_NEGATIVE)):
        for opener in SST2_OPENERS:
            for phrase in phrases:
                for tail in SST2_TAILS:
                    items.append({"input": f"{opener} {phrase}{tail}.", "expected": label})
    return items


def sst2_checkpoint(seed: int, quality: float):
    """A stand-in for a fine-tuned checkpoint: right on a fixed, seed-dependent share of sentences."""

    def predict(text: str) -> str:
        right = random.Random(f"{seed}:{text}").random() < quality
        positive = any(p in text for p in SST2_POSITIVE)
        return "positive" if positive == right else "negative"

    return predict


def evals(repo: str, now: float) -> None:
    """Evaluation runs: three seeds of one fine-tuning recipe scored item by item on a held-out split.

    They share an experiment, a target and a dataset, so they form one drift suite: each newer seed
    is compared with the seeds before it.
    """
    ds = Dataset.create(
        "sst2-test",
        sst2_items(),
        description="Synthetic movie-review sentences with a positive or negative label",
        note="Initial split",
        repo=repo,
    )
    for i, (seed, quality) in enumerate([(1, 0.912), (2, 0.927), (3, 0.919)]):
        evaluate(
            ds,
            sst2_checkpoint(seed, quality),
            [ev.exact_match()],
            target_ref={"model": "bert-lr2e-05", "dataset": "sst2-test"},
            experiment="sst2-eval",
            name=f"eval-bert-lr2e-05-seed{seed}",
            tags=["holdout", f"seed{seed}"],
            created_at=now - (3.2 - i) * DAY,
            concurrency=1,
            repo=repo,
        )


QUESTIONS = [
    "Where is order A-1001?",
    "How do I change the delivery address of order B-2002?",
    "Can I return a product after 30 days?",
    "Why was I charged twice for order C-3003?",
    "Do you ship to Iceland?",
    "How do I reset my password?",
]


def traces(location: str, rng: random.Random, now: float) -> None:
    """Synthetic traces of a support assistant: retrieval, an optional tool call and a model call.
    Most belong to one run; a few have none, as traces from an application usually do."""
    run = Run(
        repo=location,
        experiment="support-bot",
        name="assistant-traces",
        tags=["tracing"],
        created_at=now - 2 * DAY,
    )
    run["assistant"] = {"model": "example-large", "top_k": 4, "prompt": "support-v3"}
    spans: list[dict] = []
    for n in range(48):
        t0 = int((now - 2 * DAY + n * 3600 + rng.random() * 600) * 1_000_000)
        tid = f"{rng.getrandbits(128):032x}"
        ids = iter(f"{rng.getrandbits(64):016x}" for _ in range(8))
        root = next(ids)
        q = QUESTIONS[n % len(QUESTIONS)]
        model = "example-small" if n % 4 == 3 else "example-large"
        failed = "order" in q and n % 5 == 0
        common = {"trace_id": tid, "run_id": run.id if n % 8 else None, "project": "support-bot"}
        t = t0 + 2_000
        retr = next(ids)
        embed_ms = int(abs(noisy(rng, 35, 8)) * 1000)
        search_ms = int(abs(noisy(rng, 120, 40)) * 1000)
        spans.append(
            {
                **common,
                "span_id": next(ids),
                "parent_id": retr,
                "name": "embed_query",
                "kind": "embedding",
                "model": "example-embed",
                "provider": "example",
                "start_time": t + 500,
                "end_time": t + 500 + embed_ms,
                "status": "ok",
                "usage": {"input_tokens": 12 + len(q) // 4},
            }
        )
        spans.append(
            {
                **common,
                "span_id": retr,
                "parent_id": root,
                "name": "search_docs",
                "kind": "retrieval",
                "start_time": t,
                "end_time": t + embed_ms + search_ms,
                "status": "ok",
                "input": {"query": q, "top_k": 4},
                "output": [f"doc-{rng.randint(100, 999)}" for _ in range(4)],
                "attributes": {"index": "help-center", "hits": 4},
            }
        )
        t += embed_ms + search_ms + 1_000
        if "order" in q:
            tool_ms = int(abs(noisy(rng, 260, 90)) * 1000)
            spans.append(
                {
                    **common,
                    "span_id": next(ids),
                    "parent_id": root,
                    "name": "lookup_order",
                    "kind": "tool",
                    "start_time": t,
                    "end_time": t + tool_ms,
                    "status": "error" if failed else "ok",
                    "error": "TimeoutError: order service did not answer in 2 s" if failed else None,
                    "input": {"order": q.split("order ")[1].split()[0].rstrip("?")},
                    "output": None if failed else {"status": "in transit", "eta_days": rng.randint(1, 4)},
                }
            )
            t += tool_ms + 1_000
        llm_ms = int(abs(noisy(rng, 1800 if model == "example-large" else 700, 400)) * 1000)
        inp = rng.randint(900, 2600)
        out = rng.randint(80, 420)
        cached = rng.choice([0, 0, 512, 1024]) if inp > 1100 else 0
        spans.append(
            {
                **common,
                "span_id": next(ids),
                "parent_id": root,
                "name": "chat",
                "kind": "llm",
                "model": model,
                "provider": "example",
                "start_time": t,
                "end_time": t + llm_ms,
                "status": "ok",
                "input": [
                    {"role": "system", "content": "You answer questions about orders. " * 40},
                    {"role": "user", "content": q},
                ],
                "output": "Synthetic answer: " + "the parcel is on its way. " * rng.randint(2, 8),
                "usage": {"input_tokens": inp, "output_tokens": out, "cached_tokens": cached},
                "attributes": {"temperature": 0.2, "max_tokens": 512},
            }
        )
        t += llm_ms + 1_500
        spans.append(
            {
                **common,
                "span_id": root,
                "name": "answer_question",
                "kind": "chain",
                "start_time": t0,
                "end_time": t,
                "status": "error" if failed else "ok",
                "error": "TimeoutError: order service did not answer in 2 s" if failed else None,
                "input": q,
                "output": None if failed else "Synthetic answer.",
            }
        )
    spans = [{k: v for k, v in sp.items() if v is not None} for sp in spans]
    # Sent like the spans `vigily.trace` records, to the engine of the run.
    vigily.tracing.export(spans)
    run.finish(ended_at=now - 2 * DAY + 48 * 3600)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--repo", default=os.environ.get("VIGILY_REPO", ".vigily"))
    p.add_argument("--seed", type=int, default=20261107)
    p.add_argument("--long-points", type=int, default=200_000, help="points in the long run's loss series")
    args = p.parse_args()
    rng = random.Random(args.seed)
    now = time.time()
    began = time.perf_counter()
    cifar(args.repo, rng, now)
    mnist(args.repo, rng, now)
    gpt(args.repo, rng, now, args.long_points)
    bert(args.repo, rng, now)
    evals(args.repo, now)
    traces(args.repo, rng, now)
    repo = Repo(args.repo)
    runs = repo.runs()
    print(f"Logged {len(runs)} runs to {repo.location} in {time.perf_counter() - began:.1f} s.")
    print(
        "Open them with: vigily open" + ("" if args.repo == ".vigily" else f" --repo {args.repo}"),
        flush=True,
    )
    # Leave the live runs running: send everything, then skip the exit hook that would finish them.
    vigily.flush()
    os._exit(0)


if __name__ == "__main__":
    main()
