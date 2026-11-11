import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { ChartLine, Pin, SlidersHorizontal } from "lucide-react";
import { useCallback, useMemo, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { ChartControls } from "@/components/chart-controls";
import { type ChartSettings, MetricChart } from "@/components/metric-chart";
import { QueryInput } from "@/components/query-input";
import { EmptyRepository, ErrorState } from "@/components/states";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { ApiError, type XAxis, api, isSystem } from "@/lib/api";
import { mainMetrics } from "@/lib/columns";
import { metricLabel } from "@/lib/format";
import { useMediaQuery, useTitle } from "@/lib/hooks";
import { useMeta } from "@/lib/queries";
import { useSelection } from "@/lib/selection";
import { cn } from "@/lib/utils";

/** More lines than this are unreadable and slow; the newest ones are drawn. */
const MAX_RUNS = 200;

export function ExplorerPage() {
  useTitle("Metrics");
  const [sp, setSp] = useSearchParams();
  const meta = useMeta();
  const selection = useSelection();
  const wide = useMediaQuery("(min-width: 1024px)");
  const [panel, setPanel] = useState(false);
  const [pinned, setPinned] = useState<Set<string>>(new Set());
  const [metricFilter, setMetricFilter] = useState("");

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

  const explicit = sp.get("runs")?.split(",").filter(Boolean) ?? null;
  const q = sp.get("q") ?? "";
  const settings: ChartSettings = {
    x: (sp.get("x") as XAxis) || "step",
    logY: sp.get("log") === "1",
    smoothing: Math.min(0.99, Math.max(0, Number(sp.get("smooth") ?? 0) || 0)),
    groupBy: sp.get("group"),
    aggregate: sp.get("agg") === "1",
    ignoreOutliers: sp.get("outliers") !== "1",
  };
  const setSettings = (s: Partial<ChartSettings>) =>
    update((p) => {
      if (s.x !== undefined) {
        if (s.x === "step") p.delete("x");
        else p.set("x", s.x);
      }
      if (s.logY !== undefined) {
        if (s.logY) p.set("log", "1");
        else p.delete("log");
      }
      if (s.smoothing !== undefined) {
        if (s.smoothing) p.set("smooth", String(s.smoothing));
        else p.delete("smooth");
      }
      if (s.groupBy !== undefined) {
        if (s.groupBy) p.set("group", s.groupBy);
        else p.delete("group");
      }
      if (s.ignoreOutliers !== undefined) {
        if (s.ignoreOutliers) p.delete("outliers");
        else p.set("outliers", "1");
      }
      if (s.aggregate !== undefined) {
        if (s.aggregate) p.set("agg", "1");
        else p.delete("agg");
      }
    });
  const columns = Number(sp.get("cols") ?? 2) || 2;

  const runs = useQuery({
    queryKey: ["runs", q, null, false, false],
    queryFn: ({ signal }) => api.runs({ q }, signal),
    placeholderData: keepPreviousData,
    retry: (n, e) => !(e instanceof ApiError && e.status === 400) && n < 2,
  });
  const queryError = runs.error instanceof ApiError && runs.error.status === 400 ? runs.error : null;
  const byId = useMemo(() => new Map((runs.data?.rows ?? []).map((r) => [r.id, r])), [runs.data]);
  const pool = useMemo(() => {
    if (explicit) return explicit.map((id) => byId.get(id)).filter((r) => r !== undefined);
    return runs.data?.rows ?? [];
  }, [explicit, byId, runs.data]);
  const drawn = pool.slice(0, MAX_RUNS);
  const runIds = useMemo(() => drawn.map((r) => r.id), [drawn]);

  const allMetrics = useMemo(() => (meta.data?.metrics ?? []).filter((m) => !isSystem(m.name)), [meta.data]);
  const chosen = sp.getAll("m");
  const metrics = chosen.length ? chosen : meta.data ? mainMetrics(meta.data, 4, explicit ? drawn : undefined) : [];
  const toggleMetric = (key: string) =>
    update((p) => {
      const cur = p.getAll("m").length ? p.getAll("m") : metrics;
      const next = cur.includes(key) ? cur.filter((k) => k !== key) : [...cur, key];
      p.delete("m");
      for (const k of next) p.append("m", k);
      if (!next.length) p.append("m", "");
    });
  const shownMetrics = metrics.filter(Boolean);
  const togglePin = (id: string) =>
    setPinned((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  if (meta.data && meta.data.runs === 0) return <EmptyRepository repository={meta.data.repository} />;
  if (meta.isError && !meta.data) return <ErrorState error={meta.error as Error} onRetry={() => meta.refetch()} />;

  const f = metricFilter.trim().toLowerCase();
  const sourceText = explicit
    ? `${pool.length} selected ${pool.length === 1 ? "run" : "runs"}`
    : q
      ? `${pool.length} ${pool.length === 1 ? "run matches" : "runs match"} the filter`
      : `All ${pool.length} runs`;

  const controls = (
    <div className="flex flex-col gap-5">
      <section aria-labelledby="runs-h">
        <h2 id="runs-h" className="text-[17px] font-bold tracking-[-0.01em]">
          Runs
        </h2>
        <p className="mt-0.5 text-sm text-muted-foreground nums">
          {sourceText}
          {pool.length > MAX_RUNS && `; drawing the newest ${MAX_RUNS}`}.
        </p>
        {explicit ? (
          <div className="mt-2 flex flex-wrap gap-2">
            <Button size="sm" onClick={() => update((p) => p.delete("runs"))}>
              Use a filter instead
            </Button>
            <Button size="sm" variant="ghost" asChild>
              <Link to="/">Change the selection</Link>
            </Button>
          </div>
        ) : (
          <div className="mt-2 flex flex-col gap-2">
            <QueryInput value={q} onChange={(v) => update((p) => (v ? p.set("q", v) : p.delete("q")))} meta={meta.data} error={queryError} placeholder="Filter: exp:cifar lr<0.01" />
            {selection.ids.length > 0 && (
              <Button size="sm" className="self-start" onClick={() => update((p) => p.set("runs", selection.ids.join(",")))}>
                Use the {selection.ids.length} selected runs
              </Button>
            )}
          </div>
        )}
      </section>
      <section aria-labelledby="metrics-h">
        <h2 id="metrics-h" className="text-[17px] font-bold tracking-[-0.01em]">
          Metrics
        </h2>
        {allMetrics.length > 8 && <Input className="mt-2" value={metricFilter} onChange={(e) => setMetricFilter(e.target.value)} placeholder="Find a metric…" aria-label="Find a metric" />}
        <ul className="mt-1.5 flex flex-col">
          {allMetrics
            .filter((m) => m.key.toLowerCase().includes(f))
            .map((m) => {
              const l = metricLabel(m.key);
              return (
                <li key={m.key}>
                  <label className="flex min-h-7 cursor-pointer items-center gap-2 rounded-sm px-1 text-sm hover:bg-muted pointer-coarse:min-h-11">
                    <input type="checkbox" checked={shownMetrics.includes(m.key)} onChange={() => toggleMetric(m.key)} />
                    <span className="min-w-0 flex-1 truncate">
                      {l.name}
                      {l.context && <span className="ml-1 text-muted-foreground">{l.context}</span>}
                    </span>
                    <span className="text-xs text-muted-foreground nums">{m.runs}</span>
                  </label>
                </li>
              );
            })}
        </ul>
      </section>
      <section aria-labelledby="chart-h">
        <h2 id="chart-h" className="mb-3 mt-6 border-b-[1.5px] border-rule pb-2.5 text-[17px] font-bold tracking-[-0.01em]">
          Charts
        </h2>
        <ChartControls value={settings} onChange={setSettings} meta={meta.data} />
        <div className="mt-3">
          <span className="text-xs font-medium text-muted-foreground">Charts per row</span>
          <div className="mt-1 grid grid-cols-3 rounded-md bg-muted p-0.5" role="radiogroup" aria-label="Charts per row">
            {[1, 2, 3].map((c) => (
              <button
                key={c}
                type="button"
                role="radio"
                aria-checked={columns === c}
                onClick={() => update((p) => (c === 2 ? p.delete("cols") : p.set("cols", String(c))))}
                className={cn("h-7 rounded-[5px] text-sm nums pointer-coarse:h-10", columns === c ? "bg-card font-semibold border border-border" : "text-muted-foreground hover:text-foreground")}
              >
                {c}
              </button>
            ))}
          </div>
        </div>
      </section>
      {pinned.size > 0 && (
        <section aria-labelledby="pinned-h">
          <h2 id="pinned-h" className="text-[17px] font-bold tracking-[-0.01em]">
            Pinned
          </h2>
          <ul className="mt-1 flex flex-col">
            {[...pinned].map((id) => {
              const r = byId.get(id);
              return (
                <li key={id} className="flex min-h-7 items-center gap-2 text-sm">
                  <Pin className="size-3.5 shrink-0 text-brand" aria-hidden />
                  <Link to={`/runs/${id}`} className="min-w-0 flex-1 truncate hover:underline">
                    {r?.name ?? id.slice(-8)}
                  </Link>
                  <button type="button" onClick={() => togglePin(id)} className="rounded-sm px-1.5 text-xs text-muted-foreground hover:bg-muted hover:text-foreground pointer-coarse:min-h-11">
                    Unpin
                  </button>
                </li>
              );
            })}
          </ul>
        </section>
      )}
    </div>
  );

  const gridCols = wide ? columns : 1;
  return (
    <div className="flex min-h-full flex-col lg:h-full lg:flex-row">
      <aside className="shrink-0 border-b bg-sidebar px-4 py-4 lg:h-full lg:w-72 lg:overflow-y-auto lg:border-b-0 lg:border-r">
        <div className="flex items-center justify-between lg:mb-4">
          <h1 className="text-xl font-bold tracking-[-0.02em]">Metrics</h1>
          <Button size="sm" variant="outline" onClick={() => setPanel((p) => !p)} aria-expanded={panel} aria-controls="explorer-settings" className="lg:hidden">
            <SlidersHorizontal /> {panel ? "Hide settings" : "Settings"}
          </Button>
        </div>
        <div id="explorer-settings" className={cn(wide || panel ? "block" : "hidden", "mt-4 lg:mt-0")}>
          {controls}
        </div>
      </aside>
      <div className="min-w-0 flex-1 p-3 md:p-4 lg:h-full lg:overflow-y-auto">
        {runs.isPending || !meta.data ? (
          <div className="grid gap-3" style={{ gridTemplateColumns: `repeat(${gridCols}, minmax(0, 1fr))` }}>
            {Array.from({ length: 4 }, (_, i) => (
              <Skeleton key={i} className="h-[300px] rounded-lg" />
            ))}
          </div>
        ) : shownMetrics.length === 0 ? (
          <Empty title="Choose a metric" text="Tick one or more metrics in the settings to draw them for these runs." />
        ) : drawn.length === 0 ? (
          <Empty title="No runs to draw" text={explicit ? "None of the selected runs exist any more. Pick runs on the Runs page." : "No run matches the filter. Change or clear it."} />
        ) : (
          <div className="grid gap-3" style={{ gridTemplateColumns: `repeat(${gridCols}, minmax(0, 1fr))` }}>
            {shownMetrics.map((m) => (
              <MetricChart key={m} metric={m} runs={runIds} {...settings} height={columns === 1 && wide ? 360 : 260} syncKey="explorer" pinned={pinned} onPin={togglePin} />
            ))}
          </div>
        )}
        {drawn.length > 0 && shownMetrics.length > 0 && (
          <p className="mt-3 text-sm text-muted-foreground">Drag across a chart to zoom, double-click to reset. Click a run in a readout to pin it in every chart.</p>
        )}
      </div>
    </div>
  );
}

function Empty({ title, text }: { title: string; text: string }) {
  return (
    <div className="flex flex-col items-start gap-2 border bg-card px-6 py-10">
      <ChartLine className="size-5 text-muted-foreground" aria-hidden />
      <h2 className="text-[17px] font-bold tracking-[-0.01em]">{title}</h2>
      <p className="max-w-prose text-base text-muted-foreground">{text}</p>
    </div>
  );
}
