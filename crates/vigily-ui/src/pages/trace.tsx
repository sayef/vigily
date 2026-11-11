import { useQuery } from "@tanstack/react-query";
import { Copy } from "lucide-react";
import { useEffect, useMemo, useRef } from "react";
import { Link, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";
import { JsonBlock } from "@/components/evals";
import { Caption, Insight, SectionHead } from "@/components/page";
import { ErrorState } from "@/components/states";
import { Cost, PAYLOADS, TraceStatus, kib } from "@/components/traces";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip } from "@/components/ui/tooltip";
import { ApiError, type Json, type Span, type TraceRow, traceApi } from "@/lib/api";
import { useCrumbs } from "@/lib/crumbs";
import { DOCS_URL } from "@/components/layout";
import { ago, integer, latency, utc, value } from "@/lib/format";
import { useTitle } from "@/lib/hooks";
import { cn, typing } from "@/lib/utils";

/** A span placed in the tree: its depth under the root and whether it has children. */
interface Placed {
  span: Span;
  depth: number;
  children: number;
}

/** Orders spans depth first, each parent before its children, siblings by start time. A span
 * whose parent was not recorded, or whose parents loop back on themselves, starts its own branch. */
export function placeSpans(spans: Span[]): Placed[] {
  const ids = new Set(spans.map((s) => s.span_id));
  const kids = new Map<string | null, Span[]>();
  for (const s of spans) {
    const p = s.parent_id && s.parent_id !== s.span_id && ids.has(s.parent_id) ? s.parent_id : null;
    const list = kids.get(p);
    if (list) list.push(s);
    else kids.set(p, [s]);
  }
  for (const list of kids.values()) list.sort((a, b) => a.start_time - b.start_time);
  const out: Placed[] = [];
  const seen = new Set<string>();
  // Iterative, so a very deep trace cannot overflow the stack.
  const walk = (root: Span) => {
    const stack: [Span, number][] = [[root, 0]];
    while (stack.length) {
      const [s, depth] = stack.pop()!;
      if (seen.has(s.span_id)) continue;
      seen.add(s.span_id);
      const children = kids.get(s.span_id) ?? [];
      out.push({ span: s, depth, children: children.length });
      for (let i = children.length - 1; i >= 0; i--) stack.push([children[i], depth + 1]);
    }
  };
  for (const s of kids.get(null) ?? []) walk(s);
  // Spans in a parent cycle are reachable from no root: each starts its own branch.
  for (const s of [...spans].sort((a, b) => a.start_time - b.start_time)) if (!seen.has(s.span_id)) walk(s);
  return out;
}

/** The most span rows drawn; a larger trace shows the first ones and says so. */
const MAX_ROWS = 2000;

/** Tick positions for a time axis of `total` microseconds: about five round steps. */
function ticks(total: number): number[] {
  if (total <= 0) return [0];
  const raw = total / 5;
  const pow = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * pow).find((s) => s >= raw) ?? raw;
  const out: number[] = [];
  for (let t = 0; t <= total + 1e-9; t += step) out.push(t);
  return out;
}

/** An axis label: "0", "250 ms", "1.5 s". */
function tickLabel(us: number): string {
  if (us === 0) return "0";
  const ms = us / 1000;
  if (ms < 1) return `${+ms.toFixed(2)} ms`;
  if (ms < 1000) return `${+ms.toFixed(1)} ms`;
  return `${+(ms / 1000).toFixed(2)} s`;
}

export function TracePage() {
  const { id = "" } = useParams();
  const [sp, setSp] = useSearchParams();
  const trace = useQuery({
    queryKey: ["trace", id],
    queryFn: ({ signal }) => traceApi.get(id, signal),
    retry: (n, e) => !(e instanceof ApiError && e.status === 404) && n < 2,
  });
  const t = trace.data?.trace;
  useTitle(t ? `${t.name} trace` : "Trace");
  useCrumbs([{ label: "Traces", to: "/traces" }, { label: t?.name ?? "Trace", mono: true }]);

  const all = useMemo(() => placeSpans(trace.data?.spans ?? []), [trace.data]);
  const placed = useMemo(() => all.slice(0, MAX_ROWS), [all]);
  const selectedId = sp.get("span");
  const index = Math.max(
    0,
    placed.findIndex((p) => p.span.span_id === selectedId),
  );
  const select = (i: number) => {
    const p = placed[Math.max(0, Math.min(placed.length - 1, i))];
    if (p) setSp((prev) => (prev.set("span", p.span.span_id), prev), { replace: true });
  };

  // j and k (or the arrow keys) move through the spans.
  const list = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if (typing(e) || e.metaKey || e.ctrlKey || e.altKey || document.querySelector("[role=dialog]") || !placed.length) return;
      let next: number | null = null;
      if (e.key === "j" || e.key === "ArrowDown") next = index + 1;
      else if (e.key === "k" || e.key === "ArrowUp") next = index - 1;
      else if (e.key === "Home") next = 0;
      else if (e.key === "End") next = placed.length - 1;
      if (next === null) return;
      e.preventDefault();
      const n = Math.max(0, Math.min(placed.length - 1, next));
      setSp((prev) => (prev.set("span", placed[n].span.span_id), prev), { replace: true });
      list.current?.querySelector(`[data-index="${n}"]`)?.scrollIntoView({ block: "nearest" });
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, [placed, index, setSp]);

  if (trace.isPending) return <TraceSkeleton />;
  if (trace.isError) {
    const e = trace.error as Error;
    if (e instanceof ApiError && e.status === 404)
      return (
        <div className="mx-auto max-w-xl px-4 py-12">
          <h1 className="text-xl font-bold tracking-[-0.02em]">No trace with this id</h1>
          <p className="mt-2 text-muted-foreground">
            <code className="break-all">{id}</code> is not in this repository. It may have been deleted with its run, or sent to another server.
          </p>
          <Button asChild className="mt-4">
            <Link to="/traces">Back to traces</Link>
          </Button>
        </div>
      );
    return <ErrorState error={e} onRetry={() => trace.refetch()} />;
  }
  const tr = trace.data.trace;
  const current = placed[index]?.span ?? null;
  const failed = placed.filter((p) => p.span.status === "error");
  // The deepest failed span is where the error started; its ancestors only passed it on.
  const cause = failed.length ? failed.reduce((a, b) => (b.depth > a.depth ? b : a)) : null;

  return (
    <div className="px-4 pb-16 pt-4 md:px-6">
      <Header trace={tr} />
      {cause && (
        <Insight
          tag="Failed"
          tone="bad"
          action={
            cause.span.span_id !== current?.span_id ? (
              <button type="button" className="inline-flex min-h-6 items-center" onClick={() => select(placed.indexOf(cause))}>
                Show the span
              </button>
            ) : undefined
          }
        >
          <span className="font-mono text-[13px]">{cause.span.name}</span> failed{cause.span.error ? <>: {cause.span.error.replace(/[.\s]+$/, "")}.</> : "."}
          {failed.length > 1 && <span className="text-muted-foreground"> {failed.length - 1} enclosing {failed.length === 2 ? "span" : "spans"} failed with it.</span>}
        </Insight>
      )}
      <div className="grid gap-x-8 lg:grid-cols-[minmax(0,1fr)_minmax(340px,420px)]">
        <section aria-labelledby="spans-head" className="min-w-0">
          <SectionHead
            id="spans-head"
            title="Spans"
            tools={
              <span className="max-md:hidden">
                <Kbd>j</Kbd> <Kbd>k</Kbd> move
              </span>
            }
          />
          <Waterfall placed={placed} trace={tr} selected={index} onSelect={select} listRef={list} />
          {all.length > placed.length && (
            <p className="mt-2 text-sm text-muted-foreground nums">
              Showing the first {integer(placed.length)} of {integer(all.length)} spans.
            </p>
          )}
        </section>
        <section aria-labelledby="span-head" className="min-w-0">
          {current && <SpanPanel span={current} trace={tr} maxField={trace.data.max_field_bytes} />}
        </section>
      </div>
    </div>
  );
}

function Header({ trace: t }: { trace: TraceRow }) {
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(t.id);
      toast.success("Trace id copied");
    } catch {
      toast.error("Could not copy. Select the id and copy it by hand.");
    }
  };
  return (
    <header>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <h1 className="min-w-0 break-words font-mono text-xl font-bold tracking-[-0.02em]">{t.name}</h1>
        <TraceStatus status={t.status} />
      </div>
      <dl className="mt-2 flex flex-wrap gap-x-6 gap-y-1 border-b-[1.5px] border-rule pb-4 text-sm nums">
        <Fact label="Started">
          <Tooltip content={ago(t.start_time)}>
            <span>{utc(t.start_time)}</span>
          </Tooltip>
        </Fact>
        <Fact label="Duration">
          <b className="font-semibold">{latency(t.duration_us)}</b>
        </Fact>
        <Fact label="Spans">
          <span>
            {integer(t.spans)}
            {t.errors > 0 && <span className="font-bold text-danger">, {integer(t.errors)} failed</span>}
          </span>
        </Fact>
        <Fact label="Tokens">
          <span title={t.cached_tokens ? `${integer(t.cached_tokens)} of the input tokens came from a prompt cache` : undefined}>
            <b className="font-semibold">{integer(t.input_tokens)}</b> in, <b className="font-semibold">{integer(t.output_tokens)}</b> out
            {t.cached_tokens > 0 && <span className="text-muted-foreground">, {integer(t.cached_tokens)} cached</span>}
          </span>
        </Fact>
        <Fact label="Cost">
          {t.llm_calls === 0 ? <span className="text-faint">No model calls</span> : <Cost value={t.cost} currency={t.currency} unpriced={t.unpriced} className="font-semibold" />}
          {t.mixed_currency && <span className="text-muted-foreground"> (some calls priced in another currency are not included)</span>}
        </Fact>
        {t.run_id && (
          <Fact label="Run">
            <Link to={`/runs/${t.run_id}`} className="inline-flex min-h-6 items-center font-mono text-[12.5px] text-brand hover:underline">
              {t.run_name ?? t.run_id}
            </Link>
          </Fact>
        )}
        {t.eval_item && (
          <Fact label="Eval item">
            {t.run_id ? (
              <Link to={`/runs/${t.run_id}?tab=items&item=${encodeURIComponent(t.eval_item)}`} className="inline-flex min-h-6 items-center font-mono text-[12.5px] text-brand hover:underline">
                {t.eval_item}
              </Link>
            ) : (
              <span className="font-mono text-[12.5px]">{t.eval_item}</span>
            )}
          </Fact>
        )}
        {t.project && (
          <Fact label="Project">
            <span className="font-mono text-[12.5px]">{t.project}</span>
          </Fact>
        )}
        <Fact label="Payloads">
          <span>{PAYLOADS[t.payloads]}</span>
        </Fact>
        <Fact label="Id">
          <button type="button" onClick={copy} className="inline-flex min-h-6 items-center gap-1 font-mono text-[12.5px] hover:text-brand" aria-label={`Copy trace id ${t.id}`}>
            {t.id}
            <Copy className="size-3.5" aria-hidden />
          </button>
        </Fact>
      </dl>
    </header>
  );
}

function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-baseline gap-1.5">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="flex items-baseline">{children}</dd>
    </div>
  );
}

/** Spans on one time axis from the trace's start: indented by depth, bars aligned to time. */
function Waterfall({ placed, trace, selected, onSelect, listRef }: { placed: Placed[]; trace: TraceRow; selected: number; onSelect: (i: number) => void; listRef: React.Ref<HTMLDivElement> }) {
  // A loop, not a spread: a spread of thousands of arguments can exceed the call stack.
  let start = trace.start_time;
  let end = trace.end_time;
  for (const p of placed) {
    if (p.span.start_time < start) start = p.span.start_time;
    if (p.span.end_time > end) end = p.span.end_time;
  }
  const total = Math.max(1, end - start);
  const marks = ticks(total);
  const pct = (us: number) => `${(us / total) * 100}%`;
  const grid = "grid grid-cols-[minmax(7.5rem,38%)_minmax(0,1fr)_4.25rem] items-center sm:grid-cols-[minmax(10rem,36%)_minmax(0,1fr)_5rem]";
  const lines = (
    <div className="pointer-events-none absolute inset-0" aria-hidden>
      {marks.map((m) => (
        <span key={m} className="absolute inset-y-0 w-px bg-border" style={{ left: pct(m) }} />
      ))}
    </div>
  );
  return (
    <figure className="m-0">
      <div className={cn(grid, "h-8 border-b text-[11px] text-faint nums")} aria-hidden>
        <span className="text-[11px] font-bold uppercase tracking-[0.1em]">Span</span>
        <div className="relative mx-2 h-full">
          {marks.map((m, i) => (
            <span
              key={m}
              className={cn("absolute bottom-1.5 whitespace-nowrap font-mono", i % 2 === 1 && "max-sm:hidden", i === 0 ? "" : i === marks.length - 1 && (m / total) > 0.9 ? "-translate-x-full" : "-translate-x-1/2")}
              style={{ left: pct(m) }}
            >
              {tickLabel(m)}
            </span>
          ))}
        </div>
        <span className="text-right text-[11px] font-bold uppercase tracking-[0.1em]">Time</span>
      </div>
      <div ref={listRef} role="listbox" aria-label="Spans of this trace" aria-activedescendant={placed[selected] ? `span-${placed[selected].span.span_id}` : undefined}>
        {placed.map((p, i) => {
          const s = p.span;
          const on = i === selected;
          const bad = s.status === "error";
          const left = s.start_time - start;
          const width = Math.max(0, s.end_time - s.start_time);
          return (
            <div
              key={s.span_id}
              id={`span-${s.span_id}`}
              data-index={i}
              role="option"
              aria-selected={on}
              tabIndex={on ? 0 : -1}
              onClick={() => onSelect(i)}
              onKeyDown={(e) => e.key === "Enter" && onSelect(i)}
              className={cn(grid, "h-10 cursor-pointer border-b outline-offset-[-2px]", on ? "bg-muted shadow-[inset_3px_0_0_var(--rule)]" : "hover:bg-hover")}
            >
              <div className="flex min-w-0 items-baseline gap-2 pr-2" style={{ paddingLeft: 8 + Math.min(p.depth, 8) * 14 }}>
                <span className={cn("truncate font-mono text-[12.5px]", on && "font-bold", bad && "text-danger")} title={s.name}>
                  {s.name}
                </span>
                <span className="shrink-0 text-[11px] text-faint max-sm:hidden">{s.kind}</span>
              </div>
              <div className="relative mx-2 h-full">
                {lines}
                <span
                  className={cn("absolute top-1/2 h-2.5 -translate-y-1/2", bad ? "bg-danger" : on ? "bg-brand" : "bg-series", bad && on && "outline outline-2 outline-offset-1 outline-brand")}
                  style={{ left: pct(left), width: `max(2px, ${pct(width)})` }}
                  aria-hidden
                />
              </div>
              <span className={cn("text-right text-sm nums", on ? "text-foreground" : "text-muted-foreground")}>
                {latency(width)}
                <span className="sr-only">
                  , {s.kind}, starts {latency(left)} in{bad ? ", failed" : ""}
                </span>
              </span>
            </div>
          );
        })}
      </div>
      <Caption n={1} right={`${integer(placed.length)} ${placed.length === 1 ? "span" : "spans"}, ${integer(trace.llm_calls)} model ${trace.llm_calls === 1 ? "call" : "calls"}`}>
        Spans on one time axis from the start of the trace; failed spans in red.
      </Caption>
    </figure>
  );
}

const textBytes = (v: Json) => new TextEncoder().encode(typeof v === "string" ? v : JSON.stringify(v)).length;

/** A chat transcript: a list of `{role, content}` messages with text content. */
function messages(v: Json | null): { role: string; content: string }[] | null {
  if (!Array.isArray(v) || !v.length) return null;
  const out: { role: string; content: string }[] = [];
  for (const m of v) {
    if (!m || typeof m !== "object" || Array.isArray(m) || typeof m.role !== "string" || typeof m.content !== "string") return null;
    out.push({ role: m.role, content: m.content });
  }
  return out;
}

function Payload({ title, field, span, maxField }: { title: string; field: "input" | "output"; span: Span; maxField?: number }) {
  const v = span[field];
  const cut = span.truncated?.[field];
  const chat = messages(v);
  return (
    <section className="pt-4">
      <h3 className="mb-1.5 flex flex-wrap items-baseline gap-x-3 text-sm font-bold">
        {title}
        {cut !== undefined && v !== null && (
          <span className="text-[12.5px] font-normal text-warning">
            Truncated to {kib(maxField !== undefined ? Math.min(maxField, textBytes(v)) : textBytes(v))} of {kib(cut)}
          </span>
        )}
      </h3>
      {v === null ? (
        <p className="text-sm text-faint">None recorded</p>
      ) : chat ? (
        <ol className="flex max-h-96 flex-col overflow-y-auto border bg-card">
          {chat.map((m, i) => (
            <li key={i} className="border-b px-3 py-2 last:border-b-0">
              <span className="text-[11px] font-bold uppercase tracking-[0.1em] text-faint">{m.role}</span>
              <p className="mt-0.5 whitespace-pre-wrap break-words text-sm">{m.content}</p>
            </li>
          ))}
        </ol>
      ) : (
        <JsonBlock value={v} className="max-h-96 overflow-y-auto text-sm" />
      )}
    </section>
  );
}

function SpanPanel({ span: s, trace, maxField }: { span: Span; trace: TraceRow; maxField?: number }) {
  // Why the strictest policy applied: the trace names no project or run at all, or named one
  // only after this span arrived.
  const why =
    trace.project || trace.run_id
      ? "this span arrived before its trace named a project or run"
      : "this trace names no project or run";
  const offset = s.start_time - trace.start_time;
  const attrs = Object.entries(s.attributes ?? {});
  const u = s.usage;
  const hasUsage = !!u && (u.input_tokens != null || u.output_tokens != null);
  return (
    <div className="lg:sticky lg:top-0">
      <SectionHead
        id="span-head"
        title={
          <span className="flex min-w-0 flex-wrap items-baseline gap-x-2.5">
            <span className="break-all font-mono text-[15px]">{s.name}</span>
            <span className="text-[11px] font-bold uppercase tracking-[0.1em] text-faint">{s.kind}</span>
          </span>
        }
        tools={<TraceStatus status={s.status} />}
      />
      {s.status === "error" && (
        <p className="border-b py-2.5 text-sm text-danger" role="status">
          <span className="mr-2 inline-block size-[7px] bg-danger align-middle" aria-hidden />
          {s.error ?? "Failed without a message."}
        </p>
      )}
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 border-b py-2 text-sm nums [&>dd]:min-w-0 [&>dd]:py-1 [&>dt]:py-1 [&>dt]:text-muted-foreground">
        {s.model && (
          <>
            <dt>Model</dt>
            <dd className="break-all font-mono text-[12.5px]">
              {s.model}
              {s.provider && <span className="text-faint"> · {s.provider}</span>}
            </dd>
          </>
        )}
        <dt>Duration</dt>
        <dd className="font-semibold">{latency(s.end_time - s.start_time)}</dd>
        <dt>Starts</dt>
        <dd>
          <span title={utc(s.start_time)}>{offset <= 0 ? "At the start of the trace" : `${latency(offset)} into the trace`}</span>
        </dd>
        {hasUsage && (
          <>
            <dt>Tokens</dt>
            <dd>
              {integer(u?.input_tokens ?? 0)} in, {integer(u?.output_tokens ?? 0)} out
              {!!u?.cached_tokens && <span className="text-muted-foreground">, {integer(u.cached_tokens)} cached</span>}
            </dd>
          </>
        )}
        {(hasUsage || s.cost !== null) && (
          <>
            <dt>Cost</dt>
            <dd>
              <Cost value={s.cost} currency={s.currency ?? trace.currency} />
              {s.currency && s.currency !== trace.currency && (
                <span className="text-muted-foreground"> (not in the trace total, which is in {trace.currency})</span>
              )}
            </dd>
          </>
        )}
        <dt>Span id</dt>
        <dd className="break-all font-mono text-[12.5px] text-muted-foreground">{s.span_id}</dd>
      </dl>
      {s.payloads === "metadata" ? (
        <p className="border-b py-3 text-sm text-muted-foreground">
          {s.provisional ? (
            <>
              Inputs and outputs were not stored: {why}, so the strictest payload policy applied (metadata only).{" "}
              <a href={`${DOCS_URL}/guides/tracing.md#payload-modes`} target="_blank" rel="noreferrer" className="inline-flex min-h-6 items-center font-semibold text-brand hover:underline">
                About payload modes
              </a>
            </>
          ) : (
            "Inputs and outputs are not stored for this project (metadata only)."
          )}
        </p>
      ) : (
        <>
          {s.provisional && s.payloads === "truncated" && (
            <p className="pt-3 text-[12.5px] text-muted-foreground">
              Stored under the strictest payload policy: {why}.{" "}
              <a href={`${DOCS_URL}/guides/tracing.md#payload-modes`} target="_blank" rel="noreferrer" className="inline-flex min-h-6 items-center font-semibold text-brand hover:underline">
                About payload modes
              </a>
            </p>
          )}
        </>
      )}
      {s.payloads !== "metadata" && (
        <>
          <Payload title="Input" field="input" span={s} maxField={maxField} />
          <Payload title="Output" field="output" span={s} maxField={maxField} />
        </>
      )}
      <section className="pt-4">
        <h3 className="mb-1.5 text-sm font-bold">Attributes</h3>
        {attrs.length === 0 ? (
          <p className="text-sm text-faint">None recorded</p>
        ) : (
          <table className="w-full table-fixed text-sm">
            <tbody>
              {attrs.map(([k, v]) => (
                <tr key={k} className="align-top">
                  <th scope="row" className="w-2/5 break-all border-b py-1.5 pr-3 text-left font-mono text-[12.5px] font-normal text-muted-foreground">
                    {k}
                  </th>
                  <td className="break-all border-b py-1.5 font-mono text-[12.5px]">{value(v)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
              {!!s.dropped_attributes && (
          <p className="mt-1.5 text-[12.5px] text-muted-foreground nums">
            {integer(s.dropped_attributes)} more {s.dropped_attributes === 1 ? "attribute was" : "attributes were"} not stored (limits or payload mode).
          </p>
        )}
      </section>
    </div>
  );
}

function TraceSkeleton() {
  return (
    <div className="px-4 pt-4 md:px-6" aria-busy aria-label="Loading the trace">
      <Skeleton className="h-7 w-64" />
      <Skeleton className="mt-3 h-4 w-full max-w-3xl" />
      <div className="mt-8 flex flex-col gap-3">
        {Array.from({ length: 6 }, (_, i) => (
          <Skeleton key={i} className="h-5" />
        ))}
      </div>
    </div>
  );
}
