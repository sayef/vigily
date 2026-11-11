import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Download, GitCompare, Search } from "lucide-react";
import { useMemo, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { toast } from "sonner";
import { JsonBlock, Sheet, Verdict, jsonText } from "@/components/evals";
import { ErrorState } from "@/components/states";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { type ItemRow, type Json, type RunDetail, api, evalApi, targetText } from "@/lib/api";
import { integer, num, utc } from "@/lib/format";
import { useDebounced } from "@/lib/hooks";
import { cn } from "@/lib/utils";

const PAGE = 200;

/** Per-item outputs and scores of an eval run. */
export function ItemsTab({ run }: { run: RunDetail }) {
  const navigate = useNavigate();
  const data = useQuery({ queryKey: ["items", run.id], queryFn: ({ signal }) => evalApi.items(run.id, signal) });
  const [failures, setFailures] = useState(false);
  const [text, setText] = useState("");
  const q = useDebounced(text.trim().toLowerCase(), 200);
  const [shown, setShown] = useState(PAGE);
  const [sp] = useSearchParams();
  const [open, setOpen] = useState<string | null>(sp.get("item"));
  const others = useQuery({
    queryKey: ["runs", `kind:eval exp="${run.experiment ?? ""}"`, null, false, false],
    queryFn: ({ signal }) => api.runs({ q: run.experiment ? `kind:eval exp="${run.experiment}"` : "kind:eval" }, signal),
  });
  const rows = useMemo(() => {
    const all = data.data?.items ?? [];
    return all.filter(
      (r) =>
        (!failures || r.failed) &&
        (!q || r.item_id.toLowerCase().includes(q) || jsonText(r.input).toLowerCase().includes(q) || jsonText(r.output).toLowerCase().includes(q)),
    );
  }, [data.data, failures, q]);
  if (data.isError) return <ErrorState error={data.error as Error} onRetry={() => data.refetch()} className="py-6" />;
  const d = data.data;
  const scoreNames = d ? Object.keys(d.scores) : [];
  const current = d?.items.find((r) => r.item_id === open) ?? null;
  // Runs of the same target first: comparing nightly runs is the common case.
  const same = (t: typeof run.target) => targetText(t) === targetText(run.target);
  const compareTo = (others.data?.rows ?? [])
    .filter((r) => r.id !== run.id)
    .sort((x, y) => Number(same(y.target)) - Number(same(x.target)))
    .slice(0, 60);

  return (
    <div className="flex flex-col gap-4">
      {!d ? (
        <Skeleton className="h-16 max-w-3xl" />
      ) : (
        <div className="flex flex-wrap gap-x-8 gap-y-3">
          <Stat label="Items" value={integer(d.total)} detail={d.dataset ? `${d.dataset.name} v${d.dataset.version}` : undefined} />
          <Stat label="Failed" value={integer(d.failed)} detail={d.total ? `${Math.round((d.failed / d.total) * 100)} %` : undefined} />
          {d.errors > 0 && <Stat label="Errors" value={integer(d.errors)} detail="the target raised" />}
          {Object.entries(d.scores).map(([k, v]) => (
            <Stat key={k} label={k} value={num(v.mean)} detail={v.pass_rate !== null ? `${Math.round(v.pass_rate * 100)} % pass` : undefined} />
          ))}
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <div className="grid grid-cols-2 rounded-md bg-muted p-0.5" role="radiogroup" aria-label="Which items">
          {[
            [false, "All items"],
            [true, "Failures"],
          ].map(([v, l]) => (
            <button
              key={String(v)}
              type="button"
              role="radio"
              aria-checked={failures === v}
              onClick={() => {
                setFailures(v as boolean);
                setShown(PAGE);
              }}
              className={cn("h-7 rounded-[5px] px-3 text-sm pointer-coarse:h-10", failures === v ? "bg-card font-semibold border border-border" : "text-muted-foreground hover:text-foreground")}
            >
              {l as string}
            </button>
          ))}
        </div>
        <div className="relative w-full max-w-xs">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" aria-hidden />
          <Input value={text} onChange={(e) => setText(e.target.value)} placeholder="Find an id, input or output…" aria-label="Find items" className="pl-8" />
        </div>
        {compareTo.length > 0 && (
          <label className="ml-auto flex items-center gap-2 text-sm">
            <GitCompare className="size-4 text-muted-foreground" aria-hidden />
            <span className="sr-only sm:not-sr-only">Compare with</span>
            <select
              className="h-8 max-w-64 rounded-md border px-2 text-sm pointer-coarse:h-11"
              value=""
              onChange={(e) => e.target.value && navigate(`/compare?a=${e.target.value}&b=${run.id}`)}
              aria-label="Compare with another eval run"
            >
              <option value="">Another run…</option>
              {compareTo.map((r) => (
                <option key={r.id} value={r.id}>
                  {r.name}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>
      <div className="relative overflow-x-auto">
        <table className="w-full min-w-[48rem] text-sm">
          <thead>
            <tr className="text-left text-muted-foreground">
              <th scope="col" className="h-9 w-36 border-b px-3 font-medium">
                Item
              </th>
              <th scope="col" className="h-9 border-b px-3 font-medium">
                Input
              </th>
              <th scope="col" className="h-9 border-b px-3 font-medium">
                Expected
              </th>
              <th scope="col" className="h-9 border-b px-3 font-medium">
                Output
              </th>
              {scoreNames.map((s) => (
                <th key={s} scope="col" className="h-9 border-b px-3 font-medium">
                  {s}
                </th>
              ))}
              <th scope="col" className="h-9 border-b px-3 font-medium">
                Human
              </th>
            </tr>
          </thead>
          <tbody>
            {!d &&
              Array.from({ length: 8 }, (_, i) => (
                <tr key={i}>
                  {Array.from({ length: 6 }, (_, j) => (
                    <td key={j} className="h-9 border-b px-3">
                      <Skeleton className="h-3 w-24" />
                    </td>
                  ))}
                </tr>
              ))}
            {rows.slice(0, shown).map((r) => {
              const human = r.labels.at(-1);
              return (
                <tr key={r.item_id} className={cn("cursor-pointer align-top hover:bg-muted", r.failed && "bg-[color-mix(in_srgb,var(--color-danger)_5%,transparent)]")} onClick={() => setOpen(r.item_id)}>
                  <td className="border-b px-3 py-1.5">
                    <button type="button" className="inline-flex min-h-6 items-center text-left font-mono text-[12.5px] hover:underline" onClick={() => setOpen(r.item_id)}>
                      {r.item_id}
                    </button>
                  </td>
                  <td className="max-w-xs border-b px-3 py-1.5">
                    <span className="line-clamp-2 break-words">{jsonText(r.input)}</span>
                  </td>
                  <td className="border-b px-3 py-1.5">{jsonText(r.expected) || "–"}</td>
                  <td className="max-w-[14rem] border-b px-3 py-1.5">
                    {r.error ? <span className="text-danger">Error</span> : <span className="line-clamp-2 break-words">{jsonText(r.output)}</span>}
                  </td>
                  {scoreNames.map((s) => {
                    const sc = r.scores.find((x) => x.name === s);
                    return (
                      <td key={s} className="border-b px-3 py-1.5" title={sc?.reason ?? undefined}>
                        {sc?.passed !== null && sc?.passed !== undefined ? <Verdict passed={sc.passed} /> : <span className="nums">{num(sc?.value)}</span>}
                      </td>
                    );
                  })}
                  <td className="border-b px-3 py-1.5">{human ? <Verdict passed={human.passed} /> : <span className="text-muted-foreground">–</span>}</td>
                </tr>
              );
            })}
            {d && rows.length === 0 && (
              <tr>
                <td colSpan={5 + scoreNames.length} className="px-3 py-6 text-center text-muted-foreground">
                  {failures && !q ? "No item failed in this run." : "No item matches."}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
      {rows.length > shown && (
        <Button className="self-start" onClick={() => setShown((n) => n + PAGE)}>
          Show {Math.min(PAGE, rows.length - shown)} more of {integer(rows.length - shown)}
        </Button>
      )}
      <Sheet open={!!current} onOpenChange={(o) => !o && setOpen(null)} title={<span className="font-mono">{current?.item_id}</span>} wide>
        {current && <ItemDetail run={run.id} row={current} />}
      </Sheet>
    </div>
  );
}

function Stat({ label, value, detail }: { label: string; value: string; detail?: string }) {
  return (
    <div>
      <div className="text-xs font-medium text-muted-foreground">{label}</div>
      <div className="text-xl font-bold nums">{value}</div>
      {detail && <div className="text-xs text-muted-foreground">{detail}</div>}
    </div>
  );
}

function ItemDetail({ run, row }: { run: string; row: ItemRow }) {
  const qc = useQueryClient();
  const [passed, setPassed] = useState<boolean | null>(null);
  const [comment, setComment] = useState("");
  const save = useMutation({
    mutationFn: () => evalApi.label(run, { item_id: row.item_id, name: "human", passed, comment: comment.trim() || null }),
    onSuccess: () => {
      toast.success("Label saved");
      setComment("");
      setPassed(null);
      qc.invalidateQueries({ queryKey: ["items", run] });
    },
    onError: (e) => toast.error(`Not saved: ${(e as Error).message}`),
  });
  const block = (title: string, v: Json) => (
    <section>
      <h3 className="mb-1 text-sm font-semibold">{title}</h3>
      <JsonBlock value={v} />
    </section>
  );
  return (
    <div className="flex flex-col gap-4">
      {block("Input", row.input)}
      <div className="grid gap-4 md:grid-cols-2">
        {block("Expected", row.expected)}
        {row.error ? (
          <section>
            <h3 className="mb-1 text-sm font-semibold text-danger">Error</h3>
            <pre className="overflow-x-auto whitespace-pre-wrap rounded-md bg-muted px-3 py-2 font-mono text-sm">{row.error}</pre>
          </section>
        ) : (
          block("Output", row.output)
        )}
      </div>
      <section>
        <h3 className="mb-1 text-sm font-semibold">Scores</h3>
        <table className="w-full text-sm">
          <tbody>
            {row.scores.map((s) => (
              <tr key={s.name} className="align-top">
                <th scope="row" className="border-b py-1.5 pr-3 text-left font-medium">
                  {s.name}
                </th>
                <td className="border-b py-1.5 pr-3">{s.passed !== null ? <Verdict passed={s.passed} /> : null}</td>
                <td className="border-b py-1.5 pr-3 nums">{num(s.value)}</td>
                <td className="border-b py-1.5 text-muted-foreground">
                  {s.reason}
                  <span className="block font-mono text-xs">{s.evaluator_version}</span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {(row.latency_ms !== null || row.trace_id) && (
          <p className="mt-1 flex flex-wrap gap-x-3 text-xs text-muted-foreground nums">
            {row.latency_ms !== null && <span>Target answered in {num(row.latency_ms)} ms.</span>}
            {row.trace_id && (
              <Link to={`/traces/${row.trace_id}`} className="inline-flex min-h-6 items-center font-semibold text-brand hover:underline">
                Trace
              </Link>
            )}
          </p>
        )}
      </section>
      <section>
        <h3 className="mb-1 text-sm font-semibold">Human labels</h3>
        {row.labels.length === 0 ? (
          <p className="text-sm text-muted-foreground">No one has labelled this item yet.</p>
        ) : (
          <ul className="flex flex-col gap-1 text-sm">
            {row.labels.map((l) => (
              <li key={l.created_at} className="flex flex-wrap items-baseline gap-2">
                <Verdict passed={l.passed} />
                {l.comment && <span>{l.comment}</span>}
                <span className="text-xs text-muted-foreground">{utc(l.created_at)}</span>
              </li>
            ))}
          </ul>
        )}
        <form
          className="mt-3 flex flex-col gap-2 rounded-lg border p-3"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <fieldset className="flex items-center gap-2">
            <legend className="sr-only">Verdict</legend>
            {[
              [true, "Correct"],
              [false, "Wrong"],
            ].map(([v, l]) => (
              <Button key={String(v)} type="button" size="sm" variant={passed === v ? "brand" : "outline"} aria-pressed={passed === v} onClick={() => setPassed(passed === v ? null : (v as boolean))}>
                {l as string}
              </Button>
            ))}
          </fieldset>
          <label className="flex flex-col gap-1">
            <span className="text-xs font-medium text-muted-foreground">Comment</span>
            <textarea value={comment} onChange={(e) => setComment(e.target.value)} rows={2} placeholder="Why the output is right or wrong…" className="rounded-md border border-input bg-card px-2.5 py-1.5 text-base" />
          </label>
          <Button type="submit" variant="brand" className="self-start" disabled={passed === null && !comment.trim()} loading={save.isPending}>
            Save label
          </Button>
        </form>
      </section>
    </div>
  );
}

/** Where every result of an eval run came from, and the export. */
export function AuditTab({ run }: { run: RunDetail }) {
  const a = useQuery({ queryKey: ["audit", run.id], queryFn: ({ signal }) => evalApi.audit(run.id, signal) });
  if (a.isError) return <ErrorState error={a.error as Error} onRetry={() => a.refetch()} className="py-6" />;
  if (!a.data) return <Skeleton className="h-64 max-w-3xl" />;
  const d = a.data as Record<string, Record<string, Json> | Json[] | Json>;
  const ds = (d.dataset ?? {}) as Record<string, Json>;
  const sch = (d.schedule ?? null) as Record<string, Json> | null;
  const schRun = (d.schedule_run ?? null) as Record<string, Json> | null;
  const evs = (d.evaluators ?? []) as Record<string, Json>[];
  const results = (d.results ?? []) as Json[];
  const labels = (d.labels ?? []) as Json[];
  const row = (k: string, v: React.ReactNode) => (
    <div className="contents" key={k}>
      <dt className="py-1.5 text-muted-foreground">{k}</dt>
      <dd className="break-words py-1.5">{v ?? "–"}</dd>
    </div>
  );
  return (
    <div className="flex max-w-4xl flex-col gap-5">
      <p className="text-sm text-muted-foreground">
        Every result of this run traces back to the exact dataset version, target, evaluator versions and schedule below. The export holds the same record with every item's output and
        scores, as JSON, with times in UTC.
      </p>
      <div>
        <Button asChild variant="brand">
          <a href={`/api/runs/${encodeURIComponent(run.id)}/audit?download=true`} download>
            <Download /> Export the audit record (JSON)
          </a>
        </Button>
      </div>
      <dl className="grid grid-cols-[minmax(9rem,auto)_1fr] gap-x-6 border bg-card px-4 py-2 text-sm">
        {row("Run", <code>{run.id}</code>)}
        {row("Target", targetText(run.target) || "–")}
        {row("Dataset", ds.name ? <Link className="inline-flex min-h-6 items-center text-brand hover:underline" to={`/datasets/${encodeURIComponent(String(ds.name))}?version=${ds.version}`}>{`${ds.name} v${ds.version}`}</Link> : "–")}
        {row("Dataset digest", ds.digest ? <code>{String(ds.digest)}</code> : "–")}
        {row("Dataset created", ds.created_at ? String(ds.created_at) : "–")}
        {row("Storage version", ds.storage_version != null ? <code>Lance version {String(ds.storage_version)}</code> : "–")}
        {row(
          "Evaluators",
          evs.length ? (
            <ul>
              {evs.map((e) => (
                <li key={String(e.name)}>
                  {String(e.name)} <code className="text-muted-foreground">{String(e.version)}</code>
                </li>
              ))}
            </ul>
          ) : (
            "–"
          ),
        )}
        {row(
          "Schedule",
          sch ? (
            <>
              <Link className="inline-flex min-h-6 items-center text-brand hover:underline" to={`/schedules/${encodeURIComponent(String(sch.name))}`}>
                {String(sch.name)}
              </Link>
              {sch.scheduled_for ? `, for ${String(sch.scheduled_for)}` : ""}
            </>
          ) : (
            "Not scheduled"
          ),
        )}
        {schRun && row("Schedule run", `${String(schRun.id)}: ${String(schRun.status)}, attempt ${String(schRun.attempt)}, started ${String(schRun.started_at)}`)}
        {row("Recorded", `${utc(run.created_at)}${run.ended_at ? ` to ${utc(run.ended_at)}` : ""}`)}
        {row("Results", `${integer(results.length)} items, ${integer(labels.length)} human labels`)}
      </dl>
    </div>
  );
}
