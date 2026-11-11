# vigily design

vigily is an experiment tracker: a Python SDK logs runs, parameters and metric series; one Rust
binary serves a web app and a JSON API over them. This document records my decisions, the
guarantees and the alternatives I rejected. [docs/architecture.md](docs/architecture.md) is
the shorter tour of the code; [brand/DESIGN.md](brand/DESIGN.md) and
[docs/design/brief.md](docs/design/brief.md) cover the visual design.

## Goals and non-goals

Goals:

- The everyday experiment-tracking workflow (runs, hyperparameters, metrics with contexts, a runs
  table, a metric explorer, run pages) with few moving parts: one binary, one storage library, no
  database server, no Node at runtime.
- Fast with thousands of runs and series of 100 000 points or more.
- A UI for long sessions: dense, keyboard-first, legible numbers, light and dark.
- Storage anywhere kv0 can write: a local directory, S3, GCS, Azure.

Non-goals for now: images, audio, text and distribution tracking; multi-user accounts; a hosted
service; importing other trackers' repositories.

## Crates

| Crate | Depends on | Why it is separate |
|---|---|---|
| `vigily-core` | kv0 (optional) | The model, queries and chart maths are testable without HTTP or Python |
| `vigily-server` | core, ui, axum | The HTTP surface; tests drive it in memory with `tower::oneshot` |
| `vigily-ui` | nothing at runtime | `build.rs` embeds `dist`, so the binary carries the app |
| `vigily-cli` | server | The `vigily` binary |
| `vigily-py` | core, pyo3 | An abi3 extension (one wheel per platform for Python 3.11+) |

## Storage

### A trait in front of kv0

vigily-core talks to storage through two traits: `Store` (list runs, read parameters, series and
points, create and update runs) and `RunWriter` (track points, set parameters, finish). Two
implementations:

- `Kv0Store` on kv0's tracking layer (feature `kv0`, on by default).
- `MemoryStore` for tests and for embedding.

Why a trait when there is one real backend: kv0 is being rewritten at the same time as
vigily, by another author, and its tracking API is still moving. The trait isolates every change
to one file (`kv0_store.rs`) and lets every other test run without Lance. What vigily needs
from kv0 and does not get yet is in [docs/kv0-requests.md](docs/kv0-requests.md).

### Writes

The SDK writes straight to storage from the training process through kv0, which buffers points
and commits them in batches (every 50 000 points, every few seconds and on `finish()`). There is no
vigily server on the write path: training does not depend on the server being up, and many
processes can log to one repository at once, because kv0's commits are appends that do not
conflict.

Rejected: an HTTP ingest endpoint for remote tracking. It adds a server that must stay up during
training and a second protocol; object-store URIs already give remote logging. It can be added
later for clusters that cannot write to shared storage. Traces are the exception (see "LLM
tracing"): they come from applications rather than training scripts, and their payload policy and
prices belong to the operator.

### Metadata updates from the server and the SDK

Renaming a run, editing notes and tags, and archiving go through `Store::update_run`; the SDK changes
a run's status, end time, name and tags through `RunWriter`. kv0 stores each change as a row that
records which fields it sets, and merges rows field by field, newest first. Each side therefore
writes only the fields it changes: a note saved in the web app while the training script finishes
the run keeps the run finished, and the script's finish keeps the note. `update_run` reads none of
the run's points.

### Run kinds and targets

A run has a `kind` (`training` by default, `eval` for evaluations, any other word later) and an
optional `target`: a free-form JSON object naming what it trains or evaluates, such as
`{"model": "clf-v2", "prompt": "summarise-v3", "endpoint": "...", "git": "4f2c1ab"}`. Both are set
when the run is created, can be filtered (`kind:eval target.model=clf-v2`) and are shown in the runs
table once a repository holds more than training runs.

Both are columns of kv0's run table; `Run::kind()` and `Run::target()` read them. A string
target is stored as `{"ref": "..."}`. Reserved parameters under `__run__` keep only the SDK's
record of where a run came from (the schedule run that started it).

### Deleting runs

`Store::delete_runs` removes runs with their parameters, series, points, statistics and per-run
records (evaluation results, labels and feature samples). The web app asks for confirmation, then
hides the runs and waits eight seconds, with Undo, before it sends the request. Running runs are
refused, in the API and in the app: their script would keep writing to them, so they must finish
or be aborted first.

Deleting is not erasing: older storage versions still hold the data. `vigily runs purge` (command
line only) erases deleted runs through kv0's purge; the storage guide lists what it does not reach.

### Compaction

Every finished run commits a few small files per table. A repository of 2 000 runs logged one after
another holds about 2 000 fragments per table, and every scan opens all of them: the first catalog
refresh took 250 to 390 seconds on the scale test below, against 10 seconds after `optimize`.
kv0's tracking layer now compacts small files itself (kv0 item 9), and the catalog reads no points,
so start-up takes a quarter of a second on that repository without any compaction. `vigily up`
therefore does not optimize before its first refresh (`--optimize-at-start` does; it costs about
3 s and takes memory from 110 MB to about 800 MB at once); it optimizes every 15 minutes in the
background (`--optimize-every`, `--no-optimize`) and returns the freed memory afterwards. The
measurements are in docs/architecture.md, "Memory".
kv0's compaction does not conflict with concurrent appends, so training keeps logging meanwhile.

## The catalog

The runs table filters and sorts on parameters and on the last, minimum and maximum of every metric,
on every keystroke. That cannot touch storage. `Catalog` keeps an in-memory index of every run: its
metadata, flattened parameters and a `Summary` (count, first and last step, last, minimum, maximum
and where they happened, non-finite count) for every series.

A refresh runs every 2 seconds and before requests when the index is older than one second. It
reads each table at most once, in bulk:

- It first asks the store for the versions of its four tables (runs, parameters, series, metrics),
  which kv0 answers without scanning. A table whose version did not change is not read again; when
  none changed, the refresh costs four version lookups.
- Otherwise it reads every run, all parameters, all series and the **stored series statistics**
  (count, first and last step, last, minimum and maximum and where they happened, non-finite count)
  that kv0 updates with every flush. No metric points are read, at start-up or later.

Snapshots are immutable (`Arc<Snapshot>`), so requests never wait for a refresh, and concurrent
refreshes share one run.

## The query language

A search field is the fastest filter for people who know what they want, and a small language keeps
it one field rather than a form. Terms are separated by spaces and must all match; `or` separates
groups; `-` or `not` negates:

- bare words search the name, experiment, tags, id and notes;
- `field op value` compares, with `=`, `!=`, `:` (contains), `<`, `<=`, `>`, `>=`;
- fields are run fields (`status`, `exp`, `tag`, `name`, `duration`, `age`, `archived`), parameters by
  full key or by their last segment (`lr` finds `hparams.lr`), and metrics as `loss[val].min`;
- durations take units (`10m`, `2h`).

A field with several values (tags, a metric in several contexts) matches when any value matches;
`!=` requires every value to differ. A missing field equals `null` and differs from everything else.
Errors report the character position, which the UI shows under the field.

Rejected: Python-expression queries. They are powerful but need a Python evaluator (or a
sandboxed reimplementation) and are slow to type for the common cases.

## Charts

### Library: uPlot

The explorer draws tens to hundreds of runs per chart, several charts at once, with a shared
crosshair, zoom and live updates. The options I considered:

| Option | Why not (or why) |
|---|---|
| SVG libraries (Recharts, visx, Nivo) | One DOM node per point or path segment; slow past a few thousand points and heavy to re-render on hover |
| ECharts, Plotly | Canvas, capable, but 300 KB to 1 MB of JavaScript and their own theming and interaction models to undo |
| WebGL (regl, deck.gl) | Fast for millions of points, but vigily never sends millions: the server downsamples to the pixel width, so the GPU buys little and costs text rendering, accessibility and code size |
| **uPlot** | 50 KB, Canvas 2D, draws ~100 000 points in a few milliseconds, a shared-x data model, cursor sync between charts, bands, log scales, and hooks to draw on top |

uPlot's aligned-data model (one shared x array, `null` for gaps) shapes the server's chart endpoint.

### Downsampling: a min/max envelope on a shared grid

`POST /api/chart` reads a metric for many runs and returns them on one x array, at about two values
per pixel column:

- **Exact** when the distinct x values of every run in the visible range fit the budget: their union,
  with gaps.
- **Envelope** otherwise: the range is cut into one bucket per pixel with two slots each, and every run
  puts its minimum and maximum in the bucket into the slots, in the order they occurred.

A line through the envelope covers the same pixels as the raw data, so spikes, NaN gaps and
divergence stay visible. Rejected: LTTB and averaging, which pick or blend points and can hide the
one spike that explains a failed run; per-run M4 (four points per bucket), which does not align runs
onto one x array.

Zooming sends the visible range back; the server re-samples it at full resolution, keeping one point
beyond each edge so lines run to the frame. Finished series are cached raw in the server (bounded at
8 million points) so zooming does not re-read storage.

### Aggregation

Grouping by a parameter can collapse each group into a mean with a min–max band. Runs of a group
rarely log at the same steps (different batch sizes, evaluation intervals), so each run is first
reduced onto the shared grid, its empty slots inside its own range filled by linear interpolation,
and only then are the mean, minimum and maximum taken. Without the interpolation the mean jumps
between subsets of the group and draws a sawtooth.

### Smoothing, outliers, the readout

- **Smoothing** is a debiased EMA (as TensorBoard draws it) applied in the browser to the downsampled
  line, so the slider is instant. With few runs the raw line stays visible, faint, under the smoothed
  one.
- **Ignore outliers** (on by default) fits the y axis to the 2nd to 98th percentile of the values
  when a far outlier, typically a diverged run, would otherwise flatten every other curve. The
  outlier still leaves the frame visibly.
- The **readout** lists every run's value at the cursor, sorted best first (highest for names like
  accuracy, lowest otherwise). It doubles as the legend, so identity never depends on colour.

### Colour

One accent and neutrals. Every run is drawn in a neutral grey; the run under the pointer and pinned
runs are redrawn on top in the accent; a single-run chart draws its run in the accent. Groups are
told apart by line dash (solid, dashed, dotted, dash-dot) and named in the readout. Rejected: a
categorical palette. With 30 runs any palette repeats, adjacent hues cannot be told apart by
colour-blind readers, and the run you care about should be the one that stands out, not the one that
drew the brightest hue.

## Evaluations

### Records

Evaluations need rows per dataset item, which metric series (`(step, value)` pairs) cannot hold. They
live in a [`RecordStore`](crates/vigily-core/src/records.rs): JSON values under ordered string
keys, written in batches and read by key or prefix. `evals.rs` owns the key layout (`ds/`,
`dsv/`, `dsi/`, `res/`, `lab/`, `feat/`, `sch/`, `alert/`, `baseline/`); nothing else builds keys.

The production backend is kv0's **records tables** (`<repo>/records/<table>.lance`), next to the
tracking tables. Each table is its own Lance dataset with its own versions and tags, chosen by
lifecycle: one per dataset (`dataset.<name>`, its items and versions), and one per other key prefix
(`res`, `lab`, `feat`, `sch`, `schout`, `alert`, `ds`, ...). The whole key is the item id; results,
labels and feature samples also carry their run id, so a run's records are read through kv0's
run-id index and deleted with the run. Rows are appended, so many processes can write; the newest
row of a key wins and a null row deletes it, and kv0 drops superseded rows when it optimizes.
Nothing is held in memory. Rows of one table are committed together; a batch that spans tables is
committed table by table in order, so callers write referenced rows (items, outputs) before the
rows that point at them.

Giving each dataset its own table matters for audit tags: a tag keeps one table's version through
cleanup and purge, so tagging a dataset version keeps only that dataset's history, never other
runs' results, and never blocks erasing a deleted run.

Repositories from before this layout kept every record in one shared table (`records.lance`);
vigily copies it into the new tables once on open and writes a marker (`meta`), and leaves the old
table for the operator to delete.

### Datasets and versions

A dataset version is immutable and content-addressed. An item's hash (SHA-256, 16 hex digits) covers
its input, expected output and metadata in canonical JSON; items are stored once per hash and shared
between versions. A version lists its item ids and hashes in order, and its digest hashes that list.
Committing the same items again returns the existing version. Each version also records the Lance
version of the record store after its commit and tags that version (`ds-<name>-<hash>-v<n>`), so
version cleanup keeps it and an audit record can cite exactly what was stored when it was evaluated.

Rejected: using Lance versions alone as dataset versions. A Lance version covers the whole records
dataset (every dataset, result and label), so it says nothing about one dataset changing; content
hashes do, and they survive compaction and copying.

### Evaluators and targets

An evaluator scores one output against one item and returns a value, a pass or fail, and a reason.
Built-ins are versioned by vigily (`vigily/exact_match@1`); a custom evaluator's version is the
one given, or a hash of its source, so editing a judge prompt inside the function changes the version
recorded with every score. An LLM-as-judge is user code that calls the user's model client: the
server never holds model API keys and never runs inference.

`evaluate()` runs in the user's process, writes each item's output, error, latency and scores as
records, and tracks the mean of each score as the metric `eval/<score>`, so the runs table, the
explorer and drift charts work on evaluations unchanged. An eval run is an ordinary run with
`kind = "eval"` and a `target` reference.

### Schedules

The server runs commands from its configuration file on cron expressions in a time zone (stored and
compared in UTC). The scheduler never overlaps a schedule with itself, applies a missed-run policy
after downtime (`skip`, or `run_once` for the latest missed fire), retries, kills a timed-out
command's whole process group, and keeps the last 64 KiB of stdout and stderr. The command gets
`VIGILY_SCHEDULE*` variables, so runs it logs link back to the schedule run.

**Security model.** Commands run as the user that runs vigily, with its permissions and
environment. They come only from the configuration file, written by the operator; the API and the web
app can read schedules and their output but cannot create, change or start them. Starting a schedule
by hand is a shell command (`vigily schedules run NAME`), so it needs shell access.

**Leftover processes.** A command runs in its own session. When it exits or times out, its process
group and, on Linux, any process that still carries the attempt's `VIGILY_SCHEDULE_RUN` (which
catches processes that called `setsid`) get SIGTERM, then SIGKILL two seconds later; output readers
stop two seconds after that. The group leader is not reaped until then (`waitid` with `WNOWAIT`),
so neither its process id nor its group id can be reused by an unrelated process in between, and
attempt ids carry a random part and are matched together with the schedule name. Commands must not start long-lived background processes. A process
that both starts a new session and clears its environment can still escape; containing that needs
cgroups, which vigily does not manage.

**Retention.** Each schedule keeps its newest `keep_runs` runs (200) younger than `keep_days` (90)
and the output of the newest `keep_output` (50). Output lives under separate keys, so listings do not
load it. Runs left running by a server that stopped are marked interrupted at the next start.

### Drift

A **suite** is the eval runs that share an experiment, a target reference and a dataset name.
Behaviour drift compares one run's per-item scores with its baseline runs', item by item:

- pairing: items match by id and content hash; each item's score in the run is paired with the
  same item's mean over the baseline runs. The baseline runs repeat the same items, so pooling
  their scores as independent samples (as the first version did) overstated the baseline's
  precision and inflated z by about 1.3 for a 7-run baseline;
- test: Wilcoxon signed-rank on the per-item differences, two-sided, zeros dropped, with tie and
  continuity corrections (checked against scipy);
- effect: the mean per-item difference with a 95 % bootstrap interval that resamples items;
- fallback: with fewer than 10 matched items (the dataset changed), a Mann–Whitney U test of the
  run's scores against the baseline's per-item means;
- decision: drift when the run is worse, the difference is at least `min_effect` (0.02) and
  `p < alpha` (0.01);
- regressions: the items (same id and content) that passed in the last baseline run and fail now.

The baseline is either pinned runs or **the last 7 runs that did not drift themselves**. Without that
exclusion a lasting regression becomes the baseline within a week and stops being reported; with it,
a regression keeps being reported until someone fixes the target or pins a new baseline. This needs a
forward pass over a suite's history, which also gives the chart its per-run verdicts.

Non-finite scores are left out. Data drift compares a run's sampled input features (up to 5 000 values each) with its baseline runs'
pooled samples: numeric features by PSI on baseline deciles (stable < 0.1 ≤ moderate < 0.2 ≤ drift)
with a Kolmogorov–Smirnov test (asymptotic p-value) for reference, categorical features by Jensen–Shannon divergence
(base 2; stable < 0.05 ≤ moderate < 0.1 ≤ drift) with a chi-squared test for reference. With
thousands of samples every test is significant, so effect sizes decide. All statistics are
implemented in [`stats.rs`](crates/vigily-core/src/stats.rs) and tested against reference values,
so every number in the UI traces to one formula.

### Alerts and the audit trail

The server checks each suite's newest run every 30 seconds, and again whenever its number of stored
results or feature samples changes (an evaluating process may still be writing them). It raises an
alert once per drifted run and score or feature, and once per scheduled run whose last attempt
failed. A failure in one suite is logged and does not stop the others. `evaluate()` writes all
results and features to storage before it marks the run finished. Webhook deliveries are retried
twice, do not follow redirects, and record the outcome without overwriting an acknowledgement. Alerts appear in the web
app and, when `alerts.webhook` is set, are posted there as JSON.

Every eval result traces back to its dataset version and digest, item hash, target reference,
evaluator name and version, schedule and schedule run, and times in UTC. The run page shows this and
exports it as one JSON document (`GET /api/runs/{id}/audit`). This supports record keeping for
monitoring deployed models (for example post-market monitoring under the EU AI Act); vigily makes no
claim that using it satisfies any legal requirement.

## LLM tracing

### Data model

A trace is a tree of spans sharing a trace id. A span has a span id, a parent id, a name, a kind
(`llm`, `tool`, `retrieval`, `chain`, `embedding`, `agent` or another short word), start and end
times in UTC microseconds, a status (`ok`, `error`, `unset`), an error message, and JSON
attributes; a model call also has a model, a provider, an input, an output, token usage (input,
output and cached input tokens) and a cost. A trace optionally belongs to a run and to an
evaluation item, and to a project (its own `project`, else its run's experiment) that selects the
payload policy.

### Ingest through the server

Unlike metrics, spans are sent to `vigily up` (`POST /api/traces/ingest`, or OTLP/HTTP JSON at
`POST /v1/traces`) rather than written to storage by the SDK:

- the payload policy protects personal data, so it must be the operator's (`vigily.toml`), applied
  in one place, not a setting each application could forget;
- prices are the operator's too, and cost is computed once, at ingest, and stored with its currency;
- traces come from applications and services, which should not need storage credentials, and an
  OpenTelemetry exporter can only speak HTTP.

The SDK queues finished spans and a background thread sends batches (256 spans or one second), so a
traced call never waits on the network; an `atexit` hook sends what is queued. When the server is
down, batches are retried twice and dropped with one warning: tracing must never break the
application. Rejected: the SDK writing spans to storage itself, as it does metrics, for the reasons
above; and OTLP protobuf, which needs a protobuf dependency for little gain (exporters can send
JSON).

### Storage layout

Two kv0 records tables:

- `trace`: one summary row per trace (`trace/<id>`): name, times, status, span and error counts,
  token totals, cost and currency, models, payload mode, run, evaluation item, project. The traces
  list reads this table only, cached until its version changes.
- `tspan`: the spans, one row per ingested batch (`tspan/<id>/<n>`), so a batch is appended, never
  rewritten. The trace page reads a trace's chunks by key (the summary counts them).

Both rows carry the trace's run as their kv0 run id, so kv0's run index lists a run's traces and
`delete_runs` and `purge` remove them with the run; keys do not name the run, so a trace is found by
its id alone. The server serialises ingests (one lock), because a batch reads and rewrites its
trace's summary. A third table, `tsid`, lists each trace's stored span ids, so a span sent twice (a
retried batch) is counted once without the summary growing with the trace. When a later batch first
names the run, the rows stored before are rewritten under it (the old rows are superseded, and
erased by the compaction `vigily runs purge` runs first). One row per span would make the id list
unnecessary and let the page read any span by key, but needs reads of several item ids or an
item-id prefix in one query (kv0 item 23).

Until a trace's project is known (no project and no run named yet, typical for OpenTelemetry
children that arrive before their root), the strictest configured payload policy applies, so a
project's stricter policy can never be undercut by spans that arrived early. The ingest endpoints
accept only `application/json` and refuse foreign `Origin`s: a web page can send `text/plain`
POSTs to localhost without a preflight, and would otherwise plant spans.

### Payload policy and cost

`full`, `truncated` (the default: each input, output, error message and string attribute cut to
4 KiB) or `metadata` (no inputs, outputs or string attributes; error messages keep their type),
globally and per project, applied at ingest and stored with each span so the trace page can say what
was kept. Truncation keeps the default useful for debugging while bounding what is kept; it is not
anonymisation, which the docs say plainly. The SDK's redaction hook runs before a span leaves the
process; if it raises, payloads are dropped rather than sent. Cost is `(input − cached) × input +
cached × cached_input + output × output` per million tokens, from exact model names or `prefix*`
patterns. There is no built-in price list: prices change and differ by contract.

## Web app

React 19, Vite, Tailwind v4 with every colour a token in both themes, Radix primitives for dialogs,
popovers, tabs and tooltips, cmdk for the palette, TanStack Query for data and TanStack Virtual for
the runs table. The font (Recursive, Sans and Mono from one variable file) is bundled with fontsource: no CDN, so
the app works offline and air-gapped. The theme class is set before first paint.

State lives in the URL (filter, sort, columns, chosen metrics, chart settings), so every view can be
shared and Back restores it. The selected runs are kept per browser tab. Everything the pointer can
do the keyboard can: `/`, `j`, `k`, `x`, `c`, `Enter` on runs; arrows, `Home`, `End` and `Escape`
on a focused chart; `Ctrl K` everywhere.

## Server

axum on Tokio. The API is read-mostly JSON; responses are gzip-compressed. Series ids are 64-bit and
travel as strings (JavaScript numbers lose them). `vigily up` binds to `127.0.0.1` and rejects
requests whose `Host` is not a loopback name, so a DNS-rebinding page cannot read runs. There is no
authentication: vigily is a single-user tool, and a shared deployment belongs behind the
operator's own proxy.

## Python SDK

A pyo3 extension (abi3, Python 3.11+) under a thin Python layer. Values cross the boundary as JSON
text; contexts are cached per distinct dict, so `track` costs one Rust call. The GIL is released
around every call that can do I/O. The SDK records the environment (Python, platform, CPU count,
vigily version, git branch and commit) under the reserved parameter `__system__`; it records no
user names, host names or paths. Metrics named `__system__/...` are shown on the run's System tab.

## Measured

On a 20-core Linux workstation with a local SSD:

- `examples/demo.py`: 43 runs, about 1 million points, logged in 3 to 12 seconds.
- On the demo (debug build), the runs query answers in about 9 ms.
- 2 000 runs with 4.8 million points (`bench/scale.py`, release build, kv0 with its tracking additions): logged
  one after another in 107 s; start-up 0.27 s (3.41 s when optimizing first, which the server no
  longer does); the
  runs table 7 ms, a filtered and sorted query 3 to 5 ms, a 200-run chart 61 to 103 ms cold and
  6 to 17 ms cached or zoomed; 244 MB resident (about 1 GB right after a full optimize). Details in
  [docs/architecture.md](docs/architecture.md). Before kv0's tracking additions the same test
  took 250 to 386 s to start (10.4 s compacted) and 27 minutes to log.

## Project decisions

- MIT licence.
- Port 7600 by default.
- kv0 is a git dependency on its `main` branch, pinned by `Cargo.lock`, rather than a path to a
  sibling checkout: a clone builds with no extra steps and the pinned commit is reviewable. Local
  co-development uses a `[patch]` override (CONTRIBUTING.md). I built the kv0 features listed in
  `docs/kv0-requests.md` on kv0's `tracking-v2` branch and merged them into `main`.
- TypeScript 6.0 rather than 7: typescript-eslint does not support 7 yet.
