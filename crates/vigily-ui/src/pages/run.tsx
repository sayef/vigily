import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Archive, ArchiveRestore, Check, CircleSlash, Copy, GitCompare, Plus, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";
import { ChartControls } from "@/components/chart-controls";
import { type ChartSettings, MetricChart } from "@/components/metric-chart";
import { AuditTab, ItemsTab } from "@/components/eval-run";
import { ErrorState } from "@/components/states";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip } from "@/components/ui/tooltip";
import { ApiError, type Json, type RunDetail, type SeriesEntry, SYSTEM, api, isSystem, targetText, traceApi } from "@/lib/api";
import { ago, duration, higherIsBetter, integer, metricLabel, num, numFull, utc, value } from "@/lib/format";
import { useCrumbs } from "@/lib/crumbs";
import { useTitle } from "@/lib/hooks";
import { useSelection } from "@/lib/selection";
import { cn, modKey } from "@/lib/utils";

const TABS = ["items", "audit", "overview", "params", "metrics", "system", "notes"] as const;
type Tab = (typeof TABS)[number];

export function RunPage() {
  const { id = "" } = useParams();
  const [sp, setSp] = useSearchParams();
  const requested = (TABS as readonly string[]).includes(sp.get("tab") ?? "") ? (sp.get("tab") as Tab) : null;
  const run = useQuery({
    queryKey: ["run", id],
    queryFn: ({ signal }) => api.run(id, signal),
    refetchInterval: (q) => (q.state.data?.status === "running" ? 5_000 : false),
    retry: (n, e) => !(e instanceof ApiError && e.status === 404) && n < 2,
  });
  useTitle(run.data?.name ?? "Run");
  const exp = run.data?.experiment;
  useCrumbs([{ label: "Runs", to: "/" }, ...(exp ? [{ label: exp, to: `/?q=${encodeURIComponent(`exp="${exp}"`)}`, mono: true }] : []), { label: run.data?.name ?? "Run", mono: true }]);

  if (run.isPending) return <RunSkeleton />;
  if (run.isError) {
    const e = run.error as Error;
    if (e instanceof ApiError && e.status === 404)
      return (
        <div className="mx-auto max-w-xl px-4 py-12">
          <h1 className="text-xl font-bold tracking-[-0.02em]">No run with this id</h1>
          <p className="mt-2 text-muted-foreground">
            <code className="break-all">{id}</code> is not in this repository. It may have been logged to another one.
          </p>
          <Button asChild className="mt-4">
            <Link to="/">Back to runs</Link>
          </Button>
        </div>
      );
    return <ErrorState error={e} onRetry={() => run.refetch()} />;
  }
  const r = run.data;
  const isEval = r.kind === "eval";
  const tab: Tab = requested && (isEval || !["items", "audit"].includes(requested)) ? requested : "overview";
  return (
    <div className="px-4 pb-10 pt-4 md:px-6">
      <Header run={r} />
      <Tabs value={tab} onValueChange={(t) => setSp((p) => (t === "overview" ? (p.delete("tab"), p) : (p.set("tab", t), p)), { replace: true })} className="mt-4">
        <TabsList aria-label="Run sections">
          <TabsTrigger value="overview">Overview</TabsTrigger>
          {isEval && <TabsTrigger value="items">Items</TabsTrigger>}
          {isEval && <TabsTrigger value="audit">Audit</TabsTrigger>}
          <TabsTrigger value="params">Parameters</TabsTrigger>
          <TabsTrigger value="metrics">Metrics</TabsTrigger>
          <TabsTrigger value="system">System</TabsTrigger>
          <TabsTrigger value="notes">Notes</TabsTrigger>
        </TabsList>
        <TabsContent value="overview" className="pt-4 outline-none">
          <Overview run={r} />
        </TabsContent>
        {isEval && (
          <TabsContent value="items" className="pt-4 outline-none">
            <ItemsTab run={r} />
          </TabsContent>
        )}
        {isEval && (
          <TabsContent value="audit" className="pt-4 outline-none">
            <AuditTab run={r} />
          </TabsContent>
        )}
        <TabsContent value="params" className="pt-4 outline-none">
          <Params tree={r.params_tree} />
        </TabsContent>
        <TabsContent value="metrics" className="pt-4 outline-none">
          <Charts run={r} series={r.series.filter((s) => !isSystem(s.name))} empty="This run has not logged any metrics yet." />
        </TabsContent>
        <TabsContent value="system" className="pt-4 outline-none">
          <System run={r} />
        </TabsContent>
        <TabsContent value="notes" className="pt-4 outline-none">
          <Notes run={r} />
        </TabsContent>
      </Tabs>
    </div>
  );
}

/** For a running run whose process stopped: end it as aborted, after confirmation. */
function MarkAborted({ run }: { run: RunDetail }) {
  const qc = useQueryClient();
  const [confirmAbort, setConfirmAbort] = useState(false);
  const abort = useMutation({
    mutationFn: () => api.patchRun(run.id, { status: "aborted" }),
    onSuccess: () => {
      setConfirmAbort(false);
      void qc.invalidateQueries();
      toast.success("Run marked aborted. It can be deleted now.");
    },
    onError: (e) => toast.error((e as Error).message),
  });
  return (
    <>
      <Button type="button" variant="outline" size="sm" onClick={() => setConfirmAbort(true)}>
        <CircleSlash /> Mark as aborted
      </Button>
      <Dialog open={confirmAbort} onOpenChange={setConfirmAbort}>
        <DialogContent className="max-w-md p-5">
          <DialogTitle className="text-lg font-semibold">Mark this run as aborted?</DialogTitle>
          <DialogDescription className="mt-2 text-sm text-muted-foreground">
            Nothing has been heard from its process since {ago(run.last_seen)}, so it most likely stopped. The run ends at that time with the status aborted. If the process is in fact alive, it can still finish the run later.
          </DialogDescription>
          <div className="mt-5 flex justify-end gap-2">
            <DialogClose asChild>
              <Button variant="secondary">Cancel</Button>
            </DialogClose>
            <Button variant="brand" loading={abort.isPending} onClick={() => abort.mutate()}>
              <CircleSlash /> Mark as aborted
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}

function Header({ run }: { run: RunDetail }) {
  const selection = useSelection();
  const navigate = useNavigate();
  const selected = selection.has(run.id);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(run.id);
      toast.success("Run id copied");
    } catch {
      toast.error("Could not copy. Select the id and copy it by hand.");
    }
  };
  return (
    <header>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <h1 className={cn("min-w-0 break-words text-xl font-bold tracking-[-0.02em]", !run.has_name && "font-mono")}>{run.name}</h1>
        <StatusBadge status={run.status} stale={run.stale} lastSeen={run.last_seen} />
        {run.stale && <MarkAborted run={run} />}
        {run.archived && <span className="rounded-sm bg-muted px-1.5 text-xs text-muted-foreground">archived</span>}
        <div className="flex w-full gap-2 sm:ml-auto sm:w-auto">
          <Button variant={selected ? "secondary" : "outline"} onClick={() => selection.toggle(run.id)} aria-pressed={selected}>
            {selected ? <Check /> : <Plus />} {selected ? "Selected" : "Select for comparison"}
          </Button>
          <Button
            variant="brand"
            onClick={() => {
              const ids = selected ? selection.ids : [...selection.ids, run.id];
              if (!selected) selection.toggle(run.id);
              navigate(`/metrics/compare?runs=${ids.join(",")}`);
            }}
          >
            <GitCompare /> Compare{selection.ids.length > (selected ? 1 : 0) ? ` ${selected ? selection.ids.length : selection.ids.length + 1}` : ""}
          </Button>
        </div>
      </div>
      <dl className="mt-2 flex flex-wrap gap-x-6 gap-y-1 border-b-[1.5px] border-rule pb-4 text-sm">
        {run.kind !== "training" && (
          <Fact label="Kind">
            <span>{run.kind}</span>
          </Fact>
        )}
        {run.target && (
          <Fact label="Target">
            <span className="break-all">{targetText(run.target)}</span>
          </Fact>
        )}
        <Fact label="Created">
          <Tooltip content={utc(run.created_at)}>
            <span>{ago(run.created_at)}</span>
          </Tooltip>
        </Fact>
        <Fact label="Duration">
          <span className="nums">{duration(run.duration)}</span>
          {run.status === "running" && <span className="text-muted-foreground">{"\u00a0"}so far</span>}
        </Fact>
        {run.ended_at && (
          <Fact label="Ended">
            <Tooltip content={utc(run.ended_at)}>
              <span>{ago(run.ended_at)}</span>
            </Tooltip>
          </Fact>
        )}
        <Fact label="Id">
          <button type="button" onClick={copy} className="inline-flex min-h-6 items-center gap-1 rounded-sm font-mono text-[12.5px] hover:text-brand" aria-label={`Copy run id ${run.id}`}>
            {run.id}
            <Copy className="size-3.5" aria-hidden />
          </button>
        </Fact>
        <TracesFact run={run.id} />
        {run.tags.length > 0 && (
          <Fact label="Tags">
            <span className="flex flex-wrap gap-1">
              {run.tags.map((t) => (
                <Link key={t} to={`/?q=${encodeURIComponent(`tag="${t}"`)}`} className="inline-flex min-h-6 items-center rounded-sm bg-muted px-1.5 text-xs hover:bg-hover">
                  {t}
                </Link>
              ))}
            </span>
          </Fact>
        )}
      </dl>
    </header>
  );
}

/** A link to the run's traces, shown only when it has any. */
function TracesFact({ run }: { run: string }) {
  const traces = useQuery({ queryKey: ["traces", "run", run], queryFn: ({ signal }) => traceApi.list({ run, limit: 1 }, signal), retry: false });
  const n = traces.data?.total ?? 0;
  if (!n) return null;
  return (
    <div className="flex items-baseline">
      <Link to={`/traces?run=${encodeURIComponent(run)}`} className="inline-flex min-h-6 items-center font-semibold text-brand hover:underline">
        Traces ({integer(n)})
      </Link>
    </div>
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

function bestOf(s: SeriesEntry) {
  const hi = higherIsBetter(s.name);
  return { v: hi ? s.summary.max : s.summary.min, step: hi ? s.summary.max_step : s.summary.min_step, hi };
}

function Overview({ run }: { run: RunDetail }) {
  const series = run.series.filter((s) => !isSystem(s.name));
  const params = Object.entries(run.params).filter(([k]) => !isSystem(k));
  return (
    <div className="flex flex-col gap-6">
      {run.description && <p className="max-w-prose whitespace-pre-wrap text-base">{run.description}</p>}
      <div className="grid gap-6 xl:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <section aria-labelledby="results-h" className="min-w-0">
          <h2 id="results-h" className="mb-3 mt-6 border-b-[1.5px] border-rule pb-2.5 text-[17px] font-bold tracking-[-0.01em]">
            Results
          </h2>
          {series.length === 0 ? (
            <p className="text-sm text-muted-foreground">No metrics logged yet.</p>
          ) : (
            <div className="relative overflow-x-auto border-y">
              <table className="w-full whitespace-nowrap text-sm">
                <thead>
                  <tr className="text-left text-muted-foreground">
                    <th scope="col" className="h-9 border-b px-3 font-medium">
                      Metric
                    </th>
                    <th scope="col" className="h-9 border-b px-3 text-right font-medium">
                      Last
                    </th>
                    <th scope="col" className="h-9 border-b px-3 text-right font-medium">
                      Min
                    </th>
                    <th scope="col" className="h-9 border-b px-3 text-right font-medium">
                      Max
                    </th>
                    <th scope="col" className="hidden h-9 border-b px-3 text-right font-medium sm:table-cell">
                      Points
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {series.map((s) => {
                    const l = metricLabel(s.key);
                    const b = bestOf(s);
                    return (
                      <tr key={s.id} className="hover:bg-muted">
                        <th scope="row" className="h-8 border-b px-3 text-left font-medium">
                          {l.name}
                          {l.context && <span className="ml-1.5 font-normal text-muted-foreground">{l.context}</span>}
                          {s.summary.non_finite > 0 && (
                            <Tooltip content={`${s.summary.non_finite} values were NaN or infinite`}>
                              <span className="ml-2 rounded-sm bg-muted px-1 text-xs font-normal text-warning">NaN</span>
                            </Tooltip>
                          )}
                        </th>
                        <td className="h-8 border-b px-3 text-right nums" title={numFull(s.summary.last)}>
                          {num(s.summary.last)}
                        </td>
                        <td className={cn("h-8 border-b px-3 text-right nums", !b.hi && "font-semibold")} title={`${numFull(s.summary.min)} at step ${s.summary.min_step ?? "–"}`}>
                          {num(s.summary.min)}
                        </td>
                        <td className={cn("h-8 border-b px-3 text-right nums", b.hi && "font-semibold")} title={`${numFull(s.summary.max)} at step ${s.summary.max_step ?? "–"}`}>
                          {num(s.summary.max)}
                        </td>
                        <td className="hidden h-8 border-b px-3 text-right text-muted-foreground nums sm:table-cell">{integer(s.summary.count)}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </section>
        <section aria-labelledby="params-h" className="min-w-0">
          <h2 id="params-h" className="mb-3 mt-6 border-b-[1.5px] border-rule pb-2.5 text-[17px] font-bold tracking-[-0.01em]">
            Parameters
          </h2>
          {params.length === 0 ? (
            <p className="text-sm text-muted-foreground">No parameters set. Set them with <code>run["hparams"] = {"{…}"}</code>.</p>
          ) : (
            <dl className="grid grid-cols-[minmax(0,auto)_minmax(0,1fr)] gap-x-4 border-y px-3 py-1.5 text-sm">
              {params.slice(0, 14).map(([k, v]) => (
                <div key={k} className="contents">
                  <dt className="truncate py-1 font-mono text-[12.5px] text-muted-foreground" title={k}>
                    {k}
                  </dt>
                  <dd className="truncate py-1 text-right nums" title={value(v)}>
                    {value(v)}
                  </dd>
                </div>
              ))}
            </dl>
          )}
          {params.length > 14 && (
            <Link to="?tab=params" className="mt-2 inline-block py-1 text-sm text-brand hover:underline">
              All {params.length} parameters
            </Link>
          )}
        </section>
      </div>
      {series.length > 0 && (
        <section aria-labelledby="charts-h">
          <h2 id="charts-h" className="mb-3 mt-6 border-b-[1.5px] border-rule pb-2.5 text-[17px] font-bold tracking-[-0.01em]">
            Curves
          </h2>
          <div className="grid gap-3 lg:grid-cols-2">
            {series.slice(0, 4).map((s) => (
              <MetricChart key={s.id} metric={s.key} runs={[run.id]} x="step" logY={false} smoothing={0} groupBy={null} aggregate={false} ignoreOutliers height={200} syncKey={`run-${run.id}`} expandTo={`/metrics?runs=${run.id}&m=${encodeURIComponent(s.key)}`} />
            ))}
          </div>
          {series.length > 4 && (
            <Link to="?tab=metrics" className="mt-2 inline-block py-1 text-sm text-brand hover:underline">
              All {series.length} metrics
            </Link>
          )}
        </section>
      )}
    </div>
  );
}

function flat(prefix: string, v: Json, out: [string, Json][]) {
  if (v !== null && typeof v === "object" && !Array.isArray(v) && Object.keys(v).length) {
    for (const [k, x] of Object.entries(v)) flat(prefix ? `${prefix}.${k}` : k, x, out);
  } else out.push([prefix, v]);
}

function Params({ tree }: { tree: Record<string, Json> }) {
  const [filter, setFilter] = useState("");
  const user = useMemo(() => Object.fromEntries(Object.entries(tree).filter(([k]) => k !== SYSTEM)), [tree]);
  const rows = useMemo(() => {
    const out: [string, Json][] = [];
    flat("", user, out);
    return out;
  }, [user]);
  const f = filter.trim().toLowerCase();
  const shown = rows.filter(([k, v]) => !f || k.toLowerCase().includes(f) || value(v).toLowerCase().includes(f));
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(user, null, 2));
      toast.success("Parameters copied as JSON");
    } catch {
      toast.error("Could not copy to the clipboard.");
    }
  };
  if (!rows.length) return <p className="text-sm text-muted-foreground">No parameters set.</p>;
  return (
    <div className="max-w-3xl">
      <div className="mb-2 flex gap-2">
        <Input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Find a parameter or value…" aria-label="Find a parameter" className="max-w-sm" />
        <Button variant="outline" onClick={copy}>
          <Copy /> Copy as JSON
        </Button>
      </div>
      <div className="relative overflow-x-auto border-y">
        <table className="w-full text-sm">
          <caption className="sr-only">Parameters</caption>
          <tbody>
            {shown.map(([k, v]) => {
              const parts = k.split(".");
              return (
                <tr key={k} className="hover:bg-muted">
                  <th scope="row" className="h-8 border-b px-3 text-left font-normal">
                    <span className="font-mono text-[12.5px]">
                      {parts.length > 1 && <span className="text-muted-foreground">{parts.slice(0, -1).join(".")}.</span>}
                      <span className="text-foreground">{parts.at(-1)}</span>
                    </span>
                  </th>
                  <td className="h-8 break-all border-b px-3 text-right nums" title={typeof v === "number" ? numFull(v) : undefined}>
                    {typeof v === "string" ? v : value(v)}
                  </td>
                </tr>
              );
            })}
            {!shown.length && (
              <tr>
                <td colSpan={2} className="px-3 py-4 text-muted-foreground">
                  No parameter matches “{filter}”.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function Charts({ run, series, empty }: { run: RunDetail; series: SeriesEntry[]; empty: string }) {
  const [s, setS] = useState<ChartSettings>({ x: "step", logY: false, smoothing: 0, groupBy: null, aggregate: false, ignoreOutliers: true });
  if (!series.length) return <p className="text-sm text-muted-foreground">{empty}</p>;
  return (
    <div className="flex flex-col gap-4 lg:flex-row lg:items-start">
      <ChartControls value={s} onChange={(p) => setS((o) => ({ ...o, ...p }))} grouping={false} className="border-y p-3 lg:sticky lg:top-4 lg:w-60 lg:shrink-0" />
      <div className="grid min-w-0 flex-1 gap-3 xl:grid-cols-2">
        {series.map((x) => (
          <MetricChart key={x.id} metric={x.key} runs={[run.id]} {...s} height={220} syncKey={`run-${run.id}`} expandTo={`/metrics?runs=${run.id}&m=${encodeURIComponent(x.key)}`} />
        ))}
      </div>
    </div>
  );
}

function System({ run }: { run: RunDetail }) {
  const env = run.params_tree[SYSTEM];
  const rows: [string, Json][] = [];
  if (env) flat("", env, rows);
  const series = run.series.filter((s) => isSystem(s.name));
  return (
    <div className="flex flex-col gap-6">
      <section aria-labelledby="env-h" className="max-w-2xl">
        <h2 id="env-h" className="mb-3 mt-6 border-b-[1.5px] border-rule pb-2.5 text-[17px] font-bold tracking-[-0.01em]">
          Environment
        </h2>
        {rows.length === 0 ? (
          <p className="text-sm text-muted-foreground">The SDK did not record the environment for this run.</p>
        ) : (
          <dl className="grid grid-cols-[minmax(0,auto)_minmax(0,1fr)] gap-x-6 border-y px-3 py-1.5 text-sm">
            {rows.map(([k, v]) => (
              <div key={k} className="contents">
                <dt className="py-1 text-muted-foreground">{k.replace(/_/g, " ").replace(/\./g, " ")}</dt>
                <dd className={cn("break-all py-1", /commit|platform/.test(k) && "font-mono text-[12.5px]")}>{value(v)}</dd>
              </div>
            ))}
          </dl>
        )}
      </section>
      <section aria-labelledby="sys-h">
        <h2 id="sys-h" className="mb-3 mt-6 border-b-[1.5px] border-rule pb-2.5 text-[17px] font-bold tracking-[-0.01em]">
          Resource use
        </h2>
        <Charts run={run} series={series} empty="This run has no system metrics. Metrics named __system__/… appear here." />
      </section>
    </div>
  );
}

function Notes({ run }: { run: RunDetail }) {
  const qc = useQueryClient();
  const [name, setName] = useState(run.has_name ? run.name : "");
  const [experiment, setExperiment] = useState(run.experiment ?? "");
  const [description, setDescription] = useState(run.description ?? "");
  const [tags, setTags] = useState<string[]>(run.tags);
  const [tag, setTag] = useState("");
  const dirty = name !== (run.has_name ? run.name : "") || experiment !== (run.experiment ?? "") || description !== (run.description ?? "") || tags.join("\u0000") !== run.tags.join("\u0000");

  useEffect(() => {
    if (!dirty) return;
    const warn = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [dirty]);

  const done = () => {
    qc.invalidateQueries({ queryKey: ["run", run.id] });
    qc.invalidateQueries({ queryKey: ["runs"] });
    qc.invalidateQueries({ queryKey: ["meta"] });
  };
  const save = useMutation({
    mutationFn: () => api.patchRun(run.id, { name, experiment, description, tags }),
    onSuccess: () => {
      done();
      toast.success("Changes saved");
    },
    onError: (e) => toast.error(`Not saved: ${(e as Error).message}`),
  });
  const archive = useMutation({
    mutationFn: (archived: boolean) => api.patchRun(run.id, { archived }),
    onSuccess: (_r, archived) => {
      done();
      toast.success(archived ? "Run archived. It is hidden from the runs table." : "Run restored", {
        action: archived ? { label: "Undo", onClick: () => archive.mutate(false) } : undefined,
      });
    },
    onError: (e) => toast.error((e as Error).message),
  });
  const addTag = () => {
    const t = tag.trim();
    if (t && !tags.includes(t)) setTags([...tags, t]);
    setTag("");
  };

  return (
    <form
      className="flex max-w-2xl flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
      onKeyDown={(e) => {
        if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
          e.preventDefault();
          if (dirty) save.mutate();
        }
      }}
    >
      <div className="grid gap-4 sm:grid-cols-2">
        <label className="flex flex-col gap-1">
          <span className="text-xs font-medium text-muted-foreground">Name</span>
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={run.id.slice(-8)} name="name" autoComplete="off" />
        </label>
        <label className="flex flex-col gap-1">
          <span className="text-xs font-medium text-muted-foreground">Experiment</span>
          <Input value={experiment} onChange={(e) => setExperiment(e.target.value)} placeholder="No experiment" name="experiment" autoComplete="off" />
        </label>
      </div>
      <label className="flex flex-col gap-1">
        <span className="text-xs font-medium text-muted-foreground">Notes</span>
        <textarea
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          rows={6}
          name="description"
          placeholder="What this run tried, and what you learned from it…"
          className="rounded-md border border-input bg-card px-2.5 py-2 text-base placeholder:text-muted-foreground"
        />
      </label>
      <fieldset className="flex flex-col gap-1">
        <legend className="text-xs font-medium text-muted-foreground">Tags</legend>
        <div className="mt-1 flex flex-wrap items-center gap-1.5">
          {tags.map((t) => (
            <span key={t} className="inline-flex h-7 items-center gap-1 rounded-sm bg-muted pl-2 text-sm">
              {t}
              <button type="button" onClick={() => setTags(tags.filter((x) => x !== t))} aria-label={`Remove tag ${t}`} className="inline-flex size-7 items-center justify-center rounded-sm text-muted-foreground hover:text-foreground">
                <X className="size-3.5" />
              </button>
            </span>
          ))}
          <Input
            value={tag}
            onChange={(e) => setTag(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === ",") {
                e.preventDefault();
                addTag();
              }
            }}
            onBlur={addTag}
            placeholder="Add a tag…"
            aria-label="Add a tag"
            className="h-7 w-36"
          />
        </div>
      </fieldset>
      <div className="flex flex-wrap items-center gap-2 border-t pt-4">
        <Button type="submit" variant="brand" disabled={!dirty} loading={save.isPending}>
          Save changes
        </Button>
        <span className="text-xs text-muted-foreground">{dirty ? `${modKey} Enter saves` : "No unsaved changes"}</span>
        <Button type="button" variant="ghost" className="ml-auto" loading={archive.isPending} onClick={() => archive.mutate(!run.archived)}>
          {run.archived ? <ArchiveRestore /> : <Archive />} {run.archived ? "Restore run" : "Archive run"}
        </Button>
      </div>
    </form>
  );
}

function RunSkeleton() {
  return (
    <div className="px-4 pt-4 md:px-6" aria-busy aria-label="Loading the run">
      <Skeleton className="h-4 w-32" />
      <Skeleton className="mt-2 h-7 w-72" />
      <Skeleton className="mt-3 h-4 w-96 max-w-full" />
      <Skeleton className="mt-6 h-9 w-full max-w-xl" />
      <div className="mt-4 grid gap-6 xl:grid-cols-[3fr_2fr]">
        <Skeleton className="h-48" />
        <Skeleton className="h-48" />
      </div>
    </div>
  );
}
