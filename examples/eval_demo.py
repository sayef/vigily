"""Backfill 30 nights of evaluations to try vigily's datasets, evaluations, drift and alerts.

    uv run --no-sync python examples/eval_demo.py                                   # writes ./.vigily
    vigily schedules --config examples/vigily.toml run nightly-intents          # one real scheduled run
    vigily up --config examples/vigily.toml

Everything is synthetic (examples/evals/support.py): a labelled dataset of support messages in
three languages, two keyword "classifiers" as targets, and a nightly evaluation of both over the
last 30 days. From day 20 production traffic shifts to longer German messages (data drift); from day
23 an upstream change breaks intent-clf-v2 on German text (behaviour drift).
"""

from __future__ import annotations

import argparse
import os
import sys
import time
from datetime import datetime, timedelta
from zoneinfo import ZoneInfo

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "evals"))

import support  # noqa: E402
from vigily import Dataset, evaluate  # noqa: E402
from vigily import evaluators as ev  # noqa: E402

NIGHTS = 30
EVALUATORS = [
    ev.exact_match(),
    ev.custom("valid_label", lambda output: output in support.INTENTS, version="1"),
]


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--repo", default=os.environ.get("VIGILY_REPO", ".vigily"))
    args = p.parse_args()
    began = time.perf_counter()
    berlin = ZoneInfo("Europe/Berlin")
    today = datetime.now(berlin).replace(hour=2, minute=0, second=0, microsecond=0)
    if datetime.now(berlin) < today:
        today -= timedelta(days=1)
    first = today - timedelta(days=NIGHTS - 1)

    ds = Dataset.create(
        "support-intents",
        support.items(),
        description="Support messages labelled with one of five intents, in English, German and French",
        note="Initial labelled set",
        repo=args.repo,
    )
    # The runs below were "started by" the nightly schedule.
    os.environ["VIGILY_SCHEDULE"] = "nightly-intents"
    for day in range(NIGHTS):
        when = first + timedelta(days=day)
        if day == 10:
            ds = ds.add(
                support.items(extra_billing=True), note="More billing phrasings after a support escalation"
            )
        for name, make in (("intent-clf-v1", support.target_v1), ("intent-clf-v2", support.target_v2)):
            os.environ["VIGILY_SCHEDULED_FOR"] = when.astimezone(ZoneInfo("UTC")).isoformat()
            evaluate(
                ds,
                make(day),
                EVALUATORS,
                target_ref={"model": name, "endpoint": f"https://models.example.com/{name}"},
                experiment="support-intents-nightly",
                name=f"{name} {when:%Y-%m-%d}",
                features=support.traffic(day),
                created_at=when.timestamp() + (0 if name.endswith("v1") else 90),
                concurrency=1,
                repo=args.repo,
            )
    print(f"Backfilled {NIGHTS} nights for 2 targets in {time.perf_counter() - began:.1f} s.", flush=True)


if __name__ == "__main__":
    main()
