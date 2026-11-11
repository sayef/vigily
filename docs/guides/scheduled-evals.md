# Scheduled evaluations and the security model

A schedule turns an evaluation script into a nightly check. The script is ordinary Python that calls
`evaluate()`; vigily runs it on time, keeps its output and links the eval runs it logs back to the
schedule run.

## Write the script

```python
# evals/nightly.py
from vigily import Dataset, evaluate, evaluators as ev
from my_project import classify, MODEL_VERSION

evaluate(
    Dataset("support-intents"),  # the latest version
    target=classify,
    evaluators=[ev.exact_match()],
    target_ref={"model": "intent-clf", "version": MODEL_VERSION},
    experiment="support-intents-nightly",
)
```

Keep the experiment, the target reference and the dataset name stable from night to night: together
they form the suite that [drift](drift.md) compares.

## Schedule it

```toml
# vigily.toml
[[schedule]]
name = "nightly-intents"
cron = "0 2 * * *"
timezone = "Europe/Berlin"
command = ["uv", "run", "--no-sync", "python", "evals/nightly.py"]
timeout = "15m"
retries = 1
missed = "run_once"
```

```bash
vigily schedules --config vigily.toml run nightly-intents   # try it once
vigily up --config vigily.toml                              # then let the server keep time
```

**Schedules** in the web app lists each schedule with its next fire time and its runs: attempts,
exit codes, durations, the last 64 KiB of stdout and stderr, and the eval runs each attempt logged.

## How a schedule runs

- **No overlap.** A schedule that is still running is not started again; the fire is recorded as
  skipped.
- **Missed runs.** After downtime, `missed = "skip"` records the missed fires as skipped; `"run_once"` runs the
  latest missed fire once.
- **Retries and timeouts.** A failed or timed-out attempt is retried after `retry_delay`, up to
  `retries` times. A timeout stops the whole process group.
- **Interrupted runs.** Runs left running by a server that stopped are marked interrupted when it
  starts again.
- **Retention.** `keep_runs`, `keep_days` and `keep_output` bound what each schedule keeps.

## Security model

Scheduled commands are code execution on the server, so vigily keeps the rules short:

- **Commands come only from the configuration file**, written by whoever operates the server. The
  API and the web app can read schedules and their output but cannot create, change or start them.
  Starting a schedule by hand is `vigily schedules run`, which needs a shell on the server.
- **Commands run as the user that runs vigily**, with its permissions and environment plus the
  schedule's `env`. Run vigily as a dedicated, unprivileged user, and give that user only the
  credentials the evaluations need.
- **No secrets in vigily.toml.** Put API keys for model clients in the service's environment or a
  secret manager; `env` in the file is for settings.
- **Leftover processes are stopped.** Each attempt runs in its own session. When it ends, its process
  group and, on Linux, any process that still carries the attempt's `VIGILY_SCHEDULE_RUN` get
  SIGTERM, then SIGKILL two seconds later. A process that both starts a new session and clears its
  environment can escape this; containing that needs cgroups (a container or a systemd unit with
  `KillMode=control-group`), which vigily does not manage.
- **Output is data.** Captured output is shown in the web app to anyone who can reach the server.
  Do not print personal data or secrets from evaluation scripts.
