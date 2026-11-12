# FAQ

## How is vigily different from MLflow, W&B, Langfuse or Evidently?

It covers ground those tools split between them, in one self-hosted binary:

- **Training runs** like an experiment tracker (MLflow, W&B): parameters, curves, a fast runs table.
- **Evaluations** on versioned datasets with per-item results, like LLM evaluation tools (Langfuse).
- **Scheduled evaluations and drift**, like monitoring tools (Evidently), with paired tests on
  matched items rather than distribution checks alone.
- **LLM traces** with token usage and cost, linked to runs and evaluation items.

It does not do artifact storage, prompt management, or a hosted service.

## Does any data leave my infrastructure?

No. vigily writes only to the repository you choose, has no telemetry, and never calls a model.
Traces travel from your process to your own `vigily up` and nowhere else.
Evaluators that need a model call your own client from your own process.

## Do I need a database or a message queue?

No. Storage is Lance tables through kv0, on local disk or object storage.

## Can several training jobs write at once?

Yes, to the same directory or bucket. See [many writers, one reader](guides/storage.md#many-writers-one-reader).

## Is there sign-in or access control?

Tokens. On localhost, `vigily up` prints a link with the repository's token that signs the browser
in. A shared server needs an admin token and accepts scoped tokens (`read`, `ingest`, `admin`) for
people and scripts; see [Tokens](guides/configuration.md#tokens) and
[serving to a team](guides/storage.md#serving-to-a-team). Accounts and single sign-on are not
there yet.

## Can the web app run commands?

No. Schedules come only from `vigily.toml` on the server. The app and the API can read schedules and
their output but cannot create, change or start them.

## How many runs does it handle?

It is tested on 2 000 synthetic runs with 4.8 million points: start-up in about 0.2 s and every request
about 0.1 s or less. Charts draw about as many points as they have pixels, so long series stay fast.

## Does deleting a run erase it?

Deleting hides it; `vigily runs purge` erases it. See
[deleting is not erasing](guides/storage.md#deleting-is-not-erasing-purge).

## Is a drift alert a decision about a model?

No. It is evidence (a test, an effect size, the regressed items) for a person to review. vigily does
not rank or score people, and its output should not be used to evaluate individuals.

## Where are the screenshots from?

From the demo in `examples/`, which logs synthetic data only.
