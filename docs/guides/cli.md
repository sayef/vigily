# Command line

```text
vigily up                 serve the web app, the API, the schedules and the drift monitor
vigily status             whether an engine serves the repository, and where
vigily stop               stop the repository's local engine
vigily open               print the sign-in link and open it in a browser
vigily env                print VIGILY_URL and VIGILY_TOKEN as shell exports
vigily runs               list runs in a terminal, newest first
vigily runs purge         erase deleted runs (and deleted traces) from storage
vigily traces delete      delete traces by id; --erase also erases them
vigily schedules          list the engine's schedules, or run one now
vigily optimize           compact storage and build the metrics index
vigily tokens             list, create or revoke access tokens
vigily token rotate       replace the local engine's token
```

`vigily` is a client: every command but `up` talks to the engine that serves the repository over
HTTP, and the binary contains no storage code. Global options:

| Option | Default | |
|---|---|---|
| `--repo`, `-r` | `VIGILY_REPO`, then `./.vigily` | A directory or an object-store URI such as `s3://bucket/prefix` |
| `--url` | `VIGILY_URL` | An engine's address, such as a team server; the repository's local engine is not used or started then |
| `--token` | `VIGILY_TOKEN` | The token to send; a local engine's own token by default |

Without `--url`, a command finds the repository's engine through its file in the
[state directory](#the-state-directory). When none runs, it starts one in the background
(`vigily-engine --detach`, found beside `vigily`, on `PATH`, or at `VIGILY_ENGINE`), waits for it
(at most 30 seconds) and uses it; that engine stops after 30 idle minutes. Commands that only read
or change an existing repository refuse to start an engine for a directory that does not exist:
`vigily runs` on an empty directory says there is no repository. `vigily status` and `vigily stop`
never start one.

Exit codes: 0 on success, 1 on an error (the engine's message is printed), 2 for bad options.

## vigily up

| Option | Default | |
|---|---|---|
| `--host` | `127.0.0.1` | Anything but a loopback address serves every host name |
| `--port`, `-p` | `7600` | Also `VIGILY_PORT` |
| `--config` | `./vigily.toml` if present | Schedules, alert webhook, drift thresholds; also `VIGILY_CONFIG` |
| `--optimize-every` | `15m` | How often storage is optimized in the background; at least `1m` |
| `--optimize-at-start` | off | Also optimize before serving: indexed charts from the first request, about 3 s slower start-up and more memory at once |
| `--no-optimize` | off | Never optimize in the background |
| `--mode` | `local` on a loopback address, else `server` | `local`: the repository's token; `server`: needs `VIGILY_ADMIN_TOKEN` and accepts scoped tokens |
| `--token-file` | `engines/<key>.token` in the state directory | Where local mode keeps its token; also `VIGILY_TOKEN_FILE` |
| `--insecure-no-auth` | off | Check no tokens at all, for an engine behind your own access control |

In local mode `vigily up` prints a link with the repository's token; opening it signs the browser
in. On localhost the server also checks the `Host` header against DNS rebinding. See
[Tokens](configuration.md#tokens) for both modes and scoped tokens. `VIGILY_LOG` sets the log
filter, for example `VIGILY_LOG=vigily_server=debug`.

One engine serves a repository on a machine: a second `vigily up` on the same repository exits
with code 3 and names the address the first one serves. Ctrl-C and SIGTERM (`kill`,
`docker stop`) let open requests finish and write out everything held in memory before exiting.

## vigily-engine

`vigily up` runs the engine in the foreground; the same server is also its own binary,
`vigily-engine`, with the flags of `vigily up` plus:

| Option | Default | |
|---|---|---|
| `--repo`, `-r` | `VIGILY_REPO`, then `./.vigily` | The repository |
| `--state-dir` | `VIGILY_STATE_DIR`, else `~/Library/Caches/vigily` on macOS and `~/.cache/vigily` elsewhere | Where engines note their address and keep their token and log |
| `--detach` | off | Start in the background, print the address once it answers, and return |
| `--idle-timeout` | `30m` with `--detach`, else never | Stop after this long without requests, runs that showed a sign of life in the last 10 minutes, or enabled schedules; `0s` never stops |
| `--check-config FILE` | | Check a configuration file and exit: 0 when valid, 1 with the problem |

A background engine takes any free port when its own is taken. Exit codes: 0 after a clean stop, 1
on an error, 2 for bad flags, 3 when another engine already serves the repository.

### The state directory

```text
engines/<key>.json    address, process id, version, mode and token of the running engine (0600)
engines/<key>.lock    held by the engine while it runs
engines/<key>.token   the repository's local token, the same each start (0600)
engines/<key>.log     output of an engine started with --detach
```

`<key>` is the first 16 hex digits of the SHA-256 of the repository's location (an absolute path
with symbolic links resolved, or the URI). The directory is created private (0700). The JSON file
is written once the engine listens and removed when it stops; a file whose lock nobody holds is
left over from a crash and ignored. A token kept by an earlier release under `tokens/` is moved
here on the first start.

## vigily runs

```bash
vigily runs "exp:cifar10 status:finished" --sort metric:min:loss[val] -n 10
```

The query is the same language as the web app's filter (see below). `--sort` takes `run:name`,
`param:<key>` or `metric:<last|min|max>:<metric>`; `--desc` reverses it.

## vigily runs purge

Erases runs deleted in the web app from storage, with their traces, prints what it removed, and
names any storage tag that blocks it (`--force` deletes such tags). It runs a full optimize first,
so traces deleted without a run and trace rows moved under a run are erased as well; on a large
repository that takes as long as `vigily optimize`, and it may overlap a running server's
background optimize (see [deleting and erasing](tracing.md#deleting-and-erasing)). Deleting only hides runs; see
[deleting is not erasing](storage.md#deleting-is-not-erasing-purge) for what purge does and does
not reach.

## vigily traces delete

```bash
vigily traces delete 4bf92f3577b34da6a3ce929d0e0e4736            # hide it
vigily traces delete 4bf92f3577b34da6a3ce929d0e0e4736 --erase    # and erase it from storage
```

Deletes traces by id, for traces not linked to a run (those go with their run). `--erase` then
compacts and purges as `vigily runs purge` does; see
[tracing and personal data](tracing.md#deleting-and-erasing).

## vigily schedules

```bash
vigily schedules                       # the engine's schedules, with the next fire time
vigily schedules run nightly-intents   # run one now on the engine and follow it
```

The schedules are those of the engine's configuration (`vigily up --config`). `run` starts the
schedule on the engine, with the same timeout, retries and output capture as a scheduled run,
prints each attempt as it ends, and exits 1 unless the last attempt succeeded. It needs an admin
token (a local engine's own token is one); see [the security model](scheduled-evals.md#security-model).

## vigily status, stop, open, env

`vigily status` prints the engine's address, version, mode, process and how it was started, and
exits 1 when no engine serves the repository. `vigily stop` sends the local engine SIGTERM and waits
until it has flushed and exited. `vigily open` prints the link that signs a browser in (with the
local token) and opens it when a desktop is available (`--no-browser` only prints it).
`vigily env` prints `export VIGILY_URL=...` and `export VIGILY_TOKEN=...` for scripts:

```bash
eval "$(vigily env)"
```

## vigily tokens, vigily token rotate

```bash
vigily tokens list
vigily tokens create nightly-evals --scope read --scope ingest --expires-in 90d
vigily tokens revoke 3f9c0d1e2a4b5c6d
vigily token rotate
```

Access tokens carry the scopes `read`, `ingest` and `admin` (see [Tokens](configuration.md#tokens));
`create` prints the token once. An SDK or script that logs runs needs `read` and `ingest`.
`token rotate` replaces a local engine's own token: the old link, cookie and exported
`VIGILY_TOKEN` stop working, and the new link is printed.

## vigily optimize

Compacts the repository's tables and indexes metrics, on the engine. The engine does this on its
own every 15 minutes; run it by hand after a large backfill.

## The query language

| Query | Matches |
|---|---|
| `resnet` | a word in the name, experiment, tags or notes |
| `status:failed` | running, finished, failed or aborted |
| `lr<0.01 batch_size>=64` | parameters, by full dotted key or by their last part |
| `optimizer.name=adamw` | text, ignoring case |
| `loss[val]<0.3`, `loss[val].min<0.25` | a metric's last value, or `.min`, `.max`, `.count` |
| `duration>10m age<2d` | durations in s, m, h or d |
| `kind:eval target.model=clf-v2` | evaluation runs and their target |
| `tag:baseline -tag:old` | `-` or `not` negates |
| `exp:mnist or exp:cifar` | either side |

Errors name the position of the problem, and the web app shows them under the field.
