# Configuration and vigily.toml

Most of vigily needs no configuration: the SDK and the server agree on a repository location, and
the server serves it. A `vigily.toml` adds schedules, an alert webhook and drift thresholds.

## Environment variables

| Variable | Used by | Meaning |
|---|---|---|
| `VIGILY_REPO` | SDK, CLI | The repository: a directory or `s3://`, `gs://`, `az://` URI. Default `.vigily` |
| `VIGILY_PORT` | `vigily up` | The port, default 7600 |
| `VIGILY_CONFIG` | `vigily up` | The configuration file |
| `VIGILY_LOG` | CLI | Log filter, for example `info` or `vigily_server=debug` |
| `VIGILY_ADMIN_TOKEN` | `vigily up --mode server` | The admin token (at least 16 characters); required in server mode, never stored |
| `VIGILY_ADMIN_TOKEN_FILE` | `vigily up --mode server` | A file holding the admin token, instead of `VIGILY_ADMIN_TOKEN` (Docker secrets) |
| `VIGILY_TOKEN_FILE` | `vigily up` | Where the local token is kept; default `engines/<key>.token` in the state directory |
| `VIGILY_STATE_DIR` | `vigily up`, `vigily-engine` | vigily's state directory (engine addresses, local tokens, logs); default `~/Library/Caches/vigily` on macOS and `~/.cache/vigily` elsewhere |
| `VIGILY_URL`, `VIGILY_TOKEN` | CLI, SDK tracing, scheduled commands | An engine's address and the token to send; the CLI then uses that engine instead of the repository's local one. Set by the scheduler for its commands |
| `VIGILY_ENGINE` | CLI | The `vigily-engine` binary to start, when it is neither beside `vigily` nor on `PATH` |
| `VIGILY_SCHEDULE`, `VIGILY_SCHEDULE_RUN`, `VIGILY_SCHEDULED_FOR` | scheduled commands | Set by the scheduler; runs the command logs link back to the schedule run |

In the Docker image `VIGILY_REPO` is `/data/repo` and `VIGILY_PORT` is 7600; the server reads
`/data/vigily.toml` when it exists. `compose.yaml` reads these from your shell:

| Variable | Meaning |
|---|---|
| `VIGILY_IMAGE` | The image, default `ghcr.io/sayef/vigily:latest`; `compose.dev.yaml` builds `vigily:local` |
| `VIGILY_SERVER_REPO` | The server's repository, default `/data/repo`; an object-store URI works too |
| `VIGILY_DATA` | What is mounted at `/data`: the `vigily` volume, or a directory such as `./vigily-data` |
| `VIGILY_UID`, `VIGILY_GID` | The user the server runs as, default 10001; your own ids with a directory |
| `VIGILY_HOST_PORT` | The port published on `127.0.0.1`, default 7600 |
| `VIGILY_ADMIN_TOKEN` | Required: the admin token; `docker compose up` refuses to start without it |
| `VIGILY_LOG` | As above |

`VIGILY_REPO` itself is left to the SDK on your machine, so one shell can serve and log.

## Tokens

Every API request needs a token, except `GET /api/v1/health`, `/api/v1/info` and
`/api/v1/auth/me` and the web app's own files. `vigily up` runs in one of two modes.

**Local mode** (the default on `127.0.0.1`): one user on this machine. vigily keeps a random
128-bit token per repository in a file only you can read (`--token-file`, default
`engines/<key>.token` in the [state directory](cli.md#the-state-directory)), the same one each
start, and prints a link with it:

```text
Open http://127.0.0.1:7600/?token=... (Ctrl-C to stop)
```

Opening the link once stores the token in an HttpOnly, SameSite=Strict cookie and loads the page
without it. Scripts send it as `Authorization: Bearer <token>`; the SDK's span exporter reads it
from `VIGILY_TOKEN`. vigily still answers only requests addressed to `localhost`, `127.0.0.1` or
`[::1]`, against DNS rebinding. A request that uses the cookie to change something must come from
vigily's own pages (its `Origin` must match); a bearer token needs no such check.

**Server mode** (the default on any other address, or `--mode server`): a shared engine. It refuses
to start without `VIGILY_ADMIN_TOKEN` (or `VIGILY_ADMIN_TOKEN_FILE`), which opens everything and is
read from the environment only. `--insecure-no-auth` starts without any checks instead, for an
engine behind your own access control. People sign in on the web app with a token
(`POST /api/v1/auth/session`; the cookie is `Secure` behind HTTPS, also when a proxy sets
`X-Forwarded-Proto: https`).

Further tokens carry scopes, and are created by an admin:

| Scope | Allows |
|---|---|
| `read` | Every read: runs, charts, datasets, results, schedules, drift, alerts, traces |
| `ingest` | Writing: runs and points (`/api/v1/ingest`), results, features, dataset commits, spans (also OTLP at `/v1/traces`) |
| `admin` | Everything, including edits in the web app (names, tags, labels, baselines, alerts), deleting runs and traces, starting schedules, storage administration and tokens |

A script that logs runs and reads them back needs `read` and `ingest`.

```bash
curl -s -X POST http://vigily.example/api/v1/auth/tokens \
  -H "Authorization: Bearer $VIGILY_ADMIN_TOKEN" -H 'Content-Type: application/json' \
  -d '{"name": "nightly-evals", "scopes": ["read", "ingest"]}'
```

The answer holds the token (`vgy_...`) once; vigily stores only its SHA-256, as a record
`auth/token/<id>` in the repository, with its name, scopes, creation time, who created it, when it
was last used and an optional `expires_at` (UTC microseconds). `GET /api/v1/auth/tokens` lists
them and `DELETE /api/v1/auth/tokens/<id>` revokes one. Labels record who entered them (`local`,
`admin` or `token:<name>`), and the audit export shows it.

Scheduled commands get `VIGILY_URL` and, when the engine checks tokens, a `VIGILY_TOKEN` with the
`read` and `ingest` scopes that is held in memory, expires with the attempt and is revoked when the
command exits.

## vigily.toml

`vigily up` reads `./vigily.toml` when it exists, or the file given with `--config`. Relative paths
in it resolve against the file's directory. Unknown keys are errors, so a typo does not silently
disable a setting.

```toml
[[schedule]]
name = "nightly-intents"                 # unique; letters, digits, -, _ and .
description = "Score both intent classifiers on the latest dataset"
cron = "0 2 * * *"                       # minute hour day month weekday
timezone = "Europe/Berlin"               # IANA name; default UTC
command = ["uv", "run", "--no-sync", "python", "evals/nightly.py"]
workdir = "."                            # relative to this file
env = { EVAL_SAMPLE = "500" }            # added to the server's environment
timeout = "15m"                          # default 1h
retries = 1                              # default 0
retry_delay = "2m"                       # default 1m
missed = "run_once"                      # or "skip" (default): runs missed while the server was down
enabled = true
keep_runs = 200                          # newest runs kept for this schedule
keep_days = 90                           # and none older than this
keep_output = 50                         # captured output kept for the newest runs

[alerts]
webhook = "https://hooks.example.com/vigily"   # optional; JSON POST per alert

[drift]
alpha = 0.01                             # significance level for behaviour drift
min_effect = 0.02                        # smallest drop in a score that counts

[tracing]                                # see the LLM tracing guide
payloads = "truncated"                   # "full", "truncated" (default) or "metadata"
max_field_bytes = 4096                   # per field in truncated mode
currency = "EUR"                         # of the price table; default USD

[tracing.prices."example-large"]         # an example, not real prices: per million tokens
input = 2.50
output = 10.00
cached_input = 1.25                      # default: the input price

[tracing.projects.support-bot]           # per project (the run's experiment)
payloads = "metadata"
```

Durations take `s`, `m`, `h` and `d`. Cron expressions are evaluated in the schedule's time zone
and stored in UTC, so a 02:00 schedule stays at 02:00 local time across daylight-saving changes;
an expression that can never fire (such as 30 February) is rejected at start.

See [scheduled evaluations](scheduled-evals.md) for what a schedule may run and how it is contained,
and [drift methods](drift.md) for what `alpha` and `min_effect` control.
