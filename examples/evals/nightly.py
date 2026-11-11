"""The nightly evaluation that examples/vigily.toml schedules: both targets on the latest dataset.

vigily starts it with VIGILY_REPO, VIGILY_SCHEDULE, VIGILY_SCHEDULE_RUN and
VIGILY_SCHEDULED_FOR set, so the eval runs it logs link back to the schedule run.
"""

from __future__ import annotations

import os
import sys

sys.path.insert(0, os.path.dirname(__file__))

import support  # noqa: E402
from vigily import Dataset, evaluate  # noqa: E402
from vigily import evaluators as ev  # noqa: E402

# The day number keeps the simulated drift in place: the demo's 30 nights end at day 29.
DAY = 29

ds = Dataset("support-intents")
for name, make in (("intent-clf-v1", support.target_v1), ("intent-clf-v2", support.target_v2)):
    r = evaluate(
        ds,
        make(DAY),
        [ev.exact_match(), ev.custom("valid_label", lambda output: output in support.INTENTS, version="1")],
        target_ref={"model": name, "endpoint": f"https://models.example.com/{name}"},
        experiment="support-intents-nightly",
        features=support.traffic(DAY),
    )
    print(f"{name}: exact_match {r.scores['exact_match']:.3f} on {r.items} items, {len(r.failures)} failures")
