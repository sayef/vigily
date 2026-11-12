# Running a team server

One engine can serve a whole team: training jobs on any machine log to it over HTTP, and everyone
opens the same web app. This guide runs it with Docker Compose; the same steps work for
`vigily-engine --mode server` under systemd ([Serving to a team](storage.md#serving-to-a-team)).

## Start the engine

From a checkout of this repository, or any directory holding a copy of `compose.yaml`:

```bash
echo "VIGILY_ADMIN_TOKEN=$(openssl rand -hex 32)" > .env    # keep it: it opens everything
chmod 600 .env
docker compose up -d --wait
```

The container runs `vigily-engine --host 0.0.0.0 --mode server`. In server mode the engine refuses
to start without `VIGILY_ADMIN_TOKEN` (at least 16 characters, read from the environment and never
stored; `VIGILY_ADMIN_TOKEN_FILE` names a file holding it, for Docker secrets). The repository and
an optional `vigily.toml` live in the `vigily` volume under `/data`, owned by the image's
unprivileged user (uid 10001).

Compose publishes the port on `127.0.0.1:7600` only (`VIGILY_HOST_PORT` changes the port). Keep it
that way and put a reverse proxy with TLS in front ([below](#tls-with-a-reverse-proxy)).

## Tokens

Give every person and script its own token with only the scopes it needs, so that you can revoke
one without touching the others:

| Who | Scopes |
|---|---|
| A training script or CI job that logs runs | `read`, `ingest` |
| An LLM application that only sends spans | `ingest` |
| Someone who looks at results | `read` |
| Someone who edits runs, deletes data or manages tokens | `admin` |

`ingest` does not include `read`: the SDK reads a run back when it resumes one and when you call
`Repo` methods, so scripts need both. [Tokens](configuration.md#tokens) lists what each scope
allows.

Create tokens with the CLI, from inside the container or from any machine with `vigily` installed:

```bash
docker compose exec vigily sh -c \
  'vigily --url http://127.0.0.1:7600 --token "$VIGILY_ADMIN_TOKEN" tokens create train --scope read --scope ingest'

# or, from a workstation that reaches the server:
export VIGILY_URL=https://vigily.example.com VIGILY_TOKEN=<admin token>
vigily tokens create ci-nightly --scope read --scope ingest --expires-in 90d
vigily tokens list
vigily tokens revoke <id>
```

The token (`vgy_...`) is printed once; vigily keeps only its SHA-256. A training script then needs
two variables and no code changes:

```bash
VIGILY_URL=http://localhost:7600 VIGILY_TOKEN=vgy_... python train.py
```

People sign in to the web app with a token of their own. Labels and the audit trail record which
token entered what (`token:<name>`), so name tokens after people or jobs.

## Scripts in containers

A script in another container on the same Compose network reaches the engine by its service name.
`compose.yaml` has a commented `trainer` service that shows this:

```yaml
trainer:
  image: python:3.13-slim
  depends_on:
    vigily: {condition: service_healthy}
  environment:
    VIGILY_URL: http://vigily:7600
    VIGILY_TOKEN: ${VIGILY_TOKEN}
  volumes: ["./:/work:ro"]
  working_dir: /work
  command: ["sh", "-c", "pip install --quiet vigily && python train.py"]
```

The SDK is pure Python with no dependencies, so it adds almost nothing to a training image.

## When the server is down

Scripts keep running when the engine is unreachable. The SDK retries with backoff and, after two
minutes without an answer, writes batches to a spool on the script's machine; it sends them in
order once the engine answers again, or when you run `vigily sync` there. Nothing is dropped
silently. Restarts and upgrades are therefore safe while jobs are running, and a single engine is
enough for a team (it is still a single point of failure for the web app).

`docker compose stop` (and `docker stop`) send SIGTERM. The engine lets open requests finish for
up to 10 seconds, writes out what it holds in memory and exits; `compose.yaml` gives it 30 seconds
before Docker would kill it. With plain `docker stop`, pass `-t 30`.

## TLS with a reverse proxy

The engine speaks plain HTTP. For anything beyond one machine, terminate TLS in a reverse proxy
that forwards to `127.0.0.1:7600` and sets `X-Forwarded-Proto: https`, so that the sign-in cookie
is marked `Secure`. With [Caddy](https://caddyserver.com), which obtains certificates itself and
sets the header:

```text
vigily.example.com {
    reverse_proxy 127.0.0.1:7600
}
```

With nginx, raise the request size limit to the engine's (32 MiB per ingest batch); the default of
1 MiB rejects large batches:

```nginx
location / {
    proxy_pass http://127.0.0.1:7600;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
    client_max_body_size 32m;
}
```

Server mode answers any host name, so the proxy's name needs no configuration in vigily.

## Backups

Everything lives in the `vigily` volume (Compose names it `vigily_vigily`). For a consistent copy,
stop the engine briefly; scripts that log meanwhile retry, and spool if it takes longer than two
minutes:

```bash
docker compose stop vigily
docker run --rm -v vigily_vigily:/data:ro -v "$PWD":/backup debian:bookworm-slim \
  tar czf "/backup/vigily-$(date -u +%Y%m%dT%H%MZ).tar.gz" -C /data .
docker compose start vigily
```

To restore, stop the engine, empty the volume and unpack the archive into it the same way
(`tar xzf ... -C /data`), then start it again. A repository on object storage (`VIGILY_REPO` set to
an `s3://`, `gs://` or `az://` URI in the service's environment) is backed up with bucket
versioning instead. [Backups and data protection](storage.md#backups-and-data-protection) covers
what the repository holds.

## Upgrades

Pull the new image and recreate the container; the repository format does not change between
releases unless the [changelog](../changelog.md) says so:

```bash
VIGILY_IMAGE=ghcr.io/sayef/vigily:v0.1.0-beta.3 docker compose up -d --wait
```

Keep the SDK and CLI on the same minor version as the engine; `GET /api/v1/info` reports the
engine's version and API version.
