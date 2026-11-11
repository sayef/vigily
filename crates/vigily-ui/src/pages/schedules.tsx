import { useQuery } from "@tanstack/react-query";
import { CalendarClock, ShieldAlert } from "lucide-react";
import { useState } from "react";
import { Link, useParams, useSearchParams } from "react-router";
import { PageHeader, ScheduleStatusBadge, Sheet, scheduleLabel } from "@/components/evals";
import { ErrorState } from "@/components/states";
import { StatusBadge } from "@/components/status";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip } from "@/components/ui/tooltip";
import { type Schedule, type ScheduleRunSummary, evalApi, targetLabel } from "@/lib/api";
import { ago, dateTime, duration, integer, utc } from "@/lib/format";
import { useCrumbs } from "@/lib/crumbs";
import { useTitle } from "@/lib/hooks";
import { cn } from "@/lib/utils";

const DAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/** A cron expression in words for the common cases; the expression itself otherwise. */
export function cronText(cron: string, tz: string): string {
  const f = cron.trim().split(/\s+/);
  if (f.length !== 5) return `${cron} (${tz})`;
  const [m, h, dom, mon, dow] = f;
  const at = /^\d+$/.test(m) && /^\d+$/.test(h) ? `${h.padStart(2, "0")}:${m.padStart(2, "0")}` : null;
  if (at && dom === "*" && mon === "*" && dow === "*") return `Every day at ${at} ${tz}`;
  if (at && dom === "*" && mon === "*" && /^[0-6]$/.test(dow)) return `Every ${DAYS[+dow]} at ${at} ${tz}`;
  if (at && dom === "*" && mon === "*" && dow === "1-5") return `Weekdays at ${at} ${tz}`;
  if (/^\*\/\d+$/.test(m) && h === "*" && dom === "*" && mon === "*" && dow === "*") return `Every ${m.slice(2)} minutes`;
  if (/^\d+$/.test(m) && h === "*" && dom === "*" && mon === "*" && dow === "*") return `Every hour at minute ${m}`;
  return `${cron} (${tz})`;
}

const SNIPPET = `# vigily.toml, next to where you run vigily up (or --config PATH)
[[schedule]]
name = "nightly-intents"
cron = "0 2 * * *"
timezone = "Europe/Berlin"
command = ["uv", "run", "python", "evals/nightly.py"]
timeout = "30m"
retries = 1
missed = "run_once"`;

function Security() {
  return (
    <p className="flex max-w-3xl gap-2 text-sm text-muted-foreground">
      <ShieldAlert className="mt-0.5 size-4 shrink-0" aria-hidden />
      <span>
        Commands run as the user that runs vigily, with its permissions. They come only from the server’s configuration file; this page can show them but cannot add, change or start them.
      </span>
    </p>
  );
}

function Recent({ runs }: { runs: ScheduleRunSummary[] }) {
  const list = [...runs].reverse();
  const ok = list.filter((r) => r.status === "succeeded").length;
  return (
    <span className="flex items-center gap-2">
      <span className="flex gap-0.5" aria-hidden>
        {list.map((r) => (
          <Tooltip key={r.id} content={`${scheduleLabel(r.status)}, ${utc(r.scheduled_for)}`}>
            <span className={cn("block h-4 w-1.5 rounded-[2px]", r.status === "succeeded" ? "bg-success" : r.status.startsWith("skipped") ? "bg-warning" : r.status === "running" ? "bg-brand" : "bg-danger")} />
          </Tooltip>
        ))}
      </span>
      <span className="text-xs text-muted-foreground nums">
        {ok} of {list.length} succeeded
      </span>
    </span>
  );
}

export function SchedulesPage() {
  useTitle("Schedules");
  const q = useQuery({ queryKey: ["schedules"], queryFn: ({ signal }) => evalApi.schedules(signal), refetchInterval: 15_000 });
  if (q.isError) return <ErrorState error={q.error as Error} onRetry={() => q.refetch()} />;
  const d = q.data;
  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <PageHeader title="Schedules">
        {d?.config_path ? (
          <>
            Defined in <code className="text-foreground">{d.config_path}</code>. Times are shown in your time zone; hover for UTC.
          </>
        ) : d ? (
          "No configuration file is loaded."
        ) : (
          " "
        )}
      </PageHeader>
      <div className="mt-3">
        <Security />
      </div>
      {d && d.schedules.length === 0 ? (
        <div className="mt-6 max-w-2xl">
          <h2 className="text-[17px] font-bold tracking-[-0.01em]">No schedules</h2>
          <p className="mt-1 text-base text-muted-foreground">
            A schedule runs an evaluation script on a cron expression and records its output. Add one to a configuration file and restart vigily with it.
          </p>
          <pre className="mt-3 overflow-x-auto p-4 font-mono text-sm leading-relaxed">
            <code>{SNIPPET}</code>
          </pre>
        </div>
      ) : (
        <div className="relative mt-4 overflow-x-auto">
          <table className="w-full min-w-[52rem] text-sm">
            <thead>
              <tr className="text-left text-muted-foreground">
                <th scope="col" className="h-10 border-b px-3 font-medium">
                  Schedule
                </th>
                <th scope="col" className="h-10 border-b px-3 font-medium">
                  When
                </th>
                <th scope="col" className="h-10 border-b px-3 font-medium">
                  Next run
                </th>
                <th scope="col" className="h-10 border-b px-3 font-medium">
                  Last run
                </th>
                <th scope="col" className="h-10 border-b px-3 font-medium">
                  Recent runs
                </th>
              </tr>
            </thead>
            <tbody>
              {!d &&
                Array.from({ length: 2 }, (_, i) => (
                  <tr key={i}>
                    {Array.from({ length: 5 }, (_, j) => (
                      <td key={j} className="h-14 border-b px-3">
                        <Skeleton className="h-3 w-28" />
                      </td>
                    ))}
                  </tr>
                ))}
              {d?.schedules.map((s) => (
                <tr key={s.name} className="align-top hover:bg-muted">
                  <th scope="row" className="border-b px-3 py-2.5 text-left font-medium">
                    <Link to={`/schedules/${encodeURIComponent(s.name)}`} className="inline-flex min-h-6 items-center gap-2 hover:underline">
                      <CalendarClock className="size-4 text-muted-foreground" aria-hidden />
                      {s.name}
                    </Link>
                    {s.description && <span className="mt-0.5 block font-normal text-muted-foreground">{s.description}</span>}
                    {!s.enabled && <span className="mt-0.5 block font-normal text-warning">Disabled</span>}
                  </th>
                  <td className="border-b px-3 py-2.5">
                    {cronText(s.cron, s.timezone)}
                    <code className="mt-0.5 block text-xs text-muted-foreground">{s.cron}</code>
                  </td>
                  <td className="border-b px-3 py-2.5 nums">
                    {s.next_run ? (
                      <Tooltip content={utc(s.next_run)}>
                        <span>{dateTime(s.next_run)}</span>
                      </Tooltip>
                    ) : (
                      <span className="text-muted-foreground">{s.enabled ? "Not scheduled by this server" : "–"}</span>
                    )}
                  </td>
                  <td className="border-b px-3 py-2.5">
                    {s.last_run ? (
                      <span className="flex flex-col">
                        <ScheduleStatusBadge status={s.last_run.status} />
                        <Tooltip content={utc(s.last_run.started_at)}>
                          <span className="text-xs text-muted-foreground">{ago(s.last_run.started_at)}</span>
                        </Tooltip>
                      </span>
                    ) : (
                      <span className="text-muted-foreground">Never ran</span>
                    )}
                  </td>
                  <td className="border-b px-3 py-2.5">{s.recent.length ? <Recent runs={s.recent} /> : "–"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

export function SchedulePage() {
  const { name = "" } = useParams();
  useTitle(name);
  useCrumbs([{ label: "Schedules", to: "/schedules" }, { label: name, mono: true }]);
  const [sp, setSp] = useSearchParams();
  const open = sp.get("run");
  const list = useQuery({ queryKey: ["schedules"], queryFn: ({ signal }) => evalApi.schedules(signal), refetchInterval: 15_000 });
  const runs = useQuery({ queryKey: ["schedule-runs", name], queryFn: ({ signal }) => evalApi.scheduleRuns(name, signal), refetchInterval: 15_000 });
  const detail = useQuery({ queryKey: ["schedule-run", name, open], queryFn: ({ signal }) => evalApi.scheduleRun(name, open!, signal), enabled: !!open });
  const [stream, setStream] = useState<"stdout" | "stderr">("stdout");
  const s: Schedule | undefined = list.data?.schedules.find((x) => x.name === name);
  if (runs.isError) return <ErrorState error={runs.error as Error} onRetry={() => runs.refetch()} />;
  const setOpen = (id: string | null) => setSp((p) => (id ? (p.set("run", id), p) : (p.delete("run"), p)), { replace: true });
  const fact = (k: string, v: React.ReactNode) => (
    <div className="contents" key={k}>
      <dt className="py-1 text-muted-foreground">{k}</dt>
      <dd className="break-words py-1">{v}</dd>
    </div>
  );
  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <PageHeader title={name}>{s?.description ?? " "}</PageHeader>
      <div className="mt-4 grid gap-6 xl:grid-cols-[22rem_minmax(0,1fr)]">
        <section aria-labelledby="cfg-h">
          <h2 id="cfg-h" className="mb-3 mt-6 border-b-[1.5px] border-rule pb-2.5 text-[17px] font-bold tracking-[-0.01em]">
            Configuration
          </h2>
          {s ? (
            <dl className="grid grid-cols-[minmax(6rem,auto)_1fr] gap-x-4 border bg-card px-3 py-2 text-sm">
              {fact("When", cronText(s.cron, s.timezone))}
              {fact("Next run", s.next_run ? `${dateTime(s.next_run)} (${utc(s.next_run)})` : "–")}
              {fact("Command", <code className="text-[12.5px]">{s.command.join(" ")}</code>)}
              {fact("Directory", <code className="text-[12.5px]">{s.workdir}</code>)}
              {fact("Timeout", s.timeout)}
              {fact("Retries", s.retries === 0 ? "None" : String(s.retries))}
              {fact("After downtime", s.missed === "skip" ? "Skip missed runs" : "Run the latest missed run once")}
            </dl>
          ) : (
            <Skeleton className="h-48" />
          )}
          <div className="mt-3">
            <Security />
          </div>
        </section>
        <section aria-labelledby="runs-h" className="min-w-0">
          <h2 id="runs-h" className="mb-3 mt-6 border-b-[1.5px] border-rule pb-2.5 text-[17px] font-bold tracking-[-0.01em]">
            Runs
          </h2>
          <div className="relative overflow-x-auto">
            <table className="w-full min-w-[40rem] text-sm">
              <thead>
                <tr className="text-left text-muted-foreground">
                  <th scope="col" className="h-9 border-b px-3 font-medium">
                    Scheduled for
                  </th>
                  <th scope="col" className="h-9 border-b px-3 font-medium">
                    Status
                  </th>
                  <th scope="col" className="h-9 border-b px-3 text-right font-medium">
                    Took
                  </th>
                  <th scope="col" className="h-9 border-b px-3 font-medium">
                    Eval runs
                  </th>
                  <th scope="col" className="h-9 border-b px-3">
                    <span className="sr-only">Output</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {!runs.data &&
                  Array.from({ length: 4 }, (_, i) => (
                    <tr key={i}>
                      {Array.from({ length: 5 }, (_, j) => (
                        <td key={j} className="h-10 border-b px-3">
                          <Skeleton className="h-3 w-20" />
                        </td>
                      ))}
                    </tr>
                  ))}
                {runs.data?.runs.map((r) => (
                  <tr key={r.id} className="align-top hover:bg-muted">
                    <td className="border-b px-3 py-2 nums">
                      <Tooltip content={utc(r.scheduled_for)}>
                        <span>{dateTime(r.scheduled_for)}</span>
                      </Tooltip>
                      {r.attempt > 1 && <span className="block text-xs text-muted-foreground">Attempt {r.attempt}</span>}
                    </td>
                    <td className="border-b px-3 py-2">
                      <ScheduleStatusBadge status={r.status} />
                      {r.exit_code !== null && r.status !== "succeeded" && <span className="block text-xs text-muted-foreground nums">Exit status {r.exit_code}</span>}
                      {r.reason && <span className="block max-w-xs text-xs text-muted-foreground">{r.reason}</span>}
                    </td>
                    <td className="border-b px-3 py-2 text-right nums">{r.ended_at ? duration((r.ended_at - r.started_at) / 1e6) : "–"}</td>
                    <td className="border-b px-3 py-2">
                      {r.eval_runs?.length ? (
                        <ul>
                          {r.eval_runs.map((e) => (
                            <li key={e.id} className="flex items-center gap-1.5">
                              <StatusBadge status={e.status} iconOnly />
                              <Link to={`/runs/${e.id}?tab=items`} className="inline-flex min-h-6 items-center hover:underline">
                                {targetLabel(e.target) || e.name}
                              </Link>
                            </li>
                          ))}
                        </ul>
                      ) : (
                        <span className="text-muted-foreground">–</span>
                      )}
                    </td>
                    <td className="border-b px-3 py-2 text-right">
                      {r.has_output && (
                        <button type="button" onClick={() => setOpen(r.id)} className="inline-flex h-7 items-center rounded-md px-2 text-sm font-medium text-brand hover:bg-muted pointer-coarse:h-11">
                          Output
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
                {runs.data && runs.data.runs.length === 0 && (
                  <tr>
                    <td colSpan={5} className="px-3 py-6 text-center text-muted-foreground">
                      This schedule has not run yet{s?.next_run ? `; the first run is ${dateTime(s.next_run)}` : ""}.
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
          {runs.data && runs.data.runs.length > 1 && <p className="mt-2 text-xs text-muted-foreground nums">{integer(runs.data.runs.length)} runs, newest first.</p>}
        </section>
      </div>
      <Sheet open={!!open} onOpenChange={(o) => !o && setOpen(null)} title="Output" wide>
        {detail.data && (
          <div className="flex flex-col gap-3">
            <p className="text-sm text-muted-foreground">
              <ScheduleStatusBadge status={detail.data.status} /> <code className="ml-2">{detail.data.command.join(" ")}</code>
            </p>
            <div className="grid grid-cols-2 self-start rounded-md bg-muted p-0.5" role="radiogroup" aria-label="Stream">
              {(["stdout", "stderr"] as const).map((k) => (
                <button key={k} type="button" role="radio" aria-checked={stream === k} onClick={() => setStream(k)} className={cn("h-7 rounded-[5px] px-3 text-sm pointer-coarse:h-10", stream === k ? "bg-card font-semibold border border-border" : "text-muted-foreground")}>
                  {k === "stdout" ? "Standard output" : "Standard error"}
                </button>
              ))}
            </div>
            <pre className="min-h-40 overflow-x-auto whitespace-pre-wrap rounded-md bg-muted px-3 py-2 font-mono text-[12.5px] leading-relaxed">{(stream === "stdout" ? detail.data.stdout : detail.data.stderr) || "(empty)"}</pre>
            <p className="text-xs text-muted-foreground">The last 64 KiB of each stream are kept.</p>
          </div>
        )}
        {detail.isPending && open && <Skeleton className="h-40" />}
      </Sheet>
    </div>
  );
}
