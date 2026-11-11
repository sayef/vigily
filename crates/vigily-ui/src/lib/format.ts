import type { Json, XAxis } from "@/lib/api";

const sig4 = new Intl.NumberFormat(undefined, { maximumSignificantDigits: 4 });
const full = new Intl.NumberFormat(undefined, { maximumSignificantDigits: 15 });
const int = new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 });
const compactFmt = new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 });

/** A metric or parameter value in four significant digits; scientific when very small or large. */
export function num(v: number | null | undefined): string {
  if (v === null || v === undefined) return "–";
  if (Number.isNaN(v)) return "NaN";
  if (!Number.isFinite(v)) return v > 0 ? "∞" : "−∞";
  const a = Math.abs(v);
  if (a !== 0 && (a < 1e-4 || a >= 1e7)) return v.toExponential(2).replace("e", "e").replace("+", "");
  return sig4.format(v);
}

/** Every digit, for tooltips. */
export const numFull = (v: number | null | undefined) => (v === null || v === undefined ? "–" : full.format(v));

export const integer = (v: number) => int.format(v);
export const compact = (n: number) => compactFmt.format(n);

/** A parameter value as text. */
export function value(v: Json | undefined): string {
  if (v === undefined) return "";
  if (v === null) return "null";
  // Configuration is exact: integers in full, other numbers to four significant digits.
  if (typeof v === "number") return Number.isInteger(v) && Math.abs(v) < 1e15 ? int.format(v) : num(v);
  if (typeof v === "string") return v;
  if (typeof v === "boolean") return v ? "true" : "false";
  return JSON.stringify(v);
}

/** A duration in seconds: "42 s", "3 m 05 s", "2 h 14 m", "3 d 4 h". */
export function duration(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s}\u00a0s`;
  if (s < 3600) return `${Math.floor(s / 60)}\u00a0m ${String(s % 60).padStart(2, "0")}\u00a0s`;
  if (s < 86400) return `${Math.floor(s / 3600)}\u00a0h ${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}\u00a0m`;
  return `${Math.floor(s / 86400)}\u00a0d ${Math.floor((s % 86400) / 3600)}\u00a0h`;
}

/** How long ago a time in microseconds was: "just now", "4 min ago", "3 h ago", "12 d ago". */
export function ago(micros: number | null | undefined, now = Date.now()): string {
  if (!micros) return "";
  const s = Math.max(0, (now - micros / 1000) / 1000);
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.max(1, Math.round(s / 60))}\u00a0min ago`;
  if (s < 86400) return `${Math.round(s / 3600)}\u00a0h ago`;
  if (s < 86400 * 60) return `${Math.round(s / 86400)}\u00a0d ago`;
  return date(micros);
}

const dateFmt = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short", year: "numeric" });
const dateTimeFmt = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit" });

export const date = (micros: number) => dateFmt.format(new Date(micros / 1000));
const shortFmt = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short" });
/** Day and month only, for axis ticks. */
export const shortDate = (micros: number) => shortFmt.format(new Date(micros / 1000));
export const dateTime = (micros: number) => dateTimeFmt.format(new Date(micros / 1000));
/** The exact time in UTC, for tooltips and copying. */
export const utc = (micros: number) => new Date(micros / 1000).toISOString().replace("T", " ").replace(/\.\d+Z$/, " UTC");

/** An x-axis value as text. */
export function xValue(axis: XAxis, v: number): string {
  switch (axis) {
    case "step":
    case "epoch":
      return Math.abs(v) >= 10000 ? compact(v) : int.format(v);
    case "relative":
      return duration(v);
    case "wall":
      return dateTimeFmt.format(new Date(v * 1000));
  }
}

export const X_LABEL: Record<XAxis, string> = { step: "Step", epoch: "Epoch", relative: "Time since start", wall: "Wall time" };

/** Higher is better for metrics named like accuracy or reward; lower for everything else. */
export const higherIsBetter = (metric: string) => /acc|auc|f1|precision|recall|score|reward|iou|map\b|bleu|rouge|r2/i.test(metric);

/** A metric key for display: `loss[val]` reads "loss (val)". */
export function metricLabel(key: string): { name: string; context: string | null } {
  const m = key.match(/^(.*)\[(.*)\]$/);
  return m ? { name: m[1], context: m[2] } : { name: key, context: null };
}

const ms1 = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });
const s2 = new Intl.NumberFormat(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });

/** A latency in microseconds: "0.4 ms", "524 ms", "1.65 s", then minutes like `duration`. */
export function latency(us: number | null | undefined): string {
  if (us === null || us === undefined) return "–";
  const ms = us / 1000;
  if (ms < 10) return `${ms1.format(ms)}\u00a0ms`;
  if (ms < 1000) return `${int.format(ms)}\u00a0ms`;
  if (ms < 60_000) return `${s2.format(ms / 1000)}\u00a0s`;
  return duration(ms / 1000);
}

const money2 = new Intl.NumberFormat(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
const moneySmall = new Intl.NumberFormat(undefined, { maximumSignificantDigits: 2, maximumFractionDigits: 8 });

/** An amount with its currency code: "EUR 1.24", "EUR 0.00037" (two significant digits under 1). */
export function money(v: number, currency: string): string {
  const a = Math.abs(v);
  return `${currency}\u00a0${a === 0 ? "0" : a >= 1 ? money2.format(v) : moneySmall.format(v)}`;
}

/** Every digit of an amount, for tooltips. */
export const moneyFull = (v: number, currency: string) => `${currency} ${full.format(v)}`;
