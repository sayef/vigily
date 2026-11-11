# Command line

```text
vigily up         serve the web app, the API, the schedules and the drift monitor
vigily runs       list runs in a terminal, newest first
vigily runs purge erase deleted runs (and deleted traces) from storage
vigily traces delete  delete traces by id; --erase also erases them
vigily schedules  list the schedules of a configuration file, or run one now
vigily optimize   compact storage and build the metrics index
```

Every command takes `--repo` (`-r`): a directory or an object-store URI such as
`s3://bucket/prefix`. It defaults to `VIGILY_REPO`, then `./.vigily`.

## vigily up

| Option | Default | |
|---|---|---|
| `--host` | `127.0.0.1` | Anything but a loopback address serves every host name |
| `--port`, `-p` | `7600` | Also `VIGILY_PORT` |
| `--config` | `./vigily.toml` if present | Schedules, alert webhook, drift thresholds; also `VIGILY_CONFIG` |
| `--optimize-every` | `15m` | How often storage is optimized in the background; at least `1m` |
| `--optimize-at-start` | off | Also optimize before serving: indexed charts from the first request, about 3 s slower start-up and more memory at once |
| `--no-optimize` | off | Never optimize in the background |

On localhost the server checks the `Host` header against DNS rebinding. It has no sign-in: before
serving on another address, put it behind your own access control (see
[self-hosting](storage.md#serving-to-a-team)). `VIGILY_LOG` sets the log filter, for example
`VIGILY_LOG=vigily_server=debug`.

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
vigily schedules --config vigily.toml                    # list, with the next fire time
vigily schedules --config vigily.toml run nightly-intents
```

`run` executes a schedule once, now, with the same timeout, retries and output capture as the
server. It is the only way to start a schedule by hand; see [the security model](scheduled-evals.md#security-model).

## vigily optimize

Compacts the repository's tables and indexes metrics. `vigily up` does this on its own; run it by
hand after a large backfill when no server is running.

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
