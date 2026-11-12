import { keepPreviousData, useQueries, useQuery } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ArrowDown, ArrowRight, ArrowUp, Download } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { ColumnPicker } from "@/components/column-picker";
import { DeleteRuns, useHiddenRuns } from "@/components/delete-runs";
import { Sparkline } from "@/components/line-chart";
import { Figure, Figures, PageHeader, SectionHead } from "@/components/page";
import { QueryInput, type QueryInputHandle } from "@/components/query-input";
import { EmptyRepository, ErrorState } from "@/components/states";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip } from "@/components/ui/tooltip";
import { ApiError, type Meta, type RunRow, api, targetText } from "@/lib/api";
import { type ColumnDef, columnDef, defaultColumns, mainMetrics } from "@/lib/columns";
import { useCrumbs } from "@/lib/crumbs";
import { ago, duration, higherIsBetter, integer, metricLabel, num } from "@/lib/format";
import { useMediaQuery, useTitle } from "@/lib/hooks";
import { useMeta } from "@/lib/queries";
import { LETTERS, letterOf, useSelection } from "@/lib/selection";
import { cn, typing } from "@/lib/utils";

const ROW = 42;
const CHECK = 44;
const SPARK = 168;
const DAY = 86_400e6;

/** The metric to headline: a validation score if there is one, else the most common metric. */
function headlineMetric(meta: Meta, rows: RunRow[]): string | null {
  const keys = mainMetrics(meta, 8, rows.filter((r) => r.kind === "training"));
  return keys.find((k) => /val|valid|eval|test/i.test(k) && higherIsBetter(metricLabel(k).name)) ?? keys.find((k) => /val|valid|eval|test/i.test(k)) ?? keys[0] ?? null;
}

/** The loss to draw in each row: training loss if logged, else any loss. */
function lossMetric(meta: Meta, rows: RunRow[]): string | null {
  const keys = mainMetrics(meta, 12, rows.filter((r) => r.kind === "training"));
  return keys.find((k) => /loss/i.test(k) && /train/i.test(k)) ?? keys.find((k) => /loss/i.test(k)) ?? null;
}

const clock = (micros: number) => new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZoneName: "short" }).format(new Date(micros / 1000));

function csv(rows: RunRow[], columns: ColumnDef[]) {
  const esc = (s: string) => (/[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
  const head = ["id", ...columns.map((c) => (c.detail ? `${c.label} (${c.detail})` : c.label))];
  const body = rows.map((r) => [r.id, ...columns.map((c) => (c.id === "run:status" ? r.status : c.cell(r).text))]);
  const blob = new Blob([[head, ...body].map((l) => l.map(esc).join(",")).join("\n")], { type: "text/csv" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = "vigily-runs.csv";
  a.click();
  URL.revokeObjectURL(a.href);
}

export function RunsPage() {
  useTitle("Runs");
  const [sp, setSp] = useSearchParams();
  const navigate = useNavigate();
  const meta = useMeta();
  const selection = useSelection();
  const narrow = useMediaQuery("(max-width: 639px)");
  const search = useRef<QueryInputHandle>(null);

  const q = sp.get("q") ?? "";
  const sort = sp.get("sort");
  const desc = sp.get("desc") === "1";
  const archived = sp.get("archived") === "1";
  const colParams = sp.getAll("col");
  const update = useCallback(
    (f: (p: URLSearchParams) => void) =>
      setSp(
        (prev) => {
          const p = new URLSearchParams(prev);
          f(p);
          return p;
        },
        { replace: true },
      ),
    [setSp],
  );
  const setQ = useCallback((v: string) => update((p) => (v ? p.set("q", v) : p.delete("q"))), [update]);

  const columns: ColumnDef[] = useMemo(() => {
    const ids = colParams.length ? colParams : meta.data ? defaultColumns(meta.data) : ["run:status", "run:name", "run:experiment", "run:created", "run:duration"];
    const withName = ids.includes("run:name") ? ids : ["run:name", ...ids];
    // The name always comes first; it stays in view when the table scrolls sideways.
    const ordered = ["run:name", ...withName.filter((c) => c !== "run:name")];
    return ordered.map(columnDef).filter((c): c is ColumnDef => c !== null);
  }, [colParams, meta.data]);

  const runs = useQuery({
    queryKey: ["runs", q, sort, desc, archived],
    queryFn: ({ signal }) => api.runs({ q, sort, desc, archived }, signal),
    placeholderData: keepPreviousData,
    refetchInterval: meta.data?.running ? 5_000 : 30_000,
    retry: (n, e) => !(e instanceof ApiError && e.status === 400) && n < 2,
  });
  // The figures describe every run, whatever the filter.
  const all = useQuery({
    queryKey: ["runs", "", null, false, false],
    queryFn: ({ signal }) => api.runs({ q: "" }, signal),
    refetchInterval: meta.data?.running ? 5_000 : 30_000,
  });
  const queryError = runs.error instanceof ApiError && runs.error.status === 400 ? runs.error : null;
  const hidden = useHiddenRuns();
  const rows = useMemo(() => (runs.data?.rows ?? []).filter((r) => !hidden.has(r.id)), [runs.data, hidden]);
  const allRows = useMemo(() => (all.data?.rows ?? []).filter((r) => !hidden.has(r.id)), [all.data, hidden]);

  const setSort = (c: ColumnDef) => {
    update((p) => {
      if (sort === c.id) {
        if (desc === c.descFirst) {
          p.set("desc", desc ? "0" : "1");
        } else {
          p.delete("sort");
          p.delete("desc");
        }
      } else {
        p.set("sort", c.id);
        p.set("desc", c.descFirst ? "1" : "0");
      }
    });
  };

  const setColumns = (ids: string[]) =>
    update((p) => {
      p.delete("col");
      for (const id of ids) p.append("col", id);
    });

  const loss = meta.data ? lossMetric(meta.data, allRows) : null;
  const headline = meta.data ? headlineMetric(meta.data, allRows) : null;

  // Keyboard: / search, j and k move, x selects, Enter opens, c compares, Esc clears.
  const [active, setActive] = useState<number | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const virt = useVirtualizer({ count: rows.length, getScrollElement: () => scroller.current, estimateSize: () => (narrow ? 64 : ROW), overscan: 12 });
  useEffect(() => {
    virt.measure();
  }, [narrow, virt]);
  const compare = useCallback(() => {
    if (selection.ids.length) navigate(`/metrics/compare?runs=${selection.ids.join(",")}`);
  }, [navigate, selection.ids]);
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if (typing(e) || e.metaKey || e.ctrlKey || e.altKey || document.querySelector("[role=dialog]")) return;
      const move = (d: number) => {
        e.preventDefault();
        const next = Math.max(0, Math.min(rows.length - 1, (active ?? -1) + d));
        setActive(next);
        virt.scrollToIndex(next, { align: "auto" });
      };
      if (e.key === "/") {
        e.preventDefault();
        search.current?.focus();
      } else if (e.key === "j" || e.key === "ArrowDown") move(1);
      else if (e.key === "k" || e.key === "ArrowUp") move(-1);
      else if (e.key === "x" && active !== null && rows[active]) {
        e.preventDefault();
        selection.toggle(rows[active].id);
      } else if ((e.key === "Enter" || e.key === "o") && active !== null && rows[active]) {
        e.preventDefault();
        navigate(`/runs/${rows[active].id}`);
      } else if (e.key === "c") compare();
      else if (e.key === "Escape") {
        if (selection.ids.length) selection.clear();
        setActive(null);
      }
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, [rows, active, selection, navigate, compare, virt]);

  // Loss curves for the training rows in view. Each run is drawn over its own steps, so a live run
  // a few thousand steps in is not squeezed into the first pixels by a long finished run; the
  // y scale is shared per experiment so the rows stay comparable.
  const sparkIds: string[] = [];
  for (const v of virt.getVirtualItems()) {
    const r = rows[v.index];
    if (r && r.kind === "training") sparkIds.push(r.id);
  }
  sparkIds.sort();
  const sparkQueries = useQueries({
    queries: sparkIds.map((id) => ({
      queryKey: ["spark", loss, id],
      queryFn: ({ signal }: { signal: AbortSignal }) => api.chart({ runs: [id], metric: loss!, x: "step", points: 48 }, signal),
      enabled: !!loss && !narrow,
      staleTime: 15_000,
    })),
  });
  const sparkStamp = sparkQueries.map((q) => `${q.dataUpdatedAt}:${q.status}`).join(",");
  const spark = useMemo(() => {
    const series = new Map<string, (number | null)[] | null>();
    sparkIds.forEach((id, i) => {
      const d = sparkQueries[i]?.data;
      if (d) series.set(id, d.series.find((s) => s.run_id === id)?.y ?? null);
    });
    const experimentOf = new Map(rows.map((r) => [r.id, r.experiment ?? ""] as const));
    const pooled = new Map<string, number[]>();
    for (const [id, y] of series) {
      if (!y) continue;
      const g = experimentOf.get(id) ?? "";
      const ys = pooled.get(g) ?? [];
      for (const v of y) if (v !== null && isFinite(v)) ys.push(v);
      pooled.set(g, ys);
    }
    const domains = new Map<string, [number, number]>();
    for (const [g, ys] of pooled) {
      if (!ys.length) continue;
      const sorted = ys.sort((a, b) => a - b);
      // Clip the top 2 % so one diverging run does not flatten the others.
      domains.set(g, [sorted[0], sorted[Math.floor(sorted.length * 0.98)] ?? sorted.at(-1)!]);
    }
    const by = new Map<string, SparkData>();
    for (const [id, y] of series) {
      const domain = domains.get(experimentOf.get(id) ?? "");
      const finite = y?.some((v) => v !== null && isFinite(v));
      by.set(id, y && finite && domain ? { values: y, domain } : "none");
    }
    return by;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sparkStamp, sparkIds.join(","), rows]);

  // The best value in each best-of column gets a star.
  const best = useMemo(() => {
    const out = new Map<string, number>();
    for (const c of columns) {
      if (c.kind !== "metric") continue;
      const [, agg, ...k] = c.id.split(":");
      const key = k.join(":");
      const up = higherIsBetter(metricLabel(key).name);
      if (!((agg === "max" && up) || (agg === "min" && !up))) continue;
      const vals = rows.map((r) => r.metrics[key]?.[agg as "max" | "min"]).filter((v): v is number => v !== null && v !== undefined && isFinite(v));
      if (vals.length > 1) out.set(c.id, up ? Math.max(...vals) : Math.min(...vals));
    }
    return out;
  }, [columns, rows]);

  const exp = /^exp(?:eriment)?\s*[:=]\s*"?([^"\s]+)"?$/.exec(q.trim())?.[1];
  useCrumbs(exp ? [{ label: "Runs", to: "/" }, { label: exp, mono: true }] : [{ label: "Runs" }]);

  if (meta.data && meta.data.runs === 0) return <EmptyRepository repository={meta.data.repository} />;
  if (meta.isError && !meta.data) return <ErrorState error={meta.error as Error} onRetry={() => meta.refetch()} />;

  const experiments = meta.data?.experiments.filter((e) => e.name).length ?? 0;
  const total = runs.data?.total ?? meta.data?.runs ?? 0;
  const summary = !meta.data ? (
    " "
  ) : queryError ? (
    <>
      <b>{integer(total)}</b> runs; the filter has an error
    </>
  ) : q ? (
    <>
      <b>{integer(rows.length)}</b> of {integer(total)} runs match
    </>
  ) : (
    <>
      <b>{integer(total)}</b> runs
    </>
  );
  const n = selection.ids.length;
  const showSpark = !!loss && !narrow;
  const statusAt = columns.findIndex((c) => c.id === "run:status");
  const sparkAfter = statusAt >= 0 ? statusAt : 0;
  const tableWidth = CHECK + columns.reduce((s, c) => s + c.width, 0) + (showSpark ? SPARK : 0);

  // Figures.
  const now = Date.now() * 1000;
  // Every run in the running state, as the "Running" filter counts them; stale ones are named in the note.
  const running = allRows.filter((r) => r.status === "running");
  const stale = allRows.filter((r) => r.status === "running" && r.stale).length;
  const finished = (from: number, to: number) => allRows.filter((r) => r.status === "finished" && r.ended_at && r.ended_at >= from && r.ended_at < to).length;
  const week = finished(now - 7 * DAY, now + DAY);
  const lastWeek = finished(now - 14 * DAY, now - 7 * DAY);
  const failed = allRows.filter((r) => r.status === "failed").length;
  const aborted = allRows.filter((r) => r.status === "aborted").length;
  const headUp = headline ? higherIsBetter(metricLabel(headline).name) : true;
  const bestRun = headline
    ? allRows
        .filter((r) => r.metrics[headline]?.[headUp ? "max" : "min"] != null)
        .sort((a, b) => (headUp ? b.metrics[headline][headUp ? "max" : "min"]! - a.metrics[headline][headUp ? "max" : "min"]! : a.metrics[headline].min! - b.metrics[headline].min!))[0]
    : undefined;
  const headLabel = headline ? metricLabel(headline) : null;

  const quick: [string, string, number | undefined][] = [
    ["", "All", meta.data?.runs],
    ["status:running", "Running", meta.data?.running],
    ["status:failed", "Failed", allRows.length ? failed : undefined],
    ["status:finished", "Finished", allRows.length ? allRows.filter((r) => r.status === "finished").length : undefined],
  ];

  return (
    <div className="px-4 pt-5 md:px-6">
      <PageHeader
        title="Runs"
        actions={
          <>
            <ColumnPicker meta={meta.data} columns={columns.map((c) => c.id)} onChange={setColumns} onReset={() => setColumns([])} />
            <Button variant="outline" onClick={() => csv(rows, columns)} disabled={!rows.length} className="max-sm:hidden">
              <Download /> Export
            </Button>
            <Button variant="brand" onClick={compare} disabled={!n} className="max-sm:hidden">
              Compare{n ? ` ${n}` : ""}
            </Button>
          </>
        }
      >
        <span data-summary aria-live="polite">
          {summary}
        </span>
        {meta.data && (
          <span>
            <b>{experiments}</b> {experiments === 1 ? "experiment" : "experiments"}
          </span>
        )}
        {runs.data && (
          <span>
            Updated <b>{clock(runs.data.refreshed_at)}</b>
          </span>
        )}
      </PageHeader>

      <Figures label="Runs at a glance">
        <Figure label="Running now" value={allRows.length ? running.length : "–"} note={stale ? `${stale === running.length ? (stale === 1 ? "It has" : "All have") : `${stale} of them have`} no heartbeat (stale)` : running.length ? `Oldest started ${ago(Math.min(...running.map((r) => r.created_at)))}` : "Nothing training"} />
        <Figure
          label="Finished, 7 days"
          value={allRows.length ? week : "–"}
          note={
            allRows.length ? (
              <>
                <b className={week >= lastWeek ? "text-brand" : "text-foreground"}>
                  {week - lastWeek >= 0 ? "+" : "−"}
                  {Math.abs(week - lastWeek)}
                </b>{" "}
                on the week before
              </>
            ) : (
              " "
            )
          }
        />
        <Figure
          label="Failed"
          value={allRows.length ? <span className={failed ? "text-danger" : undefined}>{failed}</span> : "–"}
          unit={allRows.length ? `of ${integer(allRows.length)}` : undefined}
          note={[stale && `${stale} stale`, aborted && `${aborted} aborted`].filter(Boolean).join(", ") || "None stale or aborted"}
        />
        <Figure
          label={headLabel ? `Best ${headLabel.name}${headLabel.context ? ` ${headLabel.context}` : ""}` : "Best value"}
          value={bestRun && headline ? num(bestRun.metrics[headline][headUp ? "max" : "min"]) : "–"}
          note={bestRun ? <span className="font-mono text-[12.5px]">{bestRun.name}</span> : "No metric logged yet"}
        />
      </Figures>

      <SectionHead
        title={exp ?? (q ? "Filtered runs" : "All runs")}
        tools={
          <span role="radiogroup" aria-label="Show" className="flex flex-wrap gap-x-[18px]">
            {quick.map(([v, l, c]) => (
              <button
                key={l}
                type="button"
                role="radio"
                aria-checked={q === v}
                onClick={() => setQ(v)}
                className={cn("min-h-6 underline-offset-[5px] pointer-coarse:min-h-11", q === v ? "font-bold text-foreground underline decoration-2" : "hover:text-foreground")}
              >
                {l}
                {c !== undefined && <span className="ml-1 nums">{integer(c)}</span>}
              </button>
            ))}
          </span>
        }
      />
      <div className="flex flex-wrap items-start gap-2.5 border-b py-2.5">
        <QueryInput ref={search} value={q} onChange={setQ} meta={meta.data} error={queryError} className="min-w-[14rem] flex-1" />
        <label className="flex h-9 cursor-pointer items-center gap-2 border bg-card px-3 text-sm text-muted-foreground hover:text-foreground pointer-coarse:h-11">
          <input type="checkbox" checked={archived} onChange={(e) => update((p) => (e.target.checked ? p.set("archived", "1") : p.delete("archived")))} />
          Archived
        </label>
      </div>

      <div
        ref={scroller}
        className={cn("relative max-h-[calc(100dvh-10rem)] min-h-64 overflow-auto overscroll-contain", runs.isFetching && runs.isPlaceholderData && "opacity-80")}
        role="region"
        aria-label="Runs table"
        tabIndex={-1}
      >
        {runs.isPending ? (
          <TableSkeleton />
        ) : runs.isError && !queryError ? (
          <ErrorState error={runs.error as Error} onRetry={() => runs.refetch()} />
        ) : rows.length === 0 ? (
          <div className="mx-auto max-w-lg px-4 py-12 text-center">
            <p className="text-base">{queryError ? "Fix the filter to see runs." : q ? "No runs match this filter." : archived ? "No runs." : "Every run is archived."}</p>
            {q && (
              <Button className="mt-3" onClick={() => setQ("")}>
                Clear the filter
              </Button>
            )}
          </div>
        ) : narrow ? (
          <ul className="relative" style={{ height: virt.getTotalSize() }}>
            {virt.getVirtualItems().map((vi) => {
              const r = rows[vi.index];
              return (
                <li key={r.id} className="absolute inset-x-0" style={{ transform: `translateY(${vi.start}px)`, height: vi.size }}>
                  <MobileRow run={r} columns={columns} letter={letterOf(selection.ids, r.id)} selected={selection.has(r.id)} onToggle={() => selection.toggle(r.id)} active={active === vi.index} sort={sort} />
                </li>
              );
            })}
          </ul>
        ) : (
          <table className="border-separate border-spacing-0 text-sm" style={{ width: Math.max(tableWidth, 0), minWidth: "100%" }} aria-rowcount={rows.length + 1}>
            <colgroup>
              <col style={{ width: CHECK }} />
              {columns.map((c, i) => (
                <ColPair key={c.id} width={c.width} spark={showSpark && i === sparkAfter} />
              ))}
            </colgroup>
            <thead className="sticky top-0 z-20">
              <tr>
                <th className="sticky left-0 z-10 h-11 border-b bg-background px-3 text-left">
                  <SelectAll rows={rows} />
                </th>
                {columns.map((c, i) => {
                  const sorted = sort === c.id;
                  const Arrow = desc ? ArrowDown : ArrowUp;
                  const head = (
                    <th
                      key={c.id}
                      scope="col"
                      aria-sort={sorted ? (desc ? "descending" : "ascending") : undefined}
                      className={cn("h-11 border-b bg-background p-0", i === 0 && "sticky z-10", c.numeric ? "text-right" : "text-left")}
                      style={i === 0 ? { left: CHECK } : undefined}
                    >
                      <Tooltip content={c.kind === "param" ? `Parameter ${c.id.slice(6)}` : c.kind === "metric" ? `Metric ${c.id.split(":").slice(2).join(":")}, ${c.id.split(":")[1]} value` : undefined}>
                        <button type="button" onClick={() => setSort(c)} className={cn("flex h-11 w-full flex-col justify-center px-3 hover:text-foreground", c.numeric ? "items-end" : "items-start")}>
                          <span className={cn("flex max-w-full items-center gap-1", sorted && "text-foreground")}>
                            {c.numeric && sorted && <Arrow className="size-3.5 shrink-0" aria-hidden />}
                            <span className={cn("truncate", c.kind !== "run" && "font-mono normal-case tracking-normal text-[12px]")}>{c.label}</span>
                            {!c.numeric && sorted && <Arrow className="size-3.5 shrink-0" aria-hidden />}
                          </span>
                          {c.detail && <span className="max-w-full truncate text-[10.5px] font-normal normal-case tracking-normal">{c.detail}</span>}
                        </button>
                      </Tooltip>
                    </th>
                  );
                  if (!(showSpark && i === sparkAfter)) return head;
                  const l = metricLabel(loss!);
                  return [
                    head,
                    <th key="spark" scope="col" className="h-11 border-b bg-background px-3 text-left" title={`${loss}: every run over its own steps, on one scale per experiment`}>
                      {l.name}
                      {l.context && <span className="block text-[10.5px] font-normal normal-case tracking-normal">{l.context}, one scale</span>}
                    </th>,
                  ];
                })}
              </tr>
            </thead>
            <tbody>
              {virt.getVirtualItems().length > 0 && <tr aria-hidden style={{ height: virt.getVirtualItems()[0].start }} />}
              {virt.getVirtualItems().map((vi) => {
                const r = rows[vi.index];
                return (
                  <Row
                    key={r.id}
                    run={r}
                    columns={columns}
                    index={vi.index}
                    letter={letterOf(selection.ids, r.id)}
                    selected={selection.has(r.id)}
                    active={active === vi.index}
                    onToggle={() => selection.toggle(r.id)}
                    best={best}
                    spark={showSpark ? { after: sparkAfter, data: spark.get(r.id) ?? null, metric: loss! } : null}
                  />
                );
              })}
              {virt.getVirtualItems().length > 0 && <tr aria-hidden style={{ height: virt.getTotalSize() - (virt.getVirtualItems().at(-1)?.end ?? 0) }} />}
            </tbody>
          </table>
        )}
      </div>
      <div className="flex flex-wrap justify-between gap-2 border-b-[1.5px] border-rule py-3 text-[12.5px] text-faint nums">
        <span>
          Showing {integer(rows.length)} of {integer(total)} runs
        </span>
        <span className="max-md:hidden">
          <Kbd>j</Kbd> <Kbd>k</Kbd> move, <Kbd>x</Kbd> select, <Kbd>Enter</Kbd> open
        </span>
      </div>
      <div className={n ? "h-24" : "h-12"} />

      {n > 0 && (
        <div className="inverse sticky bottom-0 z-30 -mx-4 flex flex-wrap items-center gap-x-6 gap-y-2 px-4 py-3 pb-[max(0.75rem,env(safe-area-inset-bottom))] md:-mx-6 md:px-6" role="region" aria-label="Selected runs">
          <p className="text-base font-bold nums">
            {n} {n === 1 ? "run" : "runs"} selected
          </p>
          <span className="font-mono tracking-[0.2em] max-sm:hidden" aria-hidden>
            {selection.ids
              .slice(0, 8)
              .map((_, i) => LETTERS[i])
              .join(" ")}
            {n > 8 ? " …" : ""}
          </span>
          <span className="flex items-center gap-4 text-sm text-muted-foreground max-lg:hidden">
            <span>
              <Kbd>c</Kbd> compare
            </span>
            <span>
              <Kbd>x</Kbd> toggle
            </span>
            <span>
              <Kbd>Esc</Kbd> clear
            </span>
          </span>
          <span className="ml-auto flex items-center gap-1">
            <DeleteRuns ids={selection.ids} onDeleted={selection.clear} />
            <Button variant="ghost" size="sm" onClick={selection.clear} className="lg:hidden">
              Clear
            </Button>
            <button type="button" onClick={compare} className="ml-2 inline-flex h-10 items-center gap-2 bg-foreground px-4 text-base font-bold text-background hover:opacity-90 pointer-coarse:h-11">
              Compare {n} {n === 1 ? "run" : "runs"} <ArrowRight className="size-4" aria-hidden />
            </button>
          </span>
        </div>
      )}
    </div>
  );
}

function ColPair({ width, spark }: { width: number; spark: boolean }) {
  return (
    <>
      <col style={{ width }} />
      {spark && <col style={{ width: SPARK }} />}
    </>
  );
}

function SelectAll({ rows }: { rows: RunRow[] }) {
  const selection = useSelection();
  const allSelected = rows.length > 0 && rows.every((r) => selection.has(r.id));
  const toggleAll = () => {
    const ids = new Set(selection.ids);
    if (allSelected) rows.forEach((r) => ids.delete(r.id));
    else rows.forEach((r) => ids.add(r.id));
    selection.set([...ids]);
  };
  return <input type="checkbox" checked={allSelected} onChange={toggleAll} aria-label={allSelected ? "Unselect all shown runs" : "Select all shown runs"} />;
}

function Letter({ l }: { l: string }) {
  return (
    <span className="inline-flex size-[18px] shrink-0 items-center justify-center border-[1.5px] border-rule text-[11px] font-bold leading-none" aria-label={`run ${l}`}>
      {l}
    </span>
  );
}

function Cell({ c, run, letter, best }: { c: ColumnDef; run: RunRow; letter: string | null; best?: number }) {
  if (c.id === "run:status") return <StatusBadge status={run.status} stale={run.stale} lastSeen={run.last_seen} className="flex whitespace-nowrap" />;
  if (c.id === "run:name")
    return (
      <span className="flex min-w-0 items-center gap-2.5">
        {letter && <Letter l={letter} />}
        <Link to={`/runs/${run.id}`} className={cn("truncate py-1 font-mono text-[13px] hover:underline", !run.has_name && "text-muted-foreground")} title={run.name}>
          {run.name}
        </Link>
        {run.kind !== "training" && (
          <span className="shrink-0 border px-1 text-[11px] text-muted-foreground" title={run.target ? `Target: ${targetText(run.target)}` : undefined}>
            {run.kind}
          </span>
        )}
        {run.archived && <span className="shrink-0 border px-1 text-[11px] text-muted-foreground">archived</span>}
      </span>
    );
  const v = c.cell(run);
  if (c.id === "run:experiment" && run.experiment)
    return (
      <Link to={`/?q=${encodeURIComponent(`exp="${run.experiment}"`)}`} className="block truncate py-1 text-muted-foreground hover:text-foreground hover:underline" title={run.experiment ?? undefined}>
        {run.experiment}
      </Link>
    );
  if (best !== undefined) {
    const [, agg, ...k] = c.id.split(":");
    const raw = run.metrics[k.join(":")]?.[agg as "max" | "min"];
    if (raw === best)
      return (
        <span className="block truncate font-mono font-bold" title={`${v.title ?? v.text}; the best in view`}>
          {v.text}
          <span className="ml-1 text-[11px] text-brand" aria-label="best">
            ★
          </span>
        </span>
      );
  }
  return (
    <span className={cn("block truncate", c.kind !== "run" && "font-mono", v.text === "–" && "text-faint")} title={v.title}>
      {v.text}
    </span>
  );
}

/** A run's loss curve on its experiment's scale, or "none" when the run has not logged the metric. */
type SparkData = { values: (number | null)[]; domain: [number, number] } | "none";

interface SparkCell {
  after: number;
  /** null while loading. */
  data: SparkData | null;
  metric: string;
}

function Row({ run, columns, index, letter, selected, active, onToggle, best, spark }: { run: RunRow; columns: ColumnDef[]; index: number; letter: string | null; selected: boolean; active: boolean; onToggle: () => void; best: Map<string, number>; spark: SparkCell | null }) {
  const bg = selected ? "bg-muted" : active ? "bg-hover" : "bg-background group-hover:bg-hover";
  const sparkColor = run.status === "failed" ? "var(--danger)" : run.status === "running" ? "var(--brand)" : "var(--foreground)";
  return (
    <tr className="group" aria-rowindex={index + 2} aria-selected={selected} data-active={active || undefined}>
      <td className={cn("sticky left-0 z-[1] border-b px-3", bg, selected && "shadow-[inset_3px_0_0_var(--rule)]")} style={{ height: ROW }}>
        <input type="checkbox" checked={selected} onChange={onToggle} aria-label={`Select ${run.name}`} />
      </td>
      {columns.map((c, i) => {
        const cell = (
          <td
            key={c.id}
            className={cn("max-w-0 border-b px-3", bg, c.numeric && "text-right nums", i === 0 && "sticky z-[1]", active && i === 0 && "underline decoration-rule decoration-2 underline-offset-4")}
            style={i === 0 ? { left: CHECK, height: ROW } : { height: ROW }}
          >
            <Cell c={c} run={run} letter={letter} best={best.get(c.id)} />
          </td>
        );
        if (!spark || i !== spark.after) return cell;
        return [
          cell,
          <td key="spark" className={cn("border-b px-3", bg)}>
            {run.kind !== "training" ? (
              <span className="text-faint">–</span>
            ) : spark.data === "none" ? (
              <span className="text-xs text-faint" title={`${run.name} has not logged ${spark.metric}`}>
                {run.status === "running" ? "No points yet" : "Not logged"}
              </span>
            ) : spark.data ? (
              <Sparkline values={spark.data.values} domain={spark.data.domain} color={sparkColor} width={SPARK - 24} label={`${spark.metric} of ${run.name}`} />
            ) : (
              <Skeleton className="h-4 w-full" />
            )}
          </td>,
        ];
      })}
    </tr>
  );
}

function MobileRow({ run, columns, letter, selected, onToggle, active, sort }: { run: RunRow; columns: ColumnDef[]; letter: string | null; selected: boolean; onToggle: () => void; active: boolean; sort: string | null }) {
  const sorted = sort ? columnDef(sort) : null;
  const metric = sorted?.kind === "metric" || sorted?.kind === "param" ? sorted : columns.find((c) => c.kind === "metric");
  return (
    <div className={cn("flex h-full items-center gap-3 border-b px-1", selected ? "bg-muted shadow-[inset_3px_0_0_var(--rule)]" : active ? "bg-hover" : "bg-background")}>
      <label className="flex size-11 shrink-0 items-center justify-center">
        <input type="checkbox" checked={selected} onChange={onToggle} aria-label={`Select ${run.name}`} />
      </label>
      <div className="min-w-0 flex-1 pr-2">
        <div className="flex items-center gap-2">
          {letter && <Letter l={letter} />}
          <Link to={`/runs/${run.id}`} className="min-w-0 truncate py-1 font-mono text-[13px] hover:underline">
            {run.name}
          </Link>
          <StatusBadge status={run.status} stale={run.stale} lastSeen={run.last_seen} iconOnly />
        </div>
        <div className="flex gap-3 truncate text-sm text-muted-foreground nums">
          <span className="truncate">{run.experiment ?? "No experiment"}</span>
          <span>{duration(run.duration)}</span>
          {metric && (
            <span className="text-foreground">
              {metric.label} <span className="font-mono">{metric.cell(run).text}</span>
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function TableSkeleton() {
  return (
    <div aria-busy aria-label="Loading runs">
      <div className="flex h-11 items-center gap-6 border-b px-3">
        {Array.from({ length: 7 }, (_, i) => (
          <Skeleton key={i} className="h-3 w-16" />
        ))}
      </div>
      {Array.from({ length: 12 }, (_, i) => (
        <div key={i} className="flex items-center gap-6 border-b px-3" style={{ height: ROW }}>
          <Skeleton className="size-4" />
          <Skeleton className="h-3 w-48" />
          <Skeleton className="h-3 w-20" />
          <Skeleton className="h-3 w-28" />
          <Skeleton className="h-3 w-16" />
          <Skeleton className="ml-auto h-3 w-14" />
        </div>
      ))}
    </div>
  );
}
