import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { Maximize2, Pin, RotateCcw, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router";
import uPlot from "uplot";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip } from "@/components/ui/tooltip";
import { useTheme } from "@/components/theme";
import { type ChartData, type XAxis, api } from "@/lib/api";
import { X_LABEL, higherIsBetter, metricLabel, num, numFull, xValue } from "@/lib/format";
import { cn } from "@/lib/utils";

/** Line dashes that tell groups apart without colour, in canvas pixels before scaling. */
const DASHES: number[][] = [[], [6, 4], [1.5, 3], [9, 3, 1.5, 3]];
export const dashFor = (i: number) => DASHES[i % DASHES.length];

export interface ChartSettings {
  x: XAxis;
  logY: boolean;
  /** 0 is raw; 0.99 is heavy. */
  smoothing: number;
  groupBy: string | null;
  aggregate: boolean;
  /** Fit the y axis to the bulk of the values, so one diverged run does not flatten the rest. */
  ignoreOutliers: boolean;
}

export interface MetricChartProps extends ChartSettings {
  metric: string;
  runs: string[];
  height?: number;
  /** Charts with the same key share the crosshair. */
  syncKey?: string;
  /** Runs drawn in the accent: pinned in the readout, or hovered in a table. */
  pinned?: Set<string>;
  onPin?: (runId: string) => void;
  /** A link that opens this metric in the explorer. */
  expandTo?: string;
  className?: string;
}

/** Debiased exponential moving average, as TensorBoard draws it; gaps stay gaps. */
function ema(y: (number | null)[], w: number): (number | null)[] {
  if (w <= 0) return y;
  let acc = 0;
  let n = 0;
  return y.map((v) => {
    if (v === null || !Number.isFinite(v)) return null;
    acc = acc * w + (1 - w) * v;
    n += 1;
    return acc / (1 - Math.pow(w, n));
  });
}

/** The value at `idx`, or the nearest one within a few slots (the envelope leaves gaps). */
function valueNear(y: (number | null)[], idx: number): number | null {
  for (let d = 0; d <= 3; d++) {
    const a = y[idx - d];
    if (a !== null && a !== undefined) return a;
    const b = y[idx + d];
    if (b !== null && b !== undefined) return b;
  }
  return null;
}

function lastValue(y: (number | null)[]): { v: number | null; i: number } {
  for (let i = y.length - 1; i >= 0; i--) if (y[i] !== null && y[i] !== undefined) return { v: y[i], i };
  return { v: null, i: -1 };
}

function token(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function alpha(hex: string, a: number): string {
  return `color-mix(in srgb, ${hex} ${Math.round(a * 100)}%, transparent)`;
}

interface Line {
  id: string;
  label: string;
  group: string | null;
  groupIndex: number;
  y: (number | null)[];
  /** The true last value when the line is raw (not smoothed, not aggregated). */
  last?: number | null;
  /** Aggregates: the band around the mean. */
  min?: (number | null)[];
  max?: (number | null)[];
  runs?: number;
}

function linesOf(data: ChartData, s: ChartSettings): Line[] {
  if (s.aggregate) {
    return data.groups.map((g, i) => ({
      id: `group:${g.label}`,
      label: g.label,
      group: g.label,
      groupIndex: i,
      y: ema(g.mean, s.smoothing),
      min: g.min,
      max: g.max,
      runs: g.runs,
    }));
  }
  const groups: string[] = [];
  for (const x of data.series) if (x.group !== null && !groups.includes(x.group)) groups.push(x.group);
  groups.sort((a, b) => (Number.isFinite(+a) && Number.isFinite(+b) ? +a - +b : a.localeCompare(b)));
  return data.series.map((x) => ({
    id: x.run_id,
    label: x.name,
    group: x.group,
    groupIndex: x.group === null ? 0 : groups.indexOf(x.group),
    y: ema(x.y, s.smoothing),
    last: s.smoothing > 0 ? undefined : (x.last?.[1] ?? null),
  }));
}

/** A short name for a grouping column: `param:hparams.lr` is "lr", `param:optimizer.name` is "optimizer.name". */
function groupLabel(column: string): string {
  const key = column.split(":").slice(1).join(":") || column;
  const parts = key.split(".");
  const last = parts[parts.length - 1];
  return parts.length > 1 && /^(name|type|kind|id|value|mode)$/.test(last) ? parts.slice(-2).join(".") : last;
}

/** A y range that leaves out far outliers (a diverged run), or null when there are none. */
function robustRange(lines: Line[]): [number, number] | null {
  const vals: number[] = [];
  const total = lines.reduce((n, l) => n + l.y.length, 0);
  const stride = Math.max(1, Math.floor(total / 40_000));
  let i = 0;
  for (const l of lines)
    for (const v of l.y) {
      if (v !== null && Number.isFinite(v) && i++ % stride === 0) vals.push(v);
    }
  if (vals.length < 20) return null;
  vals.sort((a, b) => a - b);
  const q = (p: number) => vals[Math.min(vals.length - 1, Math.floor(p * vals.length))];
  const [lo, hi, min, max] = [q(0.02), q(0.98), vals[0], vals[vals.length - 1]];
  const span = Math.max(hi - lo, Math.abs(hi) * 1e-9, 1e-12);
  const top = max - hi > 3 * span ? hi + 0.15 * span : max;
  const bottom = lo - min > 3 * span ? lo - 0.15 * span : min;
  if (top === max && bottom === min) return null;
  const pad = (top - bottom) * 0.04;
  return [bottom - pad, top + pad];
}

export function MetricChart(props: MetricChartProps) {
  const { metric, runs, x, logY, smoothing, groupBy, aggregate, ignoreOutliers, height = 240, syncKey, pinned, onPin, expandTo, className } = props;
  const { theme } = useTheme();
  const wrap = useRef<HTMLDivElement>(null);
  const plotEl = useRef<HTMLDivElement>(null);
  const plot = useRef<uPlot | null>(null);
  const [width, setWidth] = useState(0);
  const [zoom, setZoom] = useState<[number, number] | null>(null);
  const [cursorIdx, setCursorIdx] = useState<number | null>(null);
  const [focusId, setFocusId] = useState<string | null>(null);
  const focusRef = useRef<string | null>(null);
  const pinnedRef = useRef<Set<string>>(pinned ?? new Set());
  pinnedRef.current = pinned ?? new Set();

  // Reset the zoom when what is drawn changes.
  const identity = `${metric}|${runs.join(",")}|${x}|${groupBy}|${aggregate}`;
  const [lastIdentity, setLastIdentity] = useState(identity);
  if (identity !== lastIdentity) {
    setLastIdentity(identity);
    setZoom(null);
  }

  useEffect(() => {
    const el = wrap.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setWidth(Math.floor(e.contentRect.width)));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Ask for about as many points as the chart has pixels, in steps of 100 so resizing rarely refetches.
  const points = Math.max(200, Math.ceil(width / 100) * 100);
  const query = useQuery({
    queryKey: ["chart", metric, runs, x, zoom, points, groupBy, aggregate],
    queryFn: ({ signal }) => api.chart({ runs, metric, x, range: zoom, points, group_by: groupBy, aggregate }, signal),
    enabled: width > 0 && runs.length > 0,
    placeholderData: keepPreviousData,
    refetchInterval: 10_000,
  });
  const data = query.data;
  const settings = useMemo(() => ({ x, logY, smoothing, groupBy, aggregate, ignoreOutliers }), [x, logY, smoothing, groupBy, aggregate, ignoreOutliers]);
  const lines = useMemo(() => (data ? linesOf(data, settings) : []), [data, settings]);
  const raw = useMemo(() => (data && smoothing > 0 && !aggregate && data.series.length <= 12 ? data.series.map((s) => s.y) : []), [data, smoothing, aggregate]);
  const nonPositive = useMemo(() => lines.some((l) => l.y.some((v) => v !== null && v <= 0)), [lines]);
  const log = logY && !nonPositive;
  const clipped = useMemo(() => (ignoreOutliers && !zoom ? robustRange(lines) : null), [ignoreOutliers, zoom, lines]);
  const groupName = groupBy ? groupLabel(groupBy) : null;

  const redraw = useCallback(() => plot.current?.redraw(false), []);
  useEffect(() => {
    redraw();
  }, [pinned, redraw]);

  // Build the plot. It is rebuilt when the data or the look changes; uPlot draws 100k points in a few ms.
  useEffect(() => {
    const el = plotEl.current;
    if (!el || !data || width <= 0) return;
    const seriesColor = token("--series");
    const strong = token("--series-strong");
    const accent = token("--brand");
    const ink = token("--muted-foreground");
    const grid = token("--border");
    const font = `12px ${token("--font-sans") || "sans-serif"}`;
    const n = lines.length;
    // A single run is the focus of its chart: drawn in the accent.
    const lineColor = aggregate ? strong : n === 1 ? accent : seriesColor;
    const series: uPlot.Series[] = [{}];
    const bands: uPlot.Band[] = [];
    for (const l of lines) series.push({ label: l.label, stroke: lineColor, width: aggregate ? 2 : 1.5, dash: dashFor(l.groupIndex), spanGaps: true, points: { show: false } });
    if (aggregate) {
      lines.forEach((l, i) => {
        series.push({ label: `${l.label} max`, stroke: "transparent", width: 0, spanGaps: true, points: { show: false } });
        series.push({ label: `${l.label} min`, stroke: "transparent", width: 0, spanGaps: true, points: { show: false } });
        bands.push({ series: [n + 1 + 2 * i, n + 2 + 2 * i], fill: alpha(strong, 0.12) });
      });
    }
    for (let i = 0; i < raw.length; i++) series.push({ stroke: alpha(seriesColor, 0.3), width: 1, spanGaps: true, points: { show: false } });
    const ys: (number | null)[][] = [
      ...lines.map((l) => l.y),
      ...(aggregate ? lines.flatMap((l) => [l.max ?? [], l.min ?? []]) : []),
      ...raw,
    ];
    const domain = zoom ?? data.domain;
    const opts: uPlot.Options = {
      width,
      height,
      pxAlign: 0,
      legend: { show: false },
      series,
      bands,
      scales: {
        x: { time: false, range: domain ? () => [domain[0], domain[1] === domain[0] ? domain[0] + 1 : domain[1]] : undefined },
        y: { distr: log ? 3 : 1, range: clipped && (!log || clipped[0] > 0) ? () => clipped : undefined },
      },
      axes: [
        {
          stroke: ink,
          font,
          size: 28,
          gap: 4,
          grid: { stroke: grid, width: 1 },
          ticks: { stroke: grid, width: 1, size: 4 },
          values: (_u, splits) => splits.map((v) => xValue(x, v)),
        },
        {
          stroke: ink,
          font,
          size: 52,
          gap: 4,
          grid: { stroke: grid, width: 1 },
          ticks: { show: false },
          // Log axes pass null for minor ticks that get no label.
          values: (_u, splits) => splits.map((v) => (v === null || v === undefined ? "" : num(v))),
        },
      ],
      cursor: {
        sync: syncKey ? { key: syncKey } : undefined,
        drag: { x: true, y: false, setScale: false },
        focus: { prox: 24 },
        points: { size: 6, width: 1.5, fill: token("--card"), stroke: (_u, i) => (focusRef.current === lines[i - 1]?.id ? accent : seriesColor) },
      },
      focus: { alpha: 0.35 },
      hooks: {
        setCursor: [
          (u) => {
            setCursorIdx(u.cursor.idx ?? null);
          },
        ],
        setSelect: [
          (u) => {
            if (u.select.width > 4) {
              const lo = u.posToVal(u.select.left, "x");
              const hi = u.posToVal(u.select.left + u.select.width, "x");
              setZoom([lo, hi]);
            }
            u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
          },
        ],
        setSeries: [
          (u, idx) => {
            const id = idx !== null && idx >= 1 && idx <= n ? lines[idx - 1].id : null;
            if (focusRef.current !== id) {
              focusRef.current = id;
              setFocusId(id);
              u.redraw(false);
            }
          },
        ],
        draw: [
          (u) => {
            // The focused and pinned runs: redrawn on top, in the accent, slightly heavier.
            const marked = lines.map((l, i) => [l, i + 1] as const).filter(([l]) => l.id === focusRef.current || pinnedRef.current.has(l.id));
            if (!marked.length) return;
            const ctx = u.ctx;
            ctx.save();
            ctx.beginPath();
            ctx.rect(u.bbox.left, u.bbox.top, u.bbox.width, u.bbox.height);
            ctx.clip();
            for (const [l, i] of marked) {
              const p = (u.series[i] as unknown as { _paths?: { stroke?: Path2D } })._paths?.stroke;
              if (!p) continue;
              ctx.strokeStyle = accent;
              ctx.lineWidth = 2.25 * uPlot.pxRatio;
              ctx.lineJoin = "round";
              ctx.setLineDash(dashFor(l.groupIndex).map((d) => d * uPlot.pxRatio));
              ctx.stroke(p);
            }
            ctx.restore();
          },
        ],
      },
    };
    const u = new uPlot(opts, [data.x, ...ys] as uPlot.AlignedData, el);
    plot.current = u;
    return () => {
      u.destroy();
      plot.current = null;
    };
    // `theme` re-reads the colour tokens.
  }, [data, lines, raw, width, height, log, zoom, x, aggregate, syncKey, theme, clipped]);

  // The readout: every line's value at the cursor (or its last value), sorted best first.
  const best = higherIsBetter(metricLabel(metric).name);
  const rows = useMemo(() => {
    const r = lines.map((l) => {
      const value = cursorIdx !== null ? valueNear(l.y, cursorIdx) : l.last !== undefined ? l.last : lastValue(l.y).v;
      return { line: l, value };
    });
    r.sort((a, b) => {
      if (a.value === null) return 1;
      if (b.value === null) return -1;
      return best ? b.value - a.value : a.value - b.value;
    });
    return r;
  }, [lines, cursorIdx, best]);
  const xAt = data && cursorIdx !== null ? data.x[cursorIdx] : null;

  // Keyboard: arrows move the crosshair, Home and End jump, Escape resets the zoom.
  const [kbdIdx, setKbdIdx] = useState<number | null>(null);
  const onKey = (e: React.KeyboardEvent) => {
    const u = plot.current;
    if (!u || !data || !data.x.length) return;
    const last = data.x.length - 1;
    let next = kbdIdx ?? cursorIdx ?? last;
    const step = e.shiftKey ? Math.max(1, Math.round(data.x.length / 20)) : 1;
    if (e.key === "ArrowRight") next = Math.min(last, next + step);
    else if (e.key === "ArrowLeft") next = Math.max(0, next - step);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = last;
    else if (e.key === "Escape" && zoom) {
      setZoom(null);
      e.preventDefault();
      return;
    } else return;
    e.preventDefault();
    setKbdIdx(next);
    u.setCursor({ left: u.valToPos(data.x[next], "x"), top: u.bbox.height / uPlot.pxRatio / 2 });
  };

  const { name, context } = metricLabel(metric);
  const loading = query.isPending && runs.length > 0;
  const shown = rows.slice(0, aggregate ? rows.length : 8);
  const hiddenCount = rows.length - shown.length;
  const missing = data?.missing.length ?? 0;

  return (
    <section className={cn("@container flex min-w-0 flex-col border bg-card", className)} aria-label={`${name}${context ? ` (${context})` : ""}`}>
      <header className="flex min-h-10 items-center gap-2 px-3 pt-2">
        <h3 className="min-w-0 truncate text-[17px] font-bold tracking-[-0.01em]">
          {name}
          {context && <span className="ml-1.5 font-normal text-muted-foreground">{context}</span>}
        </h3>
        {data && !data.exact && (
          <Tooltip content="Drawn from the minimum and maximum of each pixel column, so spikes stay visible. Drag across the chart to zoom in.">
            <span className="text-xs text-muted-foreground">downsampled</span>
          </Tooltip>
        )}
        <span className="ml-auto flex items-center gap-0.5">
          {zoom && (
            <Tooltip content="Reset zoom (Esc)">
              <button type="button" onClick={() => setZoom(null)} className="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground pointer-coarse:size-11" aria-label="Reset zoom">
                <RotateCcw className="size-4" aria-hidden />
              </button>
            </Tooltip>
          )}
          {expandTo && (
            <Tooltip content="Open in Metrics">
              <Link to={expandTo} className="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground pointer-coarse:size-11" aria-label={`Open ${metric} in Metrics`}>
                <Maximize2 className="size-4" />
              </Link>
            </Tooltip>
          )}
        </span>
      </header>
      <div className="flex flex-col gap-2 p-2 pt-1 @2xl:flex-row">
        <div
          ref={wrap}
          className="relative min-w-0 flex-1 rounded-md"
          style={{ height }}
          tabIndex={0}
          role="img"
          aria-label={`${name}${context ? ` on ${context}` : ""} for ${lines.length} ${aggregate ? "groups" : "runs"} by ${X_LABEL[x].toLowerCase()}. Arrow keys move the crosshair; drag to zoom; Escape resets.`}
          onKeyDown={onKey}
          onBlur={() => setKbdIdx(null)}
          onDoubleClick={() => setZoom(null)}
        >
          {loading && <Skeleton className="absolute inset-0" />}
          {query.isError && (
            <div className="absolute inset-0 flex items-center justify-center gap-2 p-4 text-sm text-danger">
              <TriangleAlert className="size-4 shrink-0" aria-hidden />
              {(query.error as Error).message}
            </div>
          )}
          {data && lines.length === 0 && !query.isError && (
            <div className="absolute inset-0 flex items-center justify-center p-4 text-center text-sm text-muted-foreground">
              {runs.length === 0 ? "Select runs to draw this metric." : `None of these runs logs ${metric}.`}
            </div>
          )}
          <div ref={plotEl} className={cn(query.isFetching && query.isPlaceholderData && "opacity-70")} />
        </div>
        {lines.length > 0 && (
          <div className="flex min-w-0 flex-col @2xl:w-60 @2xl:shrink-0" aria-live="off">
            <div className="flex items-baseline justify-between gap-2 px-1 pb-1 text-xs text-muted-foreground">
              <span className="truncate">{xAt !== null ? `${X_LABEL[x]} ${xValue(x, xAt)}` : "Last value"}</span>
              {lines.length > 1 && <span>{best ? "highest first" : "lowest first"}</span>}
            </div>
            <ol className={cn("min-h-0 overflow-y-auto", "@2xl:max-h-[calc(var(--h)-1.5rem)]")} style={{ ["--h" as string]: `${height}px` }}>
              {shown.map(({ line, value }) => {
                const isPinned = pinned?.has(line.id);
                const isFocus = focusId === line.id;
                return (
                  <li key={line.id}>
                    <button
                      type="button"
                      onClick={() => onPin?.(line.id)}
                      onMouseEnter={() => {
                        focusRef.current = line.id;
                        setFocusId(line.id);
                        redraw();
                      }}
                      onMouseLeave={() => {
                        focusRef.current = null;
                        setFocusId(null);
                        redraw();
                      }}
                      disabled={!onPin || aggregate}
                      aria-pressed={onPin && !aggregate ? !!isPinned : undefined}
                      title={aggregate ? undefined : isPinned ? "Unpin this run" : "Pin this run in the accent"}
                      className={cn(
                        "grid min-h-6 w-full grid-cols-[18px_minmax(4.5rem,auto)_1fr_14px] items-center gap-x-2 rounded-sm px-1 py-0.5 text-left text-sm disabled:cursor-default pointer-coarse:min-h-11",
                        (isFocus || isPinned) && "bg-muted",
                        onPin && !aggregate && "hover:bg-muted",
                      )}
                    >
                      <svg width="18" height="8" aria-hidden className={isFocus || isPinned ? "text-brand" : aggregate ? "text-series-strong" : "text-muted-foreground"}>
                        <line x1="1" y1="4" x2="17" y2="4" stroke="currentColor" strokeWidth="2" strokeDasharray={dashFor(line.groupIndex).join(" ") || undefined} strokeLinecap="round" />
                      </svg>
                      <span className="nums truncate text-right font-semibold" title={numFull(value)}>
                        {num(value)}
                      </span>
                      <span className="min-w-0 truncate text-muted-foreground">
                        {aggregate ? (
                          <>
                            <span className="text-foreground">
                              {groupName && line.label !== "All runs" ? `${groupName} ${line.label}` : line.label}
                            </span>
                            {line.runs !== undefined && <span className="ml-1.5 text-xs">{line.runs === 1 ? "1 run" : `${line.runs} runs`}</span>}
                          </>
                        ) : (
                          <>
                            {line.label}
                            {line.group !== null && <span className="ml-1.5 text-xs">{groupName ? `${groupName} ${line.group}` : line.group}</span>}
                          </>
                        )}
                      </span>
                      {isPinned ? <Pin className="size-3.5 text-brand" aria-hidden /> : <span />}
                    </button>
                  </li>
                );
              })}
            </ol>
            {(hiddenCount > 0 || missing > 0) && (
              <p className="px-1 pt-1 text-xs text-muted-foreground">
                {hiddenCount > 0 && `${hiddenCount} more runs. `}
                {missing > 0 && `${missing} selected ${missing === 1 ? "run does" : "runs do"} not log this metric.`}
              </p>
            )}
            {nonPositive && logY && <p className="px-1 pt-1 text-xs text-warning">Log scale needs positive values; showing linear.</p>}
          </div>
        )}
      </div>
    </section>
  );
}
