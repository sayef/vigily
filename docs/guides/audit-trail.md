# The audit trail

Every eval result in vigily can be traced to what produced it. An eval run's page shows the trail
on its **Audit** tab; **Export the audit record (JSON)** there, or `GET /api/v1/runs/{id}/audit`,
returns it as one JSON document.

## What is recorded

| Part | Fields |
|---|---|
| Run | id, name, experiment, kind, status, tags, notes, start and end in UTC, environment |
| Target | the `target_ref` you passed: model, prompt, git commit, anything else |
| Dataset | name, version, digest, the storage version and tag that hold it, and each item's id and content hash |
| Evaluators | name and version of each, including the source hash of custom evaluators |
| Results | per item: output, error, latency, every score with its value, pass or fail and reason |
| Labels | human labels added in the web app, with comment and time |
| Schedule | schedule name, schedule run, attempt, scheduled time, command, exit code |
| Storage | the repository and the storage version the export was read from |
| Export | the vigily version and the export time in UTC |

Each dataset version tags the storage version that holds it, so cleaning up old storage versions
never removes it. Dataset versions are immutable and content-addressed, so the digest in an export identifies exactly
the items that were evaluated, even after later versions or compaction.

## Using it

- Attach the export to a model change request or a release record.
- Compare two exports to see what changed between evaluations.
- Keep exports with your quality records for monitoring deployed models.

The trail supports record keeping for monitoring deployed models, for example post-market
monitoring under the EU AI Act. Using vigily does not by itself satisfy any legal requirement;
whether your records are sufficient is for your own review.

Exports contain the outputs and items as stored. If items hold personal data, the export does too:
handle it under the same rules as the dataset.
