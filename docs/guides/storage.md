# Self-hosting and storage

vigily stores everything in a **repository**: a location that holds a few Lance tables written by
[kv0](https://github.com/sayef/kv0). Runs, parameters and metric points live in kv0's tracking
tables; datasets, eval results, labels, schedules and alerts live in its key-value table under
`<repo>/records`. There is no database server.

## Locations

| Location | Example | Build feature |
|---|---|---|
| Local directory | `.vigily`, `/srv/vigily` | always |
| Amazon S3 or S3-compatible | `s3://bucket/prefix` | `s3` |
| Google Cloud Storage | `gs://bucket/prefix` | `gcs` |
| Azure Blob Storage | `az://container/prefix` | `azure` |

Object storage support is compiled in on request, which keeps the default binary small:

```bash
cargo build --release -p vigily-cli --features s3              # or gcs, azure, or several
uv run --no-sync maturin develop --release --features s3         # the Python SDK, likewise
```

Credentials come from the environment, the way each cloud's tools expect them (for S3:
`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` or a profile, `AWS_REGION`, and `AWS_ENDPOINT` for an
S3-compatible server). Prefer short-lived credentials such as SSO or workload identity over
long-lived keys, and keep the bucket in the region where the data may live.

## Many writers, one reader

Training jobs on different machines can write to the same repository at once: each run is its own
set of rows, and record writes are appends. One `vigily up` serves it and picks up new runs as they
appear. kv0 compacts the small files each finished run adds, and the server optimizes storage every
15 minutes; run `vigily optimize` by hand after a large backfill.

## Serving to a team

Run `vigily up` in server mode: it needs an admin token from the environment and accepts scoped
tokens for people and scripts (see [Tokens](configuration.md#tokens)). To share it:

1. Run it as a dedicated, unprivileged user with `--mode server` and `VIGILY_ADMIN_TOKEN` (or
   `VIGILY_ADMIN_TOKEN_FILE`) set, bound to localhost.
2. Put a reverse proxy in front that terminates TLS and sets `X-Forwarded-Proto: https`, and
   forward to `127.0.0.1:7600`.
3. If the proxy runs elsewhere, use `--host 0.0.0.0` and restrict the port to the proxy with a
   firewall rule.
4. Create a token per person or script with the scopes it needs, and revoke it when it is no
   longer used.

A systemd unit is enough:

```ini
[Unit]
Description=vigily
After=network-online.target

[Service]
User=vigily
WorkingDirectory=/srv/vigily
# A file only the vigily user can read, holding a long random value.
Environment=VIGILY_ADMIN_TOKEN_FILE=/srv/vigily/admin-token
ExecStart=/usr/local/bin/vigily up --mode server --repo /srv/vigily/repo --config /srv/vigily/vigily.toml
KillMode=control-group
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

`KillMode=control-group` also stops anything a scheduled command left behind when the service stops.

## Deleting runs

Select runs in the runs table and choose **Delete**. After you confirm, the runs disappear and you
have eight seconds to undo; then they are deleted with their parameters, metrics, evaluation
results and traces. Runs that are still running cannot be deleted: finish or abort them first.

### Deleting is not erasing: purge

Storage is versioned: a delete writes a new version without the runs, and the older versions that
still contain them stay on disk or in the bucket. Deleting in the web app (or with
`Repo.delete_runs`) therefore **hides** runs; it does not erase them. To erase them, for example
for an erasure request under GDPR Article 17, an operator runs:

```bash
vigily runs purge            # erase every run deleted so far
vigily runs purge --force    # also delete storage tags that keep deleted data
```

Purge rewrites the files that still hold rows of deleted runs (their parameters, metrics, eval
results, labels, feature samples and traces) and removes every older version except tagged ones, then
prints what it removed. It is a command-line tool on purpose: the web app can delete, but only
someone with shell access to the repository can erase.

- **Tags.** A tag keeps its version through a purge. If one keeps a version that still holds
  deleted data, purge stops, names the tag and changes nothing; `--force` deletes such tags.
  The tags vigily makes for dataset versions (cited in audit records) keep only their dataset's
  table, which holds no run data, so they never block a purge and `--force` does not remove them.
- **Concurrency.** Training can keep writing. A server or SDK reading an older version at that
  moment may have to retry one query; run purge when that is acceptable.

What purge does not reach (from kv0's operations guide):

- the write-ahead log of a training process that crashed before committing, on its own machine,
  until a process opens the repository there again (which replays the log and drops writes to
  deleted runs);
- the id of each deleted run, kept as a tombstone for 30 days so the run cannot come back;
- copies elsewhere: backups, object-store versioning or replication, caches;
- `<repo>/records.lance`, the records table of versions before per-table records, which vigily
  migrates once and leaves in place: delete it by hand after upgrading (purge reminds you);
- **disk blocks**: purge deletes files, it does not overwrite them. On a local disk or SSD the
  data may stay readable to someone with access to the raw device until the blocks are reused.
  Where that matters, keep repositories on encrypted volumes (and destroy the key with the disk),
  or use secure deletion at the storage layer; on object storage, deletion is the provider's.

## Upgrading from the shared records table

Repositories written before per-table records keep evaluation data in one table,
`<repo>/records.lance`. The first time vigily opens such a repository it copies the newest value of
every record into the per-table layout (`<repo>/records/<table>.lance`), tags dataset versions in
their new tables, and writes a marker last, so it never migrates again.

- **Upgrade with one process.** Stop training scripts and servers, then run one vigily command
  (`vigily runs -n 1` will do) and let it finish before starting the rest. On a local disk a
  lock (an operating-system lock on `<repo>/.migrating`, released when its process exits, even after
  a crash) makes a second process wait for the first; on object storage there is
  no lock, so this matters there.
- **Interrupted?** Run it again. Keys that already exist in the new layout are never overwritten,
  so a retry (or a late second process) cannot replace values written since; dataset versions
  still missing a tag are tagged on every open.
- **The old table stays.** Once you have checked the data in the web app, delete
  `<repo>/records.lance` (it still holds every record as of the upgrade, deleted ones included).
  `vigily runs purge` reminds you while it exists.

## Backups and data protection

A local repository is a directory: back it up like one, while no compaction runs, or snapshot the
volume. On object storage, use bucket versioning. Eval items, outputs and captured schedule output
are stored as given, and trace payloads as the payload mode allows
([tracing and personal data](tracing.md#tracing-and-personal-data)), so keep personal data out of
datasets or anonymise it first, and delete a
repository's data by deleting its location.
