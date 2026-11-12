//! LLM traces: spans with timing, status, model, token usage and cost.
//!
//! A trace is a tree of spans that share a trace id. Spans arrive in batches (the Python SDK
//! exports them as they end; an OpenTelemetry exporter sends OTLP/HTTP JSON, see [`from_otlp`]),
//! and [`ingest`] applies the payload policy ([`PayloadMode`]), computes each LLM span's cost from
//! the price table, and stores them in two records tables:
//!
//! - `trace`: one small summary row per trace (`trace/<trace id>`): name, times, status, span and
//!   error counts, token totals, cost and currency, models, and the run and evaluation item it
//!   belongs to. The traces list reads only this table.
//! - `tspan`: the spans, one row per ingested batch of a trace (`tspan/<trace id>/<chunk>`), so a
//!   batch is appended and never rewritten. The trace page reads a trace's chunks by key.
//!
//! - `tsid`: the ids of a trace's stored spans (`tsid/<trace id>`), so a span sent again is
//!   ignored without reading the spans; kept out of the summary so the list stays small.
//!
//! All rows carry the trace's run as their kv0 run id, so deleting the run deletes its traces and
//! `vigily runs purge` erases them. A run named only by a later batch is linked then, and the rows
//! written before are moved under it. Until a trace's project is known, the strictest payload
//! policy of the configuration applies.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::records::RecordStore;

/// Key prefix of trace summaries.
pub const TRACE: &str = "trace/";
/// Key prefix of span chunks.
pub const SPANS: &str = "tspan/";
/// Key prefix of a trace's stored span ids.
pub const SPAN_IDS: &str = "tsid/";

/// In `truncated` mode, the most payload bytes stored per span (inputs, outputs, error and
/// attributes together).
pub const SPAN_PAYLOAD_BUDGET: usize = 64 * 1024;
/// The most attributes stored per span.
pub const MAX_ATTRIBUTES: usize = 128;
/// The longest attribute key stored, in bytes.
pub const MAX_ATTRIBUTE_KEY: usize = 128;

/// The most spans accepted in one ingest request.
pub const MAX_SPANS_PER_BATCH: usize = 10_000;

pub use vigily_api::traces::{
    IngestReport, PayloadMode, Price, ProjectTracing, Span, SpanIn, TraceSummary, TracingConfig,
    Usage,
};

/// Whether `id` can be part of a record key: 1 to 64 letters, digits, `-` or `_`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn decode<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::Storage(format!("unreadable trace record: {e}")))
}

fn chunk_key(trace: &str, n: u32) -> String {
    format!("{SPANS}{trace}/{n:06}")
}

/// Cuts `s` to at most `max` bytes on a character boundary.
fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Applies the truncated policy to one value: strings are cut, other values that serialise to more
/// than `max` bytes are replaced by the cut JSON text. Returns the original size when cut.
fn truncate_value(v: Value, max: usize) -> (Value, Option<usize>) {
    match v {
        Value::String(s) if s.len() > max => {
            let n = s.len();
            (Value::String(cut(&s, max)), Some(n))
        }
        Value::String(_) | Value::Null | Value::Bool(_) | Value::Number(_) => (v, None),
        other => {
            let text = other.to_string();
            if text.len() > max {
                let n = text.len();
                (Value::String(cut(&text, max)), Some(n))
            } else {
                (other, None)
            }
        }
    }
}

/// The type of an error message: the text before the first `:` when it looks like an
/// identifier (`TimeoutError`, `httpx.ReadTimeout`), else `error`. Free text never survives.
pub fn error_type(e: &str) -> String {
    let head = e.split(':').next().unwrap_or("").trim();
    if is_identifier(head, 64) {
        head.to_string()
    } else {
        "error".into()
    }
}

/// `^[A-Za-z_][A-Za-z0-9_.]{0,max-1}$`
fn is_identifier(s: &str, max: usize) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= max
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'.')
}

/// The cost of a call in the price's currency.
pub fn cost(price: &Price, usage: &Usage) -> f64 {
    let input = usage.input_tokens.unwrap_or(0) as f64;
    let cached = (usage.cached_tokens.unwrap_or(0) as f64).min(input);
    let output = usage.output_tokens.unwrap_or(0) as f64;
    ((input - cached) * price.input
        + cached * price.cached_input.unwrap_or(price.input)
        + output * price.output)
        / 1_000_000.0
}

fn normalize_kind(k: Option<&str>) -> String {
    match k.map(|k| k.trim().to_ascii_lowercase()) {
        Some(k)
            if !k.is_empty()
                && k.len() <= 32
                && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') =>
        {
            k
        }
        _ => "other".into(),
    }
}

fn normalize_status(s: Option<&str>, error: bool) -> String {
    match s.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("error") => "error".into(),
        Some("ok") => "ok".into(),
        _ if error => "error".into(),
        Some("unset") | None => "unset".into(),
        Some(_) => "unset".into(),
    }
}

/// Prepares one span for storage under `mode`.
fn store_span(
    s: SpanIn,
    cfg: &TracingConfig,
    mode: PayloadMode,
    max: usize,
    provisional: bool,
) -> Span {
    let mut truncated = BTreeMap::new();
    let mut dropped_attributes = 0u32;
    // Attribute count and key limits, in every mode.
    let mut attrs = Map::new();
    for (k, v) in s.attributes {
        let ok = attrs.len() < MAX_ATTRIBUTES
            && k.len() <= MAX_ATTRIBUTE_KEY
            && (mode != PayloadMode::Metadata || is_attribute_key(&k));
        if ok {
            attrs.insert(k, v);
        } else {
            dropped_attributes += 1;
        }
    }
    let status = normalize_status(s.status.as_deref(), s.error.is_some());
    let usage = s.usage.filter(|u| !u.is_empty());
    let cost = match (&s.model, &usage) {
        (Some(m), Some(u)) => cfg.price(m).map(|p| cost(p, u)),
        _ => None,
    };
    let (input, output, error, attributes) = match mode {
        PayloadMode::Full => (s.input, s.output, s.error, attrs),
        PayloadMode::Truncated => {
            // Each field is cut to `max`, and all of them together to the span's budget: a
            // field that finds the budget spent is left out (and listed as truncated).
            let mut budget = SPAN_PAYLOAD_BUDGET;
            let mut one = |name: String, v: Value| -> Option<Value> {
                let size = v.to_string().len();
                if budget == 0 {
                    truncated.insert(name, size);
                    return None;
                }
                let (v, n) = truncate_value(v, max.min(budget));
                let (v, n) = match (&v, n) {
                    // A short non-string value larger than the remaining budget.
                    (Value::Array(_) | Value::Object(_), None) if size > budget => {
                        (Value::String(cut(&v.to_string(), budget)), Some(size))
                    }
                    _ => (v, n),
                };
                if let Some(n) = n {
                    truncated.insert(name, n);
                }
                budget = budget.saturating_sub(v.to_string().len());
                Some(v)
            };
            let input = s.input.and_then(|v| one("input".into(), v));
            let output = s.output.and_then(|v| one("output".into(), v));
            let error = s
                .error
                .and_then(|e| one("error".into(), Value::String(e)))
                .map(|v| match v {
                    Value::String(s) => s,
                    other => other.to_string(),
                });
            let mut attributes = Map::new();
            for (k, v) in attrs {
                if let Some(v) = one(format!("attributes.{k}"), v) {
                    attributes.insert(k, v);
                }
            }
            (input, output, error, attributes)
        }
        PayloadMode::Metadata => (
            None,
            None,
            s.error.as_deref().map(error_type),
            attrs
                .into_iter()
                .filter(|(_, v)| {
                    let keep = matches!(v, Value::Bool(_) | Value::Number(_));
                    if !keep {
                        dropped_attributes += 1;
                    }
                    keep
                })
                .collect(),
        ),
    };
    let start = s.start_time;
    let span_id = s.span_id;
    Span {
        // A span that names itself as its parent is a root.
        parent_id: s.parent_id.filter(|p| !p.is_empty() && *p != span_id),
        span_id,
        name: cut(&s.name, 256),
        kind: normalize_kind(s.kind.as_deref()),
        start_time: start,
        end_time: s.end_time.unwrap_or(start).max(start),
        status,
        error,
        attributes,
        model: s.model.map(|m| cut(&m, 128)),
        provider: s.provider.map(|p| cut(&p, 64)),
        input,
        output,
        usage,
        currency: cost.map(|_| cfg.currency.clone()),
        cost,
        payloads: mode,
        provisional,
        truncated,
        dropped_attributes,
    }
}

/// An attribute key kept in `metadata` mode: dotted identifier segments such as
/// `gen_ai.request.temperature` (a key could itself carry personal data).
fn is_attribute_key(k: &str) -> bool {
    is_identifier(k, MAX_ATTRIBUTE_KEY)
}

fn check(s: &SpanIn) -> Result<()> {
    if !valid_id(&s.trace_id) {
        return Err(Error::InvalidArgument(format!(
            "trace id {:?}: use 1 to 64 letters, digits, '-' or '_'",
            s.trace_id
        )));
    }
    if !valid_id(&s.span_id) {
        return Err(Error::InvalidArgument(format!(
            "span id {:?}: use 1 to 64 letters, digits, '-' or '_'",
            s.span_id
        )));
    }
    if s.name.is_empty() {
        return Err(Error::InvalidArgument(format!(
            "span {} has no name",
            s.span_id
        )));
    }
    Ok(())
}

/// Stores a batch of spans: applies the payload policy, prices LLM spans and updates each trace's
/// summary. `experiment_of` names a run's experiment, which is the trace's project unless a span
/// names one. Callers serialise ingests of the same trace (the server holds one lock).
pub async fn ingest(
    rs: &dyn RecordStore,
    cfg: &TracingConfig,
    spans: Vec<SpanIn>,
    experiment_of: &(dyn Fn(&str) -> Option<String> + Send + Sync),
) -> Result<IngestReport> {
    if spans.len() > MAX_SPANS_PER_BATCH {
        return Err(Error::InvalidArgument(format!(
            "{} spans in one request; send at most {MAX_SPANS_PER_BATCH}",
            spans.len()
        )));
    }
    for s in &spans {
        check(s)?;
        if let Some(r) = s.run_id.as_deref().filter(|r| !r.is_empty()) {
            if !valid_id(r) {
                return Err(Error::InvalidArgument(format!("run id {r:?} is not valid")));
            }
        }
    }
    let mut by_trace: BTreeMap<String, Vec<SpanIn>> = BTreeMap::new();
    for s in spans {
        by_trace.entry(s.trace_id.clone()).or_default().push(s);
    }
    let mut report = IngestReport::default();
    // Rows moved under a newly linked run go first, then spans, then ids, then summaries: a
    // summary never names a chunk that is not stored yet.
    let (mut moved, mut chunk_rows, mut id_rows, mut summary_rows) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (id, batch) in by_trace {
        let key = format!("{TRACE}{id}");
        let ids_key = format!("{SPAN_IDS}{id}");
        let old: Option<TraceSummary> = match rs.get(&key).await? {
            Some(v) => Some(decode(v)?),
            None => None,
        };
        let named_run = batch
            .iter()
            .find_map(|s| s.run_id.clone())
            .filter(|r| !r.is_empty());
        let old_run = old.as_ref().and_then(|o| o.run_id.clone());
        // A link interrupted half-way (the spans moved under the run, the summary not yet) is
        // finished here: the chunks record the run they are stored under.
        let chunk_run = match &old {
            Some(o) if o.run_id.is_none() && o.chunks > 0 => chunk_run(rs, &id).await?,
            _ => None,
        };
        let run_id = old_run.clone().or(chunk_run).or(named_run);
        let mut seen: BTreeSet<String> = match rs.get(&ids_key).await? {
            Some(v) => decode::<Vec<String>>(v)?.into_iter().collect(),
            None => BTreeSet::new(),
        };
        // A run named only now: move the rows stored so far under it, so deleting and purging
        // the run reaches them. The old rows are superseded by null rows (erased by a purge).
        if let (Some(o), None, Some(r)) = (&old, &old_run, &run_id) {
            let mut keys: Vec<String> = (0..o.chunks).map(|n| chunk_key(&id, n)).collect();
            keys.push(ids_key.clone());
            for k in keys {
                if let Some(mut v) = rs.get(&k).await? {
                    if let Some(o) = v.as_object_mut().filter(|o| o.contains_key("spans")) {
                        o.insert("run_id".into(), Value::String(r.clone()));
                    }
                    moved.push((k.clone(), None, Value::Null));
                    moved.push((k, Some(r.clone()), v));
                }
            }
            moved.push((key.clone(), None, Value::Null));
        }
        let project = old
            .as_ref()
            .and_then(|o| o.project.clone())
            .or_else(|| batch.iter().find_map(|s| s.project.clone()))
            .or_else(|| run_id.as_deref().and_then(experiment_of));
        let eval_item = old
            .as_ref()
            .and_then(|o| o.eval_item.clone())
            .or_else(|| batch.iter().find_map(|s| s.eval_item.clone()));
        // Until the project is known, its policy is not: apply the strictest one configured.
        let (mode, max) = match &project {
            Some(p) => cfg.policy(Some(p)),
            None => cfg.strictest(),
        };
        // Flagged only when the strictest policy actually differs from the global one.
        let provisional = project.is_none() && cfg.strictest() != cfg.policy(None);
        let mut stored = Vec::new();
        for s in batch {
            if !seen.insert(s.span_id.clone()) {
                report.duplicates += 1;
                continue;
            }
            stored.push(store_span(s, cfg, mode, max, provisional));
        }
        if stored.is_empty() && moved.is_empty() {
            continue;
        }
        report.accepted += stored.len();
        report.traces += 1;
        report.new_traces += old.is_none() as usize;
        let first_name = stored.first().map(|s| s.name.clone()).unwrap_or_default();
        let mut sum = old.unwrap_or_else(|| TraceSummary {
            id: id.clone(),
            name: first_name,
            root: None,
            run_id: None,
            eval_item: None,
            project: None,
            start_time: i64::MAX,
            end_time: i64::MIN,
            status: "ok".into(),
            spans: 0,
            errors: 0,
            llm_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            cost: None,
            unpriced: 0,
            currency: cfg.currency.clone(),
            mixed_currency: false,
            models: Vec::new(),
            payloads: mode,
            chunks: 0,
        });
        sum.run_id = run_id.clone();
        sum.project = project;
        sum.eval_item = eval_item;
        sum.payloads = mode;
        for sp in &stored {
            sum.spans = sum.spans.saturating_add(1);
            sum.start_time = sum.start_time.min(sp.start_time);
            sum.end_time = sum.end_time.max(sp.end_time);
            if sp.parent_id.is_none() && sum.root.is_none() {
                sum.root = Some(sp.span_id.clone());
                sum.name = sp.name.clone();
            }
            if sp.status == "error" {
                sum.errors = sum.errors.saturating_add(1);
                sum.status = "error".into();
            }
            if sp.kind == "llm" || (sp.model.is_some() && sp.usage.is_some()) {
                sum.llm_calls = sum.llm_calls.saturating_add(1);
            }
            if let Some(u) = &sp.usage {
                sum.input_tokens = sum.input_tokens.saturating_add(u.input_tokens.unwrap_or(0));
                sum.output_tokens = sum
                    .output_tokens
                    .saturating_add(u.output_tokens.unwrap_or(0));
                sum.cached_tokens = sum
                    .cached_tokens
                    .saturating_add(u.cached_tokens.unwrap_or(0));
                match (sp.cost, sp.currency.as_deref()) {
                    (Some(c), Some(cur)) if cur == sum.currency => {
                        sum.cost = Some(sum.cost.unwrap_or(0.0) + c)
                    }
                    (Some(_), _) => {
                        // Priced after the currency setting changed: kept on the span, left
                        // out of the trace's total rather than added in the wrong currency.
                        sum.mixed_currency = true;
                        sum.unpriced = sum.unpriced.saturating_add(1);
                    }
                    (None, _) if sp.model.is_some() => {
                        sum.unpriced = sum.unpriced.saturating_add(1)
                    }
                    (None, _) => {}
                }
            }
            if let Some(m) = &sp.model {
                if !sum.models.contains(m) && sum.models.len() < 32 {
                    sum.models.push(m.clone());
                }
            }
        }
        if !stored.is_empty() {
            chunk_rows.push((
                chunk_key(&id, sum.chunks),
                run_id.clone(),
                serde_json::json!({ "run_id": run_id, "spans": stored }),
            ));
            sum.chunks += 1;
            id_rows.push((
                ids_key,
                run_id.clone(),
                serde_json::to_value(&seen).map_err(|e| Error::Storage(e.to_string()))?,
            ));
        }
        summary_rows.push((
            key,
            run_id,
            serde_json::to_value(&sum).map_err(|e| Error::Storage(e.to_string()))?,
        ));
    }
    let mut rows = moved;
    rows.extend(chunk_rows);
    rows.extend(id_rows);
    rows.extend(summary_rows);
    if !rows.is_empty() {
        rs.put_owned(rows).await?;
    }
    Ok(report)
}

/// The run a trace's first chunk is stored under, as the chunk records it.
async fn chunk_run(rs: &dyn RecordStore, id: &str) -> Result<Option<String>> {
    Ok(rs
        .get(&chunk_key(id, 0))
        .await?
        .and_then(|c| c.get("run_id").and_then(Value::as_str).map(String::from)))
}

fn summaries(rows: Vec<(String, Value)>) -> Vec<TraceSummary> {
    let mut out: Vec<TraceSummary> = rows
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_value(v).ok())
        .collect();
    out.sort_by(|a, b| {
        b.start_time
            .cmp(&a.start_time)
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// Every trace summary, newest first.
pub async fn list(rs: &dyn RecordStore) -> Result<Vec<TraceSummary>> {
    Ok(summaries(rs.scan(TRACE).await?))
}

/// The summaries of the traces of `runs`, newest first.
pub async fn list_for_runs(rs: &dyn RecordStore, runs: &[&str]) -> Result<Vec<TraceSummary>> {
    Ok(summaries(rs.scan_owned(TRACE, runs).await?))
}

/// One trace and its spans, ordered by start time.
pub async fn get(rs: &dyn RecordStore, id: &str) -> Result<Option<(TraceSummary, Vec<Span>)>> {
    if !valid_id(id) {
        return Ok(None);
    }
    let Some(v) = rs.get(&format!("{TRACE}{id}")).await? else {
        return Ok(None);
    };
    let mut sum: TraceSummary = decode(v)?;
    if sum.run_id.is_none() && sum.chunks > 0 {
        // An interrupted link: show the run the spans are stored under (the next ingest of the
        // trace rewrites the summary).
        sum.run_id = chunk_run(rs, id).await?;
    }
    let mut spans: Vec<Span> = Vec::new();
    let mut seen = BTreeSet::new();
    for n in 0..sum.chunks {
        let Some(c) = rs.get(&chunk_key(id, n)).await? else {
            continue;
        };
        let chunk: Vec<Span> = decode(c.get("spans").cloned().unwrap_or_default())?;
        for s in chunk {
            if seen.insert(s.span_id.clone()) {
                spans.push(s);
            }
        }
    }
    break_parent_cycles(&mut spans);
    spans.sort_by(|a, b| {
        a.start_time
            .cmp(&b.start_time)
            .then_with(|| a.end_time.cmp(&b.end_time).reverse())
    });
    Ok(Some((sum, spans)))
}

/// Makes every span whose parent chain loops back on itself a root, so a malformed trace still
/// draws as a tree. Spans whose parent is missing keep it (the reader treats them as roots).
fn break_parent_cycles(spans: &mut [Span]) {
    let index: std::collections::HashMap<String, usize> = spans
        .iter()
        .enumerate()
        .map(|(i, s)| (s.span_id.clone(), i))
        .collect();
    let mut cyclic = Vec::new();
    for (i, s) in spans.iter().enumerate() {
        let mut cur = s.parent_id.as_deref();
        let mut steps = 0;
        while let Some(p) = cur {
            if p == s.span_id {
                cyclic.push(i);
                break;
            }
            if steps > spans.len() {
                // A loop above this span; the spans in it are made roots themselves.
                break;
            }
            steps += 1;
            cur = index.get(p).and_then(|&j| spans[j].parent_id.as_deref());
        }
    }
    for i in cyclic {
        spans[i].parent_id = None;
    }
}

/// Deletes one trace (its summary and spans). Returns whether it existed.
pub async fn delete(rs: &dyn RecordStore, id: &str) -> Result<bool> {
    if !valid_id(id) {
        return Ok(false);
    }
    let key = format!("{TRACE}{id}");
    let Some(v) = rs.get(&key).await? else {
        return Ok(false);
    };
    let sum: TraceSummary = decode(v)?;
    let mut rows: Vec<(String, Option<String>, Value)> = (0..sum.chunks)
        .map(|n| (chunk_key(id, n), sum.run_id.clone(), Value::Null))
        .collect();
    rows.push((format!("{SPAN_IDS}{id}"), sum.run_id.clone(), Value::Null));
    rows.push((key, sum.run_id.clone(), Value::Null));
    rs.put_owned(rows).await?;
    Ok(true)
}

// -------------------------------------------------------------------------------------------------
// OpenTelemetry (OTLP/HTTP JSON)

/// An OTLP `AnyValue` as JSON.
fn any_value(v: &Value) -> Value {
    let Some(o) = v.as_object() else {
        return Value::Null;
    };
    if let Some(s) = o.get("stringValue") {
        return s.clone();
    }
    if let Some(b) = o.get("boolValue") {
        return b.clone();
    }
    if let Some(i) = o.get("intValue") {
        // int64 travels as a string in OTLP JSON.
        return match i {
            Value::String(s) => s.parse::<i64>().map(Value::from).unwrap_or(i.clone()),
            other => other.clone(),
        };
    }
    if let Some(d) = o.get("doubleValue") {
        return d.clone();
    }
    if let Some(a) = o.get("arrayValue") {
        return Value::Array(
            a.get("values")
                .and_then(Value::as_array)
                .map(|vs| vs.iter().map(any_value).collect())
                .unwrap_or_default(),
        );
    }
    if let Some(kv) = o.get("kvlistValue") {
        return Value::Object(attributes(kv.get("values")));
    }
    if let Some(b) = o.get("bytesValue") {
        return b.clone();
    }
    Value::Null
}

fn attributes(list: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for kv in list.and_then(Value::as_array).into_iter().flatten() {
        if let (Some(k), Some(v)) = (kv.get("key").and_then(Value::as_str), kv.get("value")) {
            out.insert(k.to_string(), any_value(v));
        }
    }
    out
}

fn nanos_to_micros(v: Option<&Value>) -> Option<i64> {
    let n: i128 = match v? {
        Value::String(s) => s.parse().ok()?,
        Value::Number(n) => n.as_i64()? as i128,
        _ => return None,
    };
    Some((n / 1000) as i64)
}

fn take_str(a: &mut Map<String, Value>, keys: &[&str]) -> Option<String> {
    let mut found = None;
    for k in keys {
        if let Some(v) = a.remove(*k) {
            if found.is_none() {
                found = match v {
                    Value::String(s) => Some(s),
                    Value::Null => None,
                    other => Some(other.to_string()),
                };
            }
        }
    }
    found
}

fn take_u64(a: &mut Map<String, Value>, keys: &[&str]) -> Option<u64> {
    let mut found = None;
    for k in keys {
        if let Some(v) = a.remove(*k) {
            if found.is_none() {
                found = v
                    .as_u64()
                    .or_else(|| v.as_f64().map(|f| f.max(0.0) as u64))
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()));
            }
        }
    }
    found
}

fn take_payload(a: &mut Map<String, Value>, keys: &[&str]) -> Option<Value> {
    let mut found = None;
    for k in keys {
        if let Some(v) = a.remove(*k) {
            if found.is_none() {
                // GenAI conventions carry messages as JSON text; keep them structured.
                found = Some(match v {
                    Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
                    other => other,
                });
            }
        }
    }
    found
}

/// The vigily span kind of an OTLP span: `vigily.span.kind`, else from the GenAI operation name.
fn otlp_kind(a: &mut Map<String, Value>) -> Option<String> {
    if let Some(k) = take_str(a, &["vigily.span.kind", "openinference.span.kind"]) {
        return Some(k.to_ascii_lowercase());
    }
    let op = a.get("gen_ai.operation.name").and_then(Value::as_str)?;
    Some(
        match op {
            "chat" | "text_completion" | "generate_content" | "completion" => "llm",
            "embeddings" => "embedding",
            "execute_tool" => "tool",
            "invoke_agent" | "create_agent" => "agent",
            _ => "llm",
        }
        .into(),
    )
}

/// Converts an OTLP/HTTP JSON `ExportTraceServiceRequest` into spans. GenAI semantic-convention
/// attributes (`gen_ai.request.model`, `gen_ai.usage.input_tokens`, ...) become the span's model,
/// provider, usage, input and output; `vigily.run_id`, `vigily.eval_item` and `vigily.project`
/// (on the span or its resource) link the trace. Other attributes are kept as they are.
pub fn from_otlp(body: &Value) -> Result<Vec<SpanIn>> {
    let bad = |m: &str| Error::InvalidArgument(format!("not an OTLP JSON trace export: {m}"));
    let resource_spans = body
        .get("resourceSpans")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("no resourceSpans"))?;
    let mut out = Vec::new();
    for rs in resource_spans {
        let res = attributes(rs.pointer("/resource/attributes"));
        for ss in rs
            .get("scopeSpans")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for sp in ss
                .get("spans")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let mut a = attributes(sp.get("attributes"));
                let pick = |a: &mut Map<String, Value>, k: &str| {
                    take_str(a, &[k])
                        .or_else(|| res.get(k).and_then(Value::as_str).map(String::from))
                };
                let run_id = pick(&mut a, "vigily.run_id");
                let eval_item = pick(&mut a, "vigily.eval_item");
                let project = pick(&mut a, "vigily.project");
                let kind = otlp_kind(&mut a);
                let model = take_str(&mut a, &["gen_ai.response.model", "gen_ai.request.model"]);
                let provider = take_str(&mut a, &["gen_ai.provider.name", "gen_ai.system"]);
                let usage = Usage {
                    input_tokens: take_u64(
                        &mut a,
                        &["gen_ai.usage.input_tokens", "gen_ai.usage.prompt_tokens"],
                    ),
                    output_tokens: take_u64(
                        &mut a,
                        &[
                            "gen_ai.usage.output_tokens",
                            "gen_ai.usage.completion_tokens",
                        ],
                    ),
                    cached_tokens: take_u64(
                        &mut a,
                        &[
                            "gen_ai.usage.cache_read.input_tokens",
                            "gen_ai.usage.cache_read_input_tokens",
                            "gen_ai.usage.cached_tokens",
                        ],
                    ),
                };
                let input = take_payload(&mut a, &["gen_ai.input.messages", "gen_ai.prompt"]);
                let output = take_payload(&mut a, &["gen_ai.output.messages", "gen_ai.completion"]);
                let code = sp.pointer("/status/code");
                let is_error = matches!(code, Some(Value::Number(n)) if n.as_u64() == Some(2))
                    || matches!(code, Some(Value::String(s)) if s == "STATUS_CODE_ERROR");
                let is_ok = matches!(code, Some(Value::Number(n)) if n.as_u64() == Some(1))
                    || matches!(code, Some(Value::String(s)) if s == "STATUS_CODE_OK");
                let mut error = sp
                    .pointer("/status/message")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
                    .map(String::from);
                if is_error && error.is_none() {
                    // The exception event carries the message when the status does not.
                    error = sp
                        .get("events")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|e| e.get("name").and_then(Value::as_str) == Some("exception"))
                        .find_map(|e| {
                            let ea = attributes(e.get("attributes"));
                            let t = ea.get("exception.type").and_then(Value::as_str);
                            let m = ea.get("exception.message").and_then(Value::as_str);
                            match (t, m) {
                                (Some(t), Some(m)) => Some(format!("{t}: {m}")),
                                (t, m) => t.or(m).map(String::from),
                            }
                        })
                        .or_else(|| Some("error".into()));
                }
                let id = |k: &str| {
                    sp.get(k)
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_ascii_lowercase)
                };
                out.push(SpanIn {
                    trace_id: id("traceId").ok_or_else(|| bad("a span has no traceId"))?,
                    span_id: id("spanId").ok_or_else(|| bad("a span has no spanId"))?,
                    parent_id: id("parentSpanId"),
                    name: sp
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("span")
                        .to_string(),
                    kind,
                    start_time: nanos_to_micros(sp.get("startTimeUnixNano"))
                        .ok_or_else(|| bad("a span has no startTimeUnixNano"))?,
                    end_time: nanos_to_micros(sp.get("endTimeUnixNano")),
                    status: Some(
                        if is_error {
                            "error"
                        } else if is_ok {
                            "ok"
                        } else {
                            "unset"
                        }
                        .into(),
                    ),
                    error,
                    attributes: a,
                    run_id,
                    eval_item,
                    project,
                    model,
                    provider,
                    input,
                    output,
                    usage: Some(usage).filter(|u| !u.is_empty()),
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::MemoryRecords;
    use serde_json::json;

    fn span(trace: &str, id: &str, parent: Option<&str>, start: i64, end: i64) -> SpanIn {
        SpanIn {
            trace_id: trace.into(),
            span_id: id.into(),
            parent_id: parent.map(String::from),
            name: format!("span {id}"),
            kind: Some("chain".into()),
            start_time: start,
            end_time: Some(end),
            status: Some("ok".into()),
            ..Default::default()
        }
    }

    fn llm(trace: &str, id: &str, model: &str, i: u64, o: u64, c: u64) -> SpanIn {
        SpanIn {
            kind: Some("llm".into()),
            model: Some(model.into()),
            provider: Some("example".into()),
            usage: Some(Usage {
                input_tokens: Some(i),
                output_tokens: Some(o),
                cached_tokens: Some(c),
            }),
            input: Some(json!([{"role": "user", "content": "Question A"}])),
            output: Some(json!("Answer A")),
            ..span(trace, id, Some("root"), 10, 20)
        }
    }

    fn priced() -> TracingConfig {
        let mut cfg = TracingConfig::default();
        cfg.prices.insert(
            "model-a".into(),
            Price {
                input: 2.0,
                output: 8.0,
                cached_input: Some(0.5),
            },
        );
        cfg.prices.insert(
            "model-b*".into(),
            Price {
                input: 1.0,
                output: 1.0,
                cached_input: None,
            },
        );
        cfg
    }

    fn no_runs(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn cost_uses_cached_price_and_patterns() {
        let cfg = priced();
        let u = Usage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(500_000),
            cached_tokens: Some(400_000),
        };
        // 600k × 2 + 400k × 0.5 + 500k × 8, per million
        assert!((cost(cfg.price("model-a").unwrap(), &u) - (1.2 + 0.2 + 4.0)).abs() < 1e-12);
        assert!(cfg.price("model-b-mini").is_some());
        assert!(cfg.price("model-c").is_none());
        // Cached tokens beyond the input are not counted twice.
        let odd = Usage {
            input_tokens: Some(10),
            output_tokens: None,
            cached_tokens: Some(50),
        };
        assert!((cost(cfg.price("model-b").unwrap(), &odd) - 10.0 / 1e6).abs() < 1e-15);
    }

    #[tokio::test]
    async fn ingest_builds_summary_and_spans_across_batches() {
        let rs = MemoryRecords::new();
        let cfg = priced();
        let r = ingest(
            &rs,
            &cfg,
            vec![
                llm("t1", "a", "model-a", 1000, 200, 0),
                llm("t1", "b", "model-x", 50, 5, 0),
            ],
            &no_runs,
        )
        .await
        .unwrap();
        assert_eq!(r.accepted, 2);
        let mut root = span("t1", "root", None, 0, 100);
        root.run_id = Some("run1".into());
        root.status = Some("error".into());
        root.error = Some("ValueError: synthetic failure".into());
        let r = ingest(
            &rs,
            &cfg,
            vec![root, llm("t1", "a", "model-a", 1, 1, 0)],
            &no_runs,
        )
        .await
        .unwrap();
        assert_eq!((r.accepted, r.duplicates), (1, 1));
        let (sum, spans) = get(&rs, "t1").await.unwrap().unwrap();
        assert_eq!(sum.name, "span root");
        assert_eq!(sum.spans, 3);
        assert_eq!(sum.chunks, 2);
        assert_eq!(sum.status, "error");
        assert_eq!(sum.input_tokens, 1050);
        assert_eq!(sum.unpriced, 1);
        assert_eq!(sum.llm_calls, 2);
        assert_eq!(sum.currency, "USD");
        assert!((sum.cost.unwrap() - (1000.0 * 2.0 + 200.0 * 8.0) / 1e6).abs() < 1e-12);
        assert_eq!(sum.models, vec!["model-a", "model-x"]);
        // The second batch named the run: the trace and the chunk stored before are linked to it.
        assert_eq!(sum.run_id.as_deref(), Some("run1"));
        assert_eq!(list_for_runs(&rs, &["run1"]).await.unwrap().len(), 1);
        assert_eq!(rs.scan_owned(SPANS, &["run1"]).await.unwrap().len(), 2);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].span_id, "root");
        // The summary does not grow with the span ids; they live in their own row.
        assert!(!serde_json::to_string(&sum).unwrap().contains("\"llm1\""));
        assert_eq!(
            rs.get(&format!("{SPAN_IDS}t1"))
                .await
                .unwrap()
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(delete(&rs, "t1").await.unwrap());
        assert!(get(&rs, "t1").await.unwrap().is_none());
        assert!(rs.scan(SPANS).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn payload_modes() {
        let long = "x".repeat(5000);
        let mut s = llm("t", "a", "model-a", 1, 1, 0);
        s.input = Some(Value::String(long.clone()));
        s.output = Some(json!({"text": long}));
        s.error = Some(format!("KeyError: {long}"));
        s.attributes
            .insert("user_note".into(), json!("synthetic note"));
        s.attributes.insert("temperature".into(), json!(0.2));

        let rs = MemoryRecords::new();
        let mut cfg = TracingConfig::default();
        ingest(&rs, &cfg, vec![s.clone()], &no_runs).await.unwrap();
        let (sum, spans) = get(&rs, "t").await.unwrap().unwrap();
        assert_eq!(sum.payloads, PayloadMode::Truncated);
        let sp = &spans[0];
        assert_eq!(sp.input.as_ref().unwrap().as_str().unwrap().len(), 4096);
        assert_eq!(sp.truncated["input"], 5000);
        assert!(sp.output.as_ref().unwrap().is_string());
        assert_eq!(sp.error.as_ref().unwrap().len(), 4096);

        cfg.payloads = PayloadMode::Metadata;
        s.trace_id = "m".into();
        ingest(&rs, &cfg, vec![s.clone()], &no_runs).await.unwrap();
        let (_, spans) = get(&rs, "m").await.unwrap().unwrap();
        let sp = &spans[0];
        assert!(sp.input.is_none() && sp.output.is_none());
        assert_eq!(sp.error.as_deref(), Some("KeyError"));
        assert!(!sp.attributes.contains_key("user_note"));
        assert_eq!(sp.attributes["temperature"], json!(0.2));
        assert_eq!(sp.usage.as_ref().unwrap().input_tokens, Some(1));

        // A project override applies to traces of runs in that experiment.
        let mut cfg = TracingConfig::default();
        cfg.projects.insert(
            "support".into(),
            ProjectTracing {
                payloads: Some(PayloadMode::Full),
                max_field_bytes: None,
            },
        );
        s.trace_id = "f".into();
        s.run_id = Some("r1".into());
        let exp = |r: &str| (r == "r1").then(|| "support".to_string());
        ingest(&rs, &cfg, vec![s], &exp).await.unwrap();
        let (sum, spans) = get(&rs, "f").await.unwrap().unwrap();
        assert_eq!(sum.project.as_deref(), Some("support"));
        assert_eq!(
            spans[0].input.as_ref().unwrap().as_str().unwrap().len(),
            5000
        );
        assert_eq!(list_for_runs(&rs, &["r1"]).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rejects_bad_ids() {
        let rs = MemoryRecords::new();
        let cfg = TracingConfig::default();
        let bad = span("../x", "a", None, 0, 1);
        assert!(ingest(&rs, &cfg, vec![bad], &no_runs).await.is_err());
        assert!(get(&rs, "a/b").await.unwrap().is_none());
    }

    #[test]
    fn otlp_json_maps_genai_attributes() {
        let body = json!({"resourceSpans": [{
            "resource": {"attributes": [
                {"key": "service.name", "value": {"stringValue": "synthetic-app"}},
                {"key": "vigily.run_id", "value": {"stringValue": "run-1"}}
            ]},
            "scopeSpans": [{"spans": [{
                "traceId": "5B8EFFF798038103D269B633813FC60C",
                "spanId": "EEE19B7EC3C1B174",
                "parentSpanId": "",
                "name": "chat model-a",
                "kind": 3,
                "startTimeUnixNano": "1700000000000000000",
                "endTimeUnixNano": "1700000001500000000",
                "attributes": [
                    {"key": "gen_ai.operation.name", "value": {"stringValue": "chat"}},
                    {"key": "gen_ai.request.model", "value": {"stringValue": "model-a"}},
                    {"key": "gen_ai.system", "value": {"stringValue": "example"}},
                    {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "120"}},
                    {"key": "gen_ai.usage.output_tokens", "value": {"intValue": 30}},
                    {"key": "gen_ai.request.temperature", "value": {"doubleValue": 0.1}}
                ],
                "status": {"code": 2},
                "events": [{"name": "exception", "attributes": [
                    {"key": "exception.type", "value": {"stringValue": "TimeoutError"}},
                    {"key": "exception.message", "value": {"stringValue": "synthetic timeout"}}
                ]}]
            }]}]
        }]});
        let spans = from_otlp(&body).unwrap();
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!(s.trace_id, "5b8efff798038103d269b633813fc60c");
        assert_eq!(s.parent_id, None);
        assert_eq!(s.kind.as_deref(), Some("llm"));
        assert_eq!(s.model.as_deref(), Some("model-a"));
        assert_eq!(s.provider.as_deref(), Some("example"));
        assert_eq!(s.usage.as_ref().unwrap().input_tokens, Some(120));
        assert_eq!(s.usage.as_ref().unwrap().output_tokens, Some(30));
        assert_eq!(s.start_time, 1_700_000_000_000_000);
        assert_eq!(s.end_time, Some(1_700_000_001_500_000));
        assert_eq!(s.status.as_deref(), Some("error"));
        assert_eq!(s.error.as_deref(), Some("TimeoutError: synthetic timeout"));
        assert_eq!(s.run_id.as_deref(), Some("run-1"));
        assert_eq!(s.attributes["gen_ai.request.temperature"], json!(0.1));
        assert!(from_otlp(&json!({"x": 1})).is_err());
    }

    #[test]
    fn error_type_keeps_only_identifiers() {
        assert_eq!(error_type("KeyError: user@example.com"), "KeyError");
        assert_eq!(error_type("httpx.ReadTimeout: x"), "httpx.ReadTimeout");
        assert_eq!(error_type("no customer for user@example.com"), "error");
        assert_eq!(error_type("Customer Name: missing"), "error");
        assert_eq!(error_type(""), "error");
        assert_eq!(error_type(&"A".repeat(65)), "error");
    }

    #[tokio::test]
    async fn metadata_mode_keeps_no_free_text() {
        let rs = MemoryRecords::new();
        let cfg = TracingConfig {
            payloads: PayloadMode::Metadata,
            ..Default::default()
        };
        let mut s = span("m", "a", None, 0, 1);
        s.error = Some("no customer for user@example.com".into());
        s.attributes.insert("user@example.com".into(), json!(1));
        s.attributes.insert("retries".into(), json!(2));
        s.attributes.insert("note".into(), json!(["free", "text"]));
        ingest(&rs, &cfg, vec![s], &no_runs).await.unwrap();
        let (_, spans) = get(&rs, "m").await.unwrap().unwrap();
        assert_eq!(spans[0].error.as_deref(), Some("error"));
        assert_eq!(
            spans[0].attributes,
            json!({"retries": 2}).as_object().unwrap().clone()
        );
        assert_eq!(spans[0].dropped_attributes, 2);
    }

    #[tokio::test]
    async fn truncated_mode_has_a_per_span_budget() {
        let rs = MemoryRecords::new();
        let cfg = TracingConfig::default();
        let mut s = span("b", "a", None, 0, 1);
        s.input = Some(json!("i".repeat(10_000)));
        for n in 0..200 {
            s.attributes
                .insert(format!("k{n:03}"), json!("v".repeat(1000)));
        }
        s.attributes.insert("x".repeat(200), json!(1));
        ingest(&rs, &cfg, vec![s], &no_runs).await.unwrap();
        let (_, spans) = get(&rs, "b").await.unwrap().unwrap();
        let sp = &spans[0];
        let stored: usize = sp.input.iter().map(|v| v.to_string().len()).sum::<usize>()
            + sp.attributes
                .values()
                .map(|v| v.to_string().len())
                .sum::<usize>();
        assert!(
            stored <= SPAN_PAYLOAD_BUDGET + 2 * MAX_ATTRIBUTES,
            "{stored}"
        );
        assert!(sp.attributes.len() < MAX_ATTRIBUTES);
        assert!(sp.attributes.keys().all(|k| k.len() <= MAX_ATTRIBUTE_KEY));
        assert_eq!(sp.dropped_attributes, 200 + 1 - MAX_ATTRIBUTES as u32);
        assert!(
            sp.truncated.contains_key("attributes.k127"),
            "spent budget is reported"
        );
    }

    #[tokio::test]
    async fn unknown_project_gets_the_strictest_policy() {
        let rs = MemoryRecords::new();
        let mut cfg = TracingConfig {
            payloads: PayloadMode::Full,
            ..Default::default()
        };
        cfg.projects.insert(
            "support".into(),
            ProjectTracing {
                payloads: Some(PayloadMode::Metadata),
                max_field_bytes: None,
            },
        );
        // Children arrive before the root that names the run (of project "support").
        let mut child = llm("s", "c", "model-a", 1, 1, 0);
        child.input = Some(json!("Question A"));
        ingest(&rs, &cfg, vec![child], &no_runs).await.unwrap();
        let mut root = span("s", "root", None, 0, 100);
        root.run_id = Some("r1".into());
        root.input = Some(json!("Question A"));
        let exp = |r: &str| (r == "r1").then(|| "support".to_string());
        ingest(&rs, &cfg, vec![root], &exp).await.unwrap();
        let (sum, spans) = get(&rs, "s").await.unwrap().unwrap();
        assert_eq!(sum.project.as_deref(), Some("support"));
        assert!(
            spans.iter().all(|s| s.input.is_none()),
            "nothing stored beyond metadata"
        );
        let flag = |id: &str| spans.iter().find(|s| s.span_id == id).unwrap().provisional;
        assert!(flag("c"), "the child arrived before the project was known");
        assert!(!flag("root"));
        // Without project policies the strictest policy is the global one: nothing is flagged.
        let plain = MemoryRecords::new();
        ingest(
            &plain,
            &TracingConfig::default(),
            vec![span("p", "a", None, 0, 1)],
            &no_runs,
        )
        .await
        .unwrap();
        assert!(!get(&plain, "p").await.unwrap().unwrap().1[0].provisional);
        assert_eq!(sum.run_id.as_deref(), Some("r1"));
    }

    #[tokio::test]
    async fn parent_cycles_and_self_parents_become_roots() {
        let rs = MemoryRecords::new();
        let cfg = TracingConfig::default();
        ingest(
            &rs,
            &cfg,
            vec![
                span("c", "self", Some("self"), 0, 1),
                span("c", "x", Some("y"), 0, 1),
                span("c", "y", Some("x"), 0, 1),
                span("c", "z", Some("x"), 0, 1),
            ],
            &no_runs,
        )
        .await
        .unwrap();
        let (sum, spans) = get(&rs, "c").await.unwrap().unwrap();
        assert_eq!(sum.root.as_deref(), Some("self"));
        let parent = |id: &str| {
            spans
                .iter()
                .find(|s| s.span_id == id)
                .unwrap()
                .parent_id
                .clone()
        };
        assert_eq!(parent("self"), None);
        assert_eq!(parent("x"), None);
        assert_eq!(parent("y"), None);
        assert_eq!(parent("z").as_deref(), Some("x"));
    }

    #[tokio::test]
    async fn totals_saturate_and_currency_changes_are_not_mixed() {
        let rs = MemoryRecords::new();
        let mut cfg = priced();
        let mut a = llm("u", "a", "model-a", u64::MAX, 1, 0);
        a.start_time = i64::MIN;
        ingest(&rs, &cfg, vec![a], &no_runs).await.unwrap();
        cfg.currency = "EUR".into();
        ingest(
            &rs,
            &cfg,
            vec![llm("u", "b", "model-a", 10, 1, 0)],
            &no_runs,
        )
        .await
        .unwrap();
        let (sum, spans) = get(&rs, "u").await.unwrap().unwrap();
        assert_eq!(sum.input_tokens, u64::MAX);
        assert!(sum.duration_us() >= 0);
        assert_eq!(sum.currency, "USD");
        assert!(sum.mixed_currency);
        assert_eq!(sum.unpriced, 1);
        let b = spans.iter().find(|s| s.span_id == "b").unwrap();
        assert_eq!(b.currency.as_deref(), Some("EUR"));
    }

    /// A link interrupted after the spans moved under the run but before the summary did is
    /// shown on read and repaired by the next ingest.
    #[tokio::test]
    async fn an_interrupted_run_link_is_repaired() {
        let rs = MemoryRecords::new();
        let cfg = TracingConfig::default();
        ingest(
            &rs,
            &cfg,
            vec![span("ti", "a", Some("root"), 0, 1)],
            &no_runs,
        )
        .await
        .unwrap();
        // Simulate the crash: the chunk moved under run r1, the summary still without a run.
        let chunk = rs.get(&chunk_key("ti", 0)).await.unwrap().unwrap();
        let mut moved = chunk.clone();
        moved["run_id"] = json!("r1");
        rs.put_owned(vec![
            (chunk_key("ti", 0), None, Value::Null),
            (chunk_key("ti", 0), Some("r1".into()), moved),
        ])
        .await
        .unwrap();
        assert_eq!(list(&rs).await.unwrap()[0].run_id, None);
        assert_eq!(
            get(&rs, "ti").await.unwrap().unwrap().0.run_id.as_deref(),
            Some("r1")
        );
        // The next batch names no run; it still lands under r1, and the summary is linked.
        ingest(
            &rs,
            &cfg,
            vec![span("ti", "b", Some("root"), 0, 1)],
            &no_runs,
        )
        .await
        .unwrap();
        assert_eq!(list_for_runs(&rs, &["r1"]).await.unwrap().len(), 1);
        assert_eq!(rs.scan_owned(SPANS, &["r1"]).await.unwrap().len(), 2);
        assert_eq!(rs.scan_owned(SPAN_IDS, &["r1"]).await.unwrap().len(), 1);
    }

    /// Deleting a run deletes its traces, and a purge erases their payloads from the files.
    #[cfg(feature = "kv0")]
    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_and_purging_a_run_erases_its_traces() {
        use crate::records::Kv0Records;
        use crate::{Kv0Options, Kv0Store, NewRun, RunStatus, Store};

        const MARKER: &str = "SYNTHETICPAYLOADMARKER";
        let dir = tempfile::tempdir().unwrap();
        let loc = dir.path().join("repo");
        let loc = loc.to_str().unwrap();
        let store = Kv0Store::open(loc, Kv0Options::default()).await.unwrap();
        let rs = Kv0Records::open(loc).await.unwrap();
        let w = store.create_run(NewRun::default()).await.unwrap();
        let run = w.info().id;
        w.finish(RunStatus::Finished, None).await.unwrap();
        let keep = store.create_run(NewRun::default()).await.unwrap();
        let other = keep.info().id;
        keep.finish(RunStatus::Finished, None).await.unwrap();

        let cfg = TracingConfig::default();
        let mut a = llm("ta", "s1", "model-a", 10, 2, 0);
        a.run_id = Some(run.clone());
        a.input = Some(json!(MARKER));
        let mut b = llm("tb", "s1", "model-a", 10, 2, 0);
        b.run_id = Some(other.clone());
        ingest(&rs, &cfg, vec![a.clone()], &no_runs).await.unwrap();
        // A second chunk for the same trace.
        a.span_id = "s2".into();
        ingest(&rs, &cfg, vec![a, b], &no_runs).await.unwrap();
        assert_eq!(list(&rs).await.unwrap().len(), 2);
        assert_eq!(list_for_runs(&rs, &[run.as_str()]).await.unwrap().len(), 1);
        assert_eq!(get(&rs, "ta").await.unwrap().unwrap().1.len(), 2);

        assert_eq!(
            store.delete_runs(std::slice::from_ref(&run)).await.unwrap(),
            1
        );
        let left = list(&rs).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "tb");
        assert!(get(&rs, "ta").await.unwrap().is_none());

        fn files_contain(dir: &std::path::Path, needle: &[u8]) -> bool {
            std::fs::read_dir(dir).unwrap().flatten().any(|e| {
                let p = e.path();
                if p.is_dir() {
                    files_contain(&p, needle)
                } else {
                    std::fs::read(&p)
                        .map(|b| b.windows(needle.len()).any(|w| w == needle))
                        .unwrap_or(false)
                }
            })
        }
        assert!(files_contain(dir.path(), MARKER.as_bytes()));
        store.purge(false).await.unwrap();
        assert!(
            !files_contain(dir.path(), MARKER.as_bytes()),
            "a purge leaves no trace payload of the deleted run in storage"
        );
        assert!(get(&rs, "tb").await.unwrap().is_some());

        // A trace without a run: deleted, then erased by the purge path (optimize, then purge).
        const OTHER: &str = "SYNTHETICUNLINKEDMARKER";
        let mut c = llm("tc", "s1", "model-a", 1, 1, 0);
        c.input = Some(json!(OTHER));
        ingest(&rs, &cfg, vec![c], &no_runs).await.unwrap();
        assert!(files_contain(dir.path(), OTHER.as_bytes()));
        assert!(delete(&rs, "tc").await.unwrap());
        store.purge(false).await.unwrap();
        assert!(
            !files_contain(dir.path(), OTHER.as_bytes()),
            "a purge alone erases a deleted trace without a run"
        );

        // A trace linked to its run by a later batch: the first chunk was written without a run
        // and moved under it. Deleting the run and a purge alone erase the first chunk's payload.
        const EARLY: &str = "SYNTHETICEARLYCHUNKMARKER";
        let w = store.create_run(NewRun::default()).await.unwrap();
        let late = w.info().id;
        w.finish(RunStatus::Finished, None).await.unwrap();
        let mut early = llm("td", "child", "model-a", 1, 1, 0);
        early.input = Some(json!(EARLY));
        ingest(&rs, &cfg, vec![early], &no_runs).await.unwrap();
        let mut root = span("td", "root", None, 0, 10);
        root.run_id = Some(late.clone());
        ingest(&rs, &cfg, vec![root], &no_runs).await.unwrap();
        assert_eq!(
            get(&rs, "td").await.unwrap().unwrap().0.run_id,
            Some(late.clone())
        );
        assert!(files_contain(dir.path(), EARLY.as_bytes()));
        store
            .delete_runs(std::slice::from_ref(&late))
            .await
            .unwrap();
        assert!(get(&rs, "td").await.unwrap().is_none());
        store.purge(false).await.unwrap();
        assert!(
            !files_contain(dir.path(), EARLY.as_bytes()),
            "rows moved under a run are erased with it"
        );
    }
}
