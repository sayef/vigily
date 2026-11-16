import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Activity, BellOff, CalendarX, ChevronDown, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { Link, useSearchParams } from "react-router";
import { toast } from "sonner";
import { PageHeader } from "@/components/evals";
import { KindIcon, summaryText } from "@/components/notifications";
import { ErrorState } from "@/components/states";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip } from "@/components/ui/tooltip";
import { type Alert, type Delivery, evalApi } from "@/lib/api";
import { ago, until, utc } from "@/lib/format";
import { useTitle } from "@/lib/hooks";
import { cn } from "@/lib/utils";

const KIND: Record<Alert["kind"], { label: string; icon: typeof Activity }> = {
  behaviour_drift: { label: "Behaviour drift", icon: Activity },
  data_drift: { label: "Input drift", icon: TriangleAlert },
  schedule_failed: { label: "Schedule failed", icon: CalendarX },
};

export function AlertsPage() {
  useTitle("Alerts");
  const [sp, setSp] = useSearchParams();
  const all = sp.get("show") === "all";
  const qc = useQueryClient();
  const [shown, setShown] = useState<string | null>(null);
  const q = useQuery({ queryKey: ["alerts"], queryFn: ({ signal }) => evalApi.alerts(signal), refetchInterval: 30_000 });
  const ack = useMutation({
    mutationFn: ({ id, on }: { id: string; on: boolean }) => evalApi.ackAlert(id, on),
    onSuccess: (a) => {
      qc.invalidateQueries({ queryKey: ["alerts"] });
      qc.invalidateQueries({ queryKey: ["meta"] });
      toast.success(a.acknowledged_at ? "Alert acknowledged" : "Alert reopened", {
        action: a.acknowledged_at ? { label: "Undo", onClick: () => ack.mutate({ id: a.id, on: false }) } : undefined,
      });
    },
    onError: (e) => toast.error((e as Error).message),
  });
  if (q.isError) return <ErrorState error={q.error as Error} onRetry={() => q.refetch()} />;
  const list = (q.data ?? []).filter((a) => all || !a.acknowledged_at);
  const open = (q.data ?? []).filter((a) => !a.acknowledged_at).length;
  return (
    <div className="px-4 pb-16 pt-5 md:px-6">
      <PageHeader title="Alerts">
        {q.data ? (
          <span>
            {open} open of {q.data.length}. vigily raises an alert once per drifted run and per failed scheduled run, and sends it to the channels its{" "}
            <Link to="/settings/notifications" className="inline-flex min-h-6 items-center underline underline-offset-4 hover:text-foreground">
              routing rules
            </Link>{" "}
            pick.
          </span>
        ) : (
          " "
        )}
      </PageHeader>
      <div className="mt-4 grid w-fit grid-cols-2 rounded-md bg-muted p-0.5" role="radiogroup" aria-label="Which alerts">
        {[
          [false, "Open"],
          [true, "All"],
        ].map(([v, l]) => (
          <button
            key={String(v)}
            type="button"
            role="radio"
            aria-checked={all === v}
            onClick={() => setSp((p) => (v ? (p.set("show", "all"), p) : (p.delete("show"), p)), { replace: true })}
            className={cn("h-7 rounded-[5px] px-3 text-sm pointer-coarse:h-10", all === v ? "bg-card font-semibold border border-border" : "text-muted-foreground hover:text-foreground")}
          >
            {l as string}
          </button>
        ))}
      </div>
      <ul className="mt-4 flex max-w-4xl flex-col gap-2">
        {!q.data && Array.from({ length: 3 }, (_, i) => <Skeleton key={i} className="h-24" />)}
        {q.data && list.length === 0 && (
          <li className="flex items-start gap-3 border bg-card px-4 py-6">
            <BellOff className="mt-0.5 size-5 text-muted-foreground" aria-hidden />
            <div>
              <p className="font-medium">{all ? "No alerts yet." : "No open alerts."}</p>
              <p className="text-sm text-muted-foreground">Alerts appear when an evaluation suite drifts or a scheduled run fails.</p>
            </div>
          </li>
        )}
        {list.map((a) => {
          const k = KIND[a.kind];
          const link = a.kind === "schedule_failed" && a.schedule ? `/schedules/${encodeURIComponent(a.schedule)}` : a.suite ? `/drift/${a.suite}${a.run_id ? `?run=${a.run_id}` : ""}` : null;
          return (
            <li key={a.id} className={cn("flex flex-wrap items-start gap-x-4 gap-y-2 border bg-card px-4 py-3", a.acknowledged_at && "opacity-70")}>
              <k.icon className={cn("mt-0.5 size-5 shrink-0", a.severity === "critical" ? "text-danger" : "text-warning")} aria-hidden />
              <div className="min-w-0 flex-1 basis-60">
                <p className="flex flex-wrap items-baseline gap-x-2">
                  <span className="font-semibold">{a.title}</span>
                  <span className="text-xs text-muted-foreground">
                    {k.label}, {a.severity}
                  </span>
                </p>
                <p className="mt-0.5 max-w-prose text-sm text-muted-foreground">{a.detail}</p>
                <p className="mt-1 flex flex-wrap gap-x-3 text-xs text-muted-foreground">
                  <Tooltip content={utc(a.created_at)}>
                    <span>Raised {ago(a.created_at)}</span>
                  </Tooltip>
                  {(a.experiment || a.suite_name) && <span>{[a.experiment, a.suite_name].filter(Boolean).join(", ")}</span>}
                  {a.deliveries ? (
                    <button
                      type="button"
                      aria-expanded={shown === a.id}
                      aria-controls={`deliveries-${a.id}`}
                      onClick={() => setShown(shown === a.id ? null : a.id)}
                      className={cn("inline-flex min-h-6 items-center gap-1 underline-offset-4 hover:text-foreground hover:underline pointer-coarse:min-h-11", a.deliveries.failed > 0 && "text-danger")}
                    >
                      Delivery: {summaryText(a.deliveries)}
                      <ChevronDown className={cn("size-3.5 transition-transform duration-[160ms]", shown === a.id && "rotate-180")} aria-hidden />
                    </button>
                  ) : (
                    <span>Not sent to any channel</span>
                  )}
                  {a.acknowledged_at && <span>Acknowledged {ago(a.acknowledged_at)}</span>}
                </p>
              </div>
              <div className="flex gap-2 max-sm:pl-9">
                {link && (
                  <Button asChild size="sm" variant="outline">
                    <Link to={link}>{a.kind === "schedule_failed" ? "Open the schedule" : "Open the drift"}</Link>
                  </Button>
                )}
                <Button size="sm" variant={a.acknowledged_at ? "ghost" : "secondary"} loading={ack.isPending && ack.variables?.id === a.id} onClick={() => ack.mutate({ id: a.id, on: !a.acknowledged_at })}>
                  {a.acknowledged_at ? "Reopen" : "Acknowledge"}
                </Button>
              </div>
              {shown === a.id && <DeliveryLog id={a.id} />}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

const STATUS: Record<Delivery["status"], string> = { sent: "Sent", failed: "Failed", pending: "Retrying" };

/** Every delivery of one alert: channel, status, attempts, the last answer and the error. */
function DeliveryLog({ id }: { id: string }) {
  const q = useQuery({ queryKey: ["deliveries", id], queryFn: ({ signal }) => evalApi.deliveries(id, signal), refetchInterval: (x) => (x.state.data?.deliveries.some((d) => d.status === "pending") ? 5_000 : false) });
  return (
    <div id={`deliveries-${id}`} className="min-w-0 basis-full border-t pt-2 sm:pl-9">
      {q.isError && <p className="text-sm text-danger">{(q.error as Error).message}</p>}
      {!q.data && !q.isError && <Skeleton className="h-8" />}
      {q.data && q.data.deliveries.length === 0 && <p className="text-sm text-muted-foreground">No deliveries recorded.</p>}
      {q.data && q.data.deliveries.length > 0 && (
        <ul aria-label="Deliveries of this alert" className="text-sm">
          {q.data.deliveries.map((d) => (
            <li key={d.channel_id} className="grid gap-x-4 gap-y-0.5 border-b py-1.5 last:border-b-0 sm:grid-cols-[minmax(0,14rem)_minmax(0,1fr)]">
              <Link to={`/settings/notifications/channels/${encodeURIComponent(d.channel_id)}`} className="inline-flex min-h-6 min-w-0 items-center gap-1.5 self-start underline-offset-4 hover:underline">
                <KindIcon kind={d.kind} />
                <span className="truncate">{d.channel_name}</span>
              </Link>
              <span className="min-w-0 sm:pt-0.5">
                <span className="flex flex-wrap items-baseline gap-x-2">
                  <span className={cn("font-semibold", d.status === "failed" && "text-danger", d.status === "sent" && "text-brand", d.status === "pending" && "text-warning")}>{STATUS[d.status]}</span>
                  {d.status_code !== undefined && <span className="font-mono text-[12.5px] nums">{d.status_code}</span>}
                  <span className="text-faint nums">
                    {d.attempts} {d.attempts === 1 ? "attempt" : "attempts"}, last{" "}
                    <Tooltip content={utc(d.updated_at)}>
                      <span>{ago(d.updated_at)}</span>
                    </Tooltip>
                    {d.status === "pending" && d.next_attempt_at && <>, next {until(d.next_attempt_at)}</>}
                  </span>
                </span>
                {d.error && <span className="block max-w-prose break-words text-muted-foreground">{d.error}</span>}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
