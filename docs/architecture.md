# Architecture

A tour of the code. The reasons behind the choices are in [DESIGN.md](https://github.com/sayef/vigily/blob/main/DESIGN.md).

```text
training script ── vigily (Python) ── vigily-py (pyo3) ─┐
                                                            ├── vigily-core ── kv0 ── Lance datasets
browser ── vigily-ui (embedded) ── vigily-server (axum) ─┘
```

## vigily-api

The JSON of the HTTP API: the run model (`model.rs`), the catalog's series and key lists, chart
requests and answers, evaluation, drift and trace types, and the answer of every route. It holds
data types only and depends on serde alone, so a client can use it without the storage engine.
`vigily-core` re-exports these types under their old paths. `tests/fixtures` holds one golden JSON
file per answer, captured from the server; `tests/wire.rs` reads each into its type and writes it
back unchanged, keys in the same order.

## vigily-core

| Module | What it holds |
|---|---|
| `model.rs` | Re-exports the run model from `vigily-api`: `Run`, `RunStatus`, `RunPatch`, `Series`, `Points`, `Summary`, metric keys (`loss[val]`), JSON flattening |
| `store.rs` | The `Store` and `RunWriter` traits; `RunWriter::track_batch` writes one series' points given in columns (a loop over `track` until kv0 has `track_many`, docs/kv0-requests.md item 24) |
| `memory.rs` | `MemoryStore`, used by the tests |
| `kv0_store.rs` | `Kv0Store`, the adapter to `kv0::tracking` |
| `catalog.rs` | `Catalog` and `Snapshot`: the in-memory index of runs, flattened parameters and series summaries; experiments, metric and parameter key lists; `select` (filter and sort); column ids |
| `query.rs` | The run query language: tokenizer, parser, evaluation against a `RunView` |
| `chart.rs` | X axes, `align` (exact union or min/max envelope on a shared grid), `aggregate` (mean, min, max per group) |
| `records.rs` | The `RecordStore` trait; `MemoryRecords`; `Kv0Records` on kv0's records table |
| `evals.rs` | Datasets and versions (with idempotent commits, `idem/dataset/<key hash>`), eval results, labels, feature samples, schedule runs and state, alerts, baselines; the key layout |
| `stats.rs` | Mann–Whitney U, bootstrap intervals, PSI, Kolmogorov–Smirnov, chi-squared, Jensen–Shannon |
| `drift.rs` | Suites, behaviour drift and data drift as forward passes over a suite's history |

Column ids name what the runs table shows and sorts by: `run:name`, `param:hparams.lr`,
`metric:min:loss[val]`. A metric key is the metric name plus its context values in key order.

## vigily-server

`router()` builds the axum app; `serve()` refreshes the catalog, starts the 2-second refresh loop
and listens. Handlers are thin: parse, take a snapshot, call core, serialise.

Every route lives under `/api/v1`. The routes of earlier releases (`/api/meta`, `/api/runs`, ...)
answer as aliases of the same handlers, with `Deprecation: true`, a `Link` header naming the
`/api/v1` successor, and the flat `{"error": "..."}` body they always had; they go away before
0.1.0. On `/api/v1` every error is `{"error": {"code", "message", "position"?}}` with a code from
`not_found`, `invalid_argument`, `conflict`, `run_deleted`, `unauthorized`, `forbidden`,
`too_large`, `overloaded` or `internal`, including axum's own rejections (malformed JSON, a body
over the limit).

| Endpoint | Notes |
|---|---|
| `GET /api/v1/health` | `ok` |
| `GET /api/v1/info` | `{version, api_version: "v1", mode: "local", capabilities}` |
| `GET /api/v1/meta` | counts, experiments, metric keys with run counts, parameter keys with kinds and distinct values |
| `GET /api/v1/runs?q=&sort=&desc=&archived=&limit=&offset=&fields=` | matching rows, a page of them with `limit` and `offset`; `total` counts every run, `matched` the matching ones; `fields=summary` leaves out parameters and metric statistics; a query error is a 400 with `error.position` |
| `GET /api/v1/runs/{id}/params` | the parameters as a nested object |
| `GET /api/v1/runs/{id}/series` | the series with their statistics |
| `GET /api/v1/runs/{id}/points?name=&context=&start_time=&stop_time=` | one series' points (`context` is a JSON object; times in UTC microseconds); NaN and infinities are the strings `NaN`, `Infinity`, `-Infinity` |
| `POST /api/v1/admin/purge` | `{force}` → what was erased, with `legacy_records_present`; a tag that keeps deleted data is a 409 `tag_blocks_purge` |
| `POST /api/v1/admin/optimize` | compacts storage; `{seconds}` |
| `GET /api/v1/runs/{id}` | the row plus the parameter tree and series with summaries |
| `PATCH /api/v1/runs/{id}` | `name`, `experiment`, `description`, `tags`, `archived` |
| `POST /api/v1/chart` | `{runs, metric, x, range, points, group_by, aggregate}` → `{x, series[{run_id, name, group, y, last}], groups[{label, runs, mean, min, max, n}], domain, exact, missing}` |

Evaluation endpoints (`evals_api.rs`): `GET /api/v1/datasets`, `GET /api/v1/datasets/{name}`,
`GET /api/v1/runs/{id}/items`, `POST /api/v1/runs/{id}/labels`, `GET /api/v1/runs/{id}/audit`,
`GET /api/v1/compare?a=&b=`, `GET /api/v1/schedules`, `GET /api/v1/schedules/{name}/runs[/{id}]`,
`GET /api/v1/suites`, `GET /api/v1/suites/{key}`, `PUT /api/v1/suites/{key}/baseline`,
`GET /api/v1/suites/{key}/data-drift`, `GET /api/v1/alerts`, `POST /api/v1/alerts/{id}`, and, new with
`/api/v1`, `GET /api/v1/datasets/{name}/versions`, `GET /api/v1/datasets/{name}/versions/{v}/items`
(NDJSON, one item per line), `GET /api/v1/runs/{id}/results` and `POST /api/v1/schedules/{name}/run`
(202 with the first attempt's id, followed at `GET /api/v1/schedules/{name}/runs/{id}`; 409 while
the schedule is running). The scheduler
(`scheduler.rs`), drift monitor and webhook (`monitor.rs`) and the configuration file (`config.rs`)
start with `serve()`.

Tracing endpoints (`traces_api.rs`, model and storage in `vigily-core/src/traces.rs`):
`POST /api/v1/traces/ingest`, `POST /v1/traces` (OTLP/HTTP JSON), `GET /api/v1/traces?run=&model=&status=&since=&until=&q=`,
`GET /api/v1/traces/{id}`, `DELETE /api/v1/traces/{id}`, and `POST /api/v1/traces/delete`
(`{ids, erase, force}`) and `GET /api/v1/tracing/policy?project=` (the payload policy a client applies
before sending, and the one used while a trace's project is unknown).

Write routes for clients (`write_api.rs`, write state in `writers.rs`):

| Route | Answer |
|---|---|
| `POST /api/v1/runs` | `{id, name?, experiment?, description?, tags?, created_at?, kind?, target?}` with a client-chosen UUIDv7 (32 hex digits) → `{created, run}`; 201 when created, 200 with the existing run (unchanged) when the id exists |
| `POST /api/v1/runs/{id}/resume` | opens the run for writing → `{run, params, series[{name, context, key, last_step}]}` |
| `POST /api/v1/runs/{id}/finish` | `{status, ended_at?}` → the run; results and feature samples are flushed to storage first |
| `POST /api/v1/runs/{id}/results` | `{results: [...]}` → `{stored}`; every result names this run |
| `POST /api/v1/runs/{id}/features` | `{samples: [...]}` → `{stored}` |
| `POST /api/v1/datasets/{name}/versions` | `{items, mode?, note?, description?, source?}` with a required `Idempotency-Key` → the version (201), or the version that key committed (200, `Idempotent-Replayed: true`); keys are kept 24 hours as records |
| `POST /api/v1/ingest` | one batch `{writer, seq, ops}` (JSON) or one per line (`application/x-ndjson`) → the batch's report, or `{batches: [...]}` |
| `POST /api/v1/flush` | commits buffered writes and refreshes the catalog → `{refreshed_at}` |

An ingest batch is an ordered list of operations: `create`, `params`, `points` (one series in
columns: `steps`, `values`, optional `epochs` and `timestamps`; NaN and infinities as strings),
`update`, `heartbeat`, `finish`, `results` and `features`. The engine applies a batch's operations
in order on a blocking thread, one batch per client writer at a time, and stops at the first
failure, naming the operation (`operation 3 (points): ...`); a write to a deleted run is a 410
`run_deleted`. Steps are assigned by the client, so a resent batch replaces points rather than
adding them. On top of that the engine remembers, in memory, each writer's highest `seq` with its
latest reports and recent `Idempotency-Key`s (15 minutes), and answers a replay with the first
report (`replayed: true`) without applying it again.

Open run writers live in a registry, closed when a run finishes or is deleted, after 15 minutes
without writes, or least recently used beyond 4096. Any operation on a run counts as its
heartbeat; the engine stores at most one heartbeat row per run every two minutes. After a write
the background catalog refresh runs at once instead of waiting for its 2-second tick.

Write bodies may be gzip-compressed (`Content-Encoding: gzip`) and are limited to 32 MiB after
decompression (413 `too_large`). Ingest bodies in flight share a 256 MiB budget; above it ingest
answers 503 `overloaded` with `Retry-After: 1`. Bodies over 1 MiB are decoded off the
request-serving threads.

Routes that write outside the web app's forms (the write routes above, span ingest, admin,
deleting traces, starting a schedule) accept only `Content-Type: application/json` (ingest also
`application/x-ndjson`; resume and flush also no body) and refuse an `Origin` other than the
server's own, so a web page cannot trigger them. Routes added with `/api/v1` have no `/api` alias.

Raw points of finished series are kept in a bounded cache (8 million points), so zooming re-samples
without reading storage. Anything that is not `/api/...` or `/v1/traces` is the embedded web app; client-side routes
get `index.html`.

## vigily-ui

| Path | What it holds |
|---|---|
| `src/index.css` | Tailwind v4 tokens for both themes (see `brand/DESIGN.md`), uPlot and browser-surface theming |
| `src/lib/api.ts` | API types and the fetch client |
| `src/lib/columns.ts` | Runs-table column definitions and the default column choice |
| `src/lib/format.ts` | Numbers (four significant digits, integers in full for parameters), durations, times |
| `src/components/metric-chart.tsx` | The chart: uPlot, the readout, zoom, smoothing, outliers, keyboard |
| `src/components/query-input.tsx` | The filter field: completion, syntax help, error position |
| `src/pages/*.tsx` | Runs, Metrics, Run, Experiments |
| `scripts/qa.mjs` | Screenshots in both themes at two widths, computed checks, a keyboard-only pass |

`build.rs` embeds `dist/` into the binary; build the app before cargo.

## vigily-py

`src/lib.rs` is the native module `vigily._native` (`Repo`, `RunHandle`); values cross as JSON
text. `python/vigily/__init__.py` is the public API: `Run` (parameters by item assignment, `track`,
metadata properties, `finish`, context manager, finishing at exit) and `Repo` (reading back). One
native repository handle is shared per location and process.

## Tests

- `cargo nextest run`: core unit tests (model, query, chart, catalog against `MemoryStore`, and a
  round trip through Lance in a temporary directory) and server tests that drive the router in memory.
- `uv run --no-sync pytest`: the SDK against the built extension, including exit handling in
  subprocesses.
- `npm run qa` in `crates/vigily-ui`: the browser pass against a running server.

## Scale test

`bench/scale.py` logs synthetic runs; then time the server:

```bash
uv run --no-sync python bench/scale.py --runs 2000 --points 2000 --repo /tmp/vigily-scale
vigily up --repo /tmp/vigily-scale --port 7601
```

`bench/scale.sh` starts the server and times the API (`UP_ARGS=...` passes options to
`vigily up`). 2 000 runs in 20 experiments, three series each (`loss[train]` with 2 000 points,
`loss[val]` and `accuracy[val]` with 200), 4.8 million points, about 1.1 GB on disk. Release build,
20-core Linux workstation, local SSD. Logging, start-up and memory with kv0 8c33ef2 (the
tracking additions, now merged into kv0 `main`); request times with d2fd932:

| | Start as logged (default) | Optimizing before the first refresh (earlier default) |
|---|---|---|
| Logging the 2 000 runs, one after another | 107 s | |
| Start-up (first catalog refresh) | 0.27 s | 3.41 s (optimize included) |
| `GET /api/meta` | 0.002 s | 0.002 s |
| `GET /api/runs`, all 2 000 rows | 0.008 s | 0.007 s |
| `GET /api/runs`, filtered and sorted by a metric | 0.003 s | 0.005 s |
| `POST /api/chart`, 200 runs, cold | 0.103 s | 0.061 s |
| `POST /api/chart`, 200 runs, cached | 0.016 s | 0.017 s |
| `POST /api/chart`, 200 runs, zoomed | 0.006 s | 0.006 s |
| Server memory (RSS) | 244 MB | 1 019 MB (right after optimizing) |

The catalog reads each table once and the stored series statistics, and no metric points, so
start-up no longer grows with the number of points. kv0 compacts small files in the background
while runs are logged, which keeps logging at a steady rate (100 s for this repository with kv0
3a88fe0).

### Memory

A full optimize (sorting points by series, indexing them, folding superseded run rows) makes cold
charts faster and keeps the runs table small, but it is the most memory-hungry thing the server
does. Measured on the repository above, release build, kv0 3a88fe0 (with its lower-memory optimize),
resident memory of `vigily up`:

| | glibc allocator | glibc, memory returned after each optimize (default) | mimalloc |
|---|---|---|---|
| After start-up | 109 MB | 110 MB | 348 MB |
| After the first background optimize | 907 MB | 706 MB | 574 MB |
| After three optimizes | 911 MB | 707 MB | 514 MB |
| Peak during an optimize | 907 MB | 1 009 MB | 990 MB |
| With `--optimize-at-start`: start-up time, then memory | 3.3 s, 901 MB | 3.2 s, 773 MB | 3.2 s, 836 MB |

Decisions, from these numbers:

- **No optimize at start** (the default): start-up is 0.28 s instead of 3.2 s, and the server stays
  near 110 MB until the first background optimize, 15 minutes later. `--optimize-at-start` brings
  it back for anyone who wants indexed charts from the first request.
- **Memory is returned after each optimize** (`malloc_trim` on Linux with glibc): the server settles
  at about 700 MB instead of keeping the 900 MB it reached. mimalloc settles lower still (about
  520 MB) but costs 240 MB more at rest, so the default stays glibc.
- The rest (about 600 MB above rest) is kv0's and Lance's caches after an optimize; noted as
  kv0 work. `--optimize-every` sets the interval (default `15m`) and `--no-optimize` turns it off, for
  small machines.

Before kv0's tracking additions (per-run reads, statistics computed from every point, no automatic
compaction), the same repository took 250 to 386 s to start uncompacted and 10.4 s after
`vigily optimize`, and logging it took 27 minutes.
