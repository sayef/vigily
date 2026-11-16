//! What each kind of channel receives: one [`Message`] rendered as a Slack Block Kit message, a
//! Teams Adaptive Card, a Discord embed, vigily's webhook document or an e-mail.

use serde_json::{json, Value};
use vigily_api::evals::{Alert, AlertKind, Severity};
use vigily_api::notifications::{ChannelKind, WebhookAlert, WebhookPayload};

/// An alert as people read it, whatever the channel.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub alert_id: String,
    /// `behaviour_drift`, `data_drift`, `schedule_failed`, or `test`.
    pub kind: String,
    /// `Evaluation regression`, `Data drift`, `Schedule failed`, `Test`.
    pub kind_label: String,
    /// `warning` or `critical`.
    pub severity: String,
    pub title: String,
    /// What changed: the metric, its value against the baseline.
    pub detail: String,
    pub suite: Option<String>,
    pub run_id: Option<String>,
    pub schedule: Option<String>,
    /// UTC microseconds.
    pub created_at: i64,
    pub dedupe: String,
    /// The alert's page in the web app, when the engine knows its address.
    pub link: Option<String>,
    pub test: bool,
}

fn kind_name(k: AlertKind) -> &'static str {
    match k {
        AlertKind::BehaviourDrift => "behaviour_drift",
        AlertKind::DataDrift => "data_drift",
        AlertKind::ScheduleFailed => "schedule_failed",
    }
}

fn kind_label(k: AlertKind) -> &'static str {
    match k {
        AlertKind::BehaviourDrift => "Evaluation regression",
        AlertKind::DataDrift => "Data drift",
        AlertKind::ScheduleFailed => "Schedule failed",
    }
}

/// A path segment with everything but unreserved characters percent-encoded.
pub fn segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Message {
    /// The message of `alert`, linking into the web app at `base` (without a trailing slash).
    pub fn of(alert: &Alert, base: Option<&str>) -> Self {
        let path = match (&alert.suite, &alert.schedule, alert.kind) {
            (_, Some(s), AlertKind::ScheduleFailed) => format!("/schedules/{}", segment(s)),
            (Some(k), _, _) => format!("/drift/{}", segment(k)),
            _ => "/alerts".to_string(),
        };
        Message {
            alert_id: alert.id.clone(),
            kind: kind_name(alert.kind).into(),
            kind_label: kind_label(alert.kind).into(),
            severity: match alert.severity {
                Severity::Warning => "warning".into(),
                Severity::Critical => "critical".into(),
            },
            title: alert.title.clone(),
            detail: alert.detail.clone(),
            suite: alert.suite.clone(),
            run_id: alert.run_id.clone(),
            schedule: alert.schedule.clone(),
            created_at: alert.created_at,
            dedupe: alert.dedupe.clone(),
            link: base.map(|b| format!("{}{path}", b.trim_end_matches('/'))),
            test: false,
        }
    }

    /// A test message for channel `name`.
    pub fn test(name: &str, base: Option<&str>, now: i64) -> Self {
        Message {
            alert_id: "test".into(),
            kind: "test".into(),
            kind_label: "Test".into(),
            severity: "warning".into(),
            title: format!("Test alert for {name}"),
            detail: "This is a test sent from vigily to check that the channel works. Real alerts \
                     name what changed, such as a score against its baseline."
                .into(),
            suite: None,
            run_id: None,
            schedule: None,
            created_at: now,
            dedupe: format!("test/{now}"),
            link: base.map(|b| format!("{}/alerts", b.trim_end_matches('/'))),
            test: true,
        }
    }

    /// `2026-11-16 09:30:00 UTC`.
    pub fn time(&self) -> String {
        chrono::DateTime::from_timestamp_micros(self.created_at)
            .map(|d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_default()
    }

    /// RFC 3339 in UTC.
    pub fn rfc3339(&self) -> String {
        chrono::DateTime::from_timestamp_micros(self.created_at)
            .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default()
    }

    fn severity_label(&self) -> &'static str {
        if self.severity == "critical" {
            "Critical"
        } else {
            "Warning"
        }
    }

    /// The names that place the alert: suite, run, schedule.
    pub fn facts(&self) -> Vec<(&'static str, String)> {
        let mut f = vec![
            ("Severity", self.severity_label().to_string()),
            ("Kind", self.kind_label.clone()),
        ];
        if let Some(s) = &self.suite {
            f.push(("Suite", s.clone()));
        }
        if let Some(r) = &self.run_id {
            f.push(("Run", r.clone()));
        }
        if let Some(s) = &self.schedule {
            f.push(("Schedule", s.clone()));
        }
        f.push(("Time", self.time()));
        f
    }

    /// `[vigily] Critical: <title>`.
    pub fn subject(&self) -> String {
        format!("[vigily] {}: {}", self.severity_label(), self.title)
    }

    /// The plain-text form (e-mail).
    pub fn text(&self) -> String {
        let mut s = format!("{}\n\n{}\n\n", self.title, self.detail);
        for (k, v) in self.facts() {
            s.push_str(&format!("{k}: {v}\n"));
        }
        if let Some(l) = &self.link {
            s.push_str(&format!("\nOpen in vigily: {l}\n"));
        }
        s
    }

    /// A simple HTML form (e-mail).
    pub fn html(&self) -> String {
        let e = escape_html;
        let color = if self.severity == "critical" {
            "#b42318"
        } else {
            "#b54708"
        };
        let rows: String = self
            .facts()
            .iter()
            .map(|(k, v)| {
                format!(
                    "<tr><td style=\"padding:2px 12px 2px 0;color:#555\">{}</td><td>{}</td></tr>",
                    e(k),
                    e(v)
                )
            })
            .collect();
        let link = self
            .link
            .as_ref()
            .map(|l| format!("<p><a href=\"{}\">Open in vigily</a></p>", e(l)))
            .unwrap_or_default();
        format!(
            "<!doctype html><html><body style=\"font-family:sans-serif;font-size:14px;color:#111\">\
             <p style=\"color:{color};font-weight:600;margin:0 0 4px\">{}</p>\
             <h2 style=\"margin:0 0 12px;font-size:18px\">{}</h2><p>{}</p>\
             <table style=\"border-collapse:collapse\">{rows}</table>{link}</body></html>",
            e(self.severity_label()),
            e(&self.title),
            e(&self.detail)
        )
    }

    pub fn webhook(&self) -> WebhookPayload {
        WebhookPayload {
            event: "vigily.alert".into(),
            schema: "vigily.alert/1".into(),
            test: self.test,
            alert: WebhookAlert {
                id: self.alert_id.clone(),
                kind: self.kind.clone(),
                severity: self.severity.clone(),
                title: self.title.clone(),
                detail: self.detail.clone(),
                suite: self.suite.clone(),
                run_id: self.run_id.clone(),
                schedule: self.schedule.clone(),
                created_at: self.created_at,
                time: self.rfc3339(),
                dedupe: self.dedupe.clone(),
            },
            url: self.link.clone(),
        }
    }

    pub fn slack(&self) -> Value {
        let emoji = if self.severity == "critical" {
            ":red_circle:"
        } else {
            ":large_orange_circle:"
        };
        let fields: Vec<Value> = self
            .facts()
            .iter()
            .map(|(k, v)| json!({"type": "mrkdwn", "text": format!("*{k}*\n{}", slack_escape(v))}))
            .collect();
        let mut blocks = vec![
            json!({"type": "header", "text": {"type": "plain_text", "text": clip(&self.title, 150), "emoji": false}}),
            json!({"type": "section", "text": {"type": "mrkdwn", "text": format!("{emoji} {}", clip(&slack_escape(&self.detail), 2900))}}),
            json!({"type": "section", "fields": fields.into_iter().take(10).collect::<Vec<_>>()}),
        ];
        if let Some(l) = &self.link {
            blocks.push(json!({"type": "actions", "elements": [
                {"type": "button", "text": {"type": "plain_text", "text": "Open in vigily"}, "url": l}
            ]}));
        }
        json!({
            "text": format!("{}: {}", self.severity_label(), self.title),
            "blocks": blocks,
        })
    }

    pub fn teams(&self) -> Value {
        let facts: Vec<Value> = self
            .facts()
            .iter()
            .map(|(k, v)| json!({"title": k, "value": v}))
            .collect();
        let mut card = json!({
            "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
            "type": "AdaptiveCard",
            "version": "1.4",
            "body": [
                {"type": "TextBlock", "text": self.severity_label(), "weight": "Bolder",
                 "color": if self.severity == "critical" { "Attention" } else { "Warning" }},
                {"type": "TextBlock", "text": self.title, "size": "Large", "weight": "Bolder", "wrap": true},
                {"type": "TextBlock", "text": self.detail, "wrap": true},
                {"type": "FactSet", "facts": facts},
            ],
        });
        if let Some(l) = &self.link {
            card["actions"] =
                json!([{"type": "Action.OpenUrl", "title": "Open in vigily", "url": l}]);
        }
        json!({
            "type": "message",
            "attachments": [{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "contentUrl": null,
                "content": card,
            }],
        })
    }

    pub fn discord(&self) -> Value {
        let fields: Vec<Value> = self
            .facts()
            .iter()
            .filter(|(k, _)| *k != "Time")
            .map(|(k, v)| json!({"name": k, "value": clip(v, 1024), "inline": true}))
            .collect();
        let mut embed = json!({
            "title": clip(&self.title, 256),
            "description": clip(&self.detail, 4000),
            "color": if self.severity == "critical" { 0xB4_23_18 } else { 0xB5_47_08 },
            "fields": fields,
            "timestamp": self.rfc3339(),
            "footer": {"text": "vigily"},
        });
        if let Some(l) = &self.link {
            embed["url"] = json!(l);
        }
        json!({
            "username": "vigily",
            "content": format!("{}: {}", self.severity_label(), clip(&self.title, 1800)),
            "embeds": [embed],
            "allowed_mentions": {"parse": []},
        })
    }

    /// The JSON body an HTTP channel of `kind` posts.
    pub fn body(&self, kind: ChannelKind) -> Value {
        match kind {
            ChannelKind::Slack => self.slack(),
            ChannelKind::Teams => self.teams(),
            ChannelKind::Discord => self.discord(),
            ChannelKind::Webhook | ChannelKind::Email => {
                serde_json::to_value(self.webhook()).unwrap_or_default()
            }
        }
    }
}

/// At most `n` characters, with `…` when cut.
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Slack's mrkdwn escapes: `&`, `<` and `>`.
fn slack_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
