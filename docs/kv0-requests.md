# What vigily needs from kv0

This is my list of kv0 features that vigily needs, with the reason for each. vigily uses kv0's
tracking layer (`kv0::tracking`) through two adapters:
`crates/vigily-core/src/kv0_store.rs` (runs, parameters, series, points) and
`crates/vigily-core/src/records.rs` (datasets, evaluation results, schedules, alerts).

## Status

vigily builds against kv0 `main`, which has every feature below except 7, 23 and 24. Items 16 onward
came up later, while I was building vigily on the first ones. Where vigily uses each:

| Feature | Status | Where vigily uses it |
|---|---|---|
| 1. Bulk reads | Done | The catalog reads all parameters and all series once per refresh |
| 2. Series statistics | Done | The catalog reads stored statistics; start-up reads no points |
| 3. Metadata-only updates | Done | `update_run` from the web app reads no points |
| 4. Times on create and finish | Done | Imported runs keep their times without a second write |
| 5. Change detection | Done | A refresh skips every table whose version did not change |
| 6. Per-field updates | Done | Edits from the web app and a finishing script do not overwrite each other |
| 7. Streaming or Arrow reads | Not done | Points still arrive as vectors |
| 8. Sorted points | Done (in `optimize`) | The background optimize every 15 minutes |
| 9. Automatic compaction | Done | Start-up needs no compaction |
| 10. Kind and target columns | Done | `kind` and `target` are run columns |
| 11. Time-range reads | Done | `Repo.metric(..., start_time=, stop_time=)` in the SDK |
| 12. Records table | Done, then per table (kv0 #11) | `Kv0Records`: one table per kind and one per dataset |
| 13. Deleting runs | Done | Delete in the runs table with an undo window; running runs are refused |
| 14. No resident key-value view | Done | Records are read through indices; nothing is held in memory |
| 15. Named versions | Done, per table | A dataset version's tag keeps only its dataset's table |
| 16. Purge (kv0 #12) | Done | `vigily runs purge [--force]` |
| 17. Several runs per records query, `run_ids` (kv0 #13) | Done | Drift reads a suite's results and features in one query |
| 18. Opening repositories with the old shared records table (kv0 #13) | Done | vigily migrates it once on open and keeps it |
| 19. Tags on tables with escaped names (kv0 #13) | Fixed | vigily names dataset tables `dataset.<name>` anyway |
| 20. Lower memory for `optimize` (kv0 #14) | Done | See docs/architecture.md for `vigily up` memory with and without optimizing at start |
| 21. Folding superseded run rows (kv0 #15) | Done | Heartbeat rows (one every 2 minutes per open run) are folded by the background optimize |
| 22. A cheap check for the old shared records table | Done | `has_legacy_records()`: one directory listing on each open and in `vigily runs purge` |
| 23. Several item ids, or an item-id prefix, per records query | Not done | Traces store spans in one row per batch and keep span ids in a row of their own; see below |
| 24. Batched point writes, `track_many` | Not done | The engine's ingest writes a batch's points one `track` call at a time; see below |

The sections below are the original plans for each feature, kept for their reasoning.

## 1. Bulk reads of parameters and series

**API.** `Repo::all_params() -> HashMap<String, Value>` (run id to nested parameters) and
`Repo::all_series() -> Vec<SeriesInfo>`, each one scan of its table. Optionally both filtered by
`updated since <seq or version>`.

**Why.** vigily's catalog indexes every run's parameters and series. Today that is
`params(run_id)` and `series(run_id)` once per run, and each call flushes, checks the latest
version and scans the whole table with a filter. The first refresh is therefore O(runs × table
size): see the scale test in `docs/architecture.md`.

## 2. Series statistics

**API.** `Repo::series_stats(ids: &[u64]) -> HashMap<u64, SeriesStats>` with `count`,
`first_step`, `last_step`, `last`, `min`, `min_step`, `max`, `max_step`, `non_finite`,
`last_timestamp`. Ideally maintained on flush (a small `series_stats` table, last writer wins per
series) so the call reads one row per series.

**Why.** The runs table shows and sorts by the last, minimum and maximum of every metric. vigily
computes these itself, which means reading every point of every series once when the server starts
(then incrementally). With stored statistics, start-up reads no points at all.

## 3. Metadata-only run updates

**API.** `Repo::update_run(run_id, f: impl FnOnce(&mut RunInfo)) -> Result<RunInfo>` that reads the
current record, applies `f` and appends it, without reading points.

**Why.** Renaming, notes, tags and archiving come from the web app. `resume_run` reads every point of
the run to find its last steps, which is too slow for a click. vigily instead calls
`create_run(NewRun { run_id: Some(id), .. })` and immediately `update(|i| *i = patched)`; both rows
land in one flush and the later sequence number wins. This depends on two internals that are not
part of the API contract: that `create_run` with an existing id is allowed, and that both rows are
committed together.

## 4. Creation and end times on create and finish

**API.** `NewRun::created_at: Option<i64>` and `Run::finish_at(status, ended_at)`.

**Why.** Importing runs from another tracker or from logs needs their real times. vigily sets them
with `update` after `create_run` and before `flush`, which writes an extra row per run.

## 5. Cheap change detection

**API.** `Repo::latest_versions() -> (u64, u64, u64, u64)` that checks storage for the newest
version of each table without scanning (today `versions()` reports the handle's loaded versions).

**Why.** The server refreshes every 2 seconds. With a version check it can skip the refresh when
nothing was committed, which on object stores saves a scan of the runs table every 2 seconds.

## 6. Per-field updates, or compare-and-set, for run records

**API.** Either `update` that merges only the fields it changed (column-level last writer wins), or
`update_if(run_id, expected_seq, f)` that fails on a concurrent change.

**Why.** Run records are whole-row last writer wins. If the training process finishes a run (status
`finished`) between the web app reading the record and writing a new note, the note's row carries the
old status `running` with a newer sequence number, and the run looks like it is running forever.
vigily narrows the window (it reads immediately before writing) but cannot close it.

## 7. Reads that stream or return Arrow

**API.** `read_many` returning Arrow record batches, or a callback per series, instead of
`HashMap<u64, MetricData>` of owned vectors.

**Why.** A chart of 200 runs × 200 000 points is 40 million points; vigily reduces them to a few
thousand per run immediately. Materialising four `Vec`s per series first doubles peak memory.

## 8. Points sorted by series and step

Already planned in kv0's `TODO.md` (sort `metrics` by `(series_id, step)` during `optimize`).
vigily's reads are per series and step range, so contiguous data would make zoomed reads cheap.

## 9. Automatic compaction in the tracking layer

**API.** The tracking layer compacts a table after a flush when it holds more than `max_fragments`
fragments (as the key-value layer does), or a `RepoOptions::auto_optimize` setting.

**Why.** Every `Run::finish` flushes, so each run adds one fragment per table. In vigily's scale
test (2 000 runs logged one after another) commits slowed down as fragments accumulated, so logging
took 27 minutes, and the server's first catalog refresh took 250 to 390 seconds against 10 seconds
after `optimize()`. vigily's server called `optimize()` at start and every 15 minutes, but
training processes that log many runs pay the cost while writing.

## 10. Run kind and target columns

**API.** Two fields on `RunInfo` and `NewRun`: `kind: Option<String>` (`training`, `eval`, ...)
and `target: Option<Value>` (a JSON object: model, prompt, endpoint, git commit). Alternatively one
generic `attrs: Value` column for small run-level metadata.

**Why.** vigily runs carry a kind and a target reference. Without columns they are stored as
reserved parameters (`__run__.kind`, `__run__.target.*`), so listing runs by kind needs the
parameters of every run (item 1) instead of the runs table alone.

## 11. Time-range reads

**API.** `StepRange`-like reads by timestamp (`start_time <= timestamp < stop_time`), and a
`timestamp` index or sort order that makes them cheap.

**Why.** Evaluation and monitoring metrics are compared over wall-clock time (a nightly score over
30 days), and charts zoom on a time axis. vigily reads the whole series and filters in memory.

## 12. A per-item records table

**API.** Versioned, append-only tables of JSON-like rows with a few typed key columns, for example
`records(table, run_id, item_id, version) -> rows` and `append_records(table, rows)`, with the table's
Lance version exposed so a run can record exactly which version of a dataset it read.

**Why.** Evaluations (vigily's next milestone) store datasets (items with input, expected output
and metadata), and per run one output and several scores per item. Metric series are
`(step, value)` pairs and do not fit; a table per dataset or per run would multiply manifests
(kv0's design rejects that for series for the same reason).

## 13. Deleting runs

**API.** `Repo::delete_runs(ids)` that removes the runs' rows from all four tables (with
`cleanup_old_versions` to reclaim space).

**Why.** vigily can archive (hide) runs today; deleting needs storage support.

## 14. Records without a resident key-value view

**API.** Either the typed records table of item 12, or a key-value mode that keeps only keys (or
nothing) in memory and reads values lazily through Lance's random access (already listed in
kv0's `TODO.md`).

**Why.** vigily stores datasets, per-item eval results, labels, feature samples, schedule runs and
alerts in kv0's key-value layer today (`<repo>/records`), because it exists and works on every
backend. Its whole view is resident in memory in every process that opens it, including each
training or evaluation process, so evaluation data is bounded by RAM and every SDK process pays the
load time.

## 15. Named versions (tags)

**API.** Lance tags on a dataset (`create_tag(name, version)`, `checkout(tag)`), exposed by the
key-value layer and by the records table of item 12.

**Why.** vigily records the Lance version after each dataset commit for traceability. Tags would
let a dataset version be read back exactly as committed even after compaction and
`cleanup_old_versions`, which today could remove the version an audit record points at.

## 23. Several item ids, or an item-id prefix, per records query

**API.** `RecordQuery::item_ids: Option<Vec<String>>` (`item_id IN (...)`, using the item-id index,
chunked like `run_ids`), and ideally `RecordQuery::item_prefix: Option<String>`
(`starts_with(item_id, ...)`, served by the btree index as a range).

**Why.** LLM traces (vigily's tracing milestone) are trees of spans that arrive in batches over
seconds or minutes. The natural layout is one row per span, keyed `tspan/<trace>/<span>`, read back
by the trace id. Today a records query matches one item id or a whole table (or runs), so vigily
stores one row per ingested batch (`tspan/<trace>/<n>`), reads a trace with one query per batch, and
keeps every span id in a per-trace row (`tsid/<trace>`) to ignore a span sent twice, which grows
with the trace and is rewritten by every batch. With `item_ids` or a prefix, a trace page is one
query, and a resent span simply supersedes its own row.

## 24. Batched point writes (`track_many`)

**API.** `Run::track_many(name, context, steps: &[i64], values: &[f64], epochs: Option<&[Option<i64>]>,
timestamps: Option<&[i64]>)`: the points of one series in columns, written with one log append and
one buffer lock, the series resolved once.

**Why.** From beta.3 clients send points to the engine in columnar batches (`POST /api/v1/ingest`),
and the engine is the only writer. `RunWriter::track_batch` hands each batch's columns to storage;
today it loops over `track`, which hashes the series, takes the step map and the buffer locks and
appends to the log once per point. For a million points a second across all runs the per-point
cost has to go, and with steps assigned by the client the step bookkeeping reduces to a maximum
per batch.
