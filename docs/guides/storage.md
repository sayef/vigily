# Self-hosting and storage

vigily stores everything in a **repository**: a location that holds one
[moraine](https://github.com/sayef/moraine) database, written through
[kv0](https://github.com/sayef/kv0): Delta Lake tables (Parquet files and a commit log) in a
directory or a bucket. Runs, parameters and metric points live in kv0's tracking tables; datasets,
eval results, labels, schedules and alerts live in its records tables next to them. There is no
database server.

A repository written by vigily 0.1.0-beta.1 or earlier cannot be opened by this version; see
[Upgrading an earlier repository](#upgrading-an-earlier-repository).

## Locations

| Location | Example |
|---|---|
| Local directory | `.vigily`, `/srv/vigily` |
| Amazon S3 or S3-compatible | `s3://bucket/prefix` |

Every build reaches both. Google Cloud Storage (`gs://`) and Azure Blob Storage (`az://`) are not
supported by the storage engine yet: such a location fails with "not supported". The build
features `s3`, `gcs` and `azure` are still accepted and change nothing for now.

S3 credentials come from the `AWS_*` environment variables: `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY` and `AWS_SESSION_TOKEN`, `AWS_REGION`, and `AWS_ENDPOINT` for an
S3-compatible server. AWS profiles and SSO sessions are not read; export short-lived credentials
from them instead (`eval "$(aws configure export-credentials --format env)"`), and prefer those,
or workload identity, over long-lived keys. Keep the bucket in the region where the data may live.

## One engine, many jobs

One engine (`vigily-engine`) owns a repository and is the only process that writes it. Training
jobs log to it over HTTP: on the same machine through the repository's local engine, which the SDK
finds or starts, and from other machines through a shared engine at `VIGILY_URL`
([Running a team server](team-server.md)). The engine writes each batch of points with one storage
append and commits about once a second, so new points show up in the web app within a second or
two. kv0 compacts the small files each finished run adds, and the engine optimizes storage every
15 minutes; run `vigily optimize` by hand after a large backfill.

An SDK from an earlier development build writes the repository directly; it can still do so next to a
running engine, which picks up its runs on its next refresh, but this is deprecated.

## Serving to a team

Run the engine (`vigily-engine`, which `vigily up` starts) in server mode: it needs an admin token
from the environment and accepts scoped tokens for people and scripts (see
[Tokens](configuration.md#tokens)). With Docker, [Running a team server](team-server.md) has the
whole setup. Without it:

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
ExecStart=/usr/local/bin/vigily-engine --mode server --repo /srv/vigily/repo --config /srv/vigily/vigily.toml
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
- **disk blocks**: purge deletes files, it does not overwrite them. On a local disk or SSD the
  data may stay readable to someone with access to the raw device until the blocks are reused.
  Where that matters, keep repositories on encrypted volumes (and destroy the key with the disk),
  or use secure deletion at the storage layer; on object storage, deletion is the provider's.

## Upgrading an earlier repository

vigily 0.1.0-beta.1 and earlier stored repositories with kv0's Lance engine. kv0 now stores
everything with moraine, and this version of vigily cannot open those repositories: the engine
stops with an error that names the location and says it is a repository of an earlier vigily. It
never modifies the old repository.

I chose not to ship a migration while vigily is in beta. To upgrade:

1. Stop training scripts and the engine (`vigily down`).
2. Keep what you need with the version that wrote it, while it still serves the old repository:
   download the runs table as CSV, export the audit records of eval runs you must keep, and read
   runs, metrics, datasets and results with the Python SDK's `Repo` and `Dataset` to log them again
   into the new repository. Or keep the old repository and that release side by side to look
   things up.
3. Point `VIGILY_REPO` (or `--repo`) at a new, empty location, or move the old directory aside,
   and start the new version. It creates the repository on first use.
4. Delete the old repository once you no longer need it.

Storage versions are now readable only when they are the latest or tagged. vigily tags every
dataset version it commits, so audit records keep naming versions you can read back; nothing in
vigily reads other old versions.

## Backups and data protection

A local repository is a directory: back it up like one, while no compaction runs, or snapshot the
volume. On object storage, use bucket versioning. Eval items, outputs and captured schedule output
are stored as given, and trace payloads as the payload mode allows
([tracing and personal data](tracing.md#tracing-and-personal-data)), so keep personal data out of
datasets or anonymise it first, and delete a
repository's data by deleting its location.
