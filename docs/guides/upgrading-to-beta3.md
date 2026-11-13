# Upgrading to 0.1.0-beta.3

In 0.1.0-beta.3 one process, the engine (`vigily-engine`), owns a repository's storage. The web
app, the `vigily` command and the Python SDK are its clients and talk to it over HTTP. Before, the
SDK wrote to storage inside the training process, and `vigily up` served the same files. This page
lists what changes for each kind of user. Repositories open unchanged: there is no storage format
change and nothing to migrate.

## Python SDK

- **Install.** `pip install vigily` is now a pure-Python wheel of about 60 kB with no
  dependencies, instead of an 80 MB native extension. On a laptop, `pip install 'vigily[local]'`
  also installs the engine, which the SDK starts in the background when no engine serves the
  repository. Without it, install `vigily-engine` with the [install script](../getting-started.md)
  or point the SDK at a running engine.
- **Your code does not change.** `Run`, `Repo`, `Dataset`, `evaluate`, the evaluators, tracing and
  `log_features` keep their signatures. `Run(repo=...)` and `VIGILY_REPO` still pick the
  repository; the SDK finds or starts its engine.
- **Team servers.** `VIGILY_URL` and `VIGILY_TOKEN` (or `vigily.configure(url=..., token=...)`)
  send runs to an engine elsewhere, such as a [team server](team-server.md). The token needs the
  `read` and `ingest` scopes. No engine is started then.
- **Writes are sent in the background.** `Run()` and `track()` never wait for the network; steps
  and timestamps are assigned in your process. `Run.finish()` no longer waits for storage. Call
  `vigily.flush()` when another process must see the data right away; `Repo` reads flush first.
- **When the engine is unreachable,** the SDK retries and, after two minutes, keeps batches in a
  spool on disk; they are sent when the engine is back, or with `vigily sync`. At exit it finishes
  the runs it started and sends what is queued for up to 30 seconds (`VIGILY_EXIT_TIMEOUT`).
- **Deleted runs.** When a run is deleted in the web app while a script logs to it,
  `RunDeletedError` is raised by the next call after the engine reported it, not by the call that
  raced with the delete.
- **`__system__.git.commit`** now holds the commit hash; the branch is `__system__.git.branch`.

New: `vigily.configure(url, token, project, offline)`, `vigily.flush(timeout)`, `Repo(url=...)`,
`Repo.results(run_id)` and `vigily.tracing.export(spans)`. The environment variables are listed in
the [Python SDK guide](python-sdk.md).

## Command line

- **`vigily up` runs the engine** (`vigily-engine`) in the foreground with the same options. The
  release archive and the install script install both binaries.
- **Other commands are clients.** `vigily runs`, `optimize`, `schedules`, `traces delete` and
  `runs purge` use the repository's engine, starting one in the background when none runs, or the
  engine at `--url` (`VIGILY_URL`) with `--token` (`VIGILY_TOKEN`).
- **`vigily schedules`** lists the engine's schedules; `--config` moved to `vigily up`.
  `vigily schedules run NAME` starts the run on the engine and follows it until it is done.
- **New commands:** `vigily status`, `stop`, `open`, `env`, `sync`, `tokens list|create|revoke` and
  `token rotate`. See the [command line guide](cli.md).
- **Engines started in the background** stop after 30 idle minutes; `vigily up` never does. Their
  address, token and log live in the state directory (`~/.cache/vigily`, or
  `~/Library/Caches/vigily` on macOS; `VIGILY_STATE_DIR`).

## Sign-in and tokens

Every request now needs a token (except health, info and `auth/me`).

- **Local mode** (`vigily up` on `127.0.0.1`, the default): the engine keeps a random token per
  repository and prints a link that signs the browser in. `vigily env` prints `VIGILY_URL` and
  `VIGILY_TOKEN` for scripts that call the API.
- **Server mode** (any other address, and Docker): the engine refuses to start without
  `VIGILY_ADMIN_TOKEN`. Create tokens for people and scripts with `vigily tokens create NAME
  --scope read --scope ingest`; `ingest` alone cannot read. See [Tokens](configuration.md#tokens).

## Docker

- The image's entry point is `vigily-engine`, and its command `--host 0.0.0.0 --mode server`. If
  you passed arguments to the old `vigily up` entry point, pass engine options instead (for
  example `--config /data/vigily.toml`).
- `VIGILY_ADMIN_TOKEN` is required; the container exits without it.
- `compose.yaml` keeps the data in the `vigily` volume only. `VIGILY_DATA`, `VIGILY_UID`,
  `VIGILY_GID` and `VIGILY_SERVER_REPO` are gone: scripts no longer share a directory with the
  container but log over HTTP:

  ```bash
  docker compose exec vigily sh -c \
    'vigily --url http://127.0.0.1:7600 --token "$VIGILY_ADMIN_TOKEN" tokens create train --scope read --scope ingest'
  VIGILY_URL=http://localhost:7600 VIGILY_TOKEN=vgy_... python train.py
  ```

  If you kept the data in a directory with `VIGILY_DATA`, copy it into the volume once, with the
  engine stopped:

  ```bash
  docker run --rm -v "$PWD/vigily-data":/from:ro -v vigily_vigily:/data debian:bookworm-slim \
    sh -c 'cp -a /from/. /data/ && chown -R 10001:10001 /data'
  ```
- `docker stop` now shuts the engine down cleanly; Compose gives it 30 seconds.

## HTTP API clients

- **Routes move to `/api/v1`.** The old `/api/...` paths still answer as aliases, with a
  `Deprecation: true` header and a `Link` to the new path. **They will be removed before 0.1.0**;
  move to `/api/v1` now. OpenTelemetry's `POST /v1/traces` stays where it is.
- **Errors** on `/api/v1` are `{"error": {"code", "message", "position"?}}` with a stable `code`;
  the aliases keep the old `{"error": "..."}`.
- **Tokens** are required: `Authorization: Bearer <token>`.
- New routes cover everything the SDK and the CLI do, including writes (`POST /api/v1/ingest`),
  tokens and storage administration. The [HTTP API reference](http-api.md) lists them with their
  scopes.

## Older SDKs next to an engine

An SDK from before 0.1.0-beta.3 still writes to storage directly. It can keep doing so next to a
running engine, which picks up its runs on the next refresh, but this is deprecated: upgrade the
SDK so that the engine is the only writer.
