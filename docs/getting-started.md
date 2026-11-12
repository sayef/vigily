# Getting started

There are three ways to run vigily: install the prebuilt binary, run it in Docker, or build it from
the repository. The Python SDK is pure Python and talks to the engine over HTTP; it is not on PyPI
yet, so install it from a checkout (`pip install ./vigily` or `uv sync` in the repository).

## Install

Each GitHub release carries prebuilt `vigily` binaries for Linux (x86_64 and arm64, glibc 2.31 or
newer: Ubuntu 20.04, Debian 11, RHEL 9 and later) and macOS (Apple silicon and Intel):

```bash
curl -LsSf https://raw.githubusercontent.com/sayef/vigily/main/scripts/install.sh | sh
```

The script downloads the archive for your system, checks its SHA-256 against the release's
checksum file, and installs `vigily` to `~/.local/bin` without sudo; it tells you how to add that
directory to your `PATH` if it is not there yet. To read it before running it:

```bash
curl -LsSfO https://raw.githubusercontent.com/sayef/vigily/main/scripts/install.sh
less install.sh
sh install.sh
```

| Variable | Meaning |
|---|---|
| `VIGILY_VERSION` | The release to install, such as `v0.1.0-beta.1`; default the latest |
| `VIGILY_INSTALL_DIR` | Where to put the binary; default `~/.local/bin` |

Until 0.1.0 every release is a beta, published as a pre-release, and `latest` serves none of
them: set `VIGILY_VERSION` (or `VIGILY_IMAGE` for Docker) to a beta tag such as `v0.1.0-beta.1`.

On other systems (musl-based Linux such as Alpine, FreeBSD, Windows), build from source.

## Build from source

This takes a few minutes the first time and gives you the `vigily` and `vigily-engine` binaries and
the `vigily` Python package.

### Requirements

- Rust (stable), with `cargo`
- Node 24 (only to build the web app; nothing runs on Node afterwards)
- [uv](https://docs.astral.sh/uv/) for the Python side
- `protoc`, the Protocol Buffers compiler (`brew install protobuf` or `apt install protobuf-compiler`)

Storage comes from [kv0](https://github.com/sayef/kv0), which cargo fetches as a git dependency.

### Build

```bash
git clone https://github.com/sayef/vigily && cd vigily

# The web app first: the binary embeds crates/vigily-ui/dist when it is compiled.
(cd crates/vigily-ui && npm ci && npm run build)
cargo build --release -p vigily-cli -p vigily-server # target/release/vigily and vigily-engine

# The Python SDK (pure Python), into this repository's virtual environment. It starts
# vigily-engine from PATH (or VIGILY_ENGINE) when no engine serves the repository.
uv sync
export PATH="$PWD/target/release:$PATH"
```

## Run with Docker

Each release publishes an image for amd64 and arm64 as `ghcr.io/sayef/vigily`, tagged `vX.Y.Z`,
`X.Y` and `latest`. It runs `vigily-engine --host 0.0.0.0 --mode server`: a shared engine that
needs an admin token. From a checkout of this repository (for `compose.yaml`):

```bash
export VIGILY_ADMIN_TOKEN=$(openssl rand -hex 32)     # keep it: you sign in with it
docker compose up -d                                   # the latest release
VIGILY_IMAGE=ghcr.io/sayef/vigily:v0.1.0-beta.1 docker compose up -d   # a given one
```

Or without Compose:

```bash
docker run -d -p 127.0.0.1:7600:7600 -e VIGILY_ADMIN_TOKEN -v vigily:/data ghcr.io/sayef/vigily:latest
```

The engine refuses to start without `VIGILY_ADMIN_TOKEN` (at least 16 characters;
`VIGILY_ADMIN_TOKEN_FILE` names a file holding it instead, for Docker secrets). vigily never stores
it. Compose reads it from your shell or from a `.env` file next to `compose.yaml`.

To build the image from the checkout instead, add `compose.dev.yaml`:

```bash
docker compose -f compose.yaml -f compose.dev.yaml up -d --build
```

Open <http://localhost:7600> and sign in with the admin token. The container runs as an
unprivileged user (uid 10001) and keeps everything under `/data`: the repository in `/data/repo`,
and `/data/vigily.toml` as the configuration when it exists. Both live in the `vigily` volume, so
they survive restarts and rebuilds; `docker compose down -v` deletes them. `docker compose stop`
sends SIGTERM, and the engine writes out what it holds in memory before it exits.

Training scripts log to the container over HTTP, from this machine or any other that reaches it.
Create a token for them with the `read` and `ingest` scopes (`ingest` alone cannot read the run
back), and point the SDK at the engine:

```bash
docker compose exec vigily sh -c \
  'vigily --url http://127.0.0.1:7600 --token "$VIGILY_ADMIN_TOKEN" tokens create train --scope read --scope ingest'
VIGILY_URL=http://localhost:7600 VIGILY_TOKEN=vgy_... python train.py   # appears in the web app as it logs
```

LLM traces travel the same way (see [LLM tracing](guides/tracing.md)). The CLI is in the image
too: `docker compose exec vigily sh -c 'vigily --url http://127.0.0.1:7600 --token
"$VIGILY_ADMIN_TOKEN" runs'` lists the runs.

The port is published on `127.0.0.1` only. To share the engine with a team (tokens per person, TLS
through a reverse proxy, backups of the volume), see [Running a team server](guides/team-server.md).

Scheduled evaluations run their commands inside the engine's container, which has no Python. For
schedules, build your own image on top of this one with your evaluation's dependencies, or run
`vigily up` outside Docker.

`scripts/docker-local.sh` builds the binaries on your machine with `cargo zigbuild` and packages
them as `vigily:local` (run it with `VIGILY_IMAGE=vigily:local docker compose up -d`), which is
faster when you rebuild often.

## Try it on synthetic data

The demo logs 43 generated training runs (nothing is trained) and 30 nights of evaluations of two
toy classifiers, with drift starting on day 20:

```bash
uv run --no-sync python examples/demo.py          # training runs, into ./.vigily
uv run --no-sync python examples/eval_demo.py     # datasets, evaluations, drift
vigily stop                                       # the engine the scripts started for ./.vigily
vigily up --config examples/vigily.toml           # serve it with the demo's schedules and prices
```

Open the link `vigily up` prints (`http://127.0.0.1:7600/?token=...`): the token signs your
browser in and is kept per repository, so the link stays the same. One engine runs per
repository: a second `vigily up` on the same one says where the first is. Start on **Runs**, select a few runs with `x` and press `c` to compare
their curves; then open **Drift** to see what the nightly evaluations found.

## Log your own run

```python
from vigily import Run

with Run(experiment="cifar10", name="resnet18-lr3e-3") as run:
    run["hparams"] = {"lr": 3e-3, "batch_size": 128}
    for step in range(1000):
        loss = train_step()
        run.track(loss, name="loss", step=step, context={"subset": "train"})
```

The run lands in `./.vigily` unless you pass `repo=` or set `VIGILY_REPO`. The SDK sends it to the
engine that serves that repository, and starts one in the background when none does (it stops
after 30 minutes without use); `vigily open` in the same directory opens the web app on it. With
`VIGILY_URL` and `VIGILY_TOKEN` set, runs go to that engine instead, such as a team server.

## Next

- [Concepts](concepts.md): runs, metrics, datasets, evaluations, schedules, drift and alerts.
- [Python SDK](guides/python-sdk.md) and [command line](guides/cli.md).
- [Scheduled evaluations](guides/scheduled-evals.md), once you want vigily to keep watch overnight.
