"""A weekly LLM-as-judge evaluation, as it would be scheduled. In the demo it fails on purpose: the
judge endpoint is your own model client and must be configured by the operator, which shows how a
failed schedule looks (retries, captured output, an alert).
"""

import os
import sys

endpoint = os.environ.get("JUDGE_ENDPOINT")
if not endpoint:
    print("Scoring support-intents with the answer-quality judge", flush=True)
    print("JUDGE_ENDPOINT is not set; add it to this schedule's env in vigily.toml", file=sys.stderr)
    sys.exit(2)
print(f"Would score answers with the judge at {endpoint}")
