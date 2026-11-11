import { Columns3 } from "lucide-react";
import { useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { type Meta, isSystem } from "@/lib/api";
import { type ColumnId, RUN_FIELDS } from "@/lib/columns";
import { metricLabel } from "@/lib/format";

/** Chooses the runs table's columns: run fields, parameters, and a statistic of each metric. */
export function ColumnPicker({ meta, columns, onChange, onReset }: { meta?: Meta; columns: ColumnId[]; onChange: (c: ColumnId[]) => void; onReset: () => void }) {
  const [filter, setFilter] = useState("");
  const on = new Set(columns);
  const toggle = (id: ColumnId) => onChange(on.has(id) ? columns.filter((c) => c !== id) : [...columns, id]);
  const f = filter.trim().toLowerCase();
  const params = useMemo(() => (meta?.params ?? []).filter((p) => !isSystem(p.key) && p.key.toLowerCase().includes(f)), [meta, f]);
  const metrics = useMemo(() => (meta?.metrics ?? []).filter((m) => !isSystem(m.name) && m.key.toLowerCase().includes(f)), [meta, f]);
  const runFields = Object.entries(RUN_FIELDS).filter(([k, v]) => k !== "name" && v.label.toLowerCase().includes(f));
  const row = "flex min-h-7 cursor-pointer items-center gap-2 rounded-sm px-2 text-sm hover:bg-muted pointer-coarse:min-h-11";

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline">
          <Columns3 /> Columns
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 p-0">
        <div className="sticky top-0 z-10 flex items-center gap-2 border-b bg-popover p-2">
          <Input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Find a column…" aria-label="Find a column" autoFocus />
          <Button variant="ghost" size="sm" onClick={onReset}>
            Reset
          </Button>
        </div>
        <div className="flex flex-col gap-3 p-2">
          {runFields.length > 0 && (
            <fieldset>
              <legend className="px-2 pb-1 text-xs font-medium text-muted-foreground">Run</legend>
              {runFields.map(([k, v]) => (
                <label key={k} className={row}>
                  <input type="checkbox" checked={on.has(`run:${k}`)} onChange={() => toggle(`run:${k}`)} />
                  {v.label}
                </label>
              ))}
            </fieldset>
          )}
          {params.length > 0 && (
            <fieldset>
              <legend className="px-2 pb-1 text-xs font-medium text-muted-foreground">Parameters</legend>
              {params.map((p) => (
                <label key={p.key} className={row}>
                  <input type="checkbox" checked={on.has(`param:${p.key}`)} onChange={() => toggle(`param:${p.key}`)} />
                  <span className="min-w-0 flex-1 truncate font-mono">{p.key}</span>
                  <span className="text-xs text-muted-foreground nums">{p.distinct > 64 ? "many" : p.distinct} values</span>
                </label>
              ))}
            </fieldset>
          )}
          {metrics.length > 0 && (
            <fieldset>
              <legend className="px-2 pb-1 text-xs font-medium text-muted-foreground">Metrics: last, minimum, maximum</legend>
              {metrics.map((m) => {
                const l = metricLabel(m.key);
                return (
                  <div key={m.key} className="flex min-h-7 items-center gap-2 px-2 text-sm pointer-coarse:min-h-11">
                    <span className="min-w-0 flex-1 truncate">
                      {l.name}
                      {l.context && <span className="ml-1 text-muted-foreground">{l.context}</span>}
                    </span>
                    {(["last", "min", "max"] as const).map((agg) => (
                      <label key={agg} className="flex cursor-pointer items-center gap-1 rounded-sm px-1 text-xs text-muted-foreground hover:bg-muted pointer-coarse:min-h-11">
                        <input type="checkbox" checked={on.has(`metric:${agg}:${m.key}`)} onChange={() => toggle(`metric:${agg}:${m.key}`)} aria-label={`${m.key} ${agg}`} />
                        {agg}
                      </label>
                    ))}
                  </div>
                );
              })}
            </fieldset>
          )}
          {!runFields.length && !params.length && !metrics.length && <p className="px-2 py-3 text-sm text-muted-foreground">No column matches “{filter}”.</p>}
        </div>
      </PopoverContent>
    </Popover>
  );
}
