import { keepPreviousData, useQueries, useQuery } from "@tanstack/react-query";
import { Download } from "lucide-react";
import { useMemo, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { type Line, LineChart } from "@/components/line-chart";
import { Caption, Insight, MarginNote, PageHeader, SectionHead, ToolSwitch } from "@/components/page";
import { ErrorState } from "@/components/states";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { type ChartData, type Json, type RunDetail, type XAxis, api, isSystem } from "@/lib/api";
import { useCrumbs } from "@/lib/crumbs";
import { duration, higherIsBetter, metricLabel, num, value, xValue } from "@/lib/format";
import { useTitle } from "@/lib/hooks";
import { LETTERS } from "@/lib/selection";
import { cn } from "@/lib/utils";

/** Lettered runs read well up to eight; beyond that the explorer is the better tool. */
const MAX = 8;
const RANK_COLORS = ["var(--brand)", "var(--foreground)", "var(--series-strong)", "var(--series)"];

const same = (a: Json | undefined, b: Json | undefined) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
const short = (k: string) => k.split(".").pop() ?? k;

function lastValue(y: (number | null)[]): number | null {
  for (let i = y.length - 1; i >= 0; i--) if (y[i] !== null && isFinite(y[i]!)) return y[i];
  return null;
}

/** Lines for a chart, coloured by rank at the end: the leader in ink blue, the rest ink to grey. */
function linesFor(d: ChartData | undefined, ids: string[], up: boolean): { lines: Line[]; order: string[] } {
  if (!d) return { lines: [], order: [] };
  const finals = new Map(d.series.map((s) => [s.run_id, lastValue(s.y)]));
  const order = [...ids].filter((id) => finals.get(id) != null).sort((a, b) => (up ? finals.get(b)! - finals.get(a)! : finals.get(a)! - finals.get(b)!));
  const lines = d.series
    .filter((s) => ids.includes(s.run_id))
    .map((s) => {
      const rank = order.indexOf(s.run_id);
      const letter = LETTERS[ids.indexOf(s.run_id)];
      const last = finals.get(s.run_id);
      return {
        id: s.run_id,
        name: `Run ${letter}`,
        end: `${letter} ${num(last)}`,
        color: RANK_COLORS[Math.min(rank < 0 ? 3 : rank, 3)],
        width: rank === 0 ? 2 : 1.5,
        dash: rank > 3 ? "5 3" : undefined,
        bold: rank === 0,
        points: d.x.map((x, i) => [x, s.y[i]] as [number, number | null]),
      };
    })
    .sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id));
  return { lines, order };
}

export function CompareRunsPage() {
  const [sp, setSp] = useSearchParams();
  const all = sp.get("runs")?.split(",").filter(Boolean) ?? [];
  const ids = all.slice(0, MAX);
  const x = (sp.get("x") as XAxis) === "relative" ? "relative" : "step";
  const [showAll, setShowAll] = useState(false);
  const [hover, setHover] = useState<number | null>(null);

  const details = useQueries({ queries: ids.map((id) => ({ queryKey: ["run", id], queryFn: ({ signal }: { signal: AbortSignal }) => api.run(id, signal) })) });
  const runs = details.map((d) => d.data).filter((r): r is RunDetail => !!r);
  const ready = runs.length === ids.length && ids.length > 0;
  const failed = details.find((d) => d.isError);

  // Metrics every compared run logged; the headline is a validation score, the second a loss.
  const common = useMemo(() => {
    if (!ready) return [];
    const keys = Object.keys(runs[0].metrics).filter((k) => !isSystem(k) && runs.every((r) => r.metrics[k]));
    const rank = (k: string) => (/val|valid|eval|test/i.test(k) ? 0 : 1) + (higherIsBetter(metricLabel(k).name) ? 0 : 0.5);
    return keys.sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready, ids.join(",")]);
  const primaryDefault = common[0] ?? null;
  const secondary = common.find((k) => /loss/i.test(k) && /train/i.test(k)) ?? common.find((k) => /loss/i.test(k) && k !== primaryDefault) ?? common.find((k) => k !== primaryDefault) ?? null;
  const metricParam = sp.get("metric");
  const primary = metricParam && common.includes(metricParam) ? metricParam : primaryDefault;
  const choices = [...new Set([primaryDefault, secondary, ...common].filter((k): k is string => !!k))].slice(0, 3);

  const chart = useQuery({
    queryKey: ["compare-chart", ids.join(","), primary, x],
    queryFn: ({ signal }) => api.chart({ runs: ids, metric: primary!, x, points: 400 }, signal),
    enabled: !!primary,
    placeholderData: keepPreviousData,
  });
  const chart2 = useQuery({
    queryKey: ["compare-chart", ids.join(","), secondary, x],
    queryFn: ({ signal }) => api.chart({ runs: ids, metric: secondary!, x, points: 300 }, signal),
    enabled: !!secondary && secondary !== primary,
    placeholderData: keepPreviousData,
  });

  const up = primary ? higherIsBetter(metricLabel(primary).name) : true;
  const main = linesFor(chart.data, ids, up);
  const support = linesFor(chart2.data, ids, secondary ? higherIsBetter(metricLabel(secondary).name) : false);
  const letter = (id: string) => LETTERS[ids.indexOf(id)];

  // Settings: every parameter key; the ones that differ first.
  const params = useMemo(() => {
    if (!ready) return { differ: [] as string[], equal: [] as string[] };
    const keys = [...new Set(runs.flatMap((r) => Object.keys(r.params)))].filter((k) => !isSystem(k)).sort();
    const differ = keys.filter((k) => runs.some((r) => !same(r.params[k], runs[0].params[k])));
    return { differ, equal: keys.filter((k) => !differ.includes(k)) };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready, ids.join(",")]);

  const experiments = [...new Set(runs.map((r) => r.experiment))];
  const experiment = experiments.length === 1 ? experiments[0] : null;
  useTitle(`Compare ${ids.length} runs`);
  useCrumbs([{ label: "Runs", to: "/" }, ...(experiment ? [{ label: experiment, to: `/?q=${encodeURIComponent(`exp="${experiment}"`)}`, mono: true }] : []), { label: ids.length ? `Compare ${ids.length} runs` : "Compare" }]);

  if (!ids.length)
    return (
      <div className="px-4 pt-5 md:px-6">
        <PageHeader title="Compare runs" />
        <p className="mt-4 max-w-prose text-base text-muted-foreground">
          Select runs on the{" "}
          <Link to="/" className="text-foreground underline">
            runs page
          </Link>{" "}
          (<kbd>x</kbd> or the checkboxes), then press <kbd>c</kbd>. Each run gets a letter that it keeps here.
        </p>
      </div>
    );
  if (failed) return <ErrorState error={failed.error as Error} onRetry={() => failed.refetch()} />;

  const lead = main.order[0];
  const second = main.order[1];
  const finals = new Map(chart.data?.series.map((s) => [s.run_id, lastValue(s.y)]));
  const pl = primary ? metricLabel(primary) : null;
  const metricName = pl ? `${pl.name}${pl.context ? ` ${pl.context}` : ""}` : "";
  const lastX = chart.data?.x.at(-1);
  const trail = main.order.at(-1);
  const readX = hover ?? null;

  const statRows: { label: string; cells: (string | null)[]; best?: number }[] = [];
  if (ready && primary) {
    const agg = up ? "max" : "min";
    const vals = runs.map((r) => r.metrics[primary]?.[agg] ?? null);
    const real = vals.filter((v): v is number => v !== null);
    statRows.push({ label: `${metricName}, best`, cells: vals.map(num), best: real.length > 1 ? vals.indexOf(up ? Math.max(...real) : Math.min(...real)) : undefined });
    statRows.push({ label: `${metricName}, last`, cells: runs.map((r) => num(r.metrics[primary]?.last)) });
  }
  if (ready && secondary && secondary !== primary) {
    const s = metricLabel(secondary);
    statRows.push({ label: `${s.name}${s.context ? ` ${s.context}` : ""}, last`, cells: runs.map((r) => num(r.metrics[secondary]?.last)) });
  }
  if (ready) statRows.push({ label: "duration", cells: runs.map((r) => duration(r.duration)) });

  const exportCsv = () => {
    const head = ["setting", ...runs.map((r) => `${letter(r.id)} ${r.name}`)];
    const body = [...params.differ, ...params.equal].map((k) => [k, ...runs.map((r) => value(r.params[k]))]);
    const stats = statRows.map((s) => [s.label, ...s.cells.map((c) => c ?? "")]);
    const esc = (v: string) => (/[",\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
    const blob = new Blob([[head, ...body, ...stats].map((l) => l.map(esc).join(",")).join("\n")], { type: "text/csv" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = "vigily-compare.csv";
    a.click();
    URL.revokeObjectURL(a.href);
  };

  const setParam = (k: string, v: string | null) =>
    setSp(
      (p) => {
        if (v) p.set(k, v);
        else p.delete(k);
        return p;
      },
      { replace: true },
    );
  const xLabel = x === "step" ? "training step" : "time since start";
  const fmtX = (v: number) => (x === "step" ? (Math.abs(v) >= 1000 ? `${+(v / 1000).toFixed(1)}k` : String(v)) : xValue("relative", v));

  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <PageHeader
        title={`Compare ${ids.length} ${ids.length === 1 ? "run" : "runs"}`}
        actions={
          <>
            <Button asChild variant="outline">
              <Link to="/">Add runs</Link>
            </Button>
            <Button variant="outline" onClick={exportCsv} disabled={!ready} className="max-sm:hidden">
              <Download /> Export
            </Button>
            <Button asChild variant="brand">
              <Link to={`/metrics?runs=${all.join(",")}`}>Open in explorer</Link>
            </Button>
          </>
        }
      >
        {experiment ? (
          <span>
            Experiment <b className="font-mono">{experiment}</b>
          </span>
        ) : ready ? (
          <span>
            <b>{experiments.length}</b> experiments
          </span>
        ) : (
          <span> </span>
        )}
        {ready && (
          <span>
            <b>{params.differ.length}</b> of {params.differ.length + params.equal.length} settings differ
          </span>
        )}
        {all.length > MAX && <span>The first {MAX} of {all.length} selected runs</span>}
      </PageHeader>

      {ready && primary && lead && (
        <Insight tag="Summary">
          Run {letter(lead)} leads on <span className="font-mono">{primary}</span> with <b>{num(finals.get(lead))}</b>
          {second && finals.get(second) != null && (
            <>
              , <b>{num(Math.abs(finals.get(lead)! - finals.get(second)!))}</b> ahead of {letter(second)}
            </>
          )}
          .{" "}
          {params.differ.length > 0 && params.differ.length <= 3 && `Only ${params.differ.map(short).join(" and ")} ${params.differ.length === 1 ? "differs" : "differ"}.`}
        </Insight>
      )}

      <SectionHead
        title={pl ? `${pl.name[0].toUpperCase()}${pl.name.slice(1)}${pl.context ? `, ${pl.context}` : ""}` : "Metrics"}
        tools={
          <>
            {choices.length > 1 && primary && <ToolSwitch label="Metric" options={choices.map((k) => [k, metricLabel(k).name + (metricLabel(k).context ? ` ${metricLabel(k).context}` : "")])} value={primary} onChange={(v) => setParam("metric", v === primaryDefault ? null : v)} />}
            <span aria-hidden className="max-sm:hidden">
              ·
            </span>
            <ToolSwitch
              label="X axis"
              options={[
                ["step", "By step"],
                ["relative", "By time"],
              ]}
              value={x}
              onChange={(v) => setParam("x", v === "step" ? null : v)}
            />
          </>
        }
      />
      <div className="grid gap-x-6 lg:grid-cols-[200px_minmax(0,1fr)]">
        <div className="pt-5">
          <h3 className="mb-2 text-sm font-bold">Runs</h3>
          <ul className="flex flex-col gap-2.5 max-lg:flex-row max-lg:flex-wrap max-lg:gap-x-5">
            {ids.map((id, i) => {
              const r = runs.find((q) => q.id === id);
              return (
                <li key={id} className="flex min-w-0 items-start gap-2.5">
                  <span className="mt-px inline-flex size-[18px] shrink-0 items-center justify-center border-[1.5px] border-rule text-[11px] font-bold">{LETTERS[i]}</span>
                  <span className="min-w-0">
                    <Link to={`/runs/${id}`} className="block truncate font-mono text-[12.5px] leading-6 hover:underline" title={r?.name}>
                      {r ? r.name : <Skeleton className="h-3 w-28" />}
                    </Link>
                    {r && params.differ.length > 0 && (
                      <span className="block truncate font-mono text-[11.5px] text-faint">
                        {params.differ
                          .slice(0, 3)
                          .map((k) => `${short(k)} ${value(r.params[k])}`)
                          .join("  ")}
                      </span>
                    )}
                  </span>
                </li>
              );
            })}
          </ul>
        </div>
        <figure className="min-w-0 pt-5">
          {main.lines.length ? (
            <LineChart
              lines={main.lines}
              height={360}
              xFormat={fmtX}
              label={`${primary} by ${xLabel} for runs ${ids.map(letter).join(", ")}. ${main.order.map((id) => `${letter(id)} ends at ${num(finals.get(id))}`).join("; ")}.`}
              onHover={setHover}
            />
          ) : (
            <Skeleton className="h-[360px]" />
          )}
          <p className="h-6 pt-1 font-mono text-[12px] text-muted-foreground nums" data-readout aria-live="polite">
            {readX !== null && chart.data
              ? `${x === "step" ? "Step" : "Time"} ${fmtX(readX)}: ${main.lines
                  .map((l) => {
                    const p = l.points.find((q) => q[0] === readX);
                    return `${l.name.slice(4)} ${num(p?.[1] ?? null)}`;
                  })
                  .join("  ")}`
              : ""}
          </p>
          <Caption n={1} right={lead && trail && lead !== trail && lastX !== undefined ? `Gap at ${x === "step" ? "step" : ""} ${fmtX(lastX)}: ${letter(lead)} – ${letter(trail)} = ${num(Math.abs(finals.get(lead)! - finals.get(trail)!))}` : undefined}>
            {pl?.name} by {xLabel}
            {chart.data && !chart.data.exact ? ", downsampled for drawing" : ""}. Raw values, no smoothing.
          </Caption>
        </figure>
      </div>

      <SectionHead title="Supporting measures" />
      <div className="grid gap-x-8 lg:grid-cols-[200px_minmax(0,1fr)] 2xl:grid-cols-[200px_minmax(0,1fr)_minmax(0,1fr)]">
        <div className="pt-5">
          <MarginNote title="What differs">
            Settings with the same value in every run are hidden. {params.equal.length > 0 && `${params.equal.length} more are the same.`}
          </MarginNote>
        </div>
        {secondary && secondary !== primary && (
          <figure className="min-w-0 pt-5 2xl:order-none max-2xl:order-last max-2xl:lg:col-start-2">
            {support.lines.length ? <LineChart lines={support.lines} height={240} xFormat={fmtX} label={`${secondary} by ${xLabel}`} /> : <Skeleton className="h-[240px]" />}
            <Caption n={2}>
              {metricLabel(secondary).name}
              {metricLabel(secondary).context ? ` ${metricLabel(secondary).context}` : ""} by {xLabel}.
            </Caption>
          </figure>
        )}
        <div className="min-w-0 pt-3">
          <div className="overflow-x-auto">
            <table className="w-full min-w-[22rem] border-collapse text-sm">
              <thead>
                <tr>
                  <th scope="col" className="border-b px-3 py-2.5 text-left">
                    Setting
                  </th>
                  {ids.map((id) => (
                    <th key={id} scope="col" className="border-b px-3 py-2.5 text-right">
                      {letter(id)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {!ready &&
                  Array.from({ length: 4 }, (_, i) => (
                    <tr key={i}>
                      <td colSpan={ids.length + 1} className="border-b px-3 py-3">
                        <Skeleton className="h-3 w-full" />
                      </td>
                    </tr>
                  ))}
                {[...params.differ, ...(showAll ? params.equal : [])].map((k) => {
                  const differs = params.differ.includes(k);
                  return (
                    <tr key={k} className={differs ? "bg-muted" : undefined}>
                      <th scope="row" className="border-b px-3 py-2.5 text-left font-normal">
                        <span className="font-mono text-[12.5px]">{k}</span>
                        {differs && <span className="ml-2 text-[10.5px] font-bold uppercase tracking-[0.1em] text-faint">differs</span>}
                      </th>
                      {differs || ids.length < 2 ? (
                        runs.map((r) => (
                          <td key={r.id} className="border-b px-3 py-2.5 text-right font-mono text-[13px]">
                            {value(r.params[k]) || "–"}
                          </td>
                        ))
                      ) : (
                        <td colSpan={ids.length} className="border-b px-3 py-2.5 text-right font-mono text-[13px] text-muted-foreground">
                          {value(runs[0].params[k]) || "–"}
                        </td>
                      )}
                    </tr>
                  );
                })}
                {statRows.map((s) => (
                  <tr key={s.label}>
                    <th scope="row" className="border-b px-3 py-2.5 text-left font-normal">
                      {s.label}
                    </th>
                    {s.cells.map((c, i) => (
                      <td key={i} className={cn("border-b px-3 py-2.5 text-right font-mono text-[13px] nums", s.best === i && "font-bold")}>
                        {c}
                        {s.best === i && (
                          <span className="ml-1 text-[11px] text-brand" aria-label="best">
                            ★
                          </span>
                        )}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {params.equal.length > 0 && (
            <button type="button" onClick={() => setShowAll((v) => !v)} className="mt-2 min-h-6 text-sm font-semibold underline underline-offset-4 pointer-coarse:min-h-11">
              {showAll ? "Hide the settings that are the same" : `Show all ${params.differ.length + params.equal.length} settings`}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
