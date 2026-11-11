"""Log many synthetic runs to measure vigily at scale.

uv run --no-sync python bench/scale.py --runs 2000 --points 2000 --repo /tmp/vigily-scale
vigily up --repo /tmp/vigily-scale --port 7601     # then time the API (bench/scale.sh)
"""

from __future__ import annotations

import argparse
import math
import random
import time

from vigily import Repo, Run


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--repo", required=True)
    p.add_argument("--runs", type=int, default=2000)
    p.add_argument("--points", type=int, default=2000)
    a = p.parse_args()
    rng = random.Random(1)
    began = time.perf_counter()
    for i in range(a.runs):
        run = Run(repo=a.repo, experiment=f"sweep-{i % 20}", name=f"run-{i}", capture_system=False)
        lr = 10 ** rng.uniform(-4, -1)
        run["hparams"] = {"lr": lr, "batch_size": rng.choice([32, 64, 128]), "seed": i}
        for s in range(a.points):
            loss = 0.2 + 2 * math.exp(-s / (300 + 2000 * lr)) + rng.gauss(0, 0.02)
            run.track(loss, name="loss", step=s, context={"subset": "train"})
            if s % 10 == 0:
                run.track(loss * 1.1, name="loss", step=s, context={"subset": "val"})
                run.track(1 - loss / 3, name="accuracy", step=s, context={"subset": "val"})
        run.finish()
    Repo(a.repo).flush()
    pts = a.runs * a.points * 1.2
    print(f"{a.runs} runs, {pts / 1e6:.1f} M points in {time.perf_counter() - began:.1f} s", flush=True)


if __name__ == "__main__":
    main()
