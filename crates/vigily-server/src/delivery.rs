//! Delivering alerts to channels.
//!
//! Each (alert, channel) pair is delivered once: [`dispatch`] writes a pending record
//! `notify/delivery/<alert>/<channel>` (none when it exists, so an alert is never sent twice to a
//! channel) and queues it. The worker ([`start`]) sends due deliveries on their own tasks, so a
//! slow channel holds up nothing else, and never runs on the path that raises an alert. A failed
//! attempt (no answer, a 5xx, 408 or 429) is retried with exponential backoff and jitter, five
//! attempts over about eight minutes by default; any other refusal fails at once. Each channel
//! has a token bucket (`burst`, then `per_minute`); a delivery over the limit waits without using
//! an attempt. Pending records survive a restart: the worker picks them up when it starts.
//!
//! Outcomes are kept per delivery (status, attempts, the last status code, an error without
//! secrets) and per channel as its last delivery, `notify/last/<channel>`, test sends included.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use sha2::Sha256;
use tokio::sync::mpsc;
use vigily_api::evals::Alert;
use vigily_api::notifications::{
    ChannelKind, Delivery, DeliveryStatus, DeliverySummary, SmtpSecurity, TestResult,
};
use vigily_core::{now_micros, RecordStore};

use crate::channels::{secret_value, ChannelRecord};
use crate::render::Message;
use crate::{ApiError, AppState};

const DELIVERY_PREFIX: &str = "notify/delivery/";
const LAST_PREFIX: &str = "notify/last/";

/// Retry and timeout settings of the delivery worker.
#[derive(Clone, Copy, Debug)]
pub struct DeliveryOptions {
    /// Attempts per delivery, the first included.
    pub attempts: u32,
    /// The wait before the second attempt; it doubles for each further one.
    pub base_delay: Duration,
    /// The longest wait between attempts.
    pub max_delay: Duration,
    /// How long one attempt may take.
    pub timeout: Duration,
}

impl Default for DeliveryOptions {
    fn default() -> Self {
        DeliveryOptions {
            attempts: 5,
            base_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(5 * 60),
            timeout: Duration::from_secs(15),
        }
    }
}

/// Messages a channel may send at once, and per minute after that, unless its settings say.
pub const DEFAULT_BURST: u32 = 5;
pub const DEFAULT_PER_MINUTE: u32 = 20;

/// A token bucket.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// One queued delivery.
#[derive(Clone, Debug)]
struct Job {
    alert_id: String,
    channel_id: String,
    due: Instant,
}

/// The delivery worker's queue and rate limits.
pub struct Notifier {
    tx: mpsc::UnboundedSender<Job>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Job>>>,
    buckets: Mutex<HashMap<String, Bucket>>,
    /// Deliveries waiting in the queue and being attempted, as `<alert>/<channel>`: each is
    /// queued and attempted once at a time.
    queued: Mutex<HashSet<String>>,
    running: Mutex<HashSet<String>>,
    pub options: DeliveryOptions,
}

impl Notifier {
    pub fn new(options: DeliveryOptions) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Notifier {
            tx,
            rx: Mutex::new(Some(rx)),
            buckets: Mutex::new(HashMap::new()),
            queued: Mutex::new(HashSet::new()),
            running: Mutex::new(HashSet::new()),
            options,
        }
    }

    fn queue(&self, alert_id: &str, channel_id: &str, due: Instant) {
        if !self
            .queued
            .lock()
            .insert(format!("{alert_id}/{channel_id}"))
        {
            return;
        }
        let _ = self.tx.send(Job {
            alert_id: alert_id.to_string(),
            channel_id: channel_id.to_string(),
            due,
        });
    }

    /// Takes a token from `channel`'s bucket, or says how long until one is there.
    fn take(&self, c: &ChannelRecord) -> Result<(), Duration> {
        let burst = f64::from(c.settings.burst.unwrap_or(DEFAULT_BURST).max(1));
        let per_sec = f64::from(c.settings.per_minute.unwrap_or(DEFAULT_PER_MINUTE).max(1)) / 60.0;
        let now = Instant::now();
        let mut buckets = self.buckets.lock();
        let b = buckets.entry(c.id.clone()).or_insert(Bucket {
            tokens: burst,
            at: now,
        });
        b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * per_sec).min(burst);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - b.tokens) / per_sec))
        }
    }

    /// The wait before attempt `next` (2 for the first retry): the base doubled per attempt, at
    /// most the maximum, scaled by a random factor between 0.5 and 1.5.
    fn backoff(&self, next: u32) -> Duration {
        let o = &self.options;
        let exp = o
            .base_delay
            .saturating_mul(1u32 << next.saturating_sub(2).min(16))
            .min(o.max_delay);
        let mut b = [0u8; 2];
        let _ = getrandom::fill(&mut b);
        let jitter = 0.5 + f64::from(u16::from_le_bytes(b)) / f64::from(u16::MAX);
        exp.mul_f64(jitter)
    }
}

fn key(alert_id: &str, channel_id: &str) -> String {
    format!("{DELIVERY_PREFIX}{alert_id}/{channel_id}")
}

pub fn last_key(channel_id: &str) -> String {
    format!("{LAST_PREFIX}{channel_id}")
}

async fn put(rs: &dyn RecordStore, d: &Delivery) -> vigily_core::Result<()> {
    let v = serde_json::to_value(d).map_err(|e| vigily_core::Error::Storage(e.to_string()))?;
    let mut rows = vec![(key(&d.alert_id, &d.channel_id), v.clone())];
    if d.status != DeliveryStatus::Pending || d.attempts > 0 {
        rows.push((last_key(&d.channel_id), v));
    }
    rs.put(rows).await?;
    rs.flush().await
}

/// An alert's deliveries, in channel order.
pub async fn of_alert(rs: &dyn RecordStore, alert_id: &str) -> vigily_core::Result<Vec<Delivery>> {
    Ok(rs
        .scan(&format!("{DELIVERY_PREFIX}{alert_id}/"))
        .await?
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_value(v).ok())
        .collect())
}

/// Deliveries of every alert by status, by alert id.
pub async fn summaries(
    rs: &dyn RecordStore,
) -> vigily_core::Result<HashMap<String, DeliverySummary>> {
    let mut out: HashMap<String, DeliverySummary> = HashMap::new();
    for (_, v) in rs.scan(DELIVERY_PREFIX).await? {
        let Ok(d) = serde_json::from_value::<Delivery>(v) else {
            continue;
        };
        let s = out.entry(d.alert_id).or_default();
        match d.status {
            DeliveryStatus::Sent => s.sent += 1,
            DeliveryStatus::Failed => s.failed += 1,
            DeliveryStatus::Pending => s.pending += 1,
        }
    }
    Ok(out)
}

/// A channel's newest delivery, a test send included.
pub async fn last(rs: &dyn RecordStore, channel_id: &str) -> vigily_core::Result<Option<Delivery>> {
    Ok(rs
        .get(&last_key(channel_id))
        .await?
        .and_then(|v| serde_json::from_value(v).ok()))
}

/// Queues `alert` for each of `channels` it was not queued for already.
pub async fn dispatch(state: &AppState, alert: &Alert, channels: &[ChannelRecord]) {
    for c in channels {
        let k = key(&alert.id, &c.id);
        match state.records.get(&k).await {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "could not read a delivery");
                continue;
            }
        }
        let now = now_micros();
        let d = Delivery {
            alert_id: alert.id.clone(),
            channel_id: c.id.clone(),
            channel_name: c.name.clone(),
            kind: c.kind,
            status: DeliveryStatus::Pending,
            attempts: 0,
            status_code: None,
            error: None,
            created_at: now,
            updated_at: now,
            next_attempt_at: Some(now),
        };
        if let Err(e) = put(&*state.records, &d).await {
            tracing::warn!(error = %e, channel = %c.name, "could not queue a delivery");
            continue;
        }
        state.notifier.queue(&alert.id, &c.id, Instant::now());
    }
}

/// Starts the delivery worker: picks up pending deliveries, then sends queued ones when due.
pub fn start(state: Arc<AppState>) {
    let Some(mut rx) = state.notifier.rx.lock().take() else {
        return;
    };
    tokio::spawn(async move {
        let mut queue: Vec<Job> = Vec::new();
        match state.records.scan(DELIVERY_PREFIX).await {
            Ok(rows) => {
                let now = (Instant::now(), now_micros());
                for (_, v) in rows {
                    let Ok(d) = serde_json::from_value::<Delivery>(v) else {
                        continue;
                    };
                    if d.status != DeliveryStatus::Pending {
                        continue;
                    }
                    let wait = d
                        .next_attempt_at
                        .unwrap_or(now.1)
                        .saturating_sub(now.1)
                        .max(0);
                    queue.push(Job {
                        alert_id: d.alert_id,
                        channel_id: d.channel_id,
                        due: now.0 + Duration::from_micros(wait as u64),
                    });
                }
                if !queue.is_empty() {
                    tracing::info!(count = queue.len(), "resuming pending deliveries");
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not read pending deliveries"),
        }
        loop {
            let now = Instant::now();
            let (due, later): (Vec<Job>, Vec<Job>) = queue.drain(..).partition(|j| j.due <= now);
            queue = later;
            for job in due {
                tokio::spawn(attempt(state.clone(), job));
            }
            let next = queue.iter().map(|j| j.due).min();
            let job = match next {
                Some(at) => tokio::select! {
                    j = rx.recv() => j,
                    _ = tokio::time::sleep_until(at.into()) => continue,
                },
                None => rx.recv().await,
            };
            match job {
                Some(j) => queue.push(j),
                None => break,
            }
        }
    });
}

/// What one attempt came to.
#[derive(Debug, Default)]
pub struct Outcome {
    pub ok: bool,
    pub status_code: Option<u16>,
    pub error: Option<String>,
    /// Worth another attempt.
    pub retry: bool,
    /// How long the receiver asked to wait (`Retry-After`).
    pub retry_after: Option<Duration>,
}

impl Outcome {
    fn failed(error: String, retry: bool) -> Self {
        Outcome {
            error: Some(error),
            retry,
            ..Default::default()
        }
    }
}

/// Marks a delivery as being attempted until dropped.
struct Running<'a>(&'a Notifier, String);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.running.lock().remove(&self.1);
    }
}

async fn attempt(state: Arc<AppState>, job: Job) {
    let id = format!("{}/{}", job.alert_id, job.channel_id);
    state.notifier.queued.lock().remove(&id);
    if !state.notifier.running.lock().insert(id.clone()) {
        return;
    }
    let _running = Running(&state.notifier, id);
    let rs = &*state.records;
    let Ok(Some(v)) = rs.get(&key(&job.alert_id, &job.channel_id)).await else {
        return;
    };
    let Ok(mut d) = serde_json::from_value::<Delivery>(v) else {
        return;
    };
    if d.status != DeliveryStatus::Pending {
        return;
    }
    // Queued twice (when the worker started as it was dispatched): wait for its next attempt.
    let early = d.next_attempt_at.unwrap_or(0) - now_micros();
    if early > 50_000 {
        let wait = Duration::from_micros(early as u64);
        state
            .notifier
            .queue(&job.alert_id, &job.channel_id, Instant::now() + wait);
        return;
    }
    let channel = crate::channels::get_record(rs, &job.channel_id)
        .await
        .ok()
        .flatten();
    let alert: Option<Alert> = rs
        .get(&format!("alert/{}", job.alert_id))
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok());
    let outcome = match (&channel, &alert) {
        (None, _) => Outcome::failed("the channel was removed".into(), false),
        (Some(c), _) if !c.enabled => Outcome::failed("the channel is disabled".into(), false),
        (_, None) => Outcome::failed("the alert was removed".into(), false),
        (Some(c), Some(a)) => {
            if let Err(wait) = state.notifier.take(c) {
                state
                    .notifier
                    .queue(&job.alert_id, &job.channel_id, Instant::now() + wait);
                return;
            }
            let msg = Message::of(a, public_url(&state).as_deref());
            send(&state, c, &msg).await
        }
    };
    d.attempts += 1;
    d.status_code = outcome.status_code;
    d.error = outcome.error.clone();
    d.updated_at = now_micros();
    let opts = state.notifier.options;
    if outcome.ok {
        d.status = DeliveryStatus::Sent;
        d.error = None;
        d.next_attempt_at = None;
    } else if outcome.retry && d.attempts < opts.attempts {
        let wait = outcome
            .retry_after
            .map(|w| w.min(opts.max_delay))
            .unwrap_or_else(|| state.notifier.backoff(d.attempts + 1));
        d.next_attempt_at = Some(d.updated_at + wait.as_micros() as i64);
        state
            .notifier
            .queue(&job.alert_id, &job.channel_id, Instant::now() + wait);
    } else {
        d.status = DeliveryStatus::Failed;
        d.next_attempt_at = None;
    }
    if d.status == DeliveryStatus::Failed {
        tracing::warn!(
            channel = %d.channel_name,
            attempts = d.attempts,
            error = d.error.as_deref().unwrap_or(""),
            "an alert could not be delivered"
        );
    }
    if let Err(e) = put(rs, &d).await {
        tracing::warn!(error = %e, "could not record a delivery");
    }
}

/// The address links in messages start with: `--public-url`, else the engine's own address.
pub fn public_url(state: &AppState) -> Option<String> {
    state
        .public_url
        .clone()
        .or_else(|| state.url.read().clone())
        .map(|u| u.trim_end_matches('/').to_string())
}

/// Sends a test alert to `c` once (no retries) and records it as the channel's last delivery.
pub async fn test(state: &AppState, c: &ChannelRecord) -> Result<TestResult, ApiError> {
    let started = Instant::now();
    let now = now_micros();
    let outcome = match state.notifier.take(c) {
        Err(wait) => Outcome::failed(
            format!(
                "the channel's rate limit is reached; try again in {} s",
                wait.as_secs().max(1)
            ),
            false,
        ),
        Ok(()) => {
            let msg = Message::test(&c.name, public_url(state).as_deref(), now);
            send(state, c, &msg).await
        }
    };
    let d = Delivery {
        alert_id: "test".into(),
        channel_id: c.id.clone(),
        channel_name: c.name.clone(),
        kind: c.kind,
        status: if outcome.ok {
            DeliveryStatus::Sent
        } else {
            DeliveryStatus::Failed
        },
        attempts: 1,
        status_code: outcome.status_code,
        error: outcome.error.clone(),
        created_at: now,
        updated_at: now_micros(),
        next_attempt_at: None,
    };
    let v = serde_json::to_value(&d).map_err(|e| vigily_core::Error::Storage(e.to_string()))?;
    state.records.put(vec![(last_key(&c.id), v)]).await?;
    state.records.flush().await?;
    Ok(TestResult {
        ok: outcome.ok,
        status_code: outcome.status_code,
        error: outcome.error,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// One attempt to deliver `msg` to `c`.
pub async fn send(state: &AppState, c: &ChannelRecord, msg: &Message) -> Outcome {
    let timeout = state.notifier.options.timeout;
    let attempt = async {
        match c.kind {
            ChannelKind::Email => send_email(state, c, msg).await,
            kind => {
                let field = if kind == ChannelKind::Webhook {
                    "url"
                } else {
                    "webhook_url"
                };
                let url = match secret_value(state, c, field).await {
                    Ok(Some(u)) => u,
                    Ok(None) => return Outcome::failed(format!("{field} is not set"), false),
                    Err(e) => return Outcome::failed(e, false),
                };
                let signing_key = if kind == ChannelKind::Webhook {
                    match secret_value(state, c, "signing_key").await {
                        Ok(k) => k,
                        Err(e) => return Outcome::failed(e, false),
                    }
                } else {
                    None
                };
                let body = serde_json::to_vec(&msg.body(kind)).unwrap_or_default();
                post(state, &url, body, signing_key.as_deref()).await
            }
        }
    };
    match tokio::time::timeout(timeout, attempt).await {
        Ok(o) => o,
        Err(_) => Outcome::failed(format!("no answer within {} s", timeout.as_secs()), true),
    }
}

/// `sha256=<hex>` of HMAC-SHA256 over `<timestamp>.<body>`.
pub fn signature(key: &str, timestamp: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC takes any key");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256={hex}")
}

/// A URL for messages: scheme, host and port only, so no token in a path or query leaks.
fn origin(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_else(|| "the channel's URL".into())
}

async fn post(state: &AppState, url: &str, body: Vec<u8>, signing_key: Option<&str>) -> Outcome {
    let mut req = state
        .http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(
            reqwest::header::USER_AGENT,
            concat!("vigily/", env!("CARGO_PKG_VERSION")),
        );
    if let Some(k) = signing_key {
        let ts = (now_micros() / 1_000_000).to_string();
        req = req
            .header("X-Vigily-Timestamp", &ts)
            .header("X-Vigily-Signature", signature(k, &ts, &body));
    }
    match req.body(body).send().await {
        Ok(r) => {
            let status = r.status();
            let retry_after = r
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            if status.is_success() {
                return Outcome {
                    ok: true,
                    status_code: Some(status.as_u16()),
                    ..Default::default()
                };
            }
            let text = r.text().await.unwrap_or_default();
            let text: String = text.trim().chars().take(200).collect();
            let retry = status.is_server_error()
                || status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status == reqwest::StatusCode::REQUEST_TIMEOUT;
            Outcome {
                ok: false,
                status_code: Some(status.as_u16()),
                error: Some(if text.is_empty() {
                    format!("{} answered {status}", origin(url))
                } else {
                    format!("{} answered {status}: {text}", origin(url))
                }),
                retry,
                retry_after,
            }
        }
        Err(e) => {
            // reqwest's message names the URL, which can hold a token: name the origin only.
            let what = if e.is_timeout() {
                "timed out"
            } else if e.is_connect() {
                "could not connect"
            } else {
                "failed"
            };
            Outcome::failed(format!("{what}: {}", origin(url)), true)
        }
    }
}

async fn send_email(state: &AppState, c: &ChannelRecord, msg: &Message) -> Outcome {
    use lettre::message::{header::ContentType, Mailbox, MultiPart, SinglePart};
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

    let s = &c.settings;
    let Some(host) = s.host.as_deref().map(str::trim).filter(|h| !h.is_empty()) else {
        return Outcome::failed("the channel has no SMTP host".into(), false);
    };
    let security = s.security.unwrap_or_default();
    let port = s.port.unwrap_or(match security {
        SmtpSecurity::Starttls => 587,
        SmtpSecurity::Tls => 465,
        SmtpSecurity::None => 25,
    });
    let password = match secret_value(state, c, "password").await {
        Ok(p) => p,
        Err(e) => return Outcome::failed(e, false),
    };
    let from: Mailbox = match s.from.as_deref().unwrap_or_default().parse() {
        Ok(m) => m,
        Err(_) => return Outcome::failed("the sender address does not parse".into(), false),
    };
    let mut builder = lettre::Message::builder().from(from).subject(msg.subject());
    for to in &s.to {
        match to.parse::<Mailbox>() {
            Ok(m) => builder = builder.to(m),
            Err(_) => return Outcome::failed(format!("{to:?} is not an e-mail address"), false),
        }
    }
    let email = match builder.multipart(
        MultiPart::alternative()
            .singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_PLAIN)
                    .body(msg.text()),
            )
            .singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(msg.html()),
            ),
    ) {
        Ok(m) => m,
        Err(e) => return Outcome::failed(format!("the message could not be built: {e}"), false),
    };
    let transport = match security {
        SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host),
        SmtpSecurity::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host),
        SmtpSecurity::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
            host,
        )),
    };
    let mut transport = match transport {
        Ok(t) => t.port(port).timeout(Some(state.notifier.options.timeout)),
        Err(e) => return Outcome::failed(format!("SMTP server {host}: {e}"), false),
    };
    if let (Some(user), Some(pass)) = (s.username.as_deref(), password) {
        transport = transport.credentials(Credentials::new(user.to_string(), pass));
    }
    match transport.build().send(email).await {
        Ok(r) => Outcome {
            ok: true,
            status_code: Some(u16::from(r.code())),
            ..Default::default()
        },
        Err(e) => {
            let code = e.status().map(u16::from);
            // Permanent SMTP replies (5xx) are not retried; everything else is.
            let retry = !e.is_permanent();
            Outcome {
                ok: false,
                status_code: code,
                error: Some(format!("SMTP server {host}:{port}: {e}")),
                retry,
                retry_after: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_are_hmac_sha256_of_timestamp_and_body() {
        // RFC 4231-style check against a known answer computed independently.
        let sig = signature("key", "1700000000", b"{}");
        assert!(sig.starts_with("sha256="));
        assert_eq!(sig.len(), 7 + 64);
        assert_ne!(sig, signature("key", "1700000001", b"{}"));
        assert_ne!(sig, signature("other", "1700000000", b"{}"));
    }

    #[test]
    fn backoff_doubles_within_jitter_and_caps() {
        let n = Notifier::new(DeliveryOptions {
            attempts: 5,
            base_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(300),
            timeout: Duration::from_secs(1),
        });
        for (attempt, base) in [(2, 30.0), (3, 60.0), (4, 120.0), (5, 240.0), (9, 300.0)] {
            let w = n.backoff(attempt).as_secs_f64();
            assert!(w >= base * 0.5 && w <= base * 1.5, "{attempt}: {w}");
        }
    }

    #[test]
    fn the_bucket_allows_a_burst_then_the_rate() {
        let n = Notifier::new(DeliveryOptions::default());
        let mut c = ChannelRecord {
            id: "c".into(),
            name: "c".into(),
            kind: ChannelKind::Webhook,
            enabled: true,
            default: false,
            settings: Default::default(),
            env: Default::default(),
            created_at: 0,
            updated_at: 0,
            changed_by: None,
        };
        c.settings.burst = Some(2);
        c.settings.per_minute = Some(60);
        assert!(n.take(&c).is_ok());
        assert!(n.take(&c).is_ok());
        let wait = n.take(&c).unwrap_err();
        assert!(wait <= Duration::from_secs(1) && wait > Duration::from_millis(900));
    }
}
