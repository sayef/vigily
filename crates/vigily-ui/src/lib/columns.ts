import { type Meta, type RunRow, isSystem, targetText } from "@/lib/api";
import { ago, duration, higherIsBetter, metricLabel, num, numFull, utc, value } from "@/lib/format";

/** A runs-table column id: `run:name`, `param:hparams.lr` or `metric:min:loss[val]` (as the server sorts). */
export type ColumnId = string;
export type Agg = "last" | "min" | "max";

export interface Cell {
  text: string;
  title?: string;
}

export interface ColumnDef {
  id: ColumnId;
  label: string;
  /** A second line in the header: the context or the statistic. */
  detail?: string;
  kind: "run" | "param" | "metric";
  numeric: boolean;
  width: number;
  /** The default sort direction: numbers from best, names from A. */
  descFirst: boolean;
  cell: (r: RunRow) => Cell;
}

export const RUN_FIELDS: Record<string, { label: string; width: number; numeric?: boolean }> = {
  status: { label: "Status", width: 124 },
  name: { label: "Run", width: 280 },
  experiment: { label: "Experiment", width: 140 },
  created: { label: "Created", width: 104, numeric: true },
  duration: { label: "Duration", width: 104, numeric: true },
  tags: { label: "Tags", width: 160 },
  kind: { label: "Kind", width: 88 },
  target: { label: "Target", width: 200 },
  id: { label: "Id", width: 100 },
};

export function columnDef(id: ColumnId): ColumnDef | null {
  const [kind, ...rest] = id.split(":");
  if (kind === "run") {
    const f = rest.join(":");
    const meta = RUN_FIELDS[f];
    if (!meta) return null;
    const cell = (r: RunRow): Cell => {
      switch (f) {
        case "status":
          return { text: r.status };
        case "name":
          return { text: r.name, title: r.has_name ? r.name : `Unnamed run ${r.id}` };
        case "experiment":
          return { text: r.experiment ?? "–" };
        case "created":
          return { text: ago(r.created_at), title: utc(r.created_at) };
        case "duration":
          return { text: duration(r.duration), title: r.ended_at ? `Ended ${utc(r.ended_at)}` : "Still running" };
        case "tags":
          return { text: r.tags.join(", ") };
        case "kind":
          return { text: r.kind };
        case "target": {
          const t = targetText(r.target);
          return { text: t || "–", title: t || "No target recorded" };
        }
        default:
          return { text: r.id.slice(-8), title: r.id };
      }
    };
    return { id, label: meta.label, kind: "run", numeric: !!meta.numeric, width: meta.width, descFirst: f === "created" || f === "duration", cell };
  }
  if (kind === "param") {
    const key = rest.join(":");
    const short = key.split(".").pop() ?? key;
    const parent = key.includes(".") ? key.slice(0, key.length - short.length - 1) : undefined;
    return {
      id,
      label: short,
      detail: parent,
      kind: "param",
      numeric: true,
      width: 96,
      descFirst: false,
      cell: (r) => {
        const v = r.params[key];
        const text = value(v);
        return { text: v === undefined ? "–" : text, title: v === undefined ? "Not set" : typeof v === "number" ? numFull(v) : text };
      },
    };
  }
  if (kind === "metric") {
    const [agg, ...k] = rest;
    const key = k.join(":");
    if (!["last", "min", "max"].includes(agg) || !key) return null;
    const { name, context } = metricLabel(key);
    const stat = agg === "last" ? "last" : agg === "min" ? "min" : "max";
    return {
      id,
      label: name,
      detail: [context, agg === "last" ? null : agg].filter(Boolean).join(", ") || undefined,
      kind: "metric",
      numeric: true,
      width: 104,
      descFirst: higherIsBetter(name),
      cell: (r) => {
        const m = r.metrics[key];
        if (!m) return { text: "–", title: "Not logged" };
        const v = m[stat as Agg];
        const at = agg === "last" ? m.last_step : agg === "min" ? m.min_step : m.max_step;
        return { text: num(v), title: `${numFull(v)} at step ${at ?? "–"}, ${m.count} points` };
      },
    };
  }
  return null;
}

/** The columns a fresh table shows: the run's identity, the parameters that vary and the main metrics. */
export function defaultColumns(meta: Meta): ColumnId[] {
  const base = ["run:status", "run:name", "run:experiment", "run:created", "run:duration"];
  const params = meta.params
    .filter((p) => !isSystem(p.key) && p.distinct > 1 && ["number", "text", "bool"].includes(p.kind))
    .sort((a, b) => b.runs - a.runs || a.distinct - b.distinct || a.key.localeCompare(b.key))
    .slice(0, 4)
    .map((p) => `param:${p.key}`);
  const metrics = byTraining(meta.metrics.filter((m) => !isSystem(m.name))).slice(0, 3);
  const cols = metrics.map((m) => `metric:last:${m.key}`);
  const best = metrics.find((m) => /val|valid|test|eval/i.test(m.key));
  if (best) cols.push(`metric:${higherIsBetter(best.name) ? "max" : "min"}:${best.key}`);
  return [...base, ...params, ...cols];
}

/** Validation and test series sort before training ones. */
const rank = (key: string) => (/val|valid|eval/i.test(key) ? 0 : /test/i.test(key) ? 1 : 2);

/** Training metrics first (eval runs have their own pages), then the most common, validation first. */
function byTraining<T extends { key: string; runs: number; eval_runs?: number }>(list: T[]): T[] {
  const train = (m: T) => m.runs - (m.eval_runs ?? 0);
  return [...list].sort((a, b) => train(b) - train(a) || b.runs - a.runs || rank(a.key) - rank(b.key) || a.key.localeCompare(b.key));
}

/** The metrics to chart first for `rows` (or for the repository): the ones most of them log. */
export function mainMetrics(meta: Meta, n: number, rows?: { metrics: Record<string, unknown> }[]): string[] {
  if (rows?.length) {
    const count = new Map<string, number>();
    for (const r of rows) for (const k of Object.keys(r.metrics)) if (!isSystem(k)) count.set(k, (count.get(k) ?? 0) + 1);
    return [...count.entries()]
      .sort((a, b) => b[1] - a[1] || rank(a[0]) - rank(b[0]) || a[0].localeCompare(b[0]))
      .slice(0, n)
      .map(([k]) => k);
  }
  return byTraining(meta.metrics.filter((m) => !isSystem(m.name)))
    .slice(0, n)
    .map((m) => m.key);
}
