import { useEffect, useMemo, useRef, useState } from "react";
import { cn } from "@/lib/utils";

/**
 * Editorial line charts drawn as SVG: ink and grey lines, ink blue only for the subject, labels at the
 * line ends instead of a legend, hairline grid, an ink base line. Colours are CSS variables, so both
 * themes come from the tokens.
 */
export interface Line {
  id: string;
  /** Text at the line end, e.g. "A 0.954". */
  end?: string;
  /** Name for the readout and the accessible description. */
  name: string;
  color: string;
  width?: number;
  dash?: string;
  points: [number, number | null][];
  /** Square markers on every point; `markerColor` may colour single points (drifted runs). */
  markers?: boolean;
  markerColor?: (i: number) => string | undefined;
  bold?: boolean;
}

export interface BandPoint {
  x: number;
  lo: number | null;
  hi: number | null;
}

interface Props {
  lines: Line[];
  height?: number;
  yDomain?: [number, number];
  xFormat?: (v: number) => string;
  yFormat?: (v: number) => string;
  /** Ticks on the x axis; defaults to nice numbers over the domain. */
  xTicks?: number[];
  band?: BandPoint[];
  bandLabel?: string;
  /** A vertical rule with a label, such as the selected run. */
  marker?: { x: number; label: string; sub?: string } | null;
  label: string;
  /** Called with the x the reader moves to (pointer or arrow keys); null when leaving. */
  onHover?: (x: number | null) => void;
  /** When given, clicks and arrow keys select a point of the first line. */
  onSelect?: (x: number) => void;
  selected?: number | null;
  className?: string;
}

export function niceTicks(lo: number, hi: number, n = 5): number[] {
  if (!isFinite(lo) || !isFinite(hi)) return [];
  if (hi === lo) return [lo];
  const span = hi - lo;
  const raw = span / n;
  const mag = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => span / s <= n) ?? mag * 10;
  const out: number[] = [];
  for (let v = Math.ceil(lo / step - 1e-9) * step; v <= hi + step * 1e-9; v += step) out.push(Math.round(v / step) * step);
  return out;
}

function niceDomain(lo: number, hi: number): [number, number] {
  if (hi === lo) return [lo - 1, hi + 1];
  const t = niceTicks(lo, hi, 4);
  const step = t.length > 1 ? t[1] - t[0] : (hi - lo) / 4;
  return [Math.floor(lo / step + 1e-9) * step, Math.ceil(hi / step - 1e-9) * step];
}

export function useWidth<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setWidth(Math.floor(e.contentRect.width)));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, width] as const;
}

const fmt = (v: number) => (Math.abs(v) >= 1000 ? `${+(v / 1000).toFixed(1)}k` : String(+v.toFixed(3)));

export function LineChart({ lines, height = 300, yDomain, xFormat = fmt, yFormat, xTicks, band, bandLabel, marker, label, onHover, onSelect, selected, className }: Props) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const [hover, setHover] = useState<number | null>(null);
  const hasEnds = lines.some((l) => l.end);
  const m = { l: 44, r: hasEnds ? (width < 480 ? 64 : 92) : 12, t: marker ? 34 : 12, b: 26 };

  const { xs, x0, x1, y0, y1 } = useMemo(() => {
    const all = lines.flatMap((l) => l.points);
    const xsAll = all.map((p) => p[0]);
    const ys = all.map((p) => p[1]).filter((v): v is number => v !== null && isFinite(v));
    for (const b of band ?? []) for (const v of [b.lo, b.hi]) if (v !== null && isFinite(v)) ys.push(v);
    const [d0, d1] = yDomain ?? niceDomain(Math.min(...ys), Math.max(...ys));
    const first = lines[0]?.points.map((p) => p[0]) ?? [];
    return { xs: first, x0: Math.min(...xsAll), x1: Math.max(...xsAll), y0: d0, y1: d1 };
  }, [lines, band, yDomain]);

  const iw = Math.max(10, width - m.l - m.r);
  const ih = Math.max(10, height - m.t - m.b);
  const sx = (v: number) => m.l + (x1 === x0 ? iw / 2 : ((v - x0) / (x1 - x0)) * iw);
  const sy = (v: number) => m.t + ih - ((v - y0) / (y1 - y0 || 1)) * ih;
  const yt = niceTicks(y0, y1, height < 200 ? 3 : 4);
  const xt = xTicks ?? niceTicks(x0, x1, width < 480 ? 3 : 6);
  const decimals = [0, 1, 2, 3, 4].find((d) => yt.every((v) => Math.abs(+v.toFixed(d) - v) < 1e-9)) ?? 4;
  const yf = yFormat ?? ((v: number) => v.toFixed(decimals));

  const path = (pts: [number, number | null][]) => {
    let d = "";
    let pen = false;
    for (const [x, y] of pts) {
      // Series sampled at other runs' steps have holes; draw across them.
      if (y === null || !isFinite(y)) continue;
      d += `${pen ? "L" : "M"}${sx(x).toFixed(1)} ${sy(Math.max(y0, Math.min(y1, y))).toFixed(1)}`;
      pen = true;
    }
    return d;
  };

  // End labels: at each line's last value, pushed apart so they never overlap.
  const ends = useMemo(() => {
    const items = lines
      .map((l) => {
        const last = [...l.points].reverse().find((p) => p[1] !== null && isFinite(p[1]!));
        return last && l.end ? { l, x: sx(last[0]), y: sy(last[1]!), ly: sy(last[1]!) } : null;
      })
      .filter((v): v is NonNullable<typeof v> => v !== null)
      .sort((a, b) => a.y - b.y);
    const gap = 15;
    for (let i = 1; i < items.length; i++) items[i].ly = Math.max(items[i].ly, items[i - 1].ly + gap);
    const over = items.length ? items[items.length - 1].ly - (m.t + ih) : 0;
    if (over > 0) for (const it of items) it.ly -= over;
    for (let i = items.length - 2; i >= 0; i--) items[i].ly = Math.min(items[i].ly, items[i + 1].ly - gap);
    return items;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lines, width, height, x0, x1, y0, y1]);

  const nearest = (px: number) => {
    if (!xs.length) return null;
    let best = 0;
    for (let i = 1; i < xs.length; i++) if (Math.abs(sx(xs[i]) - px) < Math.abs(sx(xs[best]) - px)) best = i;
    return xs[best];
  };
  const move = (x: number | null) => {
    setHover(x);
    onHover?.(x);
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (!xs.length) return;
    const cur = onSelect ? (selected ?? xs[xs.length - 1]) : (hover ?? xs[xs.length - 1]);
    let i = xs.indexOf(cur);
    if (i < 0) i = xs.length - 1;
    const step = e.shiftKey ? 10 : 1;
    if (e.key === "ArrowLeft") i = Math.max(0, i - step);
    else if (e.key === "ArrowRight") i = Math.min(xs.length - 1, i + step);
    else if (e.key === "Home") i = 0;
    else if (e.key === "End") i = xs.length - 1;
    else if (e.key === "Escape") return move(null);
    else return;
    e.preventDefault();
    if (onSelect) onSelect(xs[i]);
    else move(xs[i]);
  };
  const focusX = hover ?? (onSelect ? (selected ?? null) : null);

  return (
    <div
      ref={ref}
      className={cn("relative w-full touch-pan-y outline-offset-4", onSelect && "cursor-pointer", className)}
      style={{ height }}
      tabIndex={0}
      role="img"
      aria-label={label}
      onKeyDown={onKey}
      onBlur={() => move(null)}
    >
      {width > 0 && (
        <svg
          width={width}
          height={height}
          className="block select-none overflow-visible"
          onPointerMove={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            move(nearest(e.clientX - r.left));
          }}
          onPointerLeave={() => move(null)}
          onClick={(e) => {
            if (!onSelect) return;
            const r = e.currentTarget.getBoundingClientRect();
            const x = nearest(e.clientX - r.left);
            if (x !== null) onSelect(x);
          }}
        >
          {yt.map((v) => (
            <g key={v}>
              <line x1={m.l} x2={m.l + iw} y1={sy(v)} y2={sy(v)} stroke="var(--border)" strokeWidth={1} />
              <text x={m.l - 8} y={sy(v)} dy="0.32em" textAnchor="end" className="fill-faint font-mono text-[11px]">
                {yf(v)}
              </text>
            </g>
          ))}
          <line x1={m.l} x2={m.l + iw} y1={m.t + ih} y2={m.t + ih} stroke="var(--rule)" strokeWidth={1.25} />
          {xt.map((v) => (
            <g key={v}>
              <line x1={sx(v)} x2={sx(v)} y1={m.t + ih} y2={m.t + ih + 4} stroke="var(--rule)" strokeWidth={1} />
              <text x={sx(v)} y={m.t + ih + 17} textAnchor="middle" className="fill-faint font-mono text-[11px]">
                {xFormat(v)}
              </text>
            </g>
          ))}
          {band && band.length > 1 && (
            <path
              d={(() => {
                const pts = band.filter((b) => b.lo !== null && b.hi !== null);
                if (pts.length < 2) return "";
                const top = pts.map((b, i) => `${i ? "L" : "M"}${sx(b.x).toFixed(1)} ${sy(b.hi!).toFixed(1)}`).join("");
                const bottom = [...pts].reverse().map((b) => `L${sx(b.x).toFixed(1)} ${sy(b.lo!).toFixed(1)}`).join("");
                return `${top}${bottom}Z`;
              })()}
              fill="var(--cell)"
              opacity={0.85}
            >
              {bandLabel && <title>{bandLabel}</title>}
            </path>
          )}
          {marker && (
            <g>
              <line x1={sx(marker.x)} x2={sx(marker.x)} y1={m.t - 26} y2={m.t + ih} stroke="var(--rule)" strokeWidth={1} />
              <text x={sx(marker.x) + 6} y={m.t - 18} textAnchor={sx(marker.x) > width - 160 ? "end" : "start"} dx={sx(marker.x) > width - 160 ? -12 : 0} className="fill-foreground text-[12px] font-bold">
                {marker.label}
              </text>
              {marker.sub && (
                <text x={sx(marker.x) + 6} y={m.t - 4} textAnchor={sx(marker.x) > width - 160 ? "end" : "start"} dx={sx(marker.x) > width - 160 ? -12 : 0} className="fill-faint text-[11px]">
                  {marker.sub}
                </text>
              )}
            </g>
          )}
          {[...lines].reverse().map((l) => (
            <g key={l.id}>
              <path d={path(l.points)} fill="none" stroke={l.color} strokeWidth={l.width ?? 1.5} strokeDasharray={l.dash} strokeLinejoin="round" strokeLinecap="round" />
              {l.markers &&
                l.points.map(([x, y], i) =>
                  y === null ? null : <rect key={i} x={sx(x) - 3} y={sy(y) - 3} width={6} height={6} fill={l.markerColor?.(i) ?? l.color} />,
                )}
            </g>
          ))}
          {ends.map((e) => (
            <g key={e.l.id}>
              <polyline points={`${e.x + 3},${e.y} ${e.x + 9},${e.y} ${e.x + 14},${e.ly}`} fill="none" stroke="var(--series)" strokeWidth={1} />
              <text x={e.x + 17} y={e.ly} dy="0.32em" className={cn("font-mono text-[11.5px]", e.l.bold ? "font-bold" : "")} style={{ fill: e.l.color, paintOrder: "stroke", stroke: "var(--background)", strokeWidth: 3, strokeLinejoin: "round" }}>
                {e.l.end}
              </text>
            </g>
          ))}
          {focusX !== null && (
            <g pointerEvents="none">
              <line x1={sx(focusX)} x2={sx(focusX)} y1={m.t} y2={m.t + ih} stroke="var(--muted-foreground)" strokeWidth={1} strokeDasharray="2 3" />
              {lines.map((l) => {
                const p = l.points.find((q) => q[0] === focusX);
                return p && p[1] !== null ? <rect key={l.id} x={sx(p[0]) - 4} y={sy(p[1]) - 4} width={8} height={8} fill="var(--background)" stroke={l.color} strokeWidth={1.5} /> : null;
              })}
            </g>
          )}
        </svg>
      )}
    </div>
  );
}

/** A small loss curve for a table row. All rows share `domain`, so the lines compare. */
export function Sparkline({ values, domain, color, width = 120, height = 26, label }: { values: (number | null)[]; domain: [number, number]; color: string; width?: number; height?: number; label: string }) {
  const n = values.length;
  const [lo, hi] = domain;
  let d = "";
  let pen = false;
  values.forEach((v, i) => {
    if (v === null || !isFinite(v)) return;
    const x = n > 1 ? (i / (n - 1)) * (width - 2) + 1 : width / 2;
    const y = 2 + (height - 6) * (1 - (Math.max(lo, Math.min(hi, v)) - lo) / (hi - lo || 1));
    d += `${pen ? "L" : "M"}${x.toFixed(1)} ${y.toFixed(1)}`;
    pen = true;
  });
  return (
    <svg width={width} height={height} className="block" role="img" aria-label={label}>
      <line x1={0} x2={width} y1={height - 0.5} y2={height - 0.5} stroke="var(--border)" />
      <path d={d} fill="none" stroke={color} strokeWidth={1.5} strokeLinejoin="round" />
    </svg>
  );
}
