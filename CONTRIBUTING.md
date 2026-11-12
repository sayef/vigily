# Contributing

Thanks for helping. Bug reports, ideas and pull requests are welcome; please open an issue before a
large change so we can agree on the approach.

## Set up

You need Rust (stable), Node 24 (`crates/vigily-ui/.nvmrc`), [uv](https://docs.astral.sh/uv/) and
`protoc`. Storage comes from [kv0](https://github.com/sayef/kv0), a git dependency that cargo fetches
for you (see [Storage dependency](#storage-dependency)).

```bash
(cd crates/vigily-ui && npm ci && npm run build)   # before cargo: build.rs embeds dist
cargo build
uv sync --no-install-project && uv run --no-sync maturin develop
uv run --no-sync python examples/demo.py && cargo run -p vigily-cli -- up
```

For UI work, `npm run dev` in `crates/vigily-ui` serves the app with hot reload on
`http://127.0.0.1:5173` and proxies the API to a running `vigily up`.

## Storage dependency

vigily depends on kv0 by git, on its `main` branch:

```toml
kv0 = { git = "https://github.com/sayef/kv0", branch = "main", version = "0.2", default-features = false }
```

Cargo builds from git; the `version` is what crates.io will use once kv0 is published, and it keeps
`cargo deny` from treating the dependency as a wildcard. `deny.toml` allows this one git source.

`Cargo.lock` pins the exact commit, so every build uses the same kv0 until someone runs
`cargo update -p kv0`.

To work on vigily and kv0 together, point cargo at a local checkout without editing `Cargo.toml`;
keep this in your own, uncommitted `~/.cargo/config.toml` or pass it on the command line:

```toml
[patch."https://github.com/sayef/kv0"]
kv0 = { path = "../kv0/crates/kv0" }
```

vigily talks to kv0 through two thin adapters, `crates/vigily-core/src/kv0_store.rs` (runs and
metrics) and `crates/vigily-core/src/records.rs` (datasets, evaluations, schedules and alerts).
Moving to another kv0 branch is one change: the `branch` in `Cargo.toml`.

## Checks

Run these before opening a pull request; CI runs the same:

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --exclude vigily-py        # or cargo test
(cd crates/vigily-ui && npm run lint && npm run typecheck && npm run build)
uv run --no-sync ruff check && uv run --no-sync ruff format --check
uv run --no-sync maturin develop && uv run --no-sync pytest
cargo deny check --warn unmaintained                     # advisories, licences, sources
uvx --with-requirements docs/requirements.txt mkdocs build --strict
```

CI also runs these; each is a single binary, so `uvx` or `cargo install` is enough locally:

| Check | Command | Configuration |
| --- | --- | --- |
| Spelling | `typos` | `_typos.toml` |
| Workflow syntax | `actionlint` | none |
| Workflow security | `zizmor .github` | none; actions are pinned to commit hashes |
| Dockerfile | `hadolint Dockerfile` | inline `# hadolint ignore=` comments with a reason |
| Unused dependencies | `cargo machete` | `[package.metadata.cargo-machete]` in a crate's `Cargo.toml` |
| Minimum Rust version | `cargo +1.94.1 check --workspace --all-targets` | `rust-version` in `Cargo.toml` |

When a check reports a false positive, add an ignore in its configuration with a comment that says
why, rather than disabling the check.

### Pre-commit hooks

`pre-commit install` (or `prek install`) runs formatting, clippy, ruff, eslint, typos, actionlint,
zizmor, hadolint and a secret scan on each commit; `pre-commit run --all-files` runs them on
everything. The eslint hook runs `npm ci` in `crates/vigily-ui` first if `node_modules` is missing,
so it works from a fresh clone.

## UI changes

Read [brand/DESIGN.md](brand/DESIGN.md) first: one accent, colours only as tokens, numbers in tabular
figures, every state designed. With `vigily up` running on the demo repository,
`npm run qa` in `crates/vigily-ui` takes screenshots of every screen in both themes at desktop and
phone widths, checks for console errors, overflow and small targets, and runs a keyboard-only pass.
Attach the relevant screenshots to the pull request.

`scripts/screenshots.sh` regenerates the screenshots in `docs/assets/screenshots` in one command:
it builds everything, logs the demo data into a fresh repository under `/tmp/vigily`, serves it,
runs `npm run qa` and keeps the screens the README and the docs use
(`assets/src/screenshots.py`). `SKIP_BUILD=1` reuses what is already built.

## Benchmarks

`bench/scale.py` logs synthetic runs and `bench/scale.sh` times the server's API on them (see
[architecture](docs/architecture.md)). The Benchmarks workflow runs both on demand, or on a pull
request labelled `bench`, and writes the timings to the job summary; it never fails the build.

## Releasing

Releases are automated and stay off until the `RELEASE_ENABLED` repository variable is `true`
(Settings, Secrets and variables, Actions, Variables). With it on:

1. Every push to `main` updates a release pull request (Release-plz workflow,
   `release-plz.toml`): the next version in `Cargo.toml` and the notes under `## [Unreleased]` in
   `CHANGELOG.md` moved under that version. Edit the notes or the version in that pull request.
2. Merging it tags `vX.Y.Z` and creates a draft GitHub release with those notes.
3. The Release workflow then builds the binaries (Linux on glibc 2.31, macOS for both
   architectures) and the Python wheels, attaches them with their `.sha256` files and publishes the
   release as a pre-release. It pushes the image to `ghcr.io/sayef/vigily` as `vX.Y.Z` and `X.Y`
   (amd64 and arm64, with provenance and an SBOM).
4. It installs the release with `scripts/install.sh` on Linux and macOS (Install check) and runs
   the Linux binaries on older distributions (Portability).
5. Only when all of that passed does it mark the release as the latest and tag the image `latest`.
   A failed check leaves a pre-release that `latest` downloads never see; fix it and release again,
   or run the Release workflow by hand for the tag.

A tag pushed by hand (`git tag v0.2.0 && git push origin v0.2.0`) goes through steps 3 to 5 as
well. Tags with a suffix, such as `v0.2.0-rc.1`, stay pre-releases and never become `latest`.
Nothing is published to crates.io or PyPI: kv0 is not on crates.io yet.

What the workflows need:

- `RELEASE_PLZ_TOKEN` (secret, optional): a GitHub App token or fine-grained token with contents and
  pull-request write access, so the release pull request gets CI runs (one opened with the
  workflow's own token does not).
- Settings, Actions, General: "Allow GitHub Actions to create and approve pull requests".
- The first image push creates the `vigily` package on ghcr.io as private; make it public once in
  the package settings.

## Commits

Small, focused commits with an imperative subject line ("Add a log scale to charts"). Use synthetic
data in tests, fixtures and screenshots; never real names, emails or credentials.
