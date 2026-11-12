//! A typed client for an engine's `/api/v1` (feature `client`): one method per route.
//!
//! Requests carry the token as `Authorization: Bearer`, bodies over [`GZIP_ABOVE`] are sent
//! gzip-compressed, connections are kept alive, and errors come back as [`Error::Api`] with the
//! engine's status, code and message. `crate::local::resolve` finds (or starts) the engine of a
//! repository on this machine and returns a client for it.

use std::io::Write as _;
use std::time::Duration;

use reqwest::header::{HeaderValue, CONTENT_ENCODING, CONTENT_TYPE};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::auth::{CreateToken, CreatedToken, Me, Revoked, RotatedToken, SessionRequest, Tokens};
use crate::chart::{ChartRequest, ChartResponse};
use crate::evals::{
    Alert, AlertChange, Baseline, CommitDataset, CompareResponse, DataDriftResponse, DatasetInfo,
    DatasetQuery, DatasetResponse, DatasetVersion, EvalResult, Item, ItemsResponse, Label,
    NewLabel, PutFeatures, PutResults, ScheduleRunDetail, ScheduleRunStarted, ScheduleRunStatus,
    ScheduleRunsResponse, SchedulesResponse, SuiteDetail, SuiteSummary, VersionSummary,
};
use crate::ingest::{IngestBatch, IngestReport};
use crate::model::{Run, RunPatch};
use crate::runs::{
    CreateRun, CreatedRun, DeleteRuns, Deleted, FinishRun, Flushed, Meta, Optimized, PointsQuery,
    PurgeRequest, PurgeSummary, ResumedRun, RunDetail, RunFields, RunParams, RunRow, RunSeries,
    RunSummary, RunsQuery, RunsResponse, SeriesPoints, Stored,
};
use crate::traces::{
    DeleteTraces, IngestBody, TraceDeleted, TraceResponse, TracesDeleted, TracesQuery,
    TracesResponse, TracingPolicy,
};
use crate::{ErrorBody, Info};

/// Request bodies larger than this are sent gzip-compressed.
pub const GZIP_ABOVE: usize = 64 * 1024;

/// What the engine answered instead of success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    /// One of [`crate::code`], or `http_<status>` when the answer had no error body.
    pub code: String,
    pub message: String,
    /// Where in the request a query error is (`GET runs?q=`).
    pub position: Option<usize>,
}

/// Why a request failed.
#[derive(Debug)]
pub enum Error {
    /// The engine answered with an error.
    Api(ApiError),
    /// The engine could not be reached.
    Unreachable { url: String, message: String },
    /// The answer was not what the API describes.
    Unexpected(String),
}

impl Error {
    /// The engine's error code, when it answered with one.
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Api(e) => Some(&e.code),
            _ => None,
        }
    }

    /// The HTTP status, when the engine answered.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api(e) => Some(e.status),
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Api(e) => f.write_str(&e.message),
            Error::Unreachable { url, message } => write!(f, "cannot reach {url}: {message}"),
            Error::Unexpected(m) => write!(f, "unexpected answer from the engine: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A client for one engine.
#[derive(Clone, Debug)]
pub struct Client {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

/// Percent-encodes one path segment (run ids, dataset and schedule names).
fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn gzip(body: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    // Writing to a Vec cannot fail.
    let _ = enc.write_all(body);
    enc.finish().unwrap_or_default()
}

impl Client {
    /// A client for the engine at `base` (`http://127.0.0.1:7600`), sending `token` if given.
    pub fn new(base: &str, token: Option<String>) -> Client {
        let http = reqwest::Client::builder()
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Client {
            base: base.trim_end_matches('/').to_string(),
            token: token.filter(|t| !t.is_empty()),
            http,
        }
    }

    /// The engine's address.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The token sent with each request.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    fn request(&self, method: Method, url: String) -> reqwest::RequestBuilder {
        let r = self.http.request(method, url);
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    fn with_json(
        &self,
        r: reqwest::RequestBuilder,
        body: &(impl Serialize + ?Sized),
    ) -> Result<reqwest::RequestBuilder> {
        let bytes = serde_json::to_vec(body).map_err(|e| Error::Unexpected(e.to_string()))?;
        let r = r.header(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(if bytes.len() > GZIP_ABOVE {
            r.header(CONTENT_ENCODING, HeaderValue::from_static("gzip"))
                .body(gzip(&bytes))
        } else {
            r.body(bytes)
        })
    }

    async fn send(&self, r: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let res = r.send().await.map_err(|e| Error::Unreachable {
            url: self.base.clone(),
            message: e.to_string(),
        })?;
        if res.status().is_success() {
            return Ok(res);
        }
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        Err(Error::Api(match serde_json::from_str::<ErrorBody>(&text) {
            Ok(b) => ApiError {
                status: status.as_u16(),
                code: b.error.code,
                message: b.error.message,
                position: b.error.position,
            },
            Err(_) => ApiError {
                status: status.as_u16(),
                code: format!("http_{}", status.as_u16()),
                message: if text.trim().is_empty() {
                    format!("the engine answered {status}")
                } else {
                    text.trim().to_string()
                },
                position: None,
            },
        }))
    }

    async fn read<T: DeserializeOwned>(&self, r: reqwest::RequestBuilder) -> Result<T> {
        let res = self.send(r).await?;
        let bytes = res.bytes().await.map_err(|e| Error::Unreachable {
            url: self.base.clone(),
            message: e.to_string(),
        })?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Unexpected(e.to_string()))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.read(self.request(Method::GET, self.url(path))).await
    }

    async fn get_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &(impl Serialize + ?Sized),
    ) -> Result<T> {
        self.read(self.request(Method::GET, self.url(path)).query(query))
            .await
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &(impl Serialize + ?Sized),
    ) -> Result<T> {
        let r = self.with_json(self.request(method, self.url(path)), body)?;
        self.read(r).await
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &(impl Serialize + ?Sized),
    ) -> Result<T> {
        self.call(Method::POST, path, body).await
    }

    // -----------------------------------------------------------------------------------------
    // Engine

    /// `GET health`: `ok`.
    pub async fn health(&self) -> Result<String> {
        let res = self
            .send(self.request(Method::GET, self.url("/health")))
            .await?;
        res.text()
            .await
            .map_err(|e| Error::Unexpected(e.to_string()))
    }

    /// `GET info`: version, API version, mode and capabilities.
    pub async fn info(&self) -> Result<Info> {
        self.get("/info").await
    }

    /// `GET meta`: counts, experiments, metric and parameter keys.
    pub async fn meta(&self) -> Result<Meta> {
        self.get("/meta").await
    }

    // -----------------------------------------------------------------------------------------
    // Runs

    /// `GET runs` with parameters and metric statistics (`fields` is set to `full`).
    pub async fn runs(&self, query: &RunsQuery) -> Result<RunsResponse> {
        let q = RunsQuery {
            fields: RunFields::Full,
            ..query.clone()
        };
        self.get_query("/runs", &q).await
    }

    /// `GET runs?fields=summary`: rows without parameters and metric statistics.
    pub async fn run_summaries(&self, query: &RunsQuery) -> Result<RunsResponse<RunSummary>> {
        let q = RunsQuery {
            fields: RunFields::Summary,
            ..query.clone()
        };
        self.get_query("/runs", &q).await
    }

    /// `GET runs/{id}`.
    pub async fn run(&self, id: &str) -> Result<RunDetail> {
        self.get(&format!("/runs/{}", seg(id))).await
    }

    /// `PATCH runs/{id}`.
    pub async fn patch_run(&self, id: &str, patch: &RunPatch) -> Result<RunRow> {
        self.call(Method::PATCH, &format!("/runs/{}", seg(id)), patch)
            .await
    }

    /// `POST runs/delete`.
    pub async fn delete_runs(&self, req: &DeleteRuns) -> Result<Deleted> {
        self.post("/runs/delete", req).await
    }

    /// `GET runs/{id}/params`.
    pub async fn run_params(&self, id: &str) -> Result<RunParams> {
        self.get(&format!("/runs/{}/params", seg(id))).await
    }

    /// `GET runs/{id}/series`.
    pub async fn run_series(&self, id: &str) -> Result<RunSeries> {
        self.get(&format!("/runs/{}/series", seg(id))).await
    }

    /// `GET runs/{id}/points`.
    pub async fn run_points(&self, id: &str, query: &PointsQuery) -> Result<SeriesPoints> {
        self.get_query(&format!("/runs/{}/points", seg(id)), query)
            .await
    }

    /// `POST chart`.
    pub async fn chart(&self, req: &ChartRequest) -> Result<ChartResponse> {
        self.post("/chart", req).await
    }

    /// `POST runs`: creates a run, or answers with the existing one for a known id.
    pub async fn create_run(&self, req: &CreateRun) -> Result<CreatedRun> {
        self.post("/runs", req).await
    }

    /// `POST runs/{id}/resume`.
    pub async fn resume_run(&self, id: &str) -> Result<ResumedRun> {
        let r = self.request(Method::POST, self.url(&format!("/runs/{}/resume", seg(id))));
        self.read(r).await
    }

    /// `POST runs/{id}/finish`.
    pub async fn finish_run(&self, id: &str, req: &FinishRun) -> Result<Run> {
        self.post(&format!("/runs/{}/finish", seg(id)), req).await
    }

    /// `POST runs/{id}/results`.
    pub async fn put_results(&self, id: &str, req: &PutResults) -> Result<Stored> {
        self.post(&format!("/runs/{}/results", seg(id)), req).await
    }

    /// `GET runs/{id}/results`.
    pub async fn run_results(&self, id: &str) -> Result<Vec<EvalResult>> {
        self.get(&format!("/runs/{}/results", seg(id))).await
    }

    /// `POST runs/{id}/features`.
    pub async fn put_features(&self, id: &str, req: &PutFeatures) -> Result<Stored> {
        self.post(&format!("/runs/{}/features", seg(id)), req).await
    }

    /// `GET runs/{id}/items`: an evaluation run's items; only failures with `failures`.
    pub async fn run_items(&self, id: &str, failures: bool) -> Result<ItemsResponse> {
        self.get_query(
            &format!("/runs/{}/items", seg(id)),
            &[("failures", failures)],
        )
        .await
    }

    /// `POST runs/{id}/labels`.
    pub async fn add_label(&self, id: &str, label: &NewLabel) -> Result<Label> {
        self.post(&format!("/runs/{}/labels", seg(id)), label).await
    }

    /// `GET runs/{id}/audit`: an evaluation run's audit record.
    pub async fn run_audit(&self, id: &str) -> Result<Value> {
        self.get(&format!("/runs/{}/audit", seg(id))).await
    }

    /// `GET compare?a=&b=`: two evaluation runs item by item.
    pub async fn compare(&self, a: &str, b: &str) -> Result<CompareResponse> {
        self.get_query("/compare", &[("a", a), ("b", b)]).await
    }

    // -----------------------------------------------------------------------------------------
    // Writes

    /// `POST ingest`: one batch, with an `Idempotency-Key` when given.
    pub async fn ingest(
        &self,
        batch: &IngestBatch,
        idempotency_key: Option<&str>,
    ) -> Result<IngestReport> {
        let mut r = self.request(Method::POST, self.url("/ingest"));
        if let Some(k) = idempotency_key {
            r = r.header("idempotency-key", k);
        }
        let r = self.with_json(r, batch)?;
        self.read(r).await
    }

    /// `POST flush`: writes out open runs and refreshes the catalog.
    pub async fn flush(&self) -> Result<Flushed> {
        self.read(self.request(Method::POST, self.url("/flush")))
            .await
    }

    // -----------------------------------------------------------------------------------------
    // Datasets

    /// `GET datasets`.
    pub async fn datasets(&self) -> Result<Vec<DatasetInfo>> {
        self.get("/datasets").await
    }

    /// `GET datasets/{name}`: one version's items, a page of them.
    pub async fn dataset(&self, name: &str, query: &DatasetQuery) -> Result<DatasetResponse> {
        self.get_query(&format!("/datasets/{}", seg(name)), query)
            .await
    }

    /// `GET datasets/{name}/versions`.
    pub async fn dataset_versions(&self, name: &str) -> Result<Vec<VersionSummary>> {
        self.get(&format!("/datasets/{}/versions", seg(name))).await
    }

    /// `GET datasets/{name}/versions/{version}/items`: every item, read from NDJSON.
    pub async fn dataset_items(&self, name: &str, version: u32) -> Result<Vec<Item>> {
        let r = self.request(
            Method::GET,
            self.url(&format!("/datasets/{}/versions/{version}/items", seg(name))),
        );
        let text = self
            .send(r)
            .await?
            .text()
            .await
            .map_err(|e| Error::Unexpected(e.to_string()))?;
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).map_err(|e| Error::Unexpected(e.to_string())))
            .collect()
    }

    /// `POST datasets/{name}/versions` with an `Idempotency-Key` (reuse it on retries).
    pub async fn commit_dataset(
        &self,
        name: &str,
        req: &CommitDataset,
        idempotency_key: &str,
    ) -> Result<DatasetVersion> {
        let r = self
            .request(
                Method::POST,
                self.url(&format!("/datasets/{}/versions", seg(name))),
            )
            .header("idempotency-key", idempotency_key);
        let r = self.with_json(r, req)?;
        self.read(r).await
    }

    // -----------------------------------------------------------------------------------------
    // Schedules, suites, alerts

    /// `GET schedules`.
    pub async fn schedules(&self) -> Result<SchedulesResponse> {
        self.get("/schedules").await
    }

    /// `GET schedules/{name}/runs`.
    pub async fn schedule_runs(&self, name: &str) -> Result<ScheduleRunsResponse> {
        self.get(&format!("/schedules/{}/runs", seg(name))).await
    }

    /// `GET schedules/{name}/runs/{id}`.
    pub async fn schedule_run(&self, name: &str, id: &str) -> Result<ScheduleRunDetail> {
        self.get(&format!("/schedules/{}/runs/{}", seg(name), seg(id)))
            .await
    }

    /// `POST schedules/{name}/run`: starts the schedule's command now (202); follow it with
    /// [`Client::follow_schedule_run`].
    pub async fn start_schedule_run(&self, name: &str) -> Result<ScheduleRunStarted> {
        self.post(
            &format!("/schedules/{}/run", seg(name)),
            &Value::Object(Default::default()),
        )
        .await
    }

    /// Follows a schedule run from its first attempt `id` through its retries until the last
    /// attempt ends; `on_attempt` sees each attempt once it ended. Returns the last attempt.
    pub async fn follow_schedule_run(
        &self,
        name: &str,
        id: &str,
        mut on_attempt: impl FnMut(&ScheduleRunDetail),
    ) -> Result<ScheduleRunDetail> {
        let schedule = self
            .schedules()
            .await?
            .schedules
            .into_iter()
            .find(|s| s.name == name);
        let attempts = schedule.as_ref().map(|s| s.retries + 1).unwrap_or(1);
        let retry_wait = schedule
            .as_ref()
            .and_then(|s| parse_duration(&s.retry_delay))
            .unwrap_or(Duration::from_secs(60))
            + Duration::from_secs(30);
        let mut current = id.to_string();
        loop {
            let done = loop {
                let r = self.schedule_run(name, &current).await?;
                if r.run.status != ScheduleRunStatus::Running {
                    break r;
                }
                tokio::time::sleep(FOLLOW_EVERY).await;
            };
            on_attempt(&done);
            if done.run.status != ScheduleRunStatus::Failed
                && done.run.status != ScheduleRunStatus::TimedOut
                || done.run.attempt >= attempts
            {
                return Ok(done);
            }
            // The next attempt starts after the retry delay, for the same scheduled time.
            let deadline = tokio::time::Instant::now() + retry_wait;
            let next = loop {
                let runs = self.schedule_runs(name).await?;
                if let Some(n) = runs.runs.iter().find(|r| {
                    r.run.scheduled_for == done.run.scheduled_for
                        && r.run.attempt == done.run.attempt + 1
                }) {
                    break Some(n.run.id.clone());
                }
                if tokio::time::Instant::now() >= deadline {
                    break None;
                }
                tokio::time::sleep(FOLLOW_EVERY).await;
            };
            match next {
                Some(n) => current = n,
                None => return Ok(done),
            }
        }
    }

    /// `GET suites`.
    pub async fn suites(&self) -> Result<Vec<SuiteSummary>> {
        self.get("/suites").await
    }

    /// `GET suites/{key}`, as of evaluation run `run` when given.
    pub async fn suite(&self, key: &str, run: Option<&str>) -> Result<SuiteDetail> {
        self.get_query(&format!("/suites/{}", seg(key)), &[("run", run)])
            .await
    }

    /// `PUT suites/{key}/baseline`.
    pub async fn set_baseline(&self, key: &str, baseline: &Baseline) -> Result<Baseline> {
        self.call(
            Method::PUT,
            &format!("/suites/{}/baseline", seg(key)),
            baseline,
        )
        .await
    }

    /// `GET suites/{key}/data-drift`.
    pub async fn data_drift(&self, key: &str, run: Option<&str>) -> Result<DataDriftResponse> {
        self.get_query(&format!("/suites/{}/data-drift", seg(key)), &[("run", run)])
            .await
    }

    /// `GET alerts`.
    pub async fn alerts(&self) -> Result<Vec<Alert>> {
        self.get("/alerts").await
    }

    /// `POST alerts/{id}`: acknowledges or reopens an alert.
    pub async fn update_alert(&self, id: &str, change: &AlertChange) -> Result<Alert> {
        self.post(&format!("/alerts/{}", seg(id)), change).await
    }

    // -----------------------------------------------------------------------------------------
    // Traces

    /// `POST traces/ingest`.
    pub async fn ingest_spans(&self, body: &IngestBody) -> Result<crate::traces::IngestReport> {
        self.post("/traces/ingest", body).await
    }

    /// `POST /v1/traces` (OpenTelemetry OTLP/HTTP JSON, at the root rather than under `/api/v1`).
    pub async fn otlp_traces(&self, body: &Value) -> Result<Value> {
        let r = self.request(Method::POST, format!("{}/v1/traces", self.base));
        let r = self.with_json(r, body)?;
        self.read(r).await
    }

    /// `GET traces`.
    pub async fn traces(&self, query: &TracesQuery) -> Result<TracesResponse> {
        self.get_query("/traces", query).await
    }

    /// `GET traces/{id}`.
    pub async fn trace(&self, id: &str) -> Result<TraceResponse> {
        self.get(&format!("/traces/{}", seg(id))).await
    }

    /// `DELETE traces/{id}`.
    pub async fn delete_trace(&self, id: &str) -> Result<TraceDeleted> {
        self.read(self.request(Method::DELETE, self.url(&format!("/traces/{}", seg(id)))))
            .await
    }

    /// `POST traces/delete`: many at once, erased from storage with `erase`.
    pub async fn delete_traces(&self, req: &DeleteTraces) -> Result<TracesDeleted> {
        self.post("/traces/delete", req).await
    }

    /// `GET tracing/policy`, for `project` when given.
    pub async fn tracing_policy(&self, project: Option<&str>) -> Result<TracingPolicy> {
        self.get_query("/tracing/policy", &[("project", project)])
            .await
    }

    // -----------------------------------------------------------------------------------------
    // Admin and auth

    /// `POST admin/purge`: erases deleted runs and traces from storage.
    pub async fn purge(&self, force: bool) -> Result<PurgeSummary> {
        self.post("/admin/purge", &PurgeRequest { force }).await
    }

    /// `POST admin/optimize`.
    pub async fn optimize(&self) -> Result<Optimized> {
        self.post("/admin/optimize", &Value::Object(Default::default()))
            .await
    }

    /// `GET auth/me`.
    pub async fn me(&self) -> Result<Me> {
        self.get("/auth/me").await
    }

    /// `POST auth/session`: checks `token` and answers who it is (a browser also gets a cookie).
    pub async fn sign_in(&self, token: &str) -> Result<Me> {
        self.post(
            "/auth/session",
            &SessionRequest {
                token: token.to_string(),
            },
        )
        .await
    }

    /// `DELETE auth/session`.
    pub async fn sign_out(&self) -> Result<Me> {
        self.read(self.request(Method::DELETE, self.url("/auth/session")))
            .await
    }

    /// `GET auth/tokens`.
    pub async fn tokens(&self) -> Result<Tokens> {
        self.get("/auth/tokens").await
    }

    /// `POST auth/tokens`: the new token is in the answer, once.
    pub async fn create_token(&self, req: &CreateToken) -> Result<CreatedToken> {
        self.post("/auth/tokens", req).await
    }

    /// `POST auth/local-token/rotate`: a local engine's new token (admin).
    pub async fn rotate_local_token(&self) -> Result<RotatedToken> {
        self.post(
            "/auth/local-token/rotate",
            &Value::Object(Default::default()),
        )
        .await
    }

    /// `DELETE auth/tokens/{id}`.
    pub async fn revoke_token(&self, id: &str) -> Result<Revoked> {
        self.read(self.request(
            Method::DELETE,
            self.url(&format!("/auth/tokens/{}", seg(id))),
        ))
        .await
    }
}

/// How often [`Client::follow_schedule_run`] asks.
const FOLLOW_EVERY: Duration = Duration::from_millis(500);

/// A duration such as `90s`, `2m`, `1h` (a schedule's `retry_delay`).
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let i = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let n: f64 = s[..i].trim().parse().ok()?;
    let secs = match &s[i..] {
        "" | "s" => n,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        "d" => n * 86400.0,
        _ => return None,
    };
    (secs >= 0.0 && secs.is_finite()).then(|| Duration::from_secs_f64(secs))
}
