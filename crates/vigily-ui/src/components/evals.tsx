import * as DialogPrimitive from "@radix-ui/react-dialog";
import { Check, CircleAlert, CircleCheck, CircleMinus, Clock, Loader2, TriangleAlert, X } from "lucide-react";
import type { Json, Level, ScheduleStatus } from "@/lib/api";
import { cn } from "@/lib/utils";

/** Pass or fail as an icon and a word. */
export function Verdict({ passed, className, compact }: { passed: boolean | null; className?: string; compact?: boolean }) {
  if (passed === null) return <span className={cn("text-sm text-muted-foreground", className)}>–</span>;
  const Icon = passed ? Check : X;
  return (
    <span className={cn("inline-flex items-center gap-1 text-sm", passed ? "text-success" : "text-danger", className)}>
      <Icon className="size-3.5 shrink-0" strokeWidth={2.5} aria-hidden />
      {compact ? <span className="sr-only">{passed ? "Pass" : "Fail"}</span> : <span>{passed ? "Pass" : "Fail"}</span>}
    </span>
  );
}

const LEVEL: Record<Level, { label: string; icon: typeof Check; className: string }> = {
  stable: { label: "Stable", icon: CircleCheck, className: "text-success" },
  moderate: { label: "Moderate shift", icon: CircleAlert, className: "text-warning" },
  drift: { label: "Drift", icon: TriangleAlert, className: "text-danger" },
};

export function LevelBadge({ level, className }: { level: Level | null; className?: string }) {
  if (!level) return <span className={cn("text-sm text-muted-foreground", className)}>No data</span>;
  const l = LEVEL[level];
  return (
    <span className={cn("inline-flex items-center gap-1.5 text-sm", className)}>
      <l.icon className={cn("size-3.5 shrink-0", l.className)} aria-hidden />
      {l.label}
    </span>
  );
}

const SCHED: Record<ScheduleStatus, { label: string; icon: typeof Check; className: string }> = {
  running: { label: "Running", icon: Loader2, className: "text-brand" },
  succeeded: { label: "Succeeded", icon: CircleCheck, className: "text-success" },
  failed: { label: "Failed", icon: X, className: "text-danger" },
  timed_out: { label: "Timed out", icon: Clock, className: "text-danger" },
  skipped_overlap: { label: "Skipped, still running", icon: CircleMinus, className: "text-warning" },
  skipped_missed: { label: "Skipped, missed", icon: CircleMinus, className: "text-warning" },
  interrupted: { label: "Interrupted", icon: CircleAlert, className: "text-warning" },
};

export function ScheduleStatusBadge({ status, className, iconOnly }: { status: ScheduleStatus; className?: string; iconOnly?: boolean }) {
  const l = SCHED[status];
  return (
    <span className={cn("inline-flex items-center gap-1.5 text-sm", className)} title={iconOnly ? l.label : undefined}>
      <l.icon className={cn("size-3.5 shrink-0", l.className)} aria-hidden />
      {iconOnly ? <span className="sr-only">{l.label}</span> : l.label}
    </span>
  );
}

export const scheduleLabel = (s: ScheduleStatus) => SCHED[s].label;

/** A JSON value as text: strings as they are, everything else as JSON. */
export function jsonText(v: Json | undefined, pretty = false): string {
  if (v === undefined || v === null) return "";
  if (typeof v === "string") return v;
  return JSON.stringify(v, null, pretty ? 2 : undefined);
}

/** A value shown in full: text wraps, JSON is pretty-printed in the code face. */
export function JsonBlock({ value, className }: { value: Json | undefined; className?: string }) {
  if (value === undefined || value === null) return <p className={cn("text-sm text-muted-foreground", className)}>None</p>;
  if (typeof value === "string")
    return <p className={cn("whitespace-pre-wrap break-words bg-muted px-3 py-2 text-base", className)}>{value}</p>;
  return <pre className={cn("overflow-x-auto bg-muted px-3 py-2 font-mono text-sm", className)}>{jsonText(value, true)}</pre>;
}

/** A panel that slides in from the right: details of one item, one schedule run. */
export function Sheet({ open, onOpenChange, title, children, wide }: { open: boolean; onOpenChange: (o: boolean) => void; title: React.ReactNode; children: React.ReactNode; wide?: boolean }) {
  return (
    <DialogPrimitive.Root open={open} onOpenChange={onOpenChange}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-[var(--scrim)]" />
        <DialogPrimitive.Content
          aria-describedby={undefined}
          className={cn(
            "fixed inset-y-0 right-0 z-50 flex w-full flex-col bg-popover shadow-float outline-none data-[state=open]:animate-[sheet-in_160ms_var(--ease-out)]",
            wide ? "max-w-3xl" : "max-w-xl",
          )}
        >
          <div className="flex items-start gap-3 border-b px-4 py-3">
            <DialogPrimitive.Title className="min-w-0 flex-1 text-lg font-semibold">{title}</DialogPrimitive.Title>
            <DialogPrimitive.Close className="inline-flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground pointer-coarse:size-11" aria-label="Close">
              <X className="size-4" />
            </DialogPrimitive.Close>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-4 py-4">{children}</div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

export { PageHeader } from "@/components/page";

/** A short block that explains a method in plain words. */
export function Method({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <details className="group border-b py-3 text-sm">
      <summary className="cursor-pointer select-none font-medium marker:text-muted-foreground">{title}</summary>
      <div className="mt-2 max-w-prose space-y-2 text-muted-foreground">{children}</div>
    </details>
  );
}
