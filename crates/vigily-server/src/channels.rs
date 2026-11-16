//! Notification channels: named destinations for alerts, kept as records
//! `notify/channel/<id>`. Their secrets (webhook URLs, the signing key, the SMTP password) live in
//! the secret store as `channel.<id>.<field>`, or are read from an environment variable of the
//! engine (`{"env": "NAME"}`) when they are used. Answers never hold a secret value.
//!
//! | Method and path                                  | Scope | What it does |
//! |--------------------------------------------------|-------|--------------|
//! | `GET /api/v1/notifications/channels`             | read  | Every channel |
//! | `POST /api/v1/notifications/channels`            | admin | Adds one (201) |
//! | `GET /api/v1/notifications/channels/{id}`        | read  | One channel |
//! | `PATCH /api/v1/notifications/channels/{id}`      | admin | Changes one |
//! | `DELETE /api/v1/notifications/channels/{id}`     | admin | Removes one and its secrets |
//! | `POST /api/v1/notifications/channels/{id}/test`  | admin | Sends a test alert once |
//! | `GET /api/v1/alerts/{id}/deliveries`             | read  | An alert's deliveries |
//!
//! `[alerts] webhook` in vigily.toml (deprecated) is the read-only channel `config`: a webhook
//! channel marked default, listed with `source: config`, that the API cannot change or remove.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use vigily_api::notifications::{
    Channel, ChannelDeleted, ChannelKind, ChannelPatch, ChannelSettings, Channels, Deliveries,
    NewChannel, SecretInput, TestResult,
};
use vigily_api::secrets::SecretState;
use vigily_core::{now_micros, RecordStore};

use crate::auth::{AuthContext, Scoped};
use crate::secrets::secret_body;
use crate::{ApiError, AppState};

pub const CHANNEL_PREFIX: &str = "notify/channel/";

/// A stored channel.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelRecord {
    pub id: String,
    pub name: String,
    pub kind: ChannelKind,
    pub enabled: bool,
    pub default: bool,
    #[serde(default)]
    pub settings: ChannelSettings,
    /// Secret fields read from an environment variable: field → variable name.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub changed_by: Option<String>,
    /// The URL of the read-only channel from `[alerts] webhook`; never stored.
    #[serde(skip)]
    pub config_url: Option<String>,
}

/// The id of the channel from `[alerts] webhook`.
pub const CONFIG_ID: &str = "config";

/// The read-only channel of `[alerts] webhook`, when it is configured.
pub fn config_channel(state: &AppState) -> Option<ChannelRecord> {
    let url = state.config.alerts.webhook.clone()?;
    Some(ChannelRecord {
        id: CONFIG_ID.into(),
        name: "[alerts] webhook (vigily.toml)".into(),
        kind: ChannelKind::Webhook,
        enabled: true,
        default: true,
        settings: ChannelSettings::default(),
        env: BTreeMap::new(),
        created_at: 0,
        updated_at: 0,
        changed_by: None,
        config_url: Some(url),
    })
}

/// Every channel: the configured one first, then the stored ones in id order.
pub async fn all(state: &AppState) -> vigily_core::Result<Vec<ChannelRecord>> {
    let mut out: Vec<ChannelRecord> = config_channel(state).into_iter().collect();
    out.extend(list(&*state.records).await?);
    Ok(out)
}

/// A channel by id, the configured one included.
pub async fn find(state: &AppState, id: &str) -> vigily_core::Result<Option<ChannelRecord>> {
    if id == CONFIG_ID {
        return Ok(config_channel(state));
    }
    get_record(&*state.records, id).await
}

/// The secret store's name of a channel's secret field.
pub fn secret_name(id: &str, field: &str) -> String {
    format!("channel.{id}.{field}")
}

fn check_id(id: &str) -> Result<(), ApiError> {
    if !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("no channel with id {id}"),
        ))
    }
}

/// Refuses changes to the channel from vigily.toml.
fn read_only(c: &ChannelRecord) -> Result<(), ApiError> {
    if c.config_url.is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "this channel comes from [alerts] webhook in vigily.toml and is read-only; edit or \
             remove it there (it is deprecated: add a webhook channel instead)",
        ));
    }
    Ok(())
}

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, m)
}

/// Every stored channel, in id order.
pub async fn list(rs: &dyn RecordStore) -> vigily_core::Result<Vec<ChannelRecord>> {
    Ok(rs
        .scan(CHANNEL_PREFIX)
        .await?
        .into_iter()
        .filter_map(|(k, v)| match serde_json::from_value(v) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(key = %k, error = %e, "a stored channel does not read");
                None
            }
        })
        .collect())
}

pub async fn get_record(
    rs: &dyn RecordStore,
    id: &str,
) -> vigily_core::Result<Option<ChannelRecord>> {
    Ok(match rs.get(&format!("{CHANNEL_PREFIX}{id}")).await? {
        Some(v) => serde_json::from_value(v).ok(),
        None => None,
    })
}

async fn put_record(rs: &dyn RecordStore, c: &ChannelRecord) -> vigily_core::Result<()> {
    let v = serde_json::to_value(c).map_err(|e| vigily_core::Error::Storage(e.to_string()))?;
    rs.put(vec![(format!("{CHANNEL_PREFIX}{}", c.id), v)])
        .await?;
    rs.flush().await
}

/// A secret field's state: from the environment, stored, or none.
pub async fn secret_state(
    state: &AppState,
    c: &ChannelRecord,
    field: &str,
) -> Result<SecretState, ApiError> {
    if c.config_url.is_some() {
        return Ok(if field == "url" {
            SecretState {
                source: "config".into(),
                ..SecretState::default()
            }
        } else {
            SecretState::none()
        });
    }
    if let Some(var) = c.env.get(field) {
        return Ok(SecretState::env(var));
    }
    Ok(state
        .secrets
        .state(&*state.records, &secret_name(&c.id, field))
        .await?)
}

/// A secret field's value: from the environment or the secret store. `Ok(None)` when unset.
pub async fn secret_value(
    state: &AppState,
    c: &ChannelRecord,
    field: &str,
) -> Result<Option<String>, String> {
    if let Some(url) = &c.config_url {
        return Ok((field == "url").then(|| url.clone()));
    }
    if let Some(var) = c.env.get(field) {
        return match std::env::var(var).ok().filter(|v| !v.trim().is_empty()) {
            Some(v) => Ok(Some(v.trim().to_string())),
            None => Err(format!(
                "the environment variable {var} that holds {field} is not set on the engine"
            )),
        };
    }
    state
        .secrets
        .reveal(&*state.records, &secret_name(&c.id, field))
        .await
        .map_err(|e| e.to_string())
}

/// The channel as the API answers it.
pub async fn view(state: &AppState, c: &ChannelRecord) -> Result<Channel, ApiError> {
    let mut secrets = BTreeMap::new();
    for (field, _) in c.kind.secret_fields() {
        secrets.insert(field.to_string(), secret_state(state, c, field).await?);
    }
    Ok(Channel {
        id: c.id.clone(),
        name: c.name.clone(),
        kind: c.kind,
        enabled: c.enabled,
        default: c.default,
        settings: c.settings.clone(),
        secrets,
        source: if c.config_url.is_some() {
            "config".into()
        } else {
            "api".into()
        },
        created_at: c.created_at,
        updated_at: c.updated_at,
        changed_by: c.changed_by.clone(),
        last_delivery: crate::delivery::last(&*state.records, &c.id).await?,
    })
}

fn check_env_name(v: &str) -> Result<(), ApiError> {
    let ok = !v.is_empty()
        && v.len() <= 128
        && !v.as_bytes()[0].is_ascii_digit()
        && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "{v:?} is not an environment variable name: use letters, digits and underscores"
        )))
    }
}

/// Checks a secret field's new value without echoing it.
fn check_secret_value(kind: ChannelKind, field: &str, value: &str) -> Result<(), ApiError> {
    if matches!(field, "webhook_url" | "url") && !value.trim().is_empty() {
        let ok = reqwest::Url::parse(value.trim())
            .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some());
        if !ok {
            return Err(bad(format!(
                "{field} of a {} channel must be an http(s) URL",
                kind.as_str()
            )));
        }
    }
    Ok(())
}

fn check_settings(kind: ChannelKind, s: &ChannelSettings) -> Result<(), ApiError> {
    if s.burst == Some(0) || s.per_minute == Some(0) {
        return Err(bad("burst and per_minute must be at least 1"));
    }
    if kind != ChannelKind::Email {
        if s.host.is_some()
            || s.port.is_some()
            || s.security.is_some()
            || s.username.is_some()
            || s.from.is_some()
            || !s.to.is_empty()
        {
            return Err(bad(format!(
                "a {} channel takes no SMTP settings (host, port, security, username, from, to)",
                kind.as_str()
            )));
        }
        return Ok(());
    }
    let host = s.host.as_deref().map(str::trim).unwrap_or_default();
    if host.is_empty() {
        return Err(bad("an email channel needs the SMTP server's host"));
    }
    let from = s.from.as_deref().unwrap_or_default();
    if from.parse::<lettre::message::Mailbox>().is_err() {
        return Err(bad(
            "an email channel needs a sender address in from, such as vigily <vigily@example.com>",
        ));
    }
    if s.to.is_empty() {
        return Err(bad("an email channel needs at least one recipient in to"));
    }
    for to in &s.to {
        if to.parse::<lettre::message::Mailbox>().is_err() {
            return Err(bad(format!("{to:?} is not an e-mail address")));
        }
    }
    Ok(())
}

fn check_name(name: &str) -> Result<String, ApiError> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 64 {
        return Err(bad(
            "a channel needs a name of 1 to 64 characters, such as ml-alerts",
        ));
    }
    Ok(n.to_string())
}

async fn name_taken(
    rs: &dyn RecordStore,
    name: &str,
    except: Option<&str>,
) -> Result<(), ApiError> {
    let taken = list(rs)
        .await?
        .into_iter()
        .any(|c| c.name.eq_ignore_ascii_case(name) && Some(c.id.as_str()) != except);
    if taken {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("a channel named {name} exists already"),
        ));
    }
    Ok(())
}

fn check_secret_fields(
    kind: ChannelKind,
    secrets: &BTreeMap<String, SecretInput>,
) -> Result<(), ApiError> {
    for (field, input) in secrets {
        if !kind.secret_fields().iter().any(|(f, _)| f == field) {
            let known: Vec<_> = kind.secret_fields().iter().map(|(f, _)| *f).collect();
            return Err(bad(format!(
                "a {} channel has no secret {field}; its secrets are {}",
                kind.as_str(),
                known.join(", ")
            )));
        }
        match input {
            SecretInput::Env { env } => check_env_name(env)?,
            SecretInput::Value { value } => check_secret_value(kind, field, value)?,
        }
    }
    Ok(())
}

/// Applies secret inputs to channel `c` (its `env` map and the secret store), recording each
/// change in the audit trail.
async fn apply_secrets(
    state: &AppState,
    c: &mut ChannelRecord,
    secrets: &BTreeMap<String, SecretInput>,
    by: &str,
) -> Result<(), ApiError> {
    for (field, input) in secrets {
        let name = secret_name(&c.id, field);
        match input {
            SecretInput::Env { env } => {
                // A stored value would be shadowed: remove it.
                state.secrets.set(&*state.records, &name, "", by).await?;
                c.env.insert(field.clone(), env.clone());
                crate::audit::record(&*state.records, by, "secret.env", &name).await;
            }
            SecretInput::Value { value } => {
                state.secrets.set(&*state.records, &name, value, by).await?;
                c.env.remove(field);
                let action = if value.trim().is_empty() {
                    "secret.removed"
                } else {
                    "secret.set"
                };
                crate::audit::record(&*state.records, by, action, &name).await;
            }
        }
    }
    Ok(())
}

/// Whether every required secret of `c` has a source.
async fn check_required(state: &AppState, c: &ChannelRecord) -> Result<(), ApiError> {
    for (field, required) in c.kind.secret_fields() {
        if *required && !secret_state(state, c, field).await?.is_set() {
            return Err(bad(format!(
                "a {} channel needs {field}: send {{\"value\": \"...\"}} or {{\"env\": \"NAME\"}}",
                c.kind.as_str()
            )));
        }
    }
    Ok(())
}

/// The channel routes, relative to the API root.
pub fn routes() -> Scoped {
    Scoped::default()
        .read(
            Router::new()
                .route("/notifications/channels", get(list_channels))
                .route("/notifications/channels/{id}", get(get_channel))
                .route("/alerts/{id}/deliveries", get(deliveries)),
        )
        .admin(
            Router::new()
                .route("/notifications/channels", post(create))
                .route(
                    "/notifications/channels/{id}",
                    axum::routing::patch(update).delete(remove),
                )
                .route("/notifications/channels/{id}/test", post(test)),
        )
}

async fn list_channels(State(s): State<Arc<AppState>>) -> Result<Json<Channels>, ApiError> {
    let mut channels = Vec::new();
    for c in all(&s).await? {
        channels.push(view(&s, &c).await?);
    }
    channels.sort_by_key(|c| c.name.to_lowercase());
    Ok(Json(Channels { channels }))
}

async fn found(s: &AppState, id: &str) -> Result<ChannelRecord, ApiError> {
    check_id(id)?;
    find(s, id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no channel with id {id}")))
}

async fn get_channel(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Channel>, ApiError> {
    let c = found(&s, &id).await?;
    Ok(Json(view(&s, &c).await?))
}

const NEW_SHAPE: &str =
    "{\"name\", \"kind\": \"slack\"|\"teams\"|\"discord\"|\"webhook\"|\"email\", \
\"enabled\"?, \"default\"?, \"settings\"?, \"secrets\"?: {\"<field>\": {\"value\"} | {\"env\"}}}";

async fn create(
    State(s): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    bytes: Bytes,
) -> Result<(StatusCode, Json<Channel>), ApiError> {
    let req: NewChannel = secret_body(&bytes, NEW_SHAPE)?;
    let name = check_name(&req.name)?;
    check_settings(req.kind, &req.settings)?;
    check_secret_fields(req.kind, &req.secrets)?;
    for (field, required) in req.kind.secret_fields() {
        let given = match req.secrets.get(*field) {
            Some(SecretInput::Env { .. }) => true,
            Some(SecretInput::Value { value }) => !value.trim().is_empty(),
            None => false,
        };
        if *required && !given {
            return Err(bad(format!(
                "a {} channel needs {field}: send {{\"value\": \"...\"}} or {{\"env\": \"NAME\"}}",
                req.kind.as_str()
            )));
        }
    }
    name_taken(&*s.records, &name, None).await?;
    let now = now_micros();
    let mut c = ChannelRecord {
        id: crate::auth::new_secret()[..16].to_string(),
        name,
        kind: req.kind,
        enabled: req.enabled,
        default: req.default,
        settings: req.settings,
        env: BTreeMap::new(),
        created_at: now,
        updated_at: now,
        changed_by: Some(ctx.principal.clone()),
        config_url: None,
    };
    if let Err(e) = apply_secrets(&s, &mut c, &req.secrets, &ctx.principal).await {
        // Leave nothing behind of a channel that was not made.
        for (field, _) in c.kind.secret_fields() {
            let _ = s
                .secrets
                .set(&*s.records, &secret_name(&c.id, field), "", &ctx.principal)
                .await;
        }
        return Err(e);
    }
    put_record(&*s.records, &c).await?;
    crate::audit::record(&*s.records, &ctx.principal, "channel.created", &c.id).await;
    Ok((StatusCode::CREATED, Json(view(&s, &c).await?)))
}

async fn update(
    State(s): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Result<Json<Channel>, ApiError> {
    let req: ChannelPatch = secret_body(
        &bytes,
        "{\"name\"?, \"enabled\"?, \"default\"?, \"settings\"?, \"secrets\"?}",
    )?;
    let mut c = found(&s, &id).await?;
    read_only(&c)?;
    if let Some(n) = &req.name {
        let n = check_name(n)?;
        name_taken(&*s.records, &n, Some(&c.id)).await?;
        c.name = n;
    }
    if let Some(settings) = &req.settings {
        check_settings(c.kind, settings)?;
        c.settings = settings.clone();
    }
    check_secret_fields(c.kind, &req.secrets)?;
    for (field, required) in c.kind.secret_fields() {
        let clears = matches!(req.secrets.get(*field), Some(SecretInput::Value { value }) if value.trim().is_empty());
        if *required && clears {
            return Err(bad(format!(
                "{field} is required for a {} channel; send a new value instead",
                c.kind.as_str()
            )));
        }
    }
    if let Some(e) = req.enabled {
        c.enabled = e;
    }
    if let Some(d) = req.default {
        c.default = d;
    }
    apply_secrets(&s, &mut c, &req.secrets, &ctx.principal).await?;
    check_required(&s, &c).await?;
    c.updated_at = now_micros();
    c.changed_by = Some(ctx.principal.clone());
    put_record(&*s.records, &c).await?;
    crate::audit::record(&*s.records, &ctx.principal, "channel.updated", &c.id).await;
    Ok(Json(view(&s, &c).await?))
}

async fn remove(
    State(s): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<ChannelDeleted>, ApiError> {
    let c = found(&s, &id).await?;
    read_only(&c)?;
    for (field, _) in c.kind.secret_fields() {
        let name = secret_name(&c.id, field);
        if s.secrets.state(&*s.records, &name).await?.is_set() {
            s.secrets
                .set(&*s.records, &name, "", &ctx.principal)
                .await?;
            crate::audit::record(&*s.records, &ctx.principal, "secret.removed", &name).await;
        }
    }
    s.records
        .delete(vec![
            format!("{CHANNEL_PREFIX}{}", c.id),
            crate::delivery::last_key(&c.id),
        ])
        .await?;
    s.records.flush().await?;
    crate::audit::record(&*s.records, &ctx.principal, "channel.deleted", &c.id).await;
    Ok(Json(ChannelDeleted { deleted: true }))
}

async fn test(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<TestResult>, ApiError> {
    let c = found(&s, &id).await?;
    Ok(Json(crate::delivery::test(&s, &c).await?))
}

async fn deliveries(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Deliveries>, ApiError> {
    if s.records.get(&format!("alert/{id}")).await?.is_none() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("no alert with id {id}"),
        ));
    }
    Ok(Json(Deliveries {
        deliveries: crate::delivery::of_alert(&*s.records, &id).await?,
    }))
}
