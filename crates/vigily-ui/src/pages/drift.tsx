import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Pin } from "lucide-react";
import { useMemo, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";
import { LevelBadge, Method, jsonText } from "@/components/evals";
import { LineChart } from "@/components/line-chart";
import { Caption, Figure, Figures, Insight, MarginNote, PageHeader, SectionHead, ToolSwitch } from "@/components/page";
import { ErrorState } from "@/components/states";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip } from "@/components/ui/tooltip";
import { type Baseline, type BehaviourDrift, type FeatureDrift, type HistoryPoint, type SuiteDetail, evalApi } from "@/lib/api";
import { useCrumbs } from "@/lib/crumbs";
import { ago, dateTime, integer, num, shortDate, utc } from "@/lib/format";
import { useTitle } from "@/lib/hooks";
import { cn } from "@/lib/utils";

const pct = (v: number) => `${Math.round(v * 1000) / 10} %`;
const pValue = (p: number | null | undefined) => (p === null || p === undefined ? "–" : p < 1e-4 ? p.toExponential(1) : p.toFixed(4));
const signed = (v: number) => `${v > 0 ? "+" : v < 0 ? "−" : "±"}${num(Math.abs(v))}`;
/** Strips show the newest runs, one cell each. */
const STRIP = 30;

export function DriftPage() {
  useTitle("Drift");
  const q = useQuery({ queryKey: ["suites"], queryFn: ({ signal }) => evalApi.suites(signal), refetchInterval: 30_000 });
  if (q.isError) return <ErrorState error={q.error as Error} onRetry={() => q.refetch()} />;
  const list = (q.data ?? []).filter((s) => s.runs > 0);
  const drifting = list.filter((s) => s.behaviour.some((b) => b.drifted) || s.data === "drift");
  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <PageHeader title="Drift">
        {q.data ? (
          <>
            <span>
              <b>{list.length}</b> evaluation {list.length === 1 ? "suite" : "suites"}
            </span>
            <span>
              <b className={drifting.length ? "text-danger!" : undefined}>{drifting.length}</b> drifting
            </span>
            <span>Each newest run against its baseline</span>
          </>
        ) : (
          <span> </span>
        )}
      </PageHeader>
      {q.data && list.length > 0 && (
        <Insight tag={drifting.length ? "Drift" : "Stable"} tone={drifting.length ? "bad" : "good"} action={drifting.length ? <Link to={`/drift/${drifting[0].key}`}>Open {drifting[0].label}</Link> : undefined}>
          {drifting.length ? (
            <>
              {drifting.map((s) => s.label).join(", ")} {drifting.length === 1 ? "is" : "are"} below baseline on the newest run.
            </>
          ) : (
            <>Every suite’s newest run is within its baseline.</>
          )}
        </Insight>
      )}
      {q.data && list.length === 0 ? (
        <div className="mt-6 max-w-2xl">
          <h2 className="text-lg font-bold">Nothing to compare yet</h2>
          <p className="mt-1 text-base text-muted-foreground">
            Drift needs the same evaluation repeated: run <code>vigily.evaluate(...)</code> with the same experiment, target and dataset again, for example from a schedule. From the second run
            on, each run is compared with the ones before it.
          </p>
        </div>
      ) : (
        <div className="relative mt-2 overflow-x-auto">
          <table className="w-full min-w-[52rem] border-collapse text-sm">
            <thead>
              <tr className="text-left">
                <th scope="col" className="border-b px-3 py-2.5">
                  Suite
                </th>
                <th scope="col" className="border-b px-3 py-2.5">
                  Newest scores
                </th>
                <th scope="col" className="border-b px-3 py-2.5">
                  Behaviour
                </th>
                <th scope="col" className="border-b px-3 py-2.5">
                  Inputs
                </th>
                <th scope="col" className="border-b px-3 py-2.5 text-right">
                  Last run
                </th>
              </tr>
            </thead>
            <tbody>
              {!q.data &&
                Array.from({ length: 3 }, (_, i) => (
                  <tr key={i}>
                    {Array.from({ length: 5 }, (_, j) => (
                      <td key={j} className="h-14 border-b px-3">
                        <Skeleton className="h-3 w-28" />
                      </td>
                    ))}
                  </tr>
                ))}
              {list.map((s) => {
                const bad = s.behaviour.some((b) => b.drifted) || s.data === "drift";
                return (
                  <tr key={s.key} className={cn("align-top hover:bg-hover", bad && "shadow-[inset_3px_0_0_var(--danger)]")}>
                    <th scope="row" className="border-b px-3 py-3 text-left font-normal">
                      <Link to={`/drift/${s.key}`} className="inline-flex min-h-6 items-center font-mono text-[13.5px] font-semibold hover:underline">
                        {s.label}
                      </Link>
                      <span className="mt-0.5 block text-faint">{[s.experiment, s.dataset && `on ${s.dataset}`, `${s.runs} ${s.runs === 1 ? "run" : "runs"}`].filter(Boolean).join(", ")}</span>
                    </th>
                    <td className="border-b px-3 py-3 font-mono text-[13px] nums">
                      {s.latest &&
                        Object.entries(s.latest.scores).map(([k, v]) => (
                          <span key={k} className="block">
                            <span className="text-faint">{k}</span> {num(v)}
                          </span>
                        ))}
                    </td>
                    <td className="border-b px-3 py-3">
                      {s.behaviour.length ? (
                        s.behaviour.map((b) => (
                          <span key={b.score} className="flex items-center gap-1.5">
                            <LevelBadge level={b.drifted ? "drift" : "stable"} />
                            <span className="font-mono text-[12.5px] text-faint">{b.score}</span>
                            {b.drifted && <span className="font-mono text-xs font-bold text-danger nums">{signed(b.delta)}</span>}
                          </span>
                        ))
                      ) : (
                        <span className="text-faint">Needs a second run</span>
                      )}
                    </td>
                    <td className="border-b px-3 py-3">
                      <LevelBadge level={s.data} />
                    </td>
                    <td className="border-b px-3 py-3 text-right text-muted-foreground">
                      {s.latest && (
                        <Tooltip content={utc(s.latest.created_at)}>
                          <span>{ago(s.latest.created_at)}</span>
                        </Tooltip>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

/** One strip per score: a cell per run, red when the run drifted, darker the further below the baseline. */
function Strip({ points }: { points: HistoryPoint[] }) {
  const shown = points.slice(-STRIP);
  return (
    <span className="grid gap-[2px]" style={{ gridTemplateColumns: `repeat(${STRIP}, minmax(0, 1fr))` }}>
      {Array.from({ length: STRIP - shown.length }, (_, i) => (
        <i key={`e${i}`} className="h-[22px]" />
      ))}
      {shown.map((p) => {
        const spread = p.baseline_mean !== null && p.baseline_low !== null ? Math.max(1e-9, p.baseline_mean - p.baseline_low) : null;
        const sigma = p.value !== null && p.baseline_mean !== null && spread ? Math.abs(p.value - p.baseline_mean) / (spread / 2) : 0;
        const tone = !p.drifted ? "bg-cell" : sigma >= 6 ? "bg-danger" : "bg-danger/55";
        return <i key={p.run_id} className={cn("h-[22px]", tone)} title={`${shortDate(p.created_at)}: ${num(p.value)}${p.drifted ? ", drift" : ""}`} />;
      })}
    </span>
  );
}

function Explain({ d, cfg }: { d: BehaviourDrift; cfg: SuiteDetail["config"] }) {
  const dir = d.higher_is_better ? "lower" : "higher";
  const p = d.test?.p ?? null;
  const reasons: string[] = [];
  if (!d.worse) reasons.push(`the score is not ${dir} than the baseline`);
  if (Math.abs(d.delta) < cfg.min_effect) reasons.push(`the difference of ${num(Math.abs(d.delta))} is below the minimum effect of ${cfg.min_effect}`);
  if (p !== null && p >= cfg.alpha) reasons.push(`p = ${pValue(p)} is not below ${cfg.alpha}`);
  return (
    <p className="text-sm">
      {d.drifted ? (
        <>
          <strong>Drift.</strong> The mean is {num(d.current_mean)} against {num(d.baseline_mean)} on the baseline: a {dir} score by {num(Math.abs(d.delta))}
          {d.ci && ` (95 % interval ${num(d.ci[0])} to ${num(d.ci[1])})`}, at least the minimum effect of {cfg.min_effect}, with p = {pValue(p)} below {cfg.alpha}.
        </>
      ) : (
        <>
          <strong>No drift.</strong> The mean is {num(d.current_mean)} against {num(d.baseline_mean)}
          {d.ci && ` (difference ${num(d.delta)}, 95 % interval ${num(d.ci[0])} to ${num(d.ci[1])})`}; {reasons.join(", and ") || "within the thresholds"}.
        </>
      )}
    </p>
  );
}

function Bars({ f }: { f: FeatureDrift }) {
  const max = Math.max(...f.bins.flatMap((b) => [b.baseline, b.current]), 0.01);
  return (
    <div className="grid grid-cols-[minmax(5rem,auto)_1fr] items-center gap-x-3 gap-y-1.5 text-xs">
      {f.bins.map((b) => (
        <div key={b.label} className="contents">
          <span className="truncate text-right text-muted-foreground nums" title={b.label}>
            {b.label}
          </span>
          <span className="flex flex-col gap-0.5" aria-label={`${b.label}: baseline ${pct(b.baseline)}, this run ${pct(b.current)}`}>
            <span className="flex items-center gap-2">
              <span className="block h-2 bg-series" style={{ width: `${(b.baseline / max) * 100}%` }} />
              <span className="nums text-muted-foreground">{pct(b.baseline)}</span>
            </span>
            <span className="flex items-center gap-2">
              <span className="block h-2 bg-foreground" style={{ width: `${(b.current / max) * 100}%` }} />
              <span className="nums">{pct(b.current)}</span>
            </span>
          </span>
        </div>
      ))}
    </div>
  );
}

export function SuitePage() {
  const { key = "" } = useParams();
  const [sp, setSp] = useSearchParams();
  const run = sp.get("run");
  const qc = useQueryClient();
  const data = useQuery({ queryKey: ["suite", key, run], queryFn: ({ signal }) => evalApi.suite(key, run, signal) });
  const dd = useQuery({ queryKey: ["data-drift", key, run], queryFn: ({ signal }) => evalApi.dataDrift(key, run, signal) });
  const d = data.data;
  useTitle(d ? `Drift of ${d.label}` : "Drift");
  useCrumbs([{ label: "Drift", to: "/drift" }, { label: d?.label ?? "Suite", mono: true }]);
  const [scoreSel, setScore] = useState<string | null>(null);
  const [flips, setFlips] = useState<"regressed" | "fixed">("regressed");
  const score = scoreSel ?? d?.scores[0] ?? null;
  const select = useMemo(() => (r: string) => setSp((p) => (p.set("run", r), p), { replace: true }), [setSp]);
  const baseline = useMutation({
    mutationFn: (b: Baseline) => evalApi.setBaseline(key, b),
    onSuccess: (b) => {
      toast.success(b.mode === "pinned" ? `Baseline pinned to ${b.runs.length} ${b.runs.length === 1 ? "run" : "runs"}` : `Baseline: the last ${b.window} runs that did not drift`);
      qc.invalidateQueries({ queryKey: ["suite", key] });
      qc.invalidateQueries({ queryKey: ["data-drift", key] });
      qc.invalidateQueries({ queryKey: ["suites"] });
    },
    onError: (e) => toast.error((e as Error).message),
  });
  if (data.isError) return <ErrorState error={data.error as Error} onRetry={() => data.refetch()} />;
  const verdict = d?.drift.find((x) => x.score === score);
  const selectedRun = d?.runs.find((r) => r.run_id === d.selected);
  const points = (d && score && d.history[score]) || [];
  const selIdx = points.findIndex((p) => p.run_id === d?.selected);
  const sel = selIdx >= 0 ? points[selIdx] : points.at(-1);
  // Runs in a row, up to the selected one, that drifted.
  let streak = 0;
  for (let i = (selIdx >= 0 ? selIdx : points.length - 1); i >= 0 && points[i].drifted; i--) streak++;
  const drifting = d?.drift.filter((x) => x.drifted) ?? [];
  const lastRun = d?.runs.at(-1);

  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <PageHeader
        title={d?.label ?? " "}
        actions={
          d?.selected && (
            <>
              <Button asChild variant="outline">
                <Link to="/alerts">Alerts</Link>
              </Button>
              <Button asChild variant="brand">
                <Link to={`/runs/${d.selected}?tab=items`}>{verdict && verdict.regressions.length ? `Review ${verdict.regressions.length} items` : "Open the run"}</Link>
              </Button>
            </>
          )
        }
      >
        {d ? (
          <>
            {d.schedule ? (
              <span>
                Scheduled by <b className="font-mono">{d.schedule}</b>
              </span>
            ) : (
              <span>Run by hand</span>
            )}
            {d.dataset && (
              <span>
                Dataset <b className="font-mono">{d.dataset}</b>
              </span>
            )}
            <span>
              <b>{d.runs.length}</b> {d.runs.length === 1 ? "run" : "runs"}
            </span>
            {lastRun && (
              <span>
                Last run <b>{dateTime(lastRun.created_at)}</b>
              </span>
            )}
          </>
        ) : (
          <span> </span>
        )}
      </PageHeader>

      {d && verdict && sel && (
        <Insight
          tone={verdict.drifted ? "bad" : "good"}
          tag={verdict.drifted ? `Drift on ${shortDate(sel.created_at)}` : "Within baseline"}
          action={verdict.drifted ? <Link to="/alerts">Open alerts</Link> : undefined}
        >
          {verdict.drifted ? (
            <>
              <span className="font-mono">{verdict.score}</span> {verdict.delta < 0 ? "fell" : "rose"} from {num(verdict.baseline_mean)} on the baseline to <b>{num(verdict.current_mean)}</b>.{" "}
              {verdict.regressions.length > 0 && `${verdict.regressions.length} items went from pass to fail.`}
            </>
          ) : (
            <>
              <span className="font-mono">{verdict.score}</span> is {num(verdict.current_mean)} against {num(verdict.baseline_mean)} on the baseline.
            </>
          )}
        </Insight>
      )}

      {d && verdict && (
        <Figures label="The selected run against its baseline">
          <Figure
            label={verdict.score.replace(/_/g, " ")}
            value={num(verdict.current_mean)}
            note={
              <>
                <b className={verdict.drifted ? "text-danger" : verdict.delta > 0 === verdict.higher_is_better ? "text-brand" : "text-foreground"}>{signed(verdict.delta)}</b> against baseline {num(verdict.baseline_mean)}
              </>
            }
          />
          <Figure label="Pass → fail" value={verdict.regressions.length} tone={verdict.regressions.length ? "bad" : undefined} note={`Fail → pass: ${verdict.fixed.length}`} />
          <Figure label="Scores drifting" value={drifting.length} unit={`of ${d.scores.length}`} note={drifting.length ? drifting.map((x) => x.score).join(", ") : "None"} tone={drifting.length ? "bad" : undefined} />
          <Figure label="Drifting runs in a row" value={streak} unit={streak === 1 ? "run" : "runs"} note={streak ? "No recovery yet" : "The selected run is within range"} />
        </Figures>
      )}

      <SectionHead
        id="beh-h"
        title={score ? `${score.replace(/_/g, " ")[0].toUpperCase()}${score.replace(/_/g, " ").slice(1)}, per run` : "Behaviour"}
        tools={d && d.scores.length > 1 && score ? <ToolSwitch label="Score" options={d.scores.map((s) => [s, s])} value={score} onChange={setScore} /> : undefined}
      />
      <div className="grid gap-x-6 lg:grid-cols-[200px_minmax(0,1fr)]">
        <div className="pt-5 max-lg:hidden">
          <MarginNote title="Baseline">
            {d?.baseline.mode === "pinned"
              ? `Pinned to ${d.baseline.runs.length} ${d.baseline.runs.length === 1 ? "run" : "runs"}. The band is its mean ± 2 standard deviations.`
              : `The last ${d?.baseline.window ?? 7} runs before each run that did not drift. The band is their mean ± 2 standard deviations.`}
          </MarginNote>
          <MarginNote title="Reading the chart">Red squares are runs that drifted. Click a run, or focus the chart and use the arrow keys, to inspect it.</MarginNote>
        </div>
        <figure className="min-w-0 pt-5">
          {d && score ? (
            points.length ? (
              <LineChart
                height={300}
                lines={[
                  {
                    id: "v",
                    name: score,
                    color: "var(--foreground)",
                    width: 1.5,
                    markers: true,
                    markerColor: (i) => (points[i].drifted ? "var(--danger)" : undefined),
                    points: points.map((p, i) => [i, p.value]),
                  },
                ]}
                band={points.map((p, i) => ({ x: i, lo: p.baseline_low, hi: p.baseline_high }))}
                bandLabel="Baseline mean ± 2 standard deviations"
                marker={selIdx >= 0 && selIdx < points.length - 1 ? { x: selIdx, label: "Selected run", sub: dateTime(points[selIdx].created_at) } : null}
                xFormat={(i) => (points[Math.round(i)] ? shortDate(points[Math.round(i)].created_at) : "")}
                xTicks={points.length > 1 ? Array.from(new Set([0, ...Array.from({ length: 5 }, (_, k) => Math.round(((k + 1) * (points.length - 1)) / 5))])) : [0]}
                selected={selIdx >= 0 ? selIdx : null}
                onSelect={(i) => points[i] && select(points[i].run_id)}
                label={`${score} per run over time with the baseline band; drifted runs are red. Left and right arrows select a run.`}
              />
            ) : (
              <p className="py-10 text-muted-foreground">No runs with this score yet.</p>
            )
          ) : (
            <Skeleton className="h-[300px]" />
          )}
          <Caption n={1} right={sel ? `${dateTime(sel.created_at)}: ${num(sel.value)}, baseline ${num(sel.baseline_mean)}` : undefined}>
            {score} on each run of the suite{points.length ? `, ${points.length} runs` : ""}.
          </Caption>
        </figure>
      </div>

      {d && d.scores.length > 0 && (
        <>
          <SectionHead title="By score" tools={<span>Newest {STRIP} runs, oldest left</span>} />
          <div className="grid gap-x-6 lg:grid-cols-[200px_minmax(0,1fr)]">
            <div className="pt-4 max-lg:hidden">
              <MarginNote title="Reading the strips">One cell per run. Red means the run drifted on that score; darker is further below its own baseline. Grey is within range.</MarginNote>
            </div>
            <ul className="pt-1">
              {d.scores.map((s) => {
                const pts = d.history[s] ?? [];
                const last = pts.at(-1);
                const delta = last && last.value !== null && last.baseline_mean !== null ? last.value - last.baseline_mean : null;
                return (
                  <li key={s} className="grid grid-cols-[minmax(6rem,10rem)_minmax(0,1fr)_4.5rem] items-center gap-3 border-b py-2 sm:grid-cols-[200px_minmax(0,1fr)_5rem]">
                    <button type="button" onClick={() => setScore(s)} className={cn("min-h-6 truncate text-left font-mono text-[13px] hover:underline", s === score && "font-bold")}>
                      {s}
                    </button>
                    <Strip points={pts} />
                    <span className={cn("text-right font-mono text-[13px] nums", last?.drifted && "font-bold text-danger")}>{delta === null ? "–" : signed(delta)}</span>
                  </li>
                );
              })}
            </ul>
          </div>
        </>
      )}

      {d && verdict && (
        <>
          <SectionHead
            title="Items that flipped"
            tools={
              <ToolSwitch
                label="Direction"
                options={[
                  ["regressed", `Pass → fail ${verdict.regressions.length}`],
                  ["fixed", `Fail → pass ${verdict.fixed.length}`],
                ]}
                value={flips}
                onChange={setFlips}
              />
            }
          />
          <div className="grid gap-x-6 lg:grid-cols-[200px_minmax(0,1fr)]">
            <div className="pt-4 max-lg:hidden">
              <MarginNote title="Compared with">The last baseline run: items that passed there and fail in the selected run, or the other way round.</MarginNote>
            </div>
            <div className="min-w-0 overflow-x-auto">
              {(flips === "regressed" ? verdict.regressions.length : verdict.fixed.length) === 0 ? (
                <p className="py-4 text-sm text-muted-foreground">{flips === "regressed" ? "No item that passed in the last baseline run fails now." : "No item that failed in the last baseline run passes now."}</p>
              ) : (
                <table className="w-full min-w-[36rem] border-collapse text-sm">
                  <thead>
                    <tr className="text-left">
                      <th scope="col" className="border-b px-3 py-2.5">
                        Item
                      </th>
                      <th scope="col" className="border-b px-3 py-2.5">
                        Input
                      </th>
                      {flips === "regressed" && (
                        <th scope="col" className="border-b px-3 py-2.5 text-right">
                          {verdict.score}
                        </th>
                      )}
                      <th scope="col" className="border-b px-3 py-2.5">
                        Now
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {(flips === "regressed" ? verdict.regressions : verdict.fixed.map((f) => ({ ...f, before: null, after: null }))).slice(0, 200).map((f, i) => (
                      <tr key={f.item_id} className="hover:bg-hover">
                        <td className="border-b px-3 py-2.5">
                          <Link to={`/runs/${d.selected}?tab=items&item=${encodeURIComponent(f.item_id)}`} className="inline-flex min-h-6 items-center font-mono text-[12.5px] text-faint hover:text-foreground hover:underline">
                            {f.item_id}
                          </Link>
                        </td>
                        <td className="max-w-md border-b px-3 py-2.5">
                          <span className="line-clamp-2 break-words">{flips === "regressed" ? jsonText(verdict.regression_inputs[i]) : "–"}</span>
                        </td>
                        {flips === "regressed" && (
                          <td className="whitespace-nowrap border-b px-3 py-2.5 text-right font-mono text-[13px] nums">
                            {num(f.before)} → {num(f.after)}
                          </td>
                        )}
                        <td className={cn("border-b px-3 py-2.5 font-bold", flips === "regressed" ? "text-danger" : "text-brand")}>{flips === "regressed" ? "Fail" : "Pass"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>
          </div>

          <SectionHead id="ver-h" title={`Verdict for the run of ${selectedRun ? dateTime(selectedRun.created_at) : "–"}`} />
          <div className="grid gap-x-6 lg:grid-cols-[200px_minmax(0,1fr)]">
            <div className="pt-4 max-lg:hidden">
              <MarginNote title="Thresholds">
                Drift needs a worse score by at least {d.config.min_effect} with p below {d.config.alpha}. Set in the server configuration.
              </MarginNote>
            </div>
            <div className="flex max-w-3xl flex-col gap-3 pt-4">
              <Explain d={verdict} cfg={d.config} />
              <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
                <dt className="text-faint">Items</dt>
                <dd className="nums">
                  {verdict.paired
                    ? `${integer(verdict.n_current)} items, each compared with its own mean over ${verdict.baseline_runs.length} baseline runs`
                    : `${integer(verdict.n_current)} in this run against ${integer(verdict.n_baseline)} per-item baseline means (the items changed, so they cannot be paired)`}
                </dd>
                <dt className="text-faint">Test</dt>
                <dd className="nums">
                  {verdict.test ? `${verdict.test.name === "wilcoxon signed-rank" ? "Wilcoxon signed-rank" : "Mann–Whitney U"}, z = ${num(verdict.test.z)}, p = ${pValue(verdict.test.p)}` : "No item changed"}
                </dd>
                <dt className="text-faint">Baseline</dt>
                <dd>
                  {d.baseline.mode === "rolling"
                    ? `The last ${d.baseline.window} finished runs before each run that did not drift themselves, so a lasting drop keeps being reported until you pin a new baseline.`
                    : `Pinned to ${d.baseline.runs.length} ${d.baseline.runs.length === 1 ? "run" : "runs"}.`}
                </dd>
              </dl>
              <div className="flex flex-wrap gap-2">
                <Button variant="outline" loading={baseline.isPending} onClick={() => baseline.mutate({ mode: "pinned", window: d.baseline.window, runs: [d.selected!], updated_at: 0 })}>
                  <Pin /> Pin this run as the baseline
                </Button>
                <Button variant="outline" loading={baseline.isPending} onClick={() => baseline.mutate({ mode: "pinned", window: d.baseline.window, runs: verdict.baseline_runs, updated_at: 0 })}>
                  Pin the current baseline runs
                </Button>
                {d.baseline.mode === "pinned" && (
                  <Button variant="ghost" loading={baseline.isPending} onClick={() => baseline.mutate({ mode: "rolling", window: 7, runs: [], updated_at: 0 })}>
                    Back to rolling
                  </Button>
                )}
              </div>
            </div>
          </div>
        </>
      )}

      <SectionHead id="data-h" title="Inputs" tools={<span>Grey: baseline share of each bin; ink: the selected run</span>} />
      {dd.data && dd.data.features.length === 0 && <p className="py-4 text-sm text-muted-foreground">This run logged no input features. Pass features= to evaluate() to monitor them.</p>}
      <div className="grid gap-x-8 lg:grid-cols-2 2xl:grid-cols-3">
        {dd.data?.features.map((f) => (
          <article key={f.feature} className="border-b py-4">
            <header className="mb-3 flex flex-wrap items-baseline gap-x-3 gap-y-1">
              <h3 className="font-mono text-[13px] font-bold">{f.feature}</h3>
              <LevelBadge level={f.level} />
              <span className="ml-auto text-xs text-faint nums">
                {f.note
                  ? f.note
                  : f.kind === "numeric"
                    ? `PSI ${num(f.psi)}, KS D ${num(f.ks?.d)} (asymptotic p ${pValue(f.ks?.p)})`
                    : `JS ${num(f.js)}, χ² p ${pValue(f.chi_squared?.p)}`}
              </span>
            </header>
            {f.note ? <p className="text-sm text-muted-foreground">{f.note === "no finite values in this run" ? "Every value of this feature was missing, infinite or not a number in this run." : "The baseline runs have no finite values of this feature to compare with."}</p> : <Bars f={f} />}
            <p className="mt-2 text-xs text-faint nums">
              {integer(f.n_current)} values in this run, {integer(f.n_baseline)} in the baseline.
            </p>
          </article>
        ))}
        {dd.isPending && <Skeleton className="mt-4 h-48" />}
      </div>

      <section className="mt-8 flex max-w-4xl flex-col border-t-[1.5px] border-rule">
        <Method title="How behaviour drift is decided">
          <p>
            Each item’s score in the run is compared with the same item’s mean score over the baseline runs (items match by id and content). The baseline runs repeat the same items, so they are
            averaged per item rather than counted as independent samples.
          </p>
          <p>
            The p-value comes from a Wilcoxon signed-rank test on those per-item differences, which assumes no particular distribution. The 95 % interval of the mean difference resamples items
            (1 000 bootstrap resamples). When fewer than 10 items match, for example after the dataset changed, the run is compared with the baseline’s per-item means by a Mann–Whitney U test
            instead.
          </p>
          <p>
            A run drifts when it is worse than the baseline, the difference is at least {d?.config.min_effect ?? 0.02} and p is below {d?.config.alpha ?? 0.01}. Both thresholds are set in the server
            configuration (<code>[drift]</code>).
          </p>
        </Method>
        <Method title="How input drift is decided">
          <p>
            Numeric features: the population stability index (PSI) on the baseline’s deciles. Below {dd.data?.thresholds.psi_moderate ?? 0.1} is stable, up to {dd.data?.thresholds.psi_drift ?? 0.2}{" "}
            a moderate shift, above that drift. The Kolmogorov–Smirnov statistic and its asymptotic p-value are shown for reference.
          </p>
          <p>
            Categorical features: the Jensen–Shannon divergence (base 2, from 0 for identical to 1 for disjoint). Below {dd.data?.thresholds.js_moderate ?? 0.05} is stable, up to{" "}
            {dd.data?.thresholds.js_drift ?? 0.1} moderate, above that drift. A chi-squared test is shown for reference.
          </p>
          <p>With thousands of samples every test reports a tiny p-value, so the size of the shift decides, not significance.</p>
        </Method>
      </section>
    </div>
  );
}
