# TODO

Next, roughly in order.

## Storage

- Stream points as Arrow (kv0 item 7) for very long series.

## Tracking

- System metrics from the SDK (CPU, memory, GPU) on a background thread, as `__system__/...` series.
- Images, histograms and text per step.
- Remote logging over HTTP for clusters that cannot write to shared storage.
- Import from MLflow and TensorBoard logs (`created_at` and `timestamp` already accept past times).

## Evaluations

- Datasets: edit items in the web app, split into subsets, sample for quick runs.
- More built-in evaluators (BLEU/ROUGE, embedding similarity) behind optional dependencies.
- Drift: per-segment analysis (by item metadata such as language), seasonal baselines, and
  multiple-testing control when many scores and features are checked.
- Alert routing rules, quiet hours, and notifications by e-mail.
- A signed (hash-chained) audit log of label and baseline changes.

## Tracing

- One row per span once kv0 reads several item ids per query (kv0 item 23).
- OTLP protobuf, and gRPC, if exporters that cannot send JSON matter.
- Retention for traces not linked to a run (a maximum age per project), and erasing them with purge.
- Search in span inputs and outputs; cost and latency charts over time per model.
- Avoid reading chunk 0 on every ingest and page load for traces without a run: a cheap marker on
  the summary or the `tsid` row would say whether a link to a run is pending.
- The traces list shows no run for a half-linked trace (spans moved under the run, summary not yet)
  until the trace's next ingest repairs the summary.

## Web app

- Parameter comparison view (parallel coordinates or a scatter of a parameter against a metric).
- Saved views: named filters, columns and chart settings.
- Resizable and reorderable table columns.
- Show the selected runs on the runs page when the filter hides them.
- Compare two runs side by side: parameters that differ, curves overlaid.
- Live tailing of running runs without polling (server-sent events).
- Bulk actions on the selection: archive, tag.

## Project

- Publish to crates.io and PyPI once kv0 publishes.
- Publish the documentation site on GitHub Pages once the repository is public.
