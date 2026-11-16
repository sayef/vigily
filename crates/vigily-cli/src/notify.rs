//! `vigily channels`, `vigily rules` and `vigily secrets`: notification channels, routing rules
//! and the state of secrets, through the engine's API (the same routes the web app uses).
//!
//! Secret values never travel as arguments, where shell history and process listings would keep
//! them: they are asked for without echo, or read from standard input with `--stdin`.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use vigily_api::client::{Client, Error};
use vigily_api::evals::{AlertKind, Severity};
use vigily_api::notifications::{
    Channel, ChannelKind, ChannelPatch, ChannelSettings, Delivery, DeliveryStatus, NewChannel,
    Rule, RuleMatch, RulesUpdate, SecretInput, SmtpSecurity,
};
use vigily_api::secrets::SecretState;

use crate::explain;
use crate::render::{count, table, utc};

#[derive(Subcommand)]
pub enum ChannelsAction {
    /// List the channels: kind, routing, secrets (their state, never a value), last delivery.
    List,
    /// Add a channel. The required secret (webhook_url, or url for a webhook) is asked for
    /// without echo unless --env names a variable for it.
    Add {
        /// A name such as ml-alerts or on-call mail.
        name: String,
        /// slack, teams, discord, webhook or email.
        #[arg(long)]
        kind: String,
        /// The default routing rule sends every alert here (unless a rule stops it).
        #[arg(long)]
        default: bool,
        /// Add it switched off.
        #[arg(long)]
        disabled: bool,
        #[command(flatten)]
        settings: SettingsArgs,
        #[command(flatten)]
        secrets: SecretArgs,
    },
    /// Change a channel (by id or name). Settings not given stay as they are.
    Edit {
        /// The channel's id or name.
        channel: String,
        /// A new name.
        #[arg(long)]
        name: Option<String>,
        /// Switch it on.
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        /// Switch it off: nothing is sent to it until it is switched on again.
        #[arg(long)]
        disable: bool,
        /// Whether the default routing rule sends here: true or false.
        #[arg(long, value_name = "true|false")]
        default: Option<bool>,
        #[command(flatten)]
        settings: SettingsArgs,
        #[command(flatten)]
        secrets: SecretArgs,
        /// Remove an optional secret (signing_key, password); repeat for several.
        #[arg(long = "remove-secret", value_name = "FIELD")]
        remove: Vec<String>,
    },
    /// Delete a channel (by id or name) and its stored secrets. Rules stop sending to it.
    Remove {
        /// The channel's id or name.
        channel: String,
    },
    /// Send a test alert to a channel (by id or name) once, without retries. Exits 1 when it
    /// fails.
    Test {
        /// The channel's id or name.
        channel: String,
    },
}

/// Settings of a channel that are not secret.
#[derive(Args, Default, Clone)]
pub struct SettingsArgs {
    /// E-mail: the SMTP server's host name.
    #[arg(long)]
    pub host: Option<String>,
    /// E-mail: the SMTP port (587 for starttls, 465 for tls, 25 for none by default).
    #[arg(long)]
    pub port: Option<u16>,
    /// E-mail: starttls (default), tls, or none (only for a relay on a trusted network).
    #[arg(long)]
    pub security: Option<String>,
    /// E-mail: the SMTP user name; its password is the `password` secret.
    #[arg(long)]
    pub username: Option<String>,
    /// E-mail: the sender, such as "vigily <vigily@example.com>".
    #[arg(long)]
    pub from: Option<String>,
    /// E-mail: a recipient; repeat for several (replaces the list when editing).
    #[arg(long = "to", value_name = "ADDRESS")]
    pub to: Vec<String>,
    /// Messages sent at once before the per-minute rate applies (5 by default).
    #[arg(long)]
    pub burst: Option<u32>,
    /// Messages per minute after the burst (20 by default).
    #[arg(long)]
    pub per_minute: Option<u32>,
}

/// Where a channel's secret values come from.
#[derive(Args, Default, Clone)]
pub struct SecretArgs {
    /// Store a value for this secret field (webhook_url, url, signing_key, password), asked for
    /// without echo; repeat for several.
    #[arg(long = "secret", value_name = "FIELD")]
    pub secret: Vec<String>,
    /// Read the secret values from standard input instead of asking, one line per value, in the
    /// order webhook_url or url, signing_key, password.
    #[arg(long)]
    pub stdin: bool,
    /// Let the engine read FIELD from its own environment variable NAME when it sends
    /// (FIELD=NAME); no value is stored.
    #[arg(long = "env", value_name = "FIELD=NAME")]
    pub env: Vec<String>,
}

#[derive(Subcommand)]
pub enum RulesAction {
    /// The routing rules in order, then the default rule.
    List,
    /// The rules as a file to edit and apply with `vigily rules set` (TOML, or JSON with --json).
    Show {
        /// JSON instead of TOML.
        #[arg(long)]
        json: bool,
    },
    /// Replace the rules with those in FILE (TOML, or JSON for a .json file; `-` reads standard
    /// input). When the file has a version and the rules changed since, nothing is saved.
    Set {
        /// The rules file.
        file: String,
    },
}

#[derive(Subcommand)]
pub enum SecretsAction {
    /// Whether an encryption key is set, and the state of every secret (never a value).
    Status,
}

/// Reads one secret value; the argument names it, for a prompt.
pub type ReadSecret<'a> = &'a mut dyn FnMut(&str) -> Result<String>;

/// The reader the command line uses: standard input, one line per call, or a prompt on the
/// terminal without echo.
pub fn secret_reader(stdin: bool) -> impl FnMut(&str) -> Result<String> {
    move |what: &str| {
        let value = if stdin {
            let mut line = String::new();
            let n = std::io::stdin()
                .lock()
                .read_line(&mut line)
                .context("could not read standard input")?;
            if n == 0 {
                bail!("standard input ended before the value of {what}");
            }
            line.trim_end_matches(['\r', '\n']).to_string()
        } else {
            rpassword::prompt_password(format!("{what} (not shown): ")).with_context(|| {
                format!(
                    "could not ask for {what} on the terminal; pass --stdin to read it from \
                     standard input"
                )
            })?
        };
        if value.is_empty() {
            bail!("the value of {what} is empty; nothing was changed");
        }
        Ok(value)
    }
}

fn parse_kind(s: &str) -> Result<ChannelKind> {
    serde_json::from_value(serde_json::Value::String(s.trim().to_lowercase())).map_err(|_| {
        anyhow::anyhow!("unknown kind {s:?}: use slack, teams, discord, webhook or email")
    })
}

fn parse_security(s: &str) -> Result<SmtpSecurity> {
    serde_json::from_value(serde_json::Value::String(s.trim().to_lowercase()))
        .map_err(|_| anyhow::anyhow!("unknown security {s:?}: use starttls, tls or none"))
}

/// `settings` with the fields given on the command line replaced.
fn apply_settings(mut s: ChannelSettings, a: &SettingsArgs) -> Result<ChannelSettings> {
    if let Some(v) = &a.host {
        s.host = Some(v.clone()).filter(|v| !v.is_empty());
    }
    if let Some(v) = a.port {
        s.port = Some(v);
    }
    if let Some(v) = &a.security {
        s.security = Some(parse_security(v)?);
    }
    if let Some(v) = &a.username {
        s.username = Some(v.clone()).filter(|v| !v.is_empty());
    }
    if let Some(v) = &a.from {
        s.from = Some(v.clone()).filter(|v| !v.is_empty());
    }
    if !a.to.is_empty() {
        s.to =
            a.to.iter()
                .flat_map(|t| t.split(','))
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
    }
    if let Some(v) = a.burst {
        s.burst = Some(v);
    }
    if let Some(v) = a.per_minute {
        s.per_minute = Some(v);
    }
    Ok(s)
}

/// The secret fields to send: environment references, values read (in the kind's field order)
/// and removals. `required` also reads the kind's required fields that nothing else sets.
fn secret_inputs(
    kind: ChannelKind,
    a: &SecretArgs,
    remove: &[String],
    required: bool,
    read: ReadSecret,
) -> Result<BTreeMap<String, SecretInput>> {
    let fields = kind.secret_fields();
    let known = |f: &str| -> Result<()> {
        if fields.iter().any(|(n, _)| *n == f) {
            Ok(())
        } else {
            let names: Vec<&str> = fields.iter().map(|(n, _)| *n).collect();
            bail!(
                "a {} channel has no secret {f:?}; its secrets are {}",
                kind.as_str(),
                names.join(", ")
            )
        }
    };
    let mut out = BTreeMap::new();
    let set = |out: &mut BTreeMap<String, SecretInput>, f: &str, v: SecretInput| {
        if out.insert(f.to_string(), v).is_some() {
            bail!("{f} is given more than once");
        }
        Ok(())
    };
    for e in &a.env {
        let Some((f, name)) = e.split_once('=') else {
            bail!("--env takes FIELD=NAME, such as webhook_url=SLACK_WEBHOOK_URL");
        };
        let (f, name) = (f.trim(), name.trim());
        known(f)?;
        set(
            &mut out,
            f,
            SecretInput::Env {
                env: name.to_string(),
            },
        )?;
    }
    for f in remove {
        known(f)?;
        if fields.iter().any(|(n, req)| n == f && *req) {
            bail!("{f} is required and cannot be removed; replace it with --secret or --env");
        }
        set(
            &mut out,
            f,
            SecretInput::Value {
                value: String::new(),
            },
        )?;
    }
    for f in &a.secret {
        known(f)?;
    }
    for (f, req) in fields {
        let asked = a.secret.iter().any(|s| s == f);
        if asked || (required && *req && !out.contains_key(*f)) {
            if out.contains_key(*f) {
                bail!("{f} is given more than once");
            }
            let value = read(f)?;
            set(&mut out, f, SecretInput::Value { value })?;
        }
    }
    Ok(out)
}

/// The channel with this id, or else this name (any case).
async fn find(c: &Client, key: &str) -> Result<Channel> {
    let list = c.channels().await.map_err(|e| explain(e, c))?.channels;
    if let Some(ch) = list.iter().find(|ch| ch.id == key) {
        return Ok(ch.clone());
    }
    if let Some(ch) = list.iter().find(|ch| ch.name.eq_ignore_ascii_case(key)) {
        return Ok(ch.clone());
    }
    bail!("no channel with the id or name {key:?}; `vigily channels list` shows them")
}

/// A secret's state in words: `stored …abcd`, `env SLACK_WEBHOOK_URL`, `vigily.toml`, `not set`.
pub fn secret_text(s: &SecretState) -> String {
    if s.undecryptable {
        return "UNDECRYPTABLE (store it again, or set VIGILY_SECRET_KEY_PREVIOUS)".into();
    }
    match s.source.as_str() {
        "stored" => match &s.hint {
            Some(h) => format!("stored {h}"),
            None => "stored".into(),
        },
        "config" => "vigily.toml".into(),
        "none" => "not set".into(),
        other => match other.strip_prefix("env:") {
            Some(name) => format!("env {name}"),
            None => other.to_string(),
        },
    }
}

fn delivery_text(d: &Delivery) -> String {
    let status = match d.status {
        DeliveryStatus::Sent => "sent",
        DeliveryStatus::Failed => "failed",
        DeliveryStatus::Pending => "retrying",
    };
    let code = d.status_code.map(|c| format!(" {c}")).unwrap_or_default();
    let test = if d.alert_id == "test" { " (test)" } else { "" };
    format!("{status}{code}{test} {}", utc(d.updated_at))
}

/// `vigily channels ...`.
pub async fn channels(
    c: &Client,
    out: &mut dyn Write,
    action: &ChannelsAction,
    read: ReadSecret<'_>,
) -> Result<ExitCode> {
    match action {
        ChannelsAction::List => list_channels(c, out).await,
        ChannelsAction::Add {
            name,
            kind,
            default,
            disabled,
            settings,
            secrets,
        } => {
            let kind = parse_kind(kind)?;
            let settings = apply_settings(ChannelSettings::default(), settings)?;
            let secrets = secret_inputs(kind, secrets, &[], true, read)?;
            let ch = c
                .create_channel(&NewChannel {
                    name: name.clone(),
                    kind,
                    enabled: !disabled,
                    default: *default,
                    settings,
                    secrets,
                })
                .await
                .map_err(|e| explain(e, c))?;
            writeln!(
                out,
                "Added channel {} ({}, id {}).",
                ch.name,
                ch.kind.as_str(),
                ch.id
            )?;
            write_secrets(out, &ch)?;
            writeln!(out, "Send it a test alert: vigily channels test {}", ch.id)?;
            Ok(ExitCode::SUCCESS)
        }
        ChannelsAction::Edit {
            channel,
            name,
            enable,
            disable,
            default,
            settings,
            secrets,
            remove,
        } => {
            let ch = find(c, channel).await?;
            let new_settings = apply_settings(ch.settings.clone(), settings)?;
            let patch = ChannelPatch {
                name: name.clone(),
                enabled: if *enable {
                    Some(true)
                } else if *disable {
                    Some(false)
                } else {
                    None
                },
                default: *default,
                settings: (new_settings != ch.settings).then_some(new_settings),
                secrets: secret_inputs(ch.kind, secrets, remove, false, read)?,
            };
            if patch == ChannelPatch::default() {
                bail!("nothing to change: give a setting, --secret, --env or --remove-secret");
            }
            let ch = c
                .update_channel(&ch.id, &patch)
                .await
                .map_err(|e| explain(e, c))?;
            writeln!(out, "Saved channel {} (id {}).", ch.name, ch.id)?;
            write_secrets(out, &ch)?;
            Ok(ExitCode::SUCCESS)
        }
        ChannelsAction::Remove { channel } => {
            let ch = find(c, channel).await?;
            c.delete_channel(&ch.id).await.map_err(|e| explain(e, c))?;
            writeln!(out, "Removed channel {} (id {}).", ch.name, ch.id)?;
            Ok(ExitCode::SUCCESS)
        }
        ChannelsAction::Test { channel } => {
            let ch = find(c, channel).await?;
            let r = c.test_channel(&ch.id).await.map_err(|e| explain(e, c))?;
            let code = r.status_code.map(|c| format!(" ({c})")).unwrap_or_default();
            if r.ok {
                writeln!(
                    out,
                    "Sent a test alert to {}{code} in {} ms.",
                    ch.name, r.duration_ms
                )?;
                Ok(ExitCode::SUCCESS)
            } else {
                writeln!(
                    out,
                    "The test alert to {} failed{code} after {} ms: {}",
                    ch.name,
                    r.duration_ms,
                    r.error.as_deref().unwrap_or("no reason given")
                )?;
                Ok(ExitCode::FAILURE)
            }
        }
    }
}

fn write_secrets(out: &mut dyn Write, ch: &Channel) -> Result<()> {
    for (field, state) in &ch.secrets {
        writeln!(out, "  {field}: {}", secret_text(state))?;
    }
    Ok(())
}

async fn list_channels(c: &Client, out: &mut dyn Write) -> Result<ExitCode> {
    let list = c.channels().await.map_err(|e| explain(e, c))?.channels;
    if list.is_empty() {
        writeln!(
            out,
            "No channels. Add one with: vigily channels add NAME --kind slack|teams|discord|webhook|email"
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|ch| {
            vec![
                ch.id.clone(),
                ch.name.clone(),
                ch.kind.as_str().to_string(),
                if ch.enabled { "on" } else { "off" }.to_string(),
                if ch.default { "default" } else { "by rule" }.to_string(),
                ch.secrets
                    .iter()
                    .map(|(f, s)| format!("{f}: {}", secret_text(s)))
                    .collect::<Vec<_>>()
                    .join("; "),
                ch.last_delivery
                    .as_ref()
                    .map(delivery_text)
                    .unwrap_or_else(|| "none yet".into()),
            ]
        })
        .collect();
    write!(
        out,
        "{}",
        table(
            &[
                "ID",
                "NAME",
                "KIND",
                "ON",
                "ROUTING",
                "SECRETS",
                "LAST DELIVERY"
            ],
            &rows
        )
    )?;
    for ch in &list {
        if ch.source == "config" {
            writeln!(
                out,
                "{} is [alerts] webhook in vigily.toml (deprecated, read-only): change it there.",
                ch.name
            )?;
        }
        if let Some(Delivery {
            status: DeliveryStatus::Failed,
            error: Some(e),
            ..
        }) = &ch.last_delivery
        {
            writeln!(out, "{}: {e}", ch.name)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------------------------
// Rules

/// The rules file: `vigily rules show` writes it, `vigily rules set` reads it. Channels are named
/// (or given by id).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesFile {
    /// The version the rules were read at; `set` refuses to save when they changed since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<u64>,
    #[serde(default)]
    rules: Vec<RuleFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    stop: bool,
    #[serde(default, rename = "match", skip_serializing_if = "MatchFile::is_empty")]
    matches: MatchFile,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchFile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    kinds: Vec<AlertKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    severities: Vec<Severity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    experiment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    suite: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schedule: Option<String>,
}

impl MatchFile {
    fn is_empty(&self) -> bool {
        self.kinds.is_empty()
            && self.severities.is_empty()
            && self.experiment.is_none()
            && self.suite.is_none()
            && self.schedule.is_none()
    }
}

fn kind_name(k: &AlertKind) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn severity_name(s: &Severity) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// `kinds behaviour_drift, severities critical, experiment intents*`, or `every alert`.
fn match_text(m: &RuleMatch) -> String {
    let mut parts = Vec::new();
    if !m.kinds.is_empty() {
        parts.push(m.kinds.iter().map(kind_name).collect::<Vec<_>>().join("|"));
    }
    if !m.severities.is_empty() {
        parts.push(
            m.severities
                .iter()
                .map(severity_name)
                .collect::<Vec<_>>()
                .join("|"),
        );
    }
    for (label, v) in [
        ("experiment", &m.experiment),
        ("suite", &m.suite),
        ("schedule", &m.schedule),
    ] {
        if let Some(v) = v {
            parts.push(format!("{label} {v}"));
        }
    }
    if parts.is_empty() {
        "every alert".into()
    } else {
        parts.join(", ")
    }
}

/// `vigily rules ...`.
pub async fn rules(c: &Client, out: &mut dyn Write, action: &RulesAction) -> Result<ExitCode> {
    let channels = c.channels().await.map_err(|e| explain(e, c))?.channels;
    let name_of = |id: &str| {
        channels
            .iter()
            .find(|ch| ch.id == id)
            .map(|ch| ch.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    match action {
        RulesAction::List => {
            let r = c.rules().await.map_err(|e| explain(e, c))?;
            if r.rules.is_empty() {
                writeln!(out, "No routing rules.")?;
            } else {
                let rows: Vec<Vec<String>> = r
                    .rules
                    .iter()
                    .enumerate()
                    .map(|(i, rule)| {
                        vec![
                            (i + 1).to_string(),
                            rule.name.clone().unwrap_or_default(),
                            match_text(&rule.matches),
                            if rule.channels.is_empty() {
                                "(none)".into()
                            } else {
                                rule.channels
                                    .iter()
                                    .map(|id| name_of(id))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            },
                            if rule.stop { "stop" } else { "" }.to_string(),
                        ]
                    })
                    .collect();
                write!(
                    out,
                    "{}",
                    table(&["#", "NAME", "MATCH", "CHANNELS", "STOP"], &rows)
                )?;
            }
            let defaults: Vec<&str> = channels
                .iter()
                .filter(|ch| ch.default && ch.enabled)
                .map(|ch| ch.name.as_str())
                .collect();
            writeln!(
                out,
                "{}every alert no rule stopped goes to the channels marked default: {}.",
                if r.rules.is_empty() { "So " } else { "Then " },
                if defaults.is_empty() {
                    "none".to_string()
                } else {
                    defaults.join(", ")
                }
            )?;
            if r.version > 0 {
                writeln!(
                    out,
                    "Version {}{}{}.",
                    r.version,
                    r.changed_by
                        .as_deref()
                        .map(|b| format!(", saved by {b}"))
                        .unwrap_or_default(),
                    r.updated_at
                        .map(|t| format!(" at {}", utc(t)))
                        .unwrap_or_default()
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
        RulesAction::Show { json } => {
            let r = c.rules().await.map_err(|e| explain(e, c))?;
            let file = RulesFile {
                version: Some(r.version),
                rules: r
                    .rules
                    .into_iter()
                    .map(|rule| RuleFile {
                        name: rule.name,
                        channels: rule.channels.iter().map(|id| name_of(id)).collect(),
                        stop: rule.stop,
                        matches: MatchFile {
                            kinds: rule.matches.kinds,
                            severities: rule.matches.severities,
                            experiment: rule.matches.experiment,
                            suite: rule.matches.suite,
                            schedule: rule.matches.schedule,
                        },
                    })
                    .collect(),
            };
            if *json {
                writeln!(out, "{}", serde_json::to_string_pretty(&file)?)?;
            } else {
                writeln!(
                    out,
                    "# vigily routing rules, in order. Alerts no rule stops also go to the channels\n\
                     # marked default. Change this file and apply it with: vigily rules set FILE\n\
                     # Kinds: behaviour_drift, data_drift, schedule_failed. Severities: warning,\n\
                     # critical. experiment, suite and schedule are globs (* any characters, ? one)."
                )?;
                write!(out, "{}", toml::to_string(&file)?)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        RulesAction::Set { file } => {
            let text = if file == "-" {
                std::io::read_to_string(std::io::stdin())
                    .context("could not read standard input")?
            } else {
                std::fs::read_to_string(file).with_context(|| format!("could not read {file}"))?
            };
            let parsed = parse_rules(file, &text)?;
            let mut rules = Vec::with_capacity(parsed.rules.len());
            for (i, r) in parsed.rules.into_iter().enumerate() {
                let mut ids = Vec::with_capacity(r.channels.len());
                for key in &r.channels {
                    let ch = channels
                        .iter()
                        .find(|ch| &ch.id == key)
                        .or_else(|| channels.iter().find(|ch| ch.name.eq_ignore_ascii_case(key)))
                        .with_context(|| {
                            format!(
                                "rule {}: no channel with the id or name {key:?}; `vigily channels \
                                 list` shows them",
                                i + 1
                            )
                        })?;
                    ids.push(ch.id.clone());
                }
                rules.push(Rule {
                    name: r.name.filter(|n| !n.is_empty()),
                    matches: RuleMatch {
                        kinds: r.matches.kinds,
                        severities: r.matches.severities,
                        experiment: r.matches.experiment,
                        suite: r.matches.suite,
                        schedule: r.matches.schedule,
                    },
                    channels: ids,
                    stop: r.stop,
                });
            }
            let version = match parsed.version {
                Some(v) => v,
                None => c.rules().await.map_err(|e| explain(e, c))?.version,
            };
            let n = rules.len();
            let saved =
                c.set_rules(&RulesUpdate { version, rules })
                    .await
                    .map_err(|e| match e {
                        Error::Api(a) if a.status == 409 => anyhow::anyhow!(
                        "{}. Nothing was saved: run `vigily rules show` for the current rules, \
                         reapply your change, and set them again",
                        a.message
                    ),
                        e => explain(e, c),
                    })?;
            writeln!(
                out,
                "Saved {} (version {}).",
                count(n, "rule", "rules"),
                saved.version
            )?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// A rules file: JSON when it is named `.json` or starts with `{`, TOML otherwise.
fn parse_rules(file: &str, text: &str) -> Result<RulesFile> {
    let json = Path::new(file)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        || (file == "-" && text.trim_start().starts_with('{'));
    if json {
        serde_json::from_str(text).with_context(|| format!("{file} is not a valid rules file"))
    } else {
        toml::from_str(text).with_context(|| format!("{file} is not a valid rules file"))
    }
}

// ---------------------------------------------------------------------------------------------
// Secrets

/// `vigily secrets status`.
pub async fn secrets(c: &Client, out: &mut dyn Write, action: &SecretsAction) -> Result<ExitCode> {
    let SecretsAction::Status = action;
    let s = c.secrets().await.map_err(|e| explain(e, c))?;
    let key = match s.key.as_str() {
        "env" => "VIGILY_SECRET_KEY",
        "file" => "generated by this engine (local mode)",
        _ => "none",
    };
    writeln!(out, "Encryption key: {key}")?;
    writeln!(
        out,
        "Previous key: {}",
        if s.previous_key {
            "set (values sealed with it are re-sealed at start)"
        } else {
            "not set"
        }
    )?;
    if !s.can_store {
        if let Some(m) = &s.message {
            writeln!(out, "{m}")?;
        }
    }
    let channels = c.channels().await.map_err(|e| explain(e, c))?.channels;
    let changed = |st: &SecretState| match (&st.changed_by, st.changed_at) {
        (Some(by), Some(at)) => format!("{by}, {}", utc(at)),
        (None, Some(at)) => utc(at),
        _ => String::new(),
    };
    let mut rows = Vec::new();
    for ch in &channels {
        for (field, st) in &ch.secrets {
            rows.push(vec![
                ch.name.clone(),
                field.clone(),
                secret_text(st),
                changed(st),
            ]);
        }
    }
    if rows.is_empty() {
        writeln!(out, "No channel secrets.")?;
    } else {
        writeln!(out)?;
        write!(
            out,
            "{}",
            table(&["CHANNEL", "SECRET", "STATE", "CHANGED"], &rows)
        )?;
    }
    let others: Vec<Vec<String>> = s
        .secrets
        .iter()
        .filter(|n| !n.name.starts_with("channel."))
        .map(|n| vec![n.name.clone(), secret_text(&n.state), changed(&n.state)])
        .collect();
    if !others.is_empty() {
        writeln!(out)?;
        write!(out, "{}", table(&["NAME", "STATE", "CHANGED"], &others))?;
    }
    let bad = s.secrets.iter().filter(|n| n.state.undecryptable).count();
    if bad > 0 {
        writeln!(
            out,
            "{} cannot be decrypted with the current or previous key: store {} again, or set \
             VIGILY_SECRET_KEY_PREVIOUS to the key {} stored with.",
            count(bad, "secret", "secrets"),
            if bad == 1 { "it" } else { "them" },
            if bad == 1 { "it was" } else { "they were" }
        )?;
    }
    Ok(ExitCode::SUCCESS)
}
