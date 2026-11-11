import { ago } from "@/lib/format";
import type { Status } from "@/lib/api";
import { cn } from "@/lib/utils";

const LOOK: Record<Status, { label: string; className: string }> = {
  running: { label: "Running", className: "font-bold text-brand" },
  finished: { label: "Finished", className: "text-muted-foreground" },
  failed: { label: "Failed", className: "font-bold text-danger" },
  aborted: { label: "Aborted", className: "font-semibold text-warning" },
};

/**
 * A run's state as a word, coloured only to repeat it: ink blue with a blinking square while running,
 * red when failed. A running run that stopped sending heartbeats (`stale`) shows as "No heartbeat",
 * with when it was last seen.
 */
export function StatusBadge({ status, stale, lastSeen, className, iconOnly }: { status: Status; stale?: boolean; lastSeen?: number; className?: string; iconOnly?: boolean }) {
  if (status === "running" && stale) {
    const label = "No heartbeat";
    const title = `Running, but nothing heard since ${ago(lastSeen)}: its process most likely stopped. A writer with a slow clock, or one busy in a single long call, can also look like this.`;
    return (
      <span className={cn("inline-flex items-center gap-1.5 text-sm font-semibold text-warning", className)} title={title}>
        <span className="inline-block size-[7px] border border-current" aria-hidden />
        {iconOnly ? <span className="sr-only">{label}</span> : <span>{label}</span>}
      </span>
    );
  }
  const l = LOOK[status];
  return (
    <span className={cn("inline-flex items-center gap-1.5 text-sm", l.className, className)} title={iconOnly ? l.label : undefined}>
      {status === "running" && <span className="live-dot" aria-hidden />}
      {status === "failed" && iconOnly && <span className="inline-block size-[7px] bg-danger" aria-hidden />}
      {iconOnly ? <span className="sr-only">{l.label}</span> : <span>{l.label}</span>}
    </span>
  );
}

export const statusLabel = (s: Status) => LOOK[s].label;
