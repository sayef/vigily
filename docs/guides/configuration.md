# Configuration and vigily.toml

Most of vigily needs no configuration: the SDK and the server agree on a repository location, and
the server serves it. A `vigily.toml` adds schedules, an alert webhook and drift thresholds.

## Environment variables

| Variable | Used by | Meaning |
|---|---|---|
| `VIGILY_REPO` | SDK, CLI | The repository: a directory or `s3://`, `gs://`, `az://` URI. Default `.vigily` |
| `VIGILY_PORT` | `vigily up` | The port, default 7600 |
| `VIGILY_CONFIG` | `vigily up`, `vigily schedules` | The configuration file |
| `VIGILY_LOG` | CLI | Log filter, for example `info` or `vigily_server=debug` |
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
| `VIGILY_LOG` | As above |

`VIGILY_REPO` itself is left to the SDK on your machine, so one shell can serve and log.

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
