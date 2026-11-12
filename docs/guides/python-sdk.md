# Python SDK

`import vigily` gives you runs, datasets, evaluators and `evaluate()`. The package is pure Python
(Python 3.11 or newer, no dependencies): it talks to the engine that owns the repository over its
HTTP API, `/api/v1`.

```bash
pip install 'vigily[local]'   # the SDK and the engine for this machine (vigily-engine)
pip install vigily            # the SDK only, for a team server or an engine installed otherwise
```

`vigily-engine` is a platform wheel holding the engine binary, for Linux (x86_64 and arm64,
glibc 2.28 or newer; x86_64 with musl) and macOS (Apple silicon and Intel). It installs
`vigily-engine` next to Python, where the SDK finds it.

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
| `Run(run_id)` | Reopen an existing run to log more (asks the engine for it) |
| `repo=` | A directory or an object-store URI; default `VIGILY_REPO`, then `./.vigily`. Ignored when `VIGILY_URL` names an engine |
| `experiment=`, `name=`, `description=`, `tags=` | Shown and searchable in the web app |
| `kind="eval"`, `target=` | Mark an evaluation run and what it evaluated |
| `created_at=` | Backfill a run with a past start time (a `datetime` or Unix seconds) |
| `capture_system=False` | Do not record the environment (Python, platform, git commit) |

`run.track(value, name=, step=, epoch=, timestamp=, context=)`: `step` defaults to one past the
series' highest step, and `timestamp` to now; both are assigned in your process, so a batch sent
twice stores nothing twice. `run.add_tag()`, `run.finish(status=...)`, `run.flush()`.

`Run()` never waits for the network: run ids (UUIDv7, 32 hex digits) are made in your process, and
everything you log is queued and sent by a background thread (see [Sending and the spool](#sending-and-the-spool)).
A run you do not finish is finished when the process exits: `failed` after an uncaught exception,
`finished` otherwise.

## Reading back

```python
from vigily import Repo

repo = Repo()  # same default location as Run; Repo(url="https://vigily.example.com") for a team server
for r in repo.runs():
    print(r["name"], r["status"])
points = repo.metric(run_id, "loss", context={"subset": "val"})
results = repo.results(eval_run_id)  # per-item results of an eval run
```

Reads first wait until what your process logged has reached the engine, so they see it.
`Repo.delete_runs()` and `Repo.optimize()` need the `admin` scope (a local engine's own token has it).

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
them to the engine with the runs; `vigily.tracing.set_redactor(...)` changes or drops spans before
they leave the process. [LLM tracing](tracing.md) covers the API, OpenTelemetry, prices and payload
modes.

## Configuration

Without configuration, the SDK uses the engine of the repository on this machine: it reads
`engines/<key>.json` in the state directory (`VIGILY_STATE_DIR`, else `~/Library/Caches/vigily`
on macOS and `~/.cache/vigily` elsewhere), the same file `vigily status` reads. When no engine
serves the repository, it starts `vigily-engine --detach` (from `VIGILY_ENGINE`, beside Python
where `pip install 'vigily[local]'` puts it, or on `PATH`) and waits for it; processes starting at
the same time share one engine. An engine started this way stops after 30 minutes without use.
Without an engine binary, logging still works: batches go to the spool, with a message saying how to
install one.

```python
import vigily

vigily.configure(url="https://vigily.example.com", token=os.environ["TEAM_TOKEN"])
```

| Setting | Environment | Meaning |
|---|---|---|
| `configure(url=)` | `VIGILY_URL` | An engine's address, such as a team server (see [Tokens](configuration.md#tokens)); no engine is started |
| `configure(token=)` | `VIGILY_TOKEN` | The token to send; it needs the `read` and `ingest` scopes. A local engine's own token is used otherwise |
| `configure(project=)` | `VIGILY_PROJECT` | The default project of traces, whose payload policy applies |
| `configure(offline=True)` | `VIGILY_OFFLINE=1` | Contact no engine: everything goes to the spool |
| | `VIGILY_REPO` | The default repository (`./.vigily`) |
| | `VIGILY_EXIT_TIMEOUT` | Seconds the exit hook spends sending what is queued (30) |
| | `VIGILY_SPOOL_AFTER` | Seconds without reaching the engine before batches go to the spool (120) |
| | `VIGILY_SPOOL_MAX_MB` | The spool's size limit (2048) |
| | `VIGILY_BUFFER_MB` | What may wait in memory before `track()` waits (64) |

## Sending and the spool

A background thread per engine sends what you log in batches: every second, or at 20 000 points or
4 MiB, gzip-compressed, each with an `Idempotency-Key`. A failed batch is sent again with
exponential backoff (honouring `Retry-After`). `vigily.flush(timeout=None)` waits until everything
logged so far has reached the engine (or the spool), and returns `False` when that takes longer than
`timeout` seconds.

- **Backpressure.** When batches pile up faster than the engine takes them, `track()` waits, up to
  five seconds, then the waiting batches go to the spool. Points are never dropped silently; spans,
  which must never slow the traced code, keep a bounded queue and are dropped with a warning when it
  is full.
- **Offline spool.** After two minutes without reaching the engine, batches go to gzip-compressed
  NDJSON files under `<state dir>/spool/<writer>/` (readable by you only), in order. They are sent
  from there once the engine answers again, by the next process that reaches it, or with
  `vigily sync`. The spool has a size limit; it never deletes data to stay under it.
- **Exit.** At exit the SDK finishes the runs it started, then sends what is queued for up to
  `VIGILY_EXIT_TIMEOUT` seconds; what is left goes to the spool, and a message says where.
- **Fork.** A forked child (a `DataLoader` worker, `multiprocessing`) gets its own sender and logs to
  the parent's runs; it does not finish them. Give each process its own series or explicit steps:
  steps are counted per process.
- **Privacy.** The redactor runs, and the engine's payload policy is applied, before a span is queued
  or spooled: a payload the engine would not store never leaves your process. Spooled spans are such
  filtered spans, in files only you can read.

### Changes from 0.1.0-beta.2

The SDK used to write to storage in your process; it now sends to an engine.

- `RunDeletedError` is raised by the next call after the engine reported the deletion (usually the
  next `track()`, `flush()` or `finish()`), not by the write that met it.
- `Run.finish()` no longer waits for storage; `vigily.flush()` or any `Repo` read does.
- Run ids are UUIDv7s made by the SDK (32 hex digits).
- `Repo.delete_runs()` and `Repo.optimize()` need an `admin` token on a team server.
