import type { ChartSettings } from "@/components/metric-chart";
import { type Meta, type XAxis, isSystem } from "@/lib/api";
import { cn } from "@/lib/utils";

const AXES: [XAxis, string][] = [
  ["step", "Step"],
  ["epoch", "Epoch"],
  ["relative", "Time"],
  ["wall", "Date"],
];

/** Columns that make useful groups: the experiment and parameters with a few distinct values. */
export function groupOptions(meta?: Meta): [string, string][] {
  const out: [string, string][] = [["run:experiment", "Experiment"], ["run:status", "Status"]];
  for (const p of meta?.params ?? []) {
    if (!isSystem(p.key) && p.distinct > 1 && p.distinct <= 12 && p.kind !== "list") out.push([`param:${p.key}`, p.key]);
  }
  return out;
}

const label = "text-xs font-medium text-muted-foreground";
const select = "h-8 w-full rounded-md border px-2 text-sm pointer-coarse:h-11";

/** X axis, smoothing, log scale, grouping and aggregation for a set of charts. */
export function ChartControls({ value, onChange, meta, className, grouping = true }: { value: ChartSettings; onChange: (s: Partial<ChartSettings>) => void; meta?: Meta; className?: string; grouping?: boolean }) {
  return (
    <div className={cn("flex flex-col gap-3", className)}>
      <fieldset>
        <legend className={label}>X axis</legend>
        <div className="mt-1 grid grid-cols-4 rounded-md bg-muted p-0.5" role="radiogroup" aria-label="X axis">
          {AXES.map(([v, l]) => (
            <button
              key={v}
              type="button"
              role="radio"
              aria-checked={value.x === v}
              onClick={() => onChange({ x: v })}
              className={cn("h-7 rounded-[5px] text-sm transition-colors duration-[120ms] pointer-coarse:h-10", value.x === v ? "bg-card font-semibold text-foreground shadow-[0_1px_2px_var(--shadow-ink)]" : "text-muted-foreground hover:text-foreground")}
            >
              {l}
            </button>
          ))}
        </div>
      </fieldset>
      <div>
        <div className="flex items-baseline justify-between">
          <label htmlFor="smoothing" className={label}>
            Smoothing
          </label>
          <output htmlFor="smoothing" className="text-sm nums">
            {value.smoothing === 0 ? "off" : value.smoothing.toFixed(2)}
          </output>
        </div>
        <input
          id="smoothing"
          type="range"
          min={0}
          max={0.99}
          step={0.01}
          value={value.smoothing}
          onChange={(e) => onChange({ smoothing: Number(e.target.value) })}
          className="mt-1 h-6 w-full pointer-coarse:h-11"
        />
      </div>
      <div className="flex flex-col">
        <label className="flex min-h-7 cursor-pointer items-center gap-2 text-sm pointer-coarse:min-h-11">
          <input type="checkbox" checked={value.logY} onChange={(e) => onChange({ logY: e.target.checked })} />
          Log scale
        </label>
        <label className="flex min-h-7 cursor-pointer items-center gap-2 text-sm pointer-coarse:min-h-11">
          <input type="checkbox" checked={value.ignoreOutliers} onChange={(e) => onChange({ ignoreOutliers: e.target.checked })} />
          Ignore outliers in the y range
        </label>
      </div>
      {grouping && (
        <>
          <div>
            <label htmlFor="group-by" className={label}>
              Group lines by
            </label>
            <select id="group-by" className={cn(select, "mt-1")} value={value.groupBy ?? ""} onChange={(e) => onChange({ groupBy: e.target.value || null })}>
              <option value="">Nothing: one line per run</option>
              {groupOptions(meta).map(([v, l]) => (
                <option key={v} value={v}>
                  {l}
                </option>
              ))}
            </select>
            <p className="mt-1 text-xs text-muted-foreground">Groups are told apart by line style.</p>
          </div>
          <label className="flex min-h-7 cursor-pointer items-center gap-2 text-sm pointer-coarse:min-h-11">
            <input type="checkbox" checked={value.aggregate} onChange={(e) => onChange({ aggregate: e.target.checked })} />
            Mean per group, with min–max band
          </label>
        </>
      )}
    </div>
  );
}
