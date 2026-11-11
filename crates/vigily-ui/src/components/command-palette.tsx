import { useQuery } from "@tanstack/react-query";
import { Command } from "cmdk";
import { Activity, Bell, CalendarClock, ChartLine, Database, FlaskConical, GitCompare, Keyboard, ListTree, Moon, Rows3, Settings, Sun, X } from "lucide-react";
import { useNavigate } from "react-router";
import { StatusBadge } from "@/components/status";
import { useTheme } from "@/components/theme";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { api, isSystem, traceApi } from "@/lib/api";
import { ago, metricLabel } from "@/lib/format";
import { useMeta } from "@/lib/queries";
import { useSelection } from "@/lib/selection";

const item =
  "flex min-h-9 cursor-default items-center gap-2.5 rounded-md px-2.5 py-1.5 text-base data-[selected=true]:bg-muted [&>svg]:size-4 [&>svg]:shrink-0 [&>svg]:text-muted-foreground";
const group = "px-1.5 pb-1 [&_[cmdk-group-heading]]:px-2.5 [&_[cmdk-group-heading]]:pb-1 [&_[cmdk-group-heading]]:pt-2 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-muted-foreground";

export function CommandPalette({ open, onOpenChange, onShortcuts }: { open: boolean; onOpenChange: (o: boolean) => void; onShortcuts: () => void }) {
  const navigate = useNavigate();
  const { theme, toggle } = useTheme();
  const selection = useSelection();
  const meta = useMeta();
  const runs = useQuery({ queryKey: ["runs", "", null, false, false], queryFn: ({ signal }) => api.runs({ q: "" }, signal), enabled: open });
  const traces = useQuery({ queryKey: ["traces", "palette"], queryFn: ({ signal }) => traceApi.list({ limit: 20 }, signal), enabled: open && !!meta.data?.traces });
  const go = (to: string) => {
    onOpenChange(false);
    navigate(to);
  };
  const run = (f: () => void) => {
    onOpenChange(false);
    f();
  };
  const n = selection.ids.length;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl">
        <DialogTitle className="sr-only">Search vigily</DialogTitle>
        <Command loop label="Search vigily" className="flex max-h-[70vh] flex-col">
          <Command.Input
            autoFocus
            placeholder="Search runs, experiments, metrics or actions…"
            className="h-12 w-full shrink-0 border-b bg-transparent px-4 text-base outline-none placeholder:text-muted-foreground"
          />
          <Command.List className="min-h-0 flex-1 overflow-y-auto overscroll-contain py-1">
            <Command.Empty className="px-4 py-6 text-center text-sm text-muted-foreground">Nothing matches. Try a run name, an experiment or a metric.</Command.Empty>
            <Command.Group heading="Go to" className={group}>
              <Command.Item className={item} onSelect={() => go("/")}>
                <Rows3 /> Runs
              </Command.Item>
              <Command.Item className={item} onSelect={() => go("/metrics")}>
                <ChartLine /> Metrics
              </Command.Item>
              <Command.Item className={item} onSelect={() => go("/experiments")}>
                <FlaskConical /> Experiments
              </Command.Item>
              <Command.Item className={item} onSelect={() => go("/traces")}>
                <ListTree /> Traces
              </Command.Item>
              <Command.Item className={item} onSelect={() => go("/datasets")}>
                <Database /> Datasets
              </Command.Item>
              <Command.Item className={item} onSelect={() => go("/drift")}>
                <Activity /> Drift
              </Command.Item>
              <Command.Item className={item} onSelect={() => go("/schedules")}>
                <CalendarClock /> Schedules
              </Command.Item>
              <Command.Item className={item} onSelect={() => go("/alerts")}>
                <Bell /> Alerts
              </Command.Item>
              <Command.Item className={item} onSelect={() => go("/settings")}>
                <Settings /> Settings
              </Command.Item>
            </Command.Group>
            <Command.Group heading="Actions" className={group}>
              {n > 0 && (
                <Command.Item className={item} onSelect={() => go(`/metrics/compare?runs=${selection.ids.join(",")}`)}>
                  <GitCompare /> Compare {n} selected {n === 1 ? "run" : "runs"}
                </Command.Item>
              )}
              {n > 0 && (
                <Command.Item className={item} onSelect={() => run(selection.clear)}>
                  <X /> Clear the selection
                </Command.Item>
              )}
              <Command.Item className={item} onSelect={() => run(toggle)}>
                {theme === "dark" ? <Sun /> : <Moon />} Switch to {theme === "dark" ? "light" : "dark"} theme
              </Command.Item>
              <Command.Item className={item} onSelect={() => run(onShortcuts)}>
                <Keyboard /> Keyboard shortcuts
              </Command.Item>
            </Command.Group>
            {!!meta.data?.experiments.length && (
              <Command.Group heading="Experiments" className={group}>
                {meta.data.experiments.map((e) => (
                  <Command.Item key={e.name || "(none)"} value={`experiment ${e.name || "no experiment"}`} className={item} onSelect={() => go(`/?q=${encodeURIComponent(e.name ? `exp="${e.name}"` : "exp=null")}`)}>
                    <FlaskConical />
                    <span className="flex-1 truncate">{e.name || "No experiment"}</span>
                    <span className="text-sm text-muted-foreground nums">{e.runs} runs</span>
                  </Command.Item>
                ))}
              </Command.Group>
            )}
            {!!meta.data?.metrics.length && (
              <Command.Group heading="Metrics" className={group}>
                {meta.data.metrics
                  .filter((m) => !isSystem(m.name))
                  .map((m) => {
                    const l = metricLabel(m.key);
                    return (
                      <Command.Item key={m.key} value={`metric ${m.key}`} className={item} onSelect={() => go(`/metrics?m=${encodeURIComponent(m.key)}`)}>
                        <ChartLine />
                        <span className="flex-1 truncate">
                          {l.name}
                          {l.context && <span className="ml-1.5 text-muted-foreground">{l.context}</span>}
                        </span>
                        <span className="text-sm text-muted-foreground nums">{m.runs} runs</span>
                      </Command.Item>
                    );
                  })}
              </Command.Group>
            )}
            {!!runs.data?.rows.length && (
              <Command.Group heading="Runs" className={group}>
                {runs.data.rows.map((r) => (
                  <Command.Item key={r.id} value={`run ${r.name} ${r.experiment ?? ""} ${r.id}`} className={item} onSelect={() => go(`/runs/${r.id}`)}>
                    <StatusBadge status={r.status} stale={r.stale} lastSeen={r.last_seen} iconOnly />
                    <span className="flex-1 truncate">{r.name}</span>
                    <span className="truncate text-sm text-muted-foreground">{r.experiment}</span>
                    <span className="shrink-0 text-sm text-muted-foreground nums">{ago(r.created_at)}</span>
                  </Command.Item>
                ))}
              </Command.Group>
            )}
            {!!traces.data?.rows.length && (
              <Command.Group heading="Recent traces" className={group}>
                {traces.data.rows.map((t) => (
                  <Command.Item key={t.id} value={`trace ${t.name} ${t.id}`} className={item} onSelect={() => go(`/traces/${t.id}`)}>
                    <ListTree />
                    <span className="flex-1 truncate font-mono text-[13px]">{t.name}</span>
                    {t.status === "error" && <span className="text-sm font-bold text-danger">Error</span>}
                    <span className="shrink-0 text-sm text-muted-foreground nums">{ago(t.start_time)}</span>
                  </Command.Item>
                ))}
              </Command.Group>
            )}
          </Command.List>
        </Command>
      </DialogContent>
    </Dialog>
  );
}
