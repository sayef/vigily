import { useQuery } from "@tanstack/react-query";
import { ArrowRight } from "lucide-react";
import { useMemo } from "react";
import { Link, useSearchParams } from "react-router";
import { PageHeader, Verdict, jsonText } from "@/components/evals";
import { ErrorState } from "@/components/states";
import { Skeleton } from "@/components/ui/skeleton";
import { type Change, type Json, evalApi } from "@/lib/api";
import { integer } from "@/lib/format";
import { useTitle } from "@/lib/hooks";
import { cn } from "@/lib/utils";

const CHANGES: [Change, string][] = [
  ["regressed", "Regressed"],
  ["fixed", "Fixed"],
  ["changed", "Output changed, still passing"],
  ["both_failed", "Failing in both"],
  ["only_a", "Only in the first run"],
  ["only_b", "Only in the second run"],
  ["both_passed", "Passing in both"],
];
const label = Object.fromEntries(CHANGES) as Record<Change, string>;

function datasetText(d: Json): string {
  if (!d || typeof d !== "object" || Array.isArray(d)) return "";
  return `${d.name} v${d.version}`;
}

export function ComparePage() {
  useTitle("Compare runs");
  const [sp, setSp] = useSearchParams();
  const a = sp.get("a") ?? "";
  const b = sp.get("b") ?? "";
  const show = (sp.get("show") as Change | null) ?? null;
  const data = useQuery({ queryKey: ["compare", a, b], queryFn: ({ signal }) => evalApi.compare(a, b, signal), enabled: !!a && !!b });
  const rows = useMemo(() => (data.data?.items ?? []).filter((r) => (show ? r.change === show : r.change !== "both_passed")), [data.data, show]);
  if (!a || !b)
    return (
      <div className="mx-auto max-w-xl px-4 py-12">
        <h1 className="border-b-[1.5px] border-rule pb-4 text-xl font-bold tracking-[-0.02em]">Compare two eval runs</h1>
        <p className="mt-2 text-muted-foreground">Open an eval run, go to its Items tab and choose another run under “Compare with”.</p>
      </div>
    );
  if (data.isError) return <ErrorState error={data.error as Error} onRetry={() => data.refetch()} />;
  const d = data.data;
  const sameData = d && datasetText(d.a.dataset) === datasetText(d.b.dataset);
  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <PageHeader title="Compare eval runs">
        {d ? (
          <span className="flex flex-wrap items-center gap-2">
            <Link to={`/runs/${d.a.id}?tab=items`} className="inline-flex min-h-6 items-center text-foreground hover:underline">
              {d.a.name}
            </Link>
            <ArrowRight className="size-4" aria-label="against" />
            <Link to={`/runs/${d.b.id}?tab=items`} className="inline-flex min-h-6 items-center text-foreground hover:underline">
              {d.b.name}
            </Link>
            <span>
              {sameData ? `Both on ${datasetText(d.a.dataset)}.` : `Different datasets: ${datasetText(d.a.dataset) || "none"} and ${datasetText(d.b.dataset) || "none"}; only shared items compare.`}
            </span>
          </span>
        ) : (
          " "
        )}
      </PageHeader>
      <div className="mt-4 flex flex-wrap gap-2" role="radiogroup" aria-label="Which items">
        {d &&
          CHANGES.map(([k, l]) => {
            const n = d.counts[k] ?? 0;
            return (
              <button
                key={k}
                type="button"
                role="radio"
                aria-checked={show === k}
                disabled={n === 0}
                onClick={() => setSp((p) => (show === k ? (p.delete("show"), p) : (p.set("show", k), p)), { replace: true })}
                className={cn(
                  "inline-flex h-8 items-center gap-2 rounded-md border px-3 text-sm disabled:opacity-45 pointer-coarse:h-11",
                  show === k ? "border-rule bg-muted font-bold" : "border-input bg-card hover:bg-muted",
                )}
              >
                {l}
                <span className="font-semibold nums">{integer(n)}</span>
              </button>
            );
          })}
      </div>
      <p className="mt-2 text-sm text-muted-foreground">{show ? `Showing ${label[show].toLowerCase()} items.` : "Showing every item whose verdict or output differs, regressions first."}</p>
      <div className="relative mt-3 overflow-x-auto">
        <table className="w-full min-w-[52rem] text-sm">
          <thead>
            <tr className="text-left text-muted-foreground">
              <th scope="col" className="h-9 border-b px-3 font-medium">
                Item
              </th>
              <th scope="col" className="h-9 border-b px-3 font-medium">
                Input
              </th>
              <th scope="col" className="h-9 border-b px-3 font-medium">
                Expected
              </th>
              <th scope="col" className="h-9 border-b px-3 font-medium">
                First run
              </th>
              <th scope="col" className="h-9 border-b px-3 font-medium">
                Second run
              </th>
              <th scope="col" className="h-9 border-b px-3 font-medium">
                Change
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
            {rows.slice(0, 500).map((r) => (
              <tr key={r.item_id} className="align-top hover:bg-muted">
                <td className="border-b px-3 py-1.5 font-mono text-[12.5px] text-muted-foreground">{r.item_id}</td>
                <td className="max-w-xs border-b px-3 py-1.5">
                  <span className="line-clamp-2 break-words">{jsonText(r.input)}</span>
                </td>
                <td className="border-b px-3 py-1.5">{jsonText(r.expected) || "–"}</td>
                {[r.a, r.b].map((side, i) => (
                  <td key={i} className="max-w-[14rem] border-b px-3 py-1.5">
                    {side ? (
                      <span className="flex flex-col gap-0.5">
                        <Verdict passed={!side.failed} />
                        <span className="line-clamp-2 break-words text-muted-foreground">{side.error ? "Error" : jsonText(side.output)}</span>
                      </span>
                    ) : (
                      <span className="text-muted-foreground">Not in this run</span>
                    )}
                  </td>
                ))}
                <td className={cn("border-b px-3 py-1.5", r.change === "regressed" && "font-semibold text-danger", r.change === "fixed" && "font-semibold text-success")}>{label[r.change]}</td>
              </tr>
            ))}
            {d && rows.length === 0 && (
              <tr>
                <td colSpan={6} className="px-3 py-6 text-center text-muted-foreground">
                  The two runs agree on every item.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
      {rows.length > 500 && <p className="mt-2 text-sm text-muted-foreground">Showing the first 500 of {integer(rows.length)} items.</p>}
    </div>
  );
}
