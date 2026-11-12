# Changelog

All notable changes to vigily are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

The first release, published as betas (0.1.0-beta.N) before 0.1.0.

### Added

- LLM tracing. `vigily.trace(...)` (a context manager, `async with`, and a decorator) records nested
  spans through `contextvars`, with `span.set_usage(...)` and an opt-in `usage_from(response)` that
  reads common client responses without importing them. Spans are exported in batches from a
  background thread and sent on normal exit. `vigily up` ingests them at `POST /api/traces/ingest`
  and accepts OpenTelemetry OTLP/HTTP JSON at `POST /v1/traces`.
- Cost from a price table in `vigily.toml` (`[tracing.prices]`, per million input, output and
  cached input tokens, in a configured currency), computed when a span arrives. No prices are
  built in.
- Payload modes for traces, global or per project: `full`, `truncated` (the default, 4 KiB per
  field) or `metadata` (no inputs or outputs); a redaction hook (`vigily.tracing.set_redactor`).
  Traces linked to a run are deleted with it and erased by `vigily runs purge`, which now
  compacts first so deleted traces without a run are erased too; `vigily traces delete [--erase]`.
  Until a trace's project is known the strictest configured policy applies; truncated spans are
  capped at 64 KiB in total and 128 attributes.
- Purging compacts first inside the storage layer, so every caller erases trace rows moved under a
  run; an interrupted link of a trace to its run is repaired on the next read or ingest; the trace
  page says when a span's payload was limited because its project was not known yet.
- The ingest endpoints accept only `application/json` and refuse other origins, so a web page
  cannot post spans to a local server.
- Traces in the web app: a Traces page under Track with filters (run, model, status, time),
  latency, tokens and cost; a trace page with a span waterfall, the selected span's inputs, outputs
  and attributes, and token and cost totals; links from runs and evaluation items.
- The demo generator logs synthetic support-assistant traces.

- A redesigned web app: warm paper neutrals and ink rules, a sidebar grouped Track / Evaluate /
  Workspace with counts, a top bar with breadcrumbs and search, summary figures, dense tables with
  loss sparklines, and a solid ink selection bar. Light and dark themes; a drawer on phones.
- Compare runs (`/metrics/compare?runs=...`): selected runs keep their letters A, B, C, D, with a
  headline chart labelled at the line ends, a supporting chart and only the settings that differ.
- Drift suites show the score per run with its baseline band, a strip per score and the items that
  flipped from pass to fail. A Settings page shows the repository and the browser's preferences.
- The documentation site in the same paper design: a masthead over an ink rule, a home page that
  opens with the point and numbered figures, ruled sections, a margin column with the page contents,
  and a Copy page button.
- `vigily up --optimize-every <duration>` and `--optimize-at-start`; the server returns memory to
  the system after each background optimize.
- `vigily runs purge [--force]`: erases deleted runs from storage and reports what it removed.
- Evaluation records in one storage table per kind (results, labels, schedules, alerts, ...) and
  one per dataset, so a dataset version's tag keeps only that dataset. Repositories with the
  earlier shared records table are migrated once on open; the old table is kept for you to delete.
- A documentation site (mkdocs-material) and brand assets (mark, wordmark, banners, social
  preview) built reproducibly by `assets/src/build.py`.
- Datasets: content-addressed, immutable versions; `Dataset.create`, `from_jsonl`, `from_csv`, `add`.
- Evaluations: `evaluate()` with exact match, contains, regex, numeric, JSON schema and custom
  evaluators (including LLM-as-judge as user code); per-item results, human labels, run comparison
  and a JSON audit export.
- Scheduled evaluations from `vigily.toml`: cron with time zones, no overlap, missed-run policy,
  retries, timeouts, captured output; `vigily schedules [run NAME]`.
- Drift: behaviour drift (Mann–Whitney U, bootstrap interval, per-item regressions) and data drift
  (PSI, Kolmogorov–Smirnov, Jensen–Shannon, chi-squared) against rolling or pinned baselines.
- Alerts for drift and failed schedules, with an optional webhook.
- Web app: Datasets, Drift, Schedules and Alerts pages; Items and Audit tabs on eval runs; Compare.
- `examples/eval_demo.py`: 30 synthetic nights of two targets with injected drift.

- `/api/v1` routes for clients other than the web app: the runs list pages with `limit` and
  `offset`, answers `matched` and takes `fields=summary`; a run's parameters, series and the points
  of one series (with a time window; NaN and infinities as strings); dataset versions and a
  version's items as NDJSON; an eval run's results; `POST admin/purge` (with
  `legacy_records_present`, and a 409 `tag_blocks_purge` when a storage tag keeps deleted data) and
  `POST admin/optimize`; `POST traces/delete` for several traces, optionally erasing them; `POST
  schedules/{name}/run`, which answers 202 with the run's id at once; and `GET tracing/policy`.
  Admin routes accept only JSON from the server's own origin, like span ingest.
- A write path for clients over HTTP: `POST /api/v1/runs` creates a run with an id the client
  chooses (creating it again answers the existing run), `POST runs/{id}/resume`, `finish`,
  `results` and `features`, `POST datasets/{name}/versions` (with a required `Idempotency-Key`,
  remembered for 24 hours), `POST flush`, and `POST ingest`: ordered batches of operations with
  points in columns, as JSON or NDJSON, optionally gzip-compressed. A replayed batch is answered
  with its first report; a batch resent after an engine restart stores nothing twice because
  clients assign steps. A finished run's results and feature samples are always in storage before
  its status changes. Write bodies are limited to 32 MiB, ingest in flight to 256 MiB (then 503
  with `Retry-After`), and a write to a deleted run answers `run_deleted`. Any write counts as the
  run's heartbeat (at most one stored every two minutes), and new data shows up without waiting
  for the next catalog refresh.

- Sign-in. `vigily up` runs in local mode on a loopback address: a random token per repository,
  kept in a private file, opens the API, and the link it prints (`/?token=...`) stores it in an
  HttpOnly, SameSite=Strict cookie. On any other address (and in Docker) it runs in server mode and
  refuses to start without `VIGILY_ADMIN_TOKEN` (or `VIGILY_ADMIN_TOKEN_FILE`) unless given
  `--insecure-no-auth`. Admins create tokens with the scopes `read`, `ingest` and `admin`
  (`/api/v1/auth/tokens`), stored as SHA-256 hashes; the web app shows a sign-in page when needed.
  Cookie-authenticated writes must come from vigily's own origin. Scheduled commands get
  `VIGILY_URL` and a short-lived token; labels record who entered them. The SDK's span exporter
  sends `VIGILY_TOKEN` (or `vigily.tracing.configure(token=...)`).
- `vigily-engine`, the server as its own binary (`vigily up` runs it in the foreground), with
  `--state-dir`, `--detach`, `--idle-timeout` and `--check-config`. One engine serves a
  repository on a machine: it holds `engines/<key>.lock` in the state directory and writes its
  address, version and local token to `engines/<key>.json` (0600) while it runs; a second engine
  on the same repository exits with code 3 and names the first one's address. An engine started
  with `--detach` logs to `engines/<key>.log`, takes a free port when its own is taken and stops
  after 30 idle minutes.

### Changed

- The local token moves to `engines/<key>.token` in the state directory, which defaults to
  `~/Library/Caches/vigily` on macOS (`~/.cache/vigily` elsewhere); a token kept by an earlier
  build under `tokens/` is moved there on the first start.
- The server stops cleanly on SIGTERM as well as Ctrl-C (`docker stop`, service managers): open
  requests get 10 seconds, then open runs, storage and records are flushed.

- Every API route except health, info and `auth/me` needs a token, including the deprecated
  `/api/*` aliases and OTLP's `/v1/traces`. `docker compose up` needs `VIGILY_ADMIN_TOKEN`.
- The HTTP API moves to `/api/v1`, with `GET /api/v1/info` (version, API version, mode and
  capabilities). Errors there are `{"error": {"code", "message", "position"?}}` with a stable code
  (`not_found`, `invalid_argument`, `conflict`, ...). The old `/api/*` paths keep working as
  deprecated aliases with a `Deprecation` header, a `Link` to the successor and the old flat error
  body; they are removed before 0.1.0. OpenTelemetry's `POST /v1/traces` is unchanged. The web app
  and the Python SDK's span exporter use `/api/v1`.
- The JSON types of the HTTP API (the run model, every route's answer, evaluation, drift and trace
  types) move to a new crate, `vigily-api`, which depends on serde alone; `vigily-core` re-exports
  them. Schedules and suites answer with typed structures instead of ad-hoc JSON. Golden JSON
  files in `crates/vigily-api/tests/fixtures` pin the wire format; the web app sees the same JSON.
- Recursive replaces Atkinson Hyperlegible as vigily's typeface, in the app, the docs and the brand
  assets: Sans Linear for text and Mono Linear for run names, values, keys and code, from one
  self-hosted variable file (SIL OFL). Bold is 660 and headings track slightly tighter; the wordmark,
  banners, social card and screenshots are regenerated.

### Fixed

- The SDK recorded the branch name as `__system__.git.commit`; it now records the commit hash
  (and the branch as `git.branch`).

- Dark mode in the docs site: the code-block copy button and the breadcrumb separator had too
  little contrast, and some Material colours did not follow the brand tokens.

- Web app edits (name, tags, notes, archive) on a running run are no longer reverted when the SDK
  finishes it: both sides re-read the run and change only their own fields.
- `evaluate()` stores results and features before marking the run finished; the drift monitor checks
  a run again when its results change, and one suite's error no longer stops the others.
- Scheduled commands that leave background processes no longer block completion or defeat the
  timeout; leftover processes are stopped.
- Schedule runs and their output are pruned (`keep_runs`, `keep_days`, `keep_output`).
- Behaviour drift pairs items with their own baseline means (Wilcoxon signed-rank) instead of
  pooling repeated baseline runs as independent samples.
- Linear-time query parsing (filters over 4 KiB are refused), no step overflow after the largest
  step, alert acknowledgements kept during webhook delivery, impossible cron expressions rejected,
  interrupted schedule runs marked as such.
- Runs table: each loss sparkline spans the run's own steps (still on one scale per experiment), so
  a live run is no longer squeezed into the first pixels by a long finished run; a training run
  without the metric says "No points yet" or "Not logged" instead of loading forever.
- Drift: a suite whose eval runs recorded no scores explains why instead of showing an endless
  placeholder, and the demo's evaluations are real `evaluate()` runs that form one scored suite.
- Web app polish: the sidebar footer keeps pre-release versions whole, header facts no longer split
  around inline code, names and states no longer break at hyphens in narrow tables, figure labels
  and notes wrap instead of truncating on phones, and the trace filters fit two per row on phones.

- `vigily-core`: runs, parameters and metric series behind a storage trait, with a kv0 and an
  in-memory backend; an incrementally refreshed catalog with per-series statistics; a run query
  language; min/max-envelope downsampling onto a shared x axis; group aggregation with interpolation.
- `vigily-server`: a JSON API (meta, runs, run detail and changes, chart data) and the embedded web
  app, answering on localhost only by default.
- `vigily` command: `up`, `runs` and `optimize`.
- Python SDK: `vigily.Run` (parameters, `track` with step, epoch, context and timestamp, tags,
  notes, `finish`, context manager, finish at exit) and `vigily.Repo` for reading back.
- Web app: runs table, metric explorer, run pages, experiments, command palette, keyboard shortcuts,
  light and dark themes.
- Run `kind` (`training`, `eval`, ...) and `target` reference, filterable as `kind:` and `target.<key>`.
- `vigily up` optimizes storage every 15 minutes in the background (`--no-optimize`).
- Runs whose process stopped (no heartbeat for 10 minutes) show as "No heartbeat" and can be marked
  aborted; runs that are not running can be deleted, with an undo window.
- `examples/demo.py` with 43 synthetic runs, and `bench/scale.py` and `bench/scale.sh` for scale tests.
