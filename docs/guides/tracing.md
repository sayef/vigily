# LLM tracing

A **trace** records one request through an LLM application: the retrieval, tool and model calls it
made, how long each took, whether it failed, the tokens each model call used and what they cost.
The Python SDK records traces with `vigily.trace(...)` and sends them to `vigily up`, which applies
the payload policy, prices every model call and stores the trace next to your runs. The web app lists
traces under **Track › Traces** and shows each one as a waterfall of its spans.

## Record traces from Python

Start the server (`vigily up`), then wrap the work you want to see:

```python
import vigily


@vigily.trace(kind="retrieval")
def search_docs(query: str) -> list[str]: ...  # arguments and return value are recorded


def answer(question: str, run: vigily.Run) -> str:
    with vigily.trace("answer_question", run=run) as root:
        root.set_input(question)
        docs = search_docs(question)
        with vigily.trace("chat", kind="llm", model="example-large", provider="example") as llm:
            llm.set_input([{"role": "user", "content": question}])
            reply = my_client.complete(question, docs)  # your own model client
            llm.set_output(reply.text)
            llm.set_usage(input_tokens=812, output_tokens=95, cached_tokens=512)
        root.set_output(reply.text)
        return reply.text
```

- **Nesting.** A `trace` opened inside another becomes its child; the outermost one starts a new
  trace. Nesting uses `contextvars`, so it follows `asyncio` tasks: `async with vigily.trace(...)`
  and decorated `async def` functions work, and coroutines that run concurrently keep their own
  parents. A plain `ThreadPoolExecutor` does not carry context into its threads; submit
  `contextvars.copy_context().run(fn, ...)` when a thread's spans should join the caller's trace.
- **Kinds.** `llm`, `tool`, `retrieval`, `chain` (the default), `embedding`, `agent`, or another
  short word. The kind is a label in the waterfall; only spans with a model and token usage are
  priced.
- **Errors.** An exception leaving a `trace` block or decorated function marks the span failed with
  the exception's type and message, and is raised again. `span.record_error(...)` does the same by
  hand.
- **Runs and evaluations.** `run=` (a `Run` or a run id) links the trace to a run: it is listed on
  the run's page and deleted with the run. Children inherit it. Inside `evaluate()`, traces your
  target starts are linked to the eval run and the item automatically, and the item's row in the
  web app opens its trace.
- **Attributes.** `span.set_attribute("top_k", 4)` or `attributes={...}`: small values that help
  you filter and read a trace (prompt version, index name, temperature). At most 128 per span, with
  keys of at most 128 bytes; the rest are counted and left out.
- **Generators.** On a generator or async generator, `@vigily.trace` records the whole iteration,
  from the first item until the generator is exhausted or closed, with the number of items as the
  attribute `items` and the return value as the output. The span is current only while the
  generator's own code runs, not in your loop between items.
- **Processes.** A process forked after tracing started (multiprocessing with `fork`) starts with an
  empty queue and its own export thread; spans queued before the fork are sent by the parent.

### Token usage

`span.set_usage(input_tokens=, output_tokens=, cached_tokens=)` records a model call's usage.
`cached_tokens` are the input tokens served from the provider's prompt cache and are counted within
`input_tokens`. `span.set_usage(response=reply)` reads the counts from a client response without
importing any client library: it understands `usage.prompt_tokens` / `completion_tokens` /
`prompt_tokens_details.cached_tokens` and `usage.input_tokens` / `output_tokens` /
`cache_read_input_tokens` (adding cache reads to the input when the API counts them apart). It is
an opt-in helper: vigily never patches or wraps a client library.

### Export

Finished spans go into a queue; a background thread sends them in batches (256 spans, or after one
second) to `POST /api/v1/traces/ingest`, so tracing never waits on the network. Queued spans are sent
when the interpreter exits normally; call `vigily.tracing.flush()` before `os._exit` or when a
short-lived worker must not lose its last spans. If the server cannot be reached, the batch is
retried twice and then dropped with one warning on stderr: tracing never fails your program.

| Setting | Default |
|---|---|
| `vigily.tracing.configure(url=...)` or `VIGILY_URL` | `http://127.0.0.1:7600` |
| `configure(enabled=False)` or `VIGILY_TRACING=off` | on |
| `configure(batch_size=, flush_interval=, max_queue=)` | 256 spans, 1 s, 10 000 queued spans |

### OpenTelemetry

The server also accepts **OTLP/HTTP with JSON encoding** at `POST /v1/traces`, so an application
instrumented with OpenTelemetry can send to vigily directly (set the exporter's endpoint to
`http://127.0.0.1:7600` and its protocol to `http/json`; protobuf is refused with 415). GenAI
semantic-convention attributes become the span's model (`gen_ai.request.model`,
`gen_ai.response.model`), provider (`gen_ai.provider.name`, `gen_ai.system`), usage
(`gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, cache reads) and inputs and outputs
(`gen_ai.input.messages`, `gen_ai.output.messages`); `gen_ai.operation.name` sets the kind.
`vigily.run_id`, `vigily.eval_item` and `vigily.project` link the trace: put them on the
**resource** (they then apply to every span the process sends) or on the root span. Children often
arrive before their root; until a batch names the run, the trace has none, and its project (hence
its payload policy) is not known, so the strictest configured policy applies to those spans. Other
attributes are kept as they are.

## Cost

Cost is computed by the server when a span arrives, from a price table in
[`vigily.toml`](configuration.md): prices per million tokens, per model, in one currency.
vigily ships **no price list**: prices change and differ by contract, so you enter the ones you
pay. The table below is an **example with made-up models and prices**, not a recommendation:

```toml
[tracing]
currency = "EUR"                       # shown with every cost; default USD

[tracing.prices."example-large"]       # an exact model name
input = 2.50                           # per million input tokens
output = 10.00                         # per million output tokens
cached_input = 1.25                    # per million cached input tokens; default: the input price

[tracing.prices."example-small*"]      # a prefix: every model whose name starts with example-small
input = 0.15
output = 0.60
```

A call costs `(input − cached) × input + cached × cached_input + output × output`, divided by a
million. An exact name wins over a pattern, and a longer pattern over a shorter one. Each trace keeps
the currency it was priced in, so changing the currency later does not relabel old costs; the
traces list totals cost per currency. If the currency changes while a trace is still receiving
spans, the later spans keep their own currency and are left out of the trace's total, which says so.
Model calls without a price show a dash and are counted as **unpriced**. Prices apply to spans that
arrive after a change; stored costs are not recomputed.

## Tracing and personal data

Prompts and model outputs often contain personal data: names, e-mail addresses, order numbers,
whatever users type. Under the GDPR, storing them is processing that needs a purpose, a legal basis
and a retention period, and your application's privacy notice should mention it. vigily gives you
the controls; deciding how to use them is yours.

### Payload modes

What the server stores of a span's **input, output, error message and string attributes** is set
globally or per project in `vigily.toml`:

| Mode | Inputs and outputs | Error message | Attributes |
|---|---|---|---|
| `full` | stored as sent | as sent | as sent |
| `truncated` (**default**) | each field cut to `max_field_bytes` (**4 KiB**) | cut to 4 KiB | values cut to 4 KiB |
| `metadata` | not stored | its type only (`TimeoutError`), else `error` | numbers and booleans with identifier keys |

In `truncated` mode a span's inputs, outputs, error and attributes together are also capped at
64 KiB; fields beyond that are left out and listed as truncated. In `metadata` mode an error keeps
its type only when its text starts with an identifier and a colon (`KeyError: ...`); any other
message, such as `no customer for user@example.com`, is stored as `error`.

`metadata` mode recognises the *shape* of an error type or attribute key, not its meaning: an
error that starts with an identifier and a colon keeps that identifier, so `JaneDoe: account
locked` is stored as `JaneDoe`, and an attribute key such as `customer_JaneDoe` is kept with its
number. If your code puts identities in exception messages or attribute names, remove them before
they reach vigily, with the redaction hook below or in your own error handling.

```toml
[tracing]
payloads = "truncated"                 # the default
max_field_bytes = 4096

[tracing.projects.support-bot]         # a project: the trace's `project`, or its run's experiment
payloads = "metadata"
```

Names, kinds, times, status, model, provider, token counts and cost are stored in every mode. The
default is `truncated` because it keeps enough of each prompt to debug with while bounding how much
is kept; it does **not** remove personal data from the first 4 KiB. Use `metadata` for projects whose
prompts you must not keep, and the redaction hook below to remove what you can recognise. Spans of
a trace whose project is not known yet (no project and no run named so far) get the strictest mode
and field limit of the global and project policies, so nothing is kept that a project's policy
would not allow. The mode is applied when a span arrives and is recorded with it, and the trace page says which mode applied
(for example "Inputs and outputs are not stored for this project"). Changing the mode does not
rewrite traces already stored.

### Redaction in your process

`vigily.tracing.set_redactor(fn)` calls `fn` on every finished span before it leaves your process.
Return the span, changed as needed, or `None` to drop it:

```python
import re
import vigily

EMAIL = re.compile(r"[\w.+-]+@[\w-]+\.[\w.]+")


def redact(span: dict) -> dict | None:
    if span["name"] == "lookup_customer":
        return None  # never send this span
    for field in ("input", "output"):
        if isinstance(span.get(field), str):
            span[field] = EMAIL.sub("[email]", span[field])
    return span


vigily.tracing.set_redactor(redact)
```

If the hook raises, the span is sent without its input and output, with only number and boolean
attributes, and with its error reduced to the exception type (or `error`), so a bug in the hook
never leaks a payload. Pattern-based redaction misses things; treat
it as a second line after choosing what to trace and which payload mode to use.

### Deleting and erasing

- Deleting a run (in the runs table, or `Repo.delete_runs`) deletes the traces linked to it, and
  `vigily runs purge` erases them from storage with the run's other data. See
  [deleting is not erasing](storage.md#deleting-is-not-erasing-purge) for what a purge does not
  reach (backups, object-store versioning, disk blocks).
- A trace not linked to a run is deleted with `vigily traces delete <id>...` or
  `DELETE /api/v1/traces/<id>`, which hides it. `vigily runs purge` (or `vigily traces delete <id>
  --erase`) then erases it: it compacts storage, which drops the deleted rows, and removes the
  older versions that still hold them. Erasing is a command-line step on purpose, as for runs.
- Both commands run a **full optimize** of the repository before purging. On a large repository
  that takes as long as `vigily optimize` (seconds on the demo, minutes on millions of points) and
  needs its memory. A running `vigily up` may start its own background optimize (every 15 minutes)
  at the same time; kv0's compactions do not conflict with each other or with writers, but two at
  once take longer and use more memory, so prefer a quiet moment or `vigily up --no-optimize`.
- When a later batch names a trace's run, the rows stored before are moved under it, so deleting
  and purging the run reaches every span. A trace keeps the first run it was linked to.

### Where traces travel

Spans go from your process to the server over HTTP; `vigily up` listens on `127.0.0.1` by default.
If you serve vigily to a team, put it behind your own proxy with TLS and sign-in
([serving to a team](storage.md#serving-to-a-team)): the ingest endpoint accepts spans from anyone
who can reach it. vigily never sends trace data anywhere else.

**Web pages.** A page open in your browser can send requests to `127.0.0.1`. The ingest endpoints
therefore accept only `Content-Type: application/json` (which a page cannot send to another origin
without the browser asking the server first, and vigily does not answer that) and refuse any
request whose `Origin` header names another site, so a page cannot plant spans in your traces. The
SDK and OpenTelemetry exporters send JSON without an `Origin` and are not affected.

## Storage

Each trace is three kinds of rows in kv0 records tables next to the runs: a small summary in the
`trace` table (name, times, status, counts, tokens, cost, models, its run and eval item), which the
traces list reads; its spans in the `tspan` table, one row per batch that arrived, which the trace
page reads by key; and the ids of its spans in the `tsid` table, so a span sent again is ignored.
All carry the trace's run id, so kv0's run index finds a run's traces and deleting the run deletes
them.
