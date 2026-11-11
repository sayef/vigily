# Concepts

vigily has seven ideas. Each builds on the one before, and all of them live in one repository: a
directory or a bucket that holds Lance tables written by kv0.

## Runs

A **run** is one execution of your code: a training job, or an evaluation. It has an id, a name, an
experiment, tags, notes, a status (`running`, `finished`, `failed`, `aborted`) and times in UTC.
Parameters are a nested dictionary (`run["hparams"] = {...}`), addressed by dotted keys such as
`hparams.optimizer.name`. Runs of `kind="eval"` also carry a **target** reference: what was evaluated,
for example `{"model": "intent-clf-v2", "git": "4f2c1ab"}`.

A run that the process does not finish is finished when the process exits, and marked `failed`
after an uncaught exception.

While a run is open the SDK sends a heartbeat every 2 minutes. A running run with no heartbeat,
metadata change or point for 10 minutes shows as **No heartbeat**: its process most likely stopped
without finishing it (killed, out of memory, the machine went away). Its page offers **Mark as
aborted**, after which it can be deleted. Two cases can look stale while the process is alive: a
writer whose clock is several minutes behind the server's, and a process that holds Python's
interpreter lock for over 10 minutes in one call (the heartbeat thread cannot run meanwhile). If such
a run is marked aborted by mistake, its process can still finish it.

## Metrics

A **metric** is a named series of `(step, value)` points, optionally with an epoch and a timestamp.
A **context** tells series of one metric apart: `loss` on `{"subset": "train"}` and on
`{"subset": "val"}` are two series, written `loss[train]` and `loss[val]` in queries and charts.

The web app never draws more points than it has pixels. The server answers with a min/max envelope
on a shared grid, so spikes survive downsampling, and re-samples when you zoom.

## Datasets

A **dataset** is a named, versioned list of items: an input, an expected output and metadata. Every
change makes a new, immutable version. Items are content-addressed (a SHA-256 hash of the canonical
JSON), so committing the same items again returns the existing version, and each version has a
digest that changes when any item does.

## Evaluations

An **evaluation** calls your target on every item of a dataset version and scores each output with
one or more **evaluators**: exact match, contains, regex, numeric tolerance, JSON schema, or your own
function (an LLM-as-judge is your own function calling your own model client). The result is an
eval run that stores every item's output, error, latency and scores, and tracks the mean of each
score as the metric `eval/<score>`, so the runs table and the charts work on evaluations unchanged.

## Schedules

A **schedule** runs a command on a cron expression in a time zone: usually a script that calls
`evaluate()`. Schedules are defined only in the server's [`vigily.toml`](guides/configuration.md);
the server never overlaps a schedule with itself, retries, enforces a timeout and keeps the output.
See [scheduled evaluations](guides/scheduled-evals.md) for the security model.

## Drift

Eval runs that share an experiment, a target and a dataset name form a **suite**. Each new run is
compared with a **baseline**: runs you pin, or the last seven runs that did not drift themselves.

- **Behaviour drift**: scores got worse on the same items. A paired test on matched items, an
  effect size with a confidence interval, and the list of items that flipped from pass to fail.
- **Data drift**: the inputs changed. Feature samples compared by PSI (numbers) or Jensen–Shannon
  divergence (categories).

[Drift methods](guides/drift.md) gives the statistics and thresholds.

## Alerts

An **alert** is raised once per drifted run and score or feature, and once per scheduled run whose
last attempt failed. Alerts appear in the web app with a count in the navigation and, when a webhook
is configured, are posted as JSON. You acknowledge them in the app; the alert keeps its delivery
outcome and the run it points to.

## Traces

A **trace** is one request through an LLM application, recorded as a tree of **spans**: the
retrieval, tool and model calls it made, each with its start and end time, status and error, and
attributes. A model call's span also carries the model, the provider, its input, output and cached
tokens, and its **cost**, computed from the price table you configure. A trace can belong to a run
(it is deleted with the run) and to an evaluation item (the item's row opens it). What is stored of
prompts and outputs is set by the **payload mode**: full, truncated to 4 KiB per field (the
default), or metadata only. See [LLM tracing](guides/tracing.md).
