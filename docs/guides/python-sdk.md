# Python SDK

`import vigily` gives you runs, datasets, evaluators and `evaluate()`. The package is a thin layer
over the Rust core (pyo3, abi3, Python 3.11 or newer); values cross the boundary as JSON.

## Runs

```python
from vigily import Run

run = Run(experiment="cifar10", name="resnet18-lr3e-3", tags=["sweep"])
run["hparams"] = {"lr": 3e-3, "batch_size": 128, "optimizer": {"name": "adamw"}}

for step, batch in enumerate(loader):
    loss = train_step(batch)
    run.track(loss, name="loss", step=step, context={"subset": "train"})
    if step % 500 == 0:
        run.track(validate(), name="loss", step=step, epoch=epoch, context={"subset": "val"})

run.finish()  # or `with Run(...) as run:`; an exception marks the run failed
```

| Argument | Meaning |
|---|---|
| `Run(run_id)` | Reopen an existing run to log more |
| `repo=` | A directory or an object-store URI; default `VIGILY_REPO`, then `./.vigily` |
| `experiment=`, `name=`, `description=`, `tags=` | Shown and searchable in the web app |
| `kind="eval"`, `target=` | Mark an evaluation run and what it evaluated |
| `created_at=` | Backfill a run with a past start time (a `datetime` or Unix seconds) |
| `capture_system=False` | Do not record the environment (Python, platform, git commit) |

`run.track(value, name=, step=, epoch=, timestamp=, context=)`: `step` defaults to one past the
series' last step. `run.add_tag()`, `run.finish(status=...)`.

## Reading back

```python
from vigily import Repo

repo = Repo()  # same default location as Run
for r in repo.runs():
    print(r["name"], r["status"])
points = repo.metric(run_id, "loss", context={"subset": "val"})
```

## Datasets

```python
from vigily import Dataset

ds = Dataset.create(
    "support-intents",
    [
        {"input": "Where is my parcel?", "expected": "shipping", "id": "q-001"},
        {"input": "Ich möchte kündigen", "expected": "cancel", "id": "q-002", "metadata": {"lang": "de"}},
    ],
)
ds = Dataset.from_jsonl("support-intents", "intents.jsonl")  # or from_csv
ds = ds.add([{"input": "...", "expected": "billing"}], note="Add billing questions")
latest = Dataset("support-intents")  # or Dataset("support-intents", version=3)
print(latest.version, latest.digest, len(latest.items()))
```

Use synthetic or properly anonymised items: dataset content is stored as given.

## Evaluate

```python
from vigily import evaluate, evaluators as ev

result = evaluate(
    ds,
    target=classify,  # input -> output: your model, your client
    evaluators=[ev.exact_match(), ev.regex(r"^[a-z_]+$"), ev.custom("judge", judge)],
    target_ref={"model": "intent-clf-v2", "git": "4f2c1ab"},
    experiment="support-intents-nightly",
    features=lambda item: {"lang": item.metadata.get("lang"), "length": len(item.input)},
)
print(result.run_id, result.scores)
```

- `concurrency=` (default 4) calls the target from that many threads.
- `pass_item=True` gives the target the whole item instead of its input.
- An exception in the target counts as an error for that item and fails its scores.
- `features=` records input features for [data drift](drift.md): a function of the item, or rows.

### Evaluators

| Evaluator | Passes when |
|---|---|
| `exact_match(case_sensitive=True, strip=True)` | output equals expected |
| `contains(...)` | output contains expected |
| `regex(pattern)` | the pattern matches the output |
| `numeric(tolerance=1e-6, relative=False)` | the number is within tolerance |
| `json_schema(schema)` | the output validates against a JSON schema subset |
| `custom(name, fn, version=None)` | `fn` says so |

`custom` calls `fn` with whichever of `input`, `output`, `expected` and `item` it accepts. It returns
a bool, a number, a `Score`, or `(value, passed, reason)`. Its version defaults to a hash of its source,
so editing a judge prompt inside the function changes the version recorded with every score.

## Traces

`vigily.trace(...)` records LLM traces (nested spans with model, token usage and cost) and sends
them to `vigily up`; `vigily.tracing.set_redactor(...)` changes or drops spans before they leave the
process. [LLM tracing](tracing.md) covers the API, OpenTelemetry, prices and payload modes.
