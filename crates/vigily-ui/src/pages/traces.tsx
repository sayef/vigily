import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { Search } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { DOCS_URL } from "@/components/layout";
import { PageHeader, SectionHead, ToolSwitch } from "@/components/page";
import { ErrorState } from "@/components/states";
import { Cost, PAYLOADS, TRACE_SNIPPET, Tokens, TraceStatus, kib } from "@/components/traces";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip } from "@/components/ui/tooltip";
import { type TraceRow, type TracesResponse, traceApi } from "@/lib/api";
import { ago, compact, integer, latency, money, moneyFull, utc } from "@/lib/format";
import { useDebounced, useTitle } from "@/lib/hooks";
import { cn, typing } from "@/lib/utils";

const PAGE = 100;
const ROW = 40;

/** The time ranges a list can be narrowed to, as a span back from now in microseconds. */
const RANGES = [
  ["1h", "1 h", 3_600e6],
  ["24h", "24 h", 86_400e6],
  ["7d", "7 d", 7 * 86_400e6],
  ["all", "All", null],
] as const;
type Range = (typeof RANGES)[number][0];

const select = "h-9 max-w-full border border-input bg-card px-2.5 text-sm pointer-coarse:h-11 max-sm:w-auto max-sm:min-w-0 max-sm:flex-1 max-sm:basis-[calc(50%-0.3125rem)]";

export function TracesPage() {
  useTitle("Traces");
  const [sp, setSp] = useSearchParams();
  const navigate = useNavigate();
  const run = sp.get("run");
  const model = sp.get("model");
  const status = sp.get("status") === "ok" || sp.get("status") === "error" ? (sp.get("status") as "ok" | "error") : null;
  const range: Range = RANGES.some(([k]) => k === sp.get("range")) ? (sp.get("range") as Range) : "all";
  const q = sp.get("q") ?? "";
  const page = Math.max(0, Number(sp.get("page")) || 0);
  const update = useCallback(
    (f: (p: URLSearchParams) => void, keepPage = false) =>
      setSp(
        (prev) => {
          const p = new URLSearchParams(prev);
          // Any change of filter starts again from the first page.
          if (!keepPage) p.delete("page");
          f(p);
          return p;
        },
        { replace: true },
      ),
    [setSp],
  );
  const set = useCallback((key: string, v: string | null) => update((p) => (v ? p.set(key, v) : p.delete(key))), [update]);

  // The search box writes to the URL a moment after typing stops.
  const [text, setText] = useState(q);
  const typed = useDebounced(text.trim(), 250);
  useEffect(() => {
    if (typed !== q) set("q", typed || null);
    // Only the debounced text drives this; the URL wins when it changes from elsewhere.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [typed]);
  useEffect(() => setText(q), [q]);

  const traces = useQuery({
    queryKey: ["traces", run, model, status, range, q, page],
    queryFn: ({ signal }) => {
      const back = RANGES.find(([k]) => k === range)?.[2] ?? null;
      return traceApi.list({ run, model, status, q, since: back === null ? null : Date.now() * 1000 - back, limit: PAGE, offset: page * PAGE }, signal);
    },
    placeholderData: keepPreviousData,
    refetchInterval: 30_000,
  });
  const d = traces.data;
  const rows = useMemo(() => d?.rows ?? [], [d]);
  const filtered = !!(run || model || status || q || range !== "all");

  // Keyboard: / search, j and k move, Enter opens.
  const search = useRef<HTMLInputElement>(null);
  const [active, setActive] = useState<number | null>(null);
  const body = useRef<HTMLTableSectionElement>(null);
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if (typing(e) || e.metaKey || e.ctrlKey || e.altKey || document.querySelector("[role=dialog]")) return;
      const move = (n: number) => {
        e.preventDefault();
        const next = Math.max(0, Math.min(rows.length - 1, (active ?? -1) + n));
        setActive(next);
        body.current?.children[next]?.scrollIntoView({ block: "nearest" });
      };
      if (e.key === "/") {
        e.preventDefault();
        search.current?.focus();
      } else if (e.key === "j" || e.key === "ArrowDown") move(1);
      else if (e.key === "k" || e.key === "ArrowUp") move(-1);
      else if ((e.key === "Enter" || e.key === "o") && active !== null && rows[active]) {
        e.preventDefault();
        navigate(`/traces/${rows[active].id}`);
      } else if (e.key === "Escape") setActive(null);
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, [rows, active, navigate]);

  if (traces.isError && !d) return <ErrorState error={traces.error as Error} onRetry={() => traces.refetch()} />;
  if (d && d.total === 0 && !filtered) return <EmptyTraces />;

  const runs = d?.runs ?? [];
  const runOptions = run && !runs.some((r) => r.id === run) ? [...runs, { id: run, name: run }] : runs;
  const models = d?.models ?? [];
  const modelOptions = model && !models.includes(model) ? [...models, model] : models;
  const first = page * PAGE;

  return (
    <div className="px-4 pt-5 md:px-6">
      <PageHeader title="Traces">
        {d ? (
          <>
            <span>
              <b>{integer(d.totals.traces)}</b> {filtered ? "match" : d.totals.traces === 1 ? "trace" : "traces"}
            </span>
            <span>Newest first</span>
            <span title={d.payloads === "metadata" ? "Spans keep timing, usage and attributes; inputs and outputs are dropped" : undefined}>
              {d.payloads === "truncated" ? `Inputs and outputs truncated to ${kib(d.max_field_bytes)} per field` : PAYLOADS[d.payloads]}
            </span>
          </>
        ) : (
          " "
        )}
      </PageHeader>

      <SectionHead
        title={filtered ? "Filtered traces" : "All traces"}
        tools={<ToolSwitch label="Time range" options={RANGES.map(([k, l]) => [k, l] as [Range, string])} value={range} onChange={(v) => set("range", v === "all" ? null : v)} />}
      />
      <div className="flex flex-wrap items-center gap-2.5 border-b py-2.5" role="search" aria-label="Filter traces">
        <label className="relative min-w-[14rem] flex-1 basis-64 max-sm:basis-full">
          <span className="sr-only">Find a trace by name or id</span>
          <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-faint" aria-hidden />
          <Input
            ref={search}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && (setText(""), e.currentTarget.blur())}
            placeholder="Find a name or trace id…"
            className="h-9 pl-8"
          />
        </label>
        <select aria-label="Run" className={cn(select, "w-44")} value={run ?? ""} onChange={(e) => set("run", e.target.value || null)}>
          <option value="">All runs</option>
          {runOptions.map((r) => (
            <option key={r.id} value={r.id}>
              {r.name}
            </option>
          ))}
        </select>
        <select aria-label="Model" className={cn(select, "w-44")} value={model ?? ""} onChange={(e) => set("model", e.target.value || null)}>
          <option value="">All models</option>
          {modelOptions.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
        <select aria-label="Status" className={cn(select, "w-32")} value={status ?? ""} onChange={(e) => set("status", e.target.value || null)}>
          <option value="">Any status</option>
          <option value="ok">OK</option>
          <option value="error">Error</option>
        </select>
        {filtered && (
          <Button variant="ghost" onClick={() => (setText(""), setSp(new URLSearchParams(), { replace: true }))}>
            Clear filters
          </Button>
        )}
      </div>

      <Totals data={d} />

      <div className={cn("overflow-x-auto overscroll-x-contain", traces.isFetching && traces.isPlaceholderData && "opacity-80")} role="region" aria-label="Traces table" tabIndex={-1}>
        {!d ? (
          <TableSkeleton />
        ) : rows.length === 0 ? (
          <div className="mx-auto max-w-lg px-4 py-12 text-center">
            <p className="text-base">No traces match these filters.</p>
            <Button className="mt-3" onClick={() => (setText(""), setSp(new URLSearchParams(), { replace: true }))}>
              Clear the filters
            </Button>
          </div>
        ) : (
          <table className="w-full min-w-[1080px] border-separate border-spacing-0 text-sm">
            <thead>
              <tr className="text-left">
                <th scope="col" className="h-10 border-b px-2.5 pl-0">
                  Name
                </th>
                <th scope="col" className="h-10 border-b px-2.5">
                  Status
                </th>
                <th scope="col" className="h-10 border-b px-2.5">
                  Started
                </th>
                <th scope="col" className="h-10 border-b px-2.5 text-right">
                  Latency
                </th>
                <th scope="col" className="h-10 border-b px-2.5 text-right">
                  Spans
                </th>
                <th scope="col" className="h-10 border-b px-2.5">
                  Models
                </th>
                <th scope="col" className="h-10 border-b px-2.5 text-right">
                  <Tooltip content="Input / output tokens, and the input tokens served from a prompt cache">
                    <span>Tokens in / out</span>
                  </Tooltip>
                </th>
                <th scope="col" className="h-10 border-b px-2.5 text-right">
                  Cost
                </th>
                <th scope="col" className="h-10 border-b px-2.5 pr-0">
                  Run
                </th>
              </tr>
            </thead>
            <tbody ref={body}>
              {rows.map((r, i) => (
                <Row key={r.id} row={r} active={active === i} onOpen={() => navigate(`/traces/${r.id}`)} />
              ))}
            </tbody>
          </table>
        )}
      </div>
      <div className="flex flex-wrap items-center justify-between gap-2 border-b-[1.5px] border-rule py-3 text-[12.5px] text-faint nums">
        <span>{d ? (d.total > rows.length ? `Showing ${integer(first + 1)}–${integer(first + rows.length)} of ${integer(d.total)} traces` : `Showing ${integer(rows.length)} ${rows.length === 1 ? "trace" : "traces"}`) : " "}</span>
        {d && d.total > PAGE && (
          <span className="flex gap-2">
            <Button size="sm" variant="outline" disabled={page === 0} onClick={() => update((p) => (page > 1 ? p.set("page", String(page - 1)) : p.delete("page")), true)}>
              Newer
            </Button>
            <Button size="sm" variant="outline" disabled={first + rows.length >= d.total} onClick={() => update((p) => p.set("page", String(page + 1)), true)}>
              Older
            </Button>
          </span>
        )}
        <span className="max-md:hidden">
          <Kbd>/</Kbd> find, <Kbd>j</Kbd> <Kbd>k</Kbd> move, <Kbd>Enter</Kbd> open
        </span>
      </div>
      <div className="h-12" />
    </div>
  );
}

/** One line of totals for every trace the filters match, not only the page shown. */
function Totals({ data }: { data?: TracesResponse }) {
  const t = data?.totals;
  const costs = t ? Object.entries(t.cost) : [];
  return (
    <dl className="flex min-h-11 flex-wrap items-center gap-x-6 gap-y-1 border-b py-2.5 text-sm nums" aria-label="Totals for the matching traces">
      {!t ? (
        <Skeleton className="h-3 w-96 max-w-full" />
      ) : (
        <>
          <Fact label="Traces">{integer(t.traces)}</Fact>
          <Fact label="Errors">
            <span className={cn(t.errors > 0 && "text-danger")}>{integer(t.errors)}</span>
            {t.traces > 0 && <span className="ml-1 font-normal text-faint">{Math.round((t.errors / t.traces) * 100)} %</span>}
          </Fact>
          <Fact label="Latency p50">{latency(t.p50_us)}</Fact>
          <Fact label="p95">{latency(t.p95_us)}</Fact>
          <Fact label="Tokens">
            <span title={`${integer(t.input_tokens)} input, ${integer(t.output_tokens)} output, ${integer(t.cached_tokens)} cached`}>
              {compact(t.input_tokens)} in <span className="font-normal text-faint">/</span> {compact(t.output_tokens)} out
              {t.cached_tokens > 0 && <span className="ml-1 font-normal text-faint">{compact(t.cached_tokens)} cached</span>}
            </span>
          </Fact>
          <Fact label="Cost">
            {costs.length === 0 ? (
              <span className="text-faint" title="No price for these models">
                —
              </span>
            ) : (
              costs.map(([c, v], i) => (
                <span key={c} title={moneyFull(v, c)}>
                  {i > 0 && ", "}
                  {money(v, c)}
                </span>
              ))
            )}
          </Fact>
          {t.unpriced > 0 && (
            <Fact label="Unpriced">
              <span title="Model calls without a configured price; they are not in the cost">{integer(t.unpriced)} calls</span>
            </Fact>
          )}
        </>
      )}
    </dl>
  );
}

function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-baseline gap-1.5">
      <dt className="text-faint">{label}</dt>
      <dd className="font-semibold">{children}</dd>
    </div>
  );
}

function Row({ row: r, active, onOpen }: { row: TraceRow; active: boolean; onOpen: () => void }) {
  const cell = "border-b px-2.5";
  return (
    <tr
      className={cn("group cursor-pointer", active ? "bg-hover" : "hover:bg-hover")}
      data-active={active || undefined}
      onClick={(e) => {
        // Links inside the row (the run) keep their own target.
        if ((e.target as HTMLElement).closest("a")) return;
        onOpen();
      }}
    >
      <td className={cn(cell, "max-w-64 pl-0")} style={{ height: ROW }}>
        <Link
          to={`/traces/${r.id}`}
          className={cn("block truncate font-mono text-[13px] leading-6 hover:underline", active && "underline decoration-rule decoration-2 underline-offset-4", active && "shadow-[inset_3px_0_0_var(--rule)] pl-2")}
          title={`${r.name} (${r.id})`}
        >
          {r.name}
        </Link>
      </td>
      <td className={cell}>
        <TraceStatus status={r.status} />
        {r.errors > 0 && r.status === "error" && r.errors > 1 && <span className="ml-1 text-xs text-faint">{r.errors} spans</span>}
      </td>
      <td className={cn(cell, "whitespace-nowrap text-muted-foreground")}>
        <span title={utc(r.start_time)}>{ago(r.start_time)}</span>
      </td>
      <td className={cn(cell, "whitespace-nowrap text-right nums")}>{latency(r.duration_us)}</td>
      <td className={cn(cell, "text-right nums")} title={`${r.spans} spans, ${r.llm_calls} model ${r.llm_calls === 1 ? "call" : "calls"}`}>
        {integer(r.spans)}
      </td>
      <td className={cn(cell, "max-w-56")}>
        <span className="block truncate font-mono text-[12.5px] text-muted-foreground" title={r.models.join(", ")}>
          {r.models.length ? r.models.join(", ") : "–"}
        </span>
      </td>
      <td className={cn(cell, "text-right")}>
        <Tokens input={r.input_tokens} output={r.output_tokens} cached={r.cached_tokens} />
      </td>
      <td className={cn(cell, "text-right nums")}>{r.llm_calls === 0 ? <span className="text-faint">–</span> : <Cost value={r.cost} currency={r.currency} unpriced={r.unpriced} />}</td>
      <td className={cn(cell, "max-w-48 pr-0")}>
        {r.run_id ? (
          <Link to={`/runs/${r.run_id}`} className="inline-flex min-h-6 max-w-full items-center truncate font-mono text-[12.5px] hover:underline" title={r.run_name ?? r.run_id}>
            <span className="truncate">{r.run_name ?? r.run_id.slice(-8)}</span>
          </Link>
        ) : (
          <span className="text-faint">–</span>
        )}
      </td>
    </tr>
  );
}

function TableSkeleton() {
  return (
    <div aria-busy aria-label="Loading traces">
      {Array.from({ length: 10 }, (_, i) => (
        <div key={i} className="flex items-center gap-6 border-b" style={{ height: ROW }}>
          <Skeleton className="h-3 w-40" />
          <Skeleton className="h-3 w-10" />
          <Skeleton className="h-3 w-20" />
          <Skeleton className="h-3 w-14" />
          <Skeleton className="h-3 w-32" />
          <Skeleton className="ml-auto h-3 w-24" />
        </div>
      ))}
    </div>
  );
}

/** No traces in the repository yet: how to send the first one. */
function EmptyTraces() {
  return (
    <div className="px-4 pt-5 md:px-6">
      <PageHeader title="Traces" />
      <div className="max-w-2xl py-8">
        <h2 className="text-[17px] font-bold tracking-[-0.01em]">No traces yet</h2>
        <p className="mt-2 text-base text-muted-foreground">
          A trace records one request through an LLM application: its retrieval, tool and model calls, how long each took, the tokens they used and what they cost. Wrap the work in{" "}
          <code className="bg-muted px-1 text-foreground">vigily.trace(...)</code> and traces appear here while the server runs.
        </p>
        <pre className="mt-4 overflow-x-auto bg-muted p-4 font-mono text-sm leading-relaxed">
          <code>{TRACE_SNIPPET}</code>
        </pre>
        <p className="mt-4 text-sm text-muted-foreground">
          The{" "}
          <a href={`${DOCS_URL}/guides/tracing.md`} target="_blank" rel="noreferrer" className="font-semibold text-foreground underline underline-offset-4">
            LLM tracing
          </a>{" "}
          guide covers nesting, token usage, prices, payload policies and OpenTelemetry.
        </p>
      </div>
    </div>
  );
}
