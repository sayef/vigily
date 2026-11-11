/** The vigily HTTP API (crates/vigily-server). Times are microseconds since the Unix epoch. */

export type Status = "running" | "finished" | "failed" | "aborted";
export type XAxis = "step" | "epoch" | "relative" | "wall";
export type Json = null | boolean | number | string | Json[] | { [k: string]: Json };

export interface MetricStat {
  last: number | null;
  min: number | null;
  max: number | null;
  count: number;
  last_step: number | null;
  min_step: number | null;
  max_step: number | null;
}

export interface RunRow {
  id: string;
  name: string;
  has_name: boolean;
  experiment: string | null;
  description: string | null;
  tags: string[];
  status: Status;
  /** `training`, `eval` or another kind. */
  kind: string;
  /** What the run trains or evaluates (model, prompt, endpoint, git commit), or null. */
  target: Record<string, Json> | null;
  archived: boolean;
  created_at: number;
  updated_at: number;
  ended_at: number | null;
  /** The last heartbeat, metadata write or point (microseconds, UTC). */
  last_seen: number;
  /** Running, but nothing heard for 10 minutes: the process most likely died. */
  stale: boolean;
  duration: number;
  params: Record<string, Json>;
  metrics: Record<string, MetricStat>;
}

export interface Summary {
  count: number;
  non_finite: number;
  first_step: number | null;
  last_step: number | null;
  last: number | null;
  min: number | null;
  min_step: number | null;
  max: number | null;
  max_step: number | null;
  last_epoch: number | null;
  first_time: number | null;
  last_time: number | null;
}

export interface SeriesEntry {
  id: string;
  run_id: string;
  name: string;
  context: Record<string, Json>;
  key: string;
  summary: Summary;
}

export interface RunDetail extends RunRow {
  params_tree: Record<string, Json>;
  series: SeriesEntry[];
}

export interface ExperimentInfo {
  name: string;
  runs: number;
  running: number;
  finished: number;
  failed: number;
  aborted: number;
  archived: number;
  first_created: number | null;
  last_created: number | null;
  metrics: string[];
}

export interface MetricInfo {
  key: string;
  name: string;
  context: Record<string, Json>;
  runs: number;
  /** How many of those runs are eval runs. */
  eval_runs: number;
}

export interface ParamInfo {
  key: string;
  kind: "number" | "text" | "bool" | "list" | "null" | "mixed";
  runs: number;
  distinct: number;
  values: Json[];
}

export interface Meta {
  version: string;
  repository: string;
  runs: number;
  running: number;
  archived: number;
  /** Runs per kind. */
  kinds: Record<string, number>;
  datasets: number;
  schedules: number;
  open_alerts: number;
  /** Stored traces; absent on servers without tracing. */
  traces?: number;
  refreshed_at: number;
  experiments: ExperimentInfo[];
  metrics: MetricInfo[];
  params: ParamInfo[];
}

export interface RunsResponse {
  total: number;
  rows: RunRow[];
  refreshed_at: number;
}

export interface ChartRequest {
  runs: string[];
  metric: string;
  x?: XAxis;
  range?: [number, number] | null;
  points?: number;
  group_by?: string | null;
  aggregate?: boolean;
}

export interface ChartSeries {
  run_id: string;
  name: string;
  group: string | null;
  y: (number | null)[];
  last: [number, number] | null;
}

export interface ChartGroup {
  label: string;
  runs: number;
  mean: (number | null)[];
  min: (number | null)[];
  max: (number | null)[];
  n: number[];
}

export interface ChartData {
  metric: string;
  x_axis: XAxis;
  x: number[];
  exact: boolean;
  domain: [number, number] | null;
  series: ChartSeries[];
  groups: ChartGroup[];
  missing: string[];
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
    readonly position?: number,
  ) {
    super(message);
  }
}

async function request<T>(method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  let res: Response;
  try {
    res = await fetch(path, {
      method,
      signal,
      headers: body === undefined ? undefined : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch (e) {
    if ((e as Error).name === "AbortError") throw e;
    throw new ApiError(0, "The vigily server did not answer. Check that `vigily up` is still running.");
  }
  if (!res.ok) {
    let message = `The server answered ${res.status}.`;
    let position: number | undefined;
    try {
      const j = await res.json();
      message = j.error ?? message;
      position = j.position;
    } catch {
      // not JSON
    }
    throw new ApiError(res.status, message, position);
  }
  return res.json() as Promise<T>;
}

export const api = {
  meta: (signal?: AbortSignal) => request<Meta>("GET", "/api/meta", undefined, signal),
  runs: (p: { q: string; sort?: string | null; desc?: boolean; archived?: boolean }, signal?: AbortSignal) => {
    const s = new URLSearchParams({ q: p.q });
    if (p.sort) s.set("sort", p.sort);
    if (p.desc) s.set("desc", "true");
    if (p.archived) s.set("archived", "true");
    return request<RunsResponse>("GET", `/api/runs?${s}`, undefined, signal);
  },
  run: (id: string, signal?: AbortSignal) => request<RunDetail>("GET", `/api/runs/${encodeURIComponent(id)}`, undefined, signal),
  patchRun: (id: string, patch: Partial<Pick<RunRow, "name" | "experiment" | "description" | "tags" | "archived">> & { status?: "aborted" }) =>
    request<RunRow>("PATCH", `/api/runs/${encodeURIComponent(id)}`, patch),
  chart: (req: ChartRequest, signal?: AbortSignal) => request<ChartData>("POST", "/api/chart", req, signal),
  /** Deletes runs that are not running; the server refuses the whole request otherwise. */
  deleteRuns: (ids: string[]) => request<{ deleted: number }>("POST", "/api/runs/delete", { ids }),
};

/** Parameters and metrics whose names start with this are captured by the SDK, not set by the user. */
export const SYSTEM = "__system__";
/** Reserved keys (`__system__`, `__run__`) are shown in their own places, not as parameters. */
export const isSystem = (key: string) => key.startsWith("__");

/** A target's short name: its model, name or reference, else every value. */
export function targetLabel(t: Record<string, Json> | null): string {
  if (!t) return "";
  for (const k of ["model", "name", "ref"]) if (typeof t[k] === "string") return t[k] as string;
  return targetText(t);
}

/** A target reference as one line: `model clf-v2, git 4f2c1ab`. */
export function targetText(t: Record<string, Json> | null): string {
  if (!t) return "";
  return Object.entries(t)
    .map(([k, v]) => (k === "ref" ? String(v) : `${k} ${typeof v === "string" ? v : JSON.stringify(v)}`))
    .join(", ");
}

// ---------------------------------------------------------------------------------------------
// Evaluations

export interface DatasetInfo {
  name: string;
  description: string | null;
  created_at: number;
  updated_at: number;
  latest: number;
  items: number;
}

export interface VersionSummary {
  version: number;
  created_at: number;
  note: string | null;
  digest: string;
  changes: { added: number; updated: number; removed: number };
  source: string | null;
  storage_version: number | null;
  items: number;
}

export interface Item {
  id: string;
  hash: string;
  input: Json;
  expected: Json;
  metadata: Record<string, Json> | null;
}

export interface DatasetResponse {
  info: DatasetInfo;
  version: VersionSummary;
  versions: VersionSummary[];
  total: number;
  items: Item[];
  runs: { id: string; name: string; created_at: number; version: number; target: Record<string, Json> | null }[];
}

export interface Score {
  name: string;
  value: number | null;
  passed: boolean | null;
  reason: string | null;
  evaluator: string;
  evaluator_version: string;
}

export interface Label {
  run_id: string;
  item_id: string;
  name: string;
  value: number | null;
  passed: boolean | null;
  comment: string | null;
  created_at: number;
}

export interface ItemRow {
  item_id: string;
  input: Json;
  expected: Json;
  metadata: Json;
  output: Json;
  error: string | null;
  latency_ms: number | null;
  scores: Score[];
  labels: Label[];
  failed: boolean;
  /** The trace recorded while the target answered this item, if any. */
  trace_id?: string | null;
}

export interface ItemsResponse {
  run_id: string;
  dataset: { name: string; version: number; digest: string; items: number } | null;
  total: number;
  failed: number;
  errors: number;
  scores: Record<string, { mean: number | null; pass_rate: number | null; n: number }>;
  items: ItemRow[];
}

export type Change = "regressed" | "fixed" | "changed" | "both_failed" | "both_passed" | "only_a" | "only_b";

export interface CompareResponse {
  a: { id: string; name: string; created_at: number; target: Record<string, Json> | null; dataset: Json };
  b: { id: string; name: string; created_at: number; target: Record<string, Json> | null; dataset: Json };
  counts: Partial<Record<Change, number>>;
  items: {
    item_id: string;
    input: Json;
    expected: Json;
    a: { output: Json; error: string | null; scores: Score[]; failed: boolean } | null;
    b: { output: Json; error: string | null; scores: Score[]; failed: boolean } | null;
    change: Change;
  }[];
}

export type ScheduleStatus = "running" | "succeeded" | "failed" | "timed_out" | "skipped_overlap" | "skipped_missed" | "interrupted";

export interface ScheduleRunSummary {
  id: string;
  schedule: string;
  scheduled_for: number;
  started_at: number;
  ended_at: number | null;
  attempt: number;
  status: ScheduleStatus;
  exit_code: number | null;
  reason: string | null;
  has_output: boolean;
  eval_runs?: { id: string; name: string; status: Status; target: Record<string, Json> | null }[];
}

export interface ScheduleRunDetail extends ScheduleRunSummary {
  command: string[];
  stdout: string;
  stderr: string;
}

export interface Schedule {
  name: string;
  description: string | null;
  cron: string;
  timezone: string;
  command: string[];
  workdir: string;
  timeout: string;
  retries: number;
  missed: "skip" | "run_once";
  enabled: boolean;
  next_run: number | null;
  last_run: ScheduleRunSummary | null;
  recent: ScheduleRunSummary[];
  runs: number;
}

export type Level = "stable" | "moderate" | "drift";

export interface SuiteRun {
  run_id: string;
  name: string;
  created_at: number;
  status: Status;
  dataset_version: number | null;
  schedule: string | null;
  scores: Record<string, number>;
}

export interface Baseline {
  mode: "rolling" | "pinned";
  window: number;
  runs: string[];
  updated_at: number;
}

export interface SuiteSummary {
  key: string;
  label: string;
  experiment: string | null;
  target: Record<string, Json> | null;
  dataset: string | null;
  schedule: string | null;
  runs: number;
  scores: string[];
  latest: SuiteRun | null;
  baseline: Baseline;
  behaviour: { score: string; drifted: boolean; delta: number; p: number | null; regressions: number }[];
  data: Level | null;
}

export interface HistoryPoint {
  run_id: string;
  created_at: number;
  value: number | null;
  baseline_mean: number | null;
  baseline_low: number | null;
  baseline_high: number | null;
  drifted: boolean;
  p: number | null;
  regressions: number;
}

export interface BehaviourDrift {
  score: string;
  run_id: string;
  baseline_runs: string[];
  higher_is_better: boolean;
  /** Items matched and tested in pairs; false for the unpaired fallback. */
  paired: boolean;
  n_current: number;
  n_baseline: number;
  current_mean: number;
  baseline_mean: number;
  baseline_std: number;
  delta: number;
  ci: [number, number] | null;
  test: { name: "wilcoxon signed-rank" | "mann-whitney u"; statistic: number; z: number; p: number } | null;
  worse: boolean;
  drifted: boolean;
  previous_run: string | null;
  regressions: { item_id: string; before: number | null; after: number | null }[];
  fixed: { item_id: string }[];
  regression_inputs: Json[];
}

export interface SuiteDetail {
  key: string;
  label: string;
  experiment: string | null;
  target: Record<string, Json> | null;
  dataset: string | null;
  schedule: string | null;
  scores: string[];
  runs: SuiteRun[];
  baseline: Baseline;
  config: { alpha: number; min_effect: number };
  history: Record<string, HistoryPoint[]>;
  selected: string | null;
  drift: BehaviourDrift[];
}

export interface FeatureDrift {
  feature: string;
  kind: "numeric" | "categorical";
  n_baseline: number;
  n_current: number;
  psi: number | null;
  ks: { d: number; p: number } | null;
  js: number | null;
  chi_squared: { statistic: number; df: number; p: number } | null;
  level: Level;
  note: string | null;
  bins: { label: string; baseline: number; current: number }[];
}

export interface DataDriftResponse {
  run_id: string;
  baseline_runs: string[];
  features: FeatureDrift[];
  thresholds: { psi_moderate: number; psi_drift: number; js_moderate: number; js_drift: number };
}

export interface Alert {
  id: string;
  kind: "behaviour_drift" | "data_drift" | "schedule_failed";
  severity: "warning" | "critical";
  title: string;
  detail: string;
  suite: string | null;
  run_id: string | null;
  schedule: string | null;
  created_at: number;
  acknowledged_at: number | null;
  delivered: boolean | null;
}

const enc = encodeURIComponent;

export const evalApi = {
  datasets: (signal?: AbortSignal) => request<DatasetInfo[]>("GET", "/api/datasets", undefined, signal),
  dataset: (name: string, p: { version?: number | null; q?: string }, signal?: AbortSignal) => {
    const s = new URLSearchParams();
    if (p.version) s.set("version", String(p.version));
    if (p.q) s.set("q", p.q);
    return request<DatasetResponse>("GET", `/api/datasets/${enc(name)}?${s}`, undefined, signal);
  },
  items: (run: string, signal?: AbortSignal) => request<ItemsResponse>("GET", `/api/runs/${enc(run)}/items`, undefined, signal),
  label: (run: string, l: { item_id: string; name: string; value?: number | null; passed?: boolean | null; comment?: string | null }) =>
    request<Label>("POST", `/api/runs/${enc(run)}/labels`, l),
  audit: (run: string, signal?: AbortSignal) => request<Record<string, Json>>("GET", `/api/runs/${enc(run)}/audit`, undefined, signal),
  compare: (a: string, b: string, signal?: AbortSignal) => request<CompareResponse>("GET", `/api/compare?a=${enc(a)}&b=${enc(b)}`, undefined, signal),
  schedules: (signal?: AbortSignal) => request<{ config_path: string | null; schedules: Schedule[] }>("GET", "/api/schedules", undefined, signal),
  scheduleRuns: (name: string, signal?: AbortSignal) => request<{ runs: ScheduleRunSummary[] }>("GET", `/api/schedules/${enc(name)}/runs`, undefined, signal),
  scheduleRun: (name: string, id: string, signal?: AbortSignal) => request<ScheduleRunDetail>("GET", `/api/schedules/${enc(name)}/runs/${enc(id)}`, undefined, signal),
  suites: (signal?: AbortSignal) => request<SuiteSummary[]>("GET", "/api/suites", undefined, signal),
  suite: (key: string, run: string | null, signal?: AbortSignal) =>
    request<SuiteDetail>("GET", `/api/suites/${enc(key)}${run ? `?run=${enc(run)}` : ""}`, undefined, signal),
  setBaseline: (key: string, b: Baseline) => request<Baseline>("PUT", `/api/suites/${enc(key)}/baseline`, b),
  dataDrift: (key: string, run: string | null, signal?: AbortSignal) =>
    request<DataDriftResponse>("GET", `/api/suites/${enc(key)}/data-drift${run ? `?run=${enc(run)}` : ""}`, undefined, signal),
  alerts: (signal?: AbortSignal) => request<Alert[]>("GET", "/api/alerts", undefined, signal),
  ackAlert: (id: string, acknowledged: boolean) => request<Alert>("POST", `/api/alerts/${enc(id)}`, { acknowledged }),
};

/** How much of a span's inputs and outputs the project stores. */
export type Payloads = "full" | "truncated" | "metadata";

export interface TraceRow {
  id: string;
  name: string;
  root: string;
  run_id: string | null;
  run_name: string | null;
  eval_item: string | null;
  project: string | null;
  start_time: number;
  end_time: number;
  duration_us: number;
  status: "ok" | "error";
  spans: number;
  errors: number;
  /** Spans that called a model. */
  llm_calls: number;
  input_tokens: number;
  output_tokens: number;
  cached_tokens: number;
  /** In `currency`; null when no model call in the trace has a price. */
  cost: number | null;
  /** Model calls without a configured price. */
  unpriced: number;
  /** Some spans were priced in another currency after a settings change; they are not in `cost`. */
  mixed_currency?: boolean;
  currency: string;
  models: string[];
  payloads: Payloads;
  chunks: number;
}

export interface TracesResponse {
  total: number;
  rows: TraceRow[];
  totals: {
    traces: number;
    errors: number;
    input_tokens: number;
    output_tokens: number;
    cached_tokens: number;
    cost: Record<string, number>;
    unpriced: number;
    p50_us: number | null;
    p95_us: number | null;
  };
  models: string[];
  runs: { id: string; name: string }[];
  payloads: Payloads;
  max_field_bytes: number;
  currency: string;
}

export interface Span {
  span_id: string;
  parent_id: string | null;
  name: string;
  /** `llm`, `tool`, `retrieval`, `chain`, `embedding`, `agent`, `other` or another word. */
  kind: string;
  start_time: number;
  end_time: number;
  status: "ok" | "error" | "unset";
  error: string | null;
  attributes: Record<string, Json>;
  model: string | null;
  provider: string | null;
  input: Json | null;
  output: Json | null;
  usage: { input_tokens?: number | null; output_tokens?: number | null; cached_tokens?: number | null } | null;
  cost: number | null;
  /** The currency of `cost`. */
  currency?: string;
  payloads: Payloads;
  /** Stored under the strictest configured policy because the trace's project was not known yet. */
  provisional?: boolean;
  /** Fields cut to the size limit, with their original size in bytes. */
  truncated?: Record<string, number>;
  /** Attributes left out by the limits or the payload mode. */
  dropped_attributes?: number;
}

export interface TraceDetail {
  trace: TraceRow;
  spans: Span[];
  /** The field limit of the trace's payload policy, in bytes. */
  max_field_bytes?: number;
}

export interface TraceFilter {
  run?: string | null;
  model?: string | null;
  status?: "ok" | "error" | null;
  since?: number | null;
  until?: number | null;
  q?: string | null;
  eval_item?: string | null;
  limit?: number;
  offset?: number;
}

export const traceApi = {
  list: (f: TraceFilter, signal?: AbortSignal) => {
    const s = new URLSearchParams();
    for (const [k, v] of Object.entries(f)) if (v !== null && v !== undefined && v !== "") s.set(k, String(v));
    return request<TracesResponse>("GET", `/api/traces?${s}`, undefined, signal);
  },
  get: (id: string, signal?: AbortSignal) => request<TraceDetail>("GET", `/api/traces/${enc(id)}`, undefined, signal),
};
