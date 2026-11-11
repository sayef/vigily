# Getting started

There are three ways to run vigily: install the prebuilt binary, run it in Docker, or build it from
the repository. The Python SDK is not on PyPI yet, so logging runs from Python needs the build.

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
| `VIGILY_VERSION` | The release to install, such as `v0.1.0`; default the latest |
| `VIGILY_INSTALL_DIR` | Where to put the binary; default `~/.local/bin` |

On other systems (musl-based Linux such as Alpine, FreeBSD, Windows), build from source.

## Build from source

This takes a few minutes the first time and gives you the `vigily` binary and the `vigily` Python
package.

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
cargo build --release -p vigily-cli                 # target/release/vigily

# The Python SDK, into this repository's virtual environment.
uv sync --no-install-project && uv run --no-sync maturin develop --release
```

## Run with Docker

Each release publishes an image for amd64 and arm64 as `ghcr.io/sayef/vigily`, tagged `vX.Y.Z`,
`X.Y` and `latest`. From a checkout of this repository (for `compose.yaml`):

```bash
docker compose up -d                                   # the latest release
VIGILY_IMAGE=ghcr.io/sayef/vigily:v0.2.0 docker compose up -d   # a given one
```

Or without Compose:

```bash
docker run -d -p 127.0.0.1:7600:7600 -v vigily:/data ghcr.io/sayef/vigily:latest
```

To build the image from the checkout instead, add `compose.dev.yaml`. Building reads kv0, a private
git dependency for now, with a GitHub token passed as a BuildKit secret; it is used only to fetch
dependencies and is not stored in the image.

```bash
GH_TOKEN=$(gh auth token) docker compose -f compose.yaml -f compose.dev.yaml up -d --build
```

Open <http://localhost:7600>. The container runs as an unprivileged user (uid 10001) and keeps
everything under `/data`: the repository in `/data/repo`, and `/data/vigily.toml` as the
configuration when it exists. Both live in the `vigily` volume, so they survive restarts and
rebuilds; `docker compose down -v` deletes them.

The port is published on `127.0.0.1` only. vigily has no sign-in, so put your own access control
in front of it before publishing it further.

The Python SDK writes to the repository directly, so a training script has to reach the same
storage as the server. On one machine, keep the data in a directory you own instead of the volume,
and run the container with your user and group ids so the files it writes stay yours:

```bash
mkdir -p vigily-data
VIGILY_DATA=./vigily-data VIGILY_UID=$(id -u) VIGILY_GID=$(id -g) docker compose up -d
VIGILY_REPO=./vigily-data/repo python train.py      # appears in the web app as it logs
```

Create the directory first: a missing one is created by Docker and owned by root. Across machines,
point both at object storage instead: set `VIGILY_SERVER_REPO=s3://bucket/prefix` (or `gs://`,
`az://`) for the container and `VIGILY_REPO` to the same URI for your scripts, with the usual cloud
credentials in both.

LLM traces need no shared storage: they arrive over HTTP (see [LLM tracing](guides/tracing.md)).
The CLI is in the image too: `docker compose exec vigily vigily runs` lists the runs.

Scheduled evaluations run their commands inside the server's container, which has no Python. For
schedules, build your own image on top of this one with your evaluation's dependencies, or run
`vigily up` outside Docker.

`scripts/docker-local.sh` builds the binary on your machine with `cargo zigbuild` and packages it
as `vigily:local` (run it with `VIGILY_IMAGE=vigily:local docker compose up -d`), which is faster
when you rebuild often.

## Try it on synthetic data

The demo logs 43 generated training runs (nothing is trained) and 30 nights of evaluations of two
toy classifiers, with drift starting on day 20:

```bash
uv run --no-sync python examples/demo.py          # training runs, into ./.vigily
uv run --no-sync python examples/eval_demo.py     # datasets, evaluations, drift
target/release/vigily up --config examples/vigily.toml
```

Open <http://127.0.0.1:7600>. Start on **Runs**, select a few runs with `x` and press `c` to compare
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

The run lands in `./.vigily` unless you pass `repo=` or set `VIGILY_REPO`. `vigily up` in the same
directory shows it; a running server picks up new runs on its own.

## Next

- [Concepts](concepts.md): runs, metrics, datasets, evaluations, schedules, drift and alerts.
- [Python SDK](guides/python-sdk.md) and [command line](guides/cli.md).
- [Scheduled evaluations](guides/scheduled-evals.md), once you want vigily to keep watch overnight.
