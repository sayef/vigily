//! Notification channels, routing rules and the delivery log.
//!
//! A channel is a named destination of one kind: Slack, Microsoft Teams or Discord (incoming
//! webhooks), a generic webhook (a JSON `POST`, optionally signed), or e-mail over SMTP. Its
//! secrets (webhook URLs, the signing key, the SMTP password) are write-only: requests send them
//! as [`SecretInput`], answers describe them as [`SecretState`]. Each alert is delivered once to
//! each channel it is routed to, with retries; [`Delivery`] records the outcome.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::secrets::SecretState;

/// What a channel delivers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    /// A Slack incoming webhook (Block Kit message).
    Slack,
    /// A Microsoft Teams incoming webhook or Workflows webhook (Adaptive Card).
    Teams,
    /// A Discord webhook (embed).
    Discord,
    /// Any HTTPS endpoint: vigily's JSON alert document, optionally signed with HMAC-SHA256.
    Webhook,
    /// E-mail over SMTP (plain text and HTML).
    Email,
}

impl ChannelKind {
    pub const ALL: [ChannelKind; 5] = [
        ChannelKind::Slack,
        ChannelKind::Teams,
        ChannelKind::Discord,
        ChannelKind::Webhook,
        ChannelKind::Email,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelKind::Slack => "slack",
            ChannelKind::Teams => "teams",
            ChannelKind::Discord => "discord",
            ChannelKind::Webhook => "webhook",
            ChannelKind::Email => "email",
        }
    }

    /// The secret fields of this kind and whether each is required.
    pub fn secret_fields(&self) -> &'static [(&'static str, bool)] {
        match self {
            ChannelKind::Slack | ChannelKind::Teams | ChannelKind::Discord => {
                &[("webhook_url", true)]
            }
            ChannelKind::Webhook => &[("url", true), ("signing_key", false)],
            ChannelKind::Email => &[("password", false)],
        }
    }
}

/// How an SMTP connection is secured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmtpSecurity {
    /// Plain connection upgraded with STARTTLS (port 587 by default).
    #[default]
    Starttls,
    /// TLS from the first byte (port 465 by default).
    Tls,
    /// No encryption (port 25 by default): only for a relay on a trusted network.
    None,
}

/// A channel's settings that are not secret. Fields that do not apply to its kind are left out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelSettings {
    /// E-mail: the SMTP server's host name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// E-mail: the SMTP port; by default 587 (STARTTLS), 465 (TLS) or 25 (none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// E-mail: `starttls` (default), `tls` or `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<SmtpSecurity>,
    /// E-mail: the SMTP user name; with a `password` secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// E-mail: the sender, such as `vigily <vigily@example.com>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// E-mail: the recipients.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,
    /// Messages sent at once before the per-minute rate applies (5 when left out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst: Option<u32>,
    /// Messages per minute after the burst (20 when left out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_minute: Option<u32>,
}

/// A notification channel as the API answers it: its secrets by state only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub kind: ChannelKind,
    pub enabled: bool,
    /// Whether the default routing rule sends alerts here.
    pub default: bool,
    pub settings: ChannelSettings,
    /// Each secret field of the kind with its state.
    pub secrets: BTreeMap<String, SecretState>,
    /// `api` for a channel made with the API, the web app or the command line; `config` for the
    /// read-only channel of `[alerts] webhook` in vigily.toml.
    pub source: String,
    /// UTC microseconds.
    pub created_at: i64,
    /// UTC microseconds.
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<String>,
    /// The newest delivery to this channel (a test send included).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery: Option<Delivery>,
}

/// `GET notifications/channels`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Channels {
    pub channels: Vec<Channel>,
}

/// A secret field in a request: a value to store (encrypted), the name of an environment variable
/// of the engine to read it from, or an empty value to remove it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SecretInput {
    Env { env: String },
    Value { value: String },
}

impl std::fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretInput::Env { env } => f.debug_struct("Env").field("env", env).finish(),
            SecretInput::Value { .. } => f
                .debug_struct("Value")
                .field("value", &"<redacted>")
                .finish(),
        }
    }
}

/// `POST notifications/channels`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewChannel {
    pub name: String,
    pub kind: ChannelKind,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub settings: ChannelSettings,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, SecretInput>,
}

fn yes() -> bool {
    true
}

/// `PATCH notifications/channels/{id}`: the fields to change. `settings` replaces the settings;
/// each field in `secrets` replaces that secret and leaves the others as they are.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<ChannelSettings>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, SecretInput>,
}

/// `DELETE notifications/channels/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelDeleted {
    pub deleted: bool,
}

/// Where a delivery stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryStatus {
    /// Waiting for its first or next attempt.
    Pending,
    /// The channel accepted it.
    Sent,
    /// Every attempt failed, or the channel refused it for good.
    Failed,
}

/// One alert's delivery to one channel (or a test send, with `alert_id` `test`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    pub alert_id: String,
    pub channel_id: String,
    pub channel_name: String,
    pub kind: ChannelKind,
    pub status: DeliveryStatus,
    pub attempts: u32,
    /// The HTTP status of the last answer (SMTP reply code for e-mail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    /// Why the last attempt failed, without secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// UTC microseconds.
    pub created_at: i64,
    /// UTC microseconds.
    pub updated_at: i64,
    /// When the next attempt is due, UTC microseconds (pending only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<i64>,
}

/// `GET alerts/{id}/deliveries`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Deliveries {
    pub deliveries: Vec<Delivery>,
}

/// `POST notifications/channels/{id}/test`: the outcome of one attempt to send a test alert.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TestResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How long the attempt took.
    pub duration_ms: u64,
}

/// The document a `webhook` channel posts (`schema` `vigily.alert/1`). Fields are only ever
/// added.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WebhookPayload {
    /// `vigily.alert`.
    pub event: String,
    /// `vigily.alert/1`.
    pub schema: String,
    /// Whether this is a test send from `POST notifications/channels/{id}/test`.
    pub test: bool,
    pub alert: WebhookAlert,
    /// The alert's page in the web app; `null` when the engine does not know its address.
    pub url: Option<String>,
}

/// The alert in a [`WebhookPayload`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WebhookAlert {
    pub id: String,
    /// `behaviour_drift`, `data_drift` or `schedule_failed`.
    pub kind: String,
    /// `warning` or `critical`.
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub suite: Option<String>,
    pub run_id: Option<String>,
    pub schedule: Option<String>,
    /// UTC microseconds.
    pub created_at: i64,
    /// The same instant as RFC 3339 in UTC (`2026-11-16T09:30:00Z`).
    pub time: String,
    /// Alerts with the same key are raised once.
    pub dedupe: String,
}

/// Deliveries of one alert by status (`GET alerts`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeliverySummary {
    pub sent: u32,
    pub failed: u32,
    pub pending: u32,
}
