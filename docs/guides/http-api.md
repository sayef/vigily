# HTTP API

The engine (`vigily-engine`, started by `vigily up`, the CLI or the SDK) serves everything the web
app, the CLI and the Python SDK use as JSON over HTTP under `/api/v1`. This page lists the routes
and the scope each needs. The request and answer types are the Rust types of the `vigily-api`
crate; `crates/vigily-api/tests/fixtures` holds a captured example of each answer.

## Conventions

**Authentication.** Every route needs a token except the ones marked *open* below. Send it as
`Authorization: Bearer <token>`; the web app uses the `vigily_token` cookie instead. In local mode
the token is the repository's own (`vigily env` prints it); in server mode it is the admin token or
a token created with `vigily tokens create` ([Tokens](configuration.md#tokens)). A missing or
unknown token is a 401 `unauthorized`, a token without the route's scope a 403 `forbidden`.

| Scope | Allows |
|---|---|
| open | No token needed |
| `read` | Reading runs, metrics, datasets, results, schedules, drift, alerts and traces |
| `ingest` | Writing runs, points, results, features, dataset versions and spans |
| `admin` | Everything, including edits, deletes, schedules, storage administration and tokens |

`admin` includes `read` and `ingest`; `ingest` does not include `read`.

**Errors.** Every error answer is `{"error": {"code", "message", "position"?}}`, where `code` is one
of `not_found`, `invalid_argument`, `conflict`, `run_deleted`, `unauthorized`, `forbidden`,
`too_large`, `overloaded` or `internal`, and `position` points into a run query that did not parse.

**Bodies.** Write routes take `Content-Type: application/json` (ingest also
`application/x-ndjson`) and may be gzip-compressed (`Content-Encoding: gzip`), up to 32 MiB after
decompression (413 `too_large`). They refuse an `Origin` header other than the engine's own, so a
web page cannot call them. NaN and infinities travel as the strings `"NaN"`, `"Infinity"` and
`"-Infinity"`. Times are UTC microseconds since the Unix epoch.

**Retries.** `POST /api/v1/ingest` and `POST /api/v1/datasets/{name}/versions` take an
`Idempotency-Key` header; a request repeated with the same key answers the first result with
`Idempotent-Replayed: true` instead of writing again. Ingest also deduplicates by each writer's
`seq`. When the engine holds too much unwritten data it answers 503 `overloaded` with
`Retry-After`.

## Engine and authentication

| Method and path | Scope | Answer |
|---|---|---|
| `GET /api/v1/health` | open | `ok` (plain text) |
| `GET /api/v1/info` | open | `{version, api_version, mode, capabilities}` |
| `GET /api/v1/auth/me` | open | `{authenticated, principal, scopes, mode, auth_required}` |
| `POST /api/v1/auth/session` | open | `{token}` → sets the sign-in cookie; `DELETE` signs out |
| `GET /api/v1/auth/tokens` | admin | the stored tokens, never their secrets |
| `POST /api/v1/auth/tokens` | admin | `{name, scopes, expires_at?}` → the token, shown once |
| `DELETE /api/v1/auth/tokens/{id}` | admin | revokes a token |
| `POST /api/v1/auth/local-token/rotate` | admin | a new local token (local mode only; 409 otherwise) |

## Runs and metrics

| Method and path | Scope | Answer |
|---|---|---|
| `GET /api/v1/meta` | read | counts, experiments, metric keys, parameter keys with kinds and values |
| `GET /api/v1/runs?q=&sort=&desc=&archived=&limit=&offset=&fields=` | read | `{rows, total, matched}`: the runs matching the [query](cli.md#the-query-language), a page of them with `limit` and `offset`; `fields=summary` leaves out parameters and metric statistics |
| `GET /api/v1/runs/{id}` | read | the run with its parameters and series |
| `GET /api/v1/runs/{id}/params` | read | the parameters as a nested object |
| `GET /api/v1/runs/{id}/series` | read | the run's series with their statistics |
| `GET /api/v1/runs/{id}/points?name=&context=&start_time=&stop_time=` | read | one series' points in columns (`context` is a JSON object) |
| `POST /api/v1/chart` | read | `{runs, metric, x?, range?, points?, group_by?, aggregate?}` → resampled series for a chart |
| `GET /api/v1/compare?a=&b=` | read | two evaluation runs side by side |
| `PATCH /api/v1/runs/{id}` | admin | changes `name`, `experiment`, `description`, `tags`, `archived` |
| `POST /api/v1/runs/delete` | admin | `{ids}` → marks runs deleted (see [purge](storage.md#deleting-is-not-erasing-purge)) |
| `GET /api/v1/runs/{id}/audit` | read | the run's audit export |
| `POST /api/v1/runs/{id}/labels` | admin | `{item_id, name, value?, passed?, ...}` → records a human label on an evaluation item |

## Writing runs

| Method and path | Scope | Answer |
|---|---|---|
| `POST /api/v1/runs` | ingest | `{id, name?, experiment?, ...}` with a client-chosen id → `{created, run}`; 201 when created, 200 when the id exists |
| `POST /api/v1/runs/{id}/resume` | ingest | opens the run for more writes → the run, its parameters and each series' last step |
| `POST /api/v1/ingest` | ingest | one batch `{writer, seq, ops}`, or one per line as NDJSON → a report per batch |
| `POST /api/v1/runs/{id}/finish` | ingest | `{status, ended_at?}` → the run |
| `POST /api/v1/runs/{id}/results` | ingest | `{results}` → `{stored}` |
| `POST /api/v1/runs/{id}/features` | ingest | `{samples}` → `{stored}` |
| `POST /api/v1/flush` | ingest | commits buffered writes and refreshes the run list → `{refreshed_at}` |

An ingest batch is an ordered list of operations on runs: `create`, `params`, `points` (one series
in columns: `steps`, `values`, optional `epochs` and `timestamps`), `update`, `heartbeat`, `finish`,
`results` and `features`. The engine applies them in order and stops at the first failure, whose
message names the operation (`operation 3 (points): ...`). Clients choose the steps, so a batch
sent twice replaces points instead of adding them. A write to a deleted run is a 410
`run_deleted`. `crates/vigily-api/tests/fixtures/ingest_batch.json` is a complete example.

## Datasets, evaluations and drift

| Method and path | Scope | Answer |
|---|---|---|
| `GET /api/v1/datasets` | read | the datasets with their latest versions |
| `GET /api/v1/datasets/{name}?version=&q=` | read | one version's items (the newest by default), optionally filtered by text |
| `GET /api/v1/datasets/{name}/versions` | read | the dataset's versions |
| `GET /api/v1/datasets/{name}/versions/{version}/items` | read | the items as NDJSON, one per line |
| `POST /api/v1/datasets/{name}/versions` | ingest | `{items, mode?, note?, ...}` with a required `Idempotency-Key` → the new version |
| `GET /api/v1/runs/{id}/items` | read | an evaluation run's items with outputs, scores and labels |
| `GET /api/v1/runs/{id}/results` | read | an evaluation run's stored results |
| `GET /api/v1/suites` | read | evaluation suites (dataset and evaluator combinations) |
| `GET /api/v1/suites/{key}` | read | a suite's history, baseline and drift |
| `GET /api/v1/suites/{key}/data-drift` | read | input drift of a suite's features |
| `PUT /api/v1/suites/{key}/baseline` | admin | sets or clears a suite's baseline run |
| `GET /api/v1/alerts` | read | drift alerts |
| `POST /api/v1/alerts/{id}` | admin | `{acknowledged}` → the alert |

## Schedules

| Method and path | Scope | Answer |
|---|---|---|
| `GET /api/v1/schedules` | read | the engine's schedules with their next and last runs |
| `GET /api/v1/schedules/{name}/runs` | read | a schedule's runs |
| `GET /api/v1/schedules/{name}/runs/{id}` | read | one schedule run with its attempts and captured output |
| `POST /api/v1/schedules/{name}/run` | admin | starts the schedule now → 202 with the run's id; 409 while it runs |

## Traces

| Method and path | Scope | Answer |
|---|---|---|
| `POST /api/v1/traces/ingest` | ingest | spans in vigily's JSON form → `{stored}` |
| `POST /v1/traces` | ingest | OpenTelemetry OTLP/HTTP (JSON), at the root where exporters expect it |
| `GET /api/v1/traces?run=&model=&status=&since=&until=&q=` | read | trace summaries |
| `GET /api/v1/traces/{id}` | read | one trace with its spans |
| `GET /api/v1/tracing/policy?project=` | read | the payload policy a client applies before sending |
| `DELETE /api/v1/traces/{id}` | admin | deletes one trace |
| `POST /api/v1/traces/delete` | admin | `{ids, erase?, force?}` → deletes or erases traces |

## Storage administration

| Method and path | Scope | Answer |
|---|---|---|
| `POST /api/v1/admin/optimize` | admin | compacts storage → `{seconds}` |
| `POST /api/v1/admin/purge` | admin | `{force?}` → erases deleted runs and traces from storage |

## Deprecated `/api` paths

The routes of releases before 0.1.0-beta.3 (`/api/meta`, `/api/runs`, `/api/chart`, ...) still
answer as aliases of their `/api/v1` successors, with the same scopes. Their answers carry
`Deprecation: true` and a `Link` header naming the successor, and their errors keep the old flat
`{"error": "..."}` form. They will be removed before 0.1.0; routes added in 0.1.0-beta.3 have no
alias. `/v1/traces` is OpenTelemetry's own path and stays.

## Example

```bash
export VIGILY_URL=http://127.0.0.1:7600 VIGILY_TOKEN=...   # vigily env prints both for a local engine
curl -s -H "Authorization: Bearer $VIGILY_TOKEN" "$VIGILY_URL/api/v1/runs?q=exp:sweep&limit=5"
curl -s -H "Authorization: Bearer $VIGILY_TOKEN" \
  "$VIGILY_URL/api/v1/runs/<id>/points?name=loss&context=%7B%22subset%22%3A%22val%22%7D"
```
