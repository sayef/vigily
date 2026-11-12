import { ChartLine } from "lucide-react";
import { Link } from "react-router";
import { EmptyRepository, ErrorState } from "@/components/states";
import { StatusBadge } from "@/components/status";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip } from "@/components/ui/tooltip";
import { type ExperimentInfo, isSystem } from "@/lib/api";
import { ago, integer, utc } from "@/lib/format";
import { useTitle } from "@/lib/hooks";
import { useMeta } from "@/lib/queries";

const runsOf = (e: ExperimentInfo) => (e.name ? `exp="${e.name}"` : "exp=null");

export function ExperimentsPage() {
  useTitle("Experiments");
  const meta = useMeta();
  if (meta.isError && !meta.data) return <ErrorState error={meta.error as Error} onRetry={() => meta.refetch()} />;
  if (meta.data && meta.data.runs === 0) return <EmptyRepository repository={meta.data.repository} />;
  const list = meta.data?.experiments ?? [];
  const named = list.filter((e) => e.name).length;
  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <div className="border-b-[1.5px] border-rule pb-4">
        <h1 className="text-xl font-bold tracking-[-0.02em]">Experiments</h1>
        <p className="mt-1.5 text-sm text-faint nums">
          {meta.data ? `${named} ${named === 1 ? "experiment" : "experiments"}, newest activity first. An experiment is the name runs are logged under.` : " "}
        </p>
      </div>
      <div className="relative mt-4 max-w-6xl overflow-x-auto">
        <table className="w-full min-w-[44rem] text-sm">
          <thead>
            <tr className="text-left text-muted-foreground">
              <th scope="col" className="h-10 border-b px-3 font-medium">
                Experiment
              </th>
              <th scope="col" className="h-10 border-b px-3 text-right font-medium">
                Runs
              </th>
              <th scope="col" className="h-10 border-b px-3 font-medium">
                State
              </th>
              <th scope="col" className="h-10 border-b px-3 font-medium">
                Last run
              </th>
              <th scope="col" className="h-10 border-b px-3 font-medium">
                Metrics
              </th>
              <th scope="col" className="h-10 border-b px-3">
                <span className="sr-only">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {!meta.data &&
              Array.from({ length: 4 }, (_, i) => (
                <tr key={i}>
                  {Array.from({ length: 6 }, (_, j) => (
                    <td key={j} className="h-11 border-b px-3">
                      <Skeleton className="h-3 w-20" />
                    </td>
                  ))}
                </tr>
              ))}
            {list.map((e) => {
              const metrics = e.metrics.filter((m) => !isSystem(m));
              return (
                <tr key={e.name || "(none)"} className="hover:bg-muted">
                  <th scope="row" className="h-11 whitespace-nowrap border-b px-3 text-left font-medium">
                    <Link to={`/?q=${encodeURIComponent(runsOf(e))}`} className="inline-flex min-h-8 items-center hover:underline">
                      {e.name || <span className="text-muted-foreground">No experiment</span>}
                    </Link>
                  </th>
                  <td className="h-11 border-b px-3 text-right nums">{integer(e.runs)}</td>
                  <td className="h-11 border-b px-3">
                    <span className="flex flex-wrap gap-x-3 gap-y-0.5 nums">
                      {e.running > 0 && <Count status="running" n={e.running} />}
                      {e.finished > 0 && <Count status="finished" n={e.finished} />}
                      {e.failed > 0 && <Count status="failed" n={e.failed} />}
                      {e.aborted > 0 && <Count status="aborted" n={e.aborted} />}
                    </span>
                  </td>
                  <td className="h-11 whitespace-nowrap border-b px-3 text-muted-foreground">
                    {e.last_created && (
                      <Tooltip content={utc(e.last_created)}>
                        <span>{ago(e.last_created)}</span>
                      </Tooltip>
                    )}
                  </td>
                  <td className="h-11 max-w-72 truncate border-b px-3 text-muted-foreground" title={metrics.join(", ")}>
                    {metrics.slice(0, 4).join(", ")}
                    {metrics.length > 4 && ` and ${metrics.length - 4} more`}
                  </td>
                  <td className="h-11 border-b px-3 text-right">
                    <Link
                      to={`/metrics?q=${encodeURIComponent(runsOf(e))}`}
                      className="inline-flex h-7 items-center gap-1.5 rounded-md px-2 text-sm font-medium text-muted-foreground hover:bg-card hover:text-foreground pointer-coarse:h-11"
                    >
                      <ChartLine className="size-4" aria-hidden /> Compare curves
                    </Link>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function Count({ status, n }: { status: "running" | "finished" | "failed" | "aborted"; n: number }) {
  return (
    <span className="inline-flex items-center gap-1 whitespace-nowrap">
      <StatusBadge status={status} iconOnly />
      {n} {status}
    </span>
  );
}
