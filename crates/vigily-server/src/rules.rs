//! Routing rules: which alerts go to which notification channels.
//!
//! The rules are one ordered list, kept as the record `notify/rules` with a version. Each rule
//! matches alerts by kind, severity and glob patterns on the experiment, the suite (its key or
//! name) and the schedule (`*` any characters, `?` one); an empty match takes every alert. Every
//! matching rule adds its channels. After the list, the default rule sends the alert to every
//! channel marked default, unless a matching rule has `stop` set. Disabled channels get nothing.
//!
//! The list is replaced as a whole: `PUT` sends the version it was read at and gets 409 when
//! someone changed it since.
//!
//! | Method and path                      | Scope | What it does |
//! |--------------------------------------|-------|--------------|
//! | `GET /api/v1/notifications/rules`    | read  | The rules and their version |
//! | `PUT /api/v1/notifications/rules`    | admin | Replaces them (`{version, rules}`) |

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Extension, Json, Router};
use vigily_api::evals::Alert;
use vigily_api::notifications::{Rule, Rules, RulesUpdate};
use vigily_core::{now_micros, RecordStore};

use crate::auth::{AuthContext, Scoped};
use crate::channels::ChannelRecord;
use crate::{ApiError, AppState};

const KEY: &str = "notify/rules";

/// The most rules a list holds.
pub const MAX_RULES: usize = 200;

/// The stored rules; version 0 when none were ever saved.
pub async fn load(rs: &dyn RecordStore) -> vigily_core::Result<Rules> {
    Ok(match rs.get(KEY).await? {
        Some(v) => serde_json::from_value(v)
            .map_err(|e| vigily_core::Error::Storage(format!("routing rules: {e}")))?,
        None => Rules {
            version: 0,
            rules: Vec::new(),
            updated_at: None,
            changed_by: None,
        },
    })
}

/// Whether `text` matches `pattern`: `*` is any run of characters, `?` one character.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Whether a pattern matches one of `values`; no pattern matches everything.
fn names(pattern: &Option<String>, values: &[Option<&str>]) -> bool {
    match pattern.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        None => true,
        Some(p) => values.iter().flatten().any(|v| glob(p, v)),
    }
}

/// Whether `rule` takes `alert`.
pub fn matches(rule: &Rule, a: &Alert) -> bool {
    let m = &rule.matches;
    (m.kinds.is_empty() || m.kinds.contains(&a.kind))
        && (m.severities.is_empty() || m.severities.contains(&a.severity))
        && names(&m.experiment, &[a.experiment.as_deref()])
        && names(&m.suite, &[a.suite.as_deref(), a.suite_name.as_deref()])
        && names(&m.schedule, &[a.schedule.as_deref()])
}

/// The ids of the channels `alert` goes to under `rules`, given every channel.
pub fn route(rules: &[Rule], channels: &[ChannelRecord], alert: &Alert) -> Vec<String> {
    let mut to = BTreeSet::new();
    let mut stopped = false;
    for r in rules {
        if matches(r, alert) {
            to.extend(r.channels.iter().cloned());
            if r.stop {
                stopped = true;
                break;
            }
        }
    }
    if !stopped {
        to.extend(channels.iter().filter(|c| c.default).map(|c| c.id.clone()));
    }
    channels
        .iter()
        .filter(|c| c.enabled && to.contains(&c.id))
        .map(|c| c.id.clone())
        .collect()
}

/// Queues `alert` for the channels the rules route it to.
pub async fn dispatch(state: &AppState, alert: &Alert) {
    let rules = match load(&*state.records).await {
        Ok(r) => r.rules,
        Err(e) => {
            // The default rule still applies.
            tracing::warn!(error = %e, "could not read the routing rules");
            Vec::new()
        }
    };
    let channels = match crate::channels::all(state).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "could not read the notification channels");
            return;
        }
    };
    let ids = route(&rules, &channels, alert);
    let to: Vec<ChannelRecord> = channels
        .into_iter()
        .filter(|c| ids.contains(&c.id))
        .collect();
    crate::delivery::dispatch(state, alert, &to).await;
}

/// The routing routes, relative to the API root.
pub fn routes() -> Scoped {
    Scoped::default()
        .read(Router::new().route("/notifications/rules", get(get_rules)))
        .admin(Router::new().route("/notifications/rules", axum::routing::put(put_rules)))
}

async fn get_rules(State(s): State<Arc<AppState>>) -> Result<Json<Rules>, ApiError> {
    Ok(Json(load(&*s.records).await?))
}

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, m)
}

async fn put_rules(
    State(s): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    bytes: Bytes,
) -> Result<Json<Rules>, ApiError> {
    let req: RulesUpdate = serde_json::from_slice(&bytes).map_err(|e| {
        bad(format!(
            "send {{\"version\", \"rules\": [{{\"name\"?, \"match\": {{\"kinds\"?, \"severities\"?, \
             \"experiment\"?, \"suite\"?, \"schedule\"?}}, \"channels\": [ids], \"stop\"?}}]}}: {e}"
        ))
    })?;
    if req.rules.len() > MAX_RULES {
        return Err(bad(format!("at most {MAX_RULES} rules")));
    }
    let channels = crate::channels::all(&s).await?;
    for (i, r) in req.rules.iter().enumerate() {
        let n = i + 1;
        if r.channels.is_empty() && !r.stop {
            return Err(bad(format!(
                "rule {n} names no channels; give it channels, or set stop to keep matching alerts \
                 from the default channels"
            )));
        }
        for c in &r.channels {
            if !channels.iter().any(|x| &x.id == c) {
                return Err(bad(format!(
                    "rule {n} names channel {c}, which does not exist"
                )));
            }
        }
        for p in [&r.matches.experiment, &r.matches.suite, &r.matches.schedule]
            .into_iter()
            .flatten()
        {
            if p.chars().count() > 256 {
                return Err(bad(format!(
                    "rule {n} has a pattern longer than 256 characters"
                )));
            }
        }
        if r.name.as_ref().is_some_and(|n| n.chars().count() > 128) {
            return Err(bad(format!(
                "rule {n} has a name longer than 128 characters"
            )));
        }
    }
    // Read, compare and write under one lock, so two writers at the same version cannot both win.
    let _guard = s.rules_lock.lock().await;
    let current = load(&*s.records).await?;
    if current.version != req.version {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "the rules changed since version {} (now version {}, by {}); read them again and \
                 reapply your change",
                req.version,
                current.version,
                current.changed_by.as_deref().unwrap_or("someone")
            ),
        ));
    }
    let next = Rules {
        version: current.version + 1,
        rules: req.rules,
        updated_at: Some(now_micros()),
        changed_by: Some(ctx.principal.clone()),
    };
    let v = serde_json::to_value(&next).map_err(|e| vigily_core::Error::Storage(e.to_string()))?;
    s.records.put(vec![(KEY.to_string(), v)]).await?;
    s.records.flush().await?;
    crate::audit::record(&*s.records, &ctx.principal, "rules.updated", "rules").await;
    Ok(Json(next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vigily_api::evals::{AlertKind, Severity};
    use vigily_api::notifications::{ChannelKind, RuleMatch};

    #[test]
    fn globs() {
        assert!(glob("*", ""));
        assert!(glob("*", "anything"));
        assert!(glob("intent-*", "intent-clf-v2"));
        assert!(glob("*-v?", "intent-clf-v2"));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("a*b*c", "aXXbYY"));
        assert!(!glob("nightly", "nightly-2"));
        assert!(glob("night?y", "nightly"));
        assert!(!glob("", "x"));
    }

    fn channel(id: &str, default: bool, enabled: bool) -> ChannelRecord {
        ChannelRecord {
            id: id.into(),
            name: id.into(),
            kind: ChannelKind::Webhook,
            enabled,
            default,
            settings: Default::default(),
            env: Default::default(),
            created_at: 0,
            updated_at: 0,
            changed_by: None,
            config_url: None,
        }
    }

    fn alert(kind: AlertKind, severity: Severity, suite: Option<&str>) -> Alert {
        Alert {
            id: "a".into(),
            kind,
            severity,
            title: String::new(),
            detail: String::new(),
            suite: suite.map(|_| "916d93593f1a".into()),
            run_id: None,
            schedule: Some("nightly".into()),
            created_at: 0,
            acknowledged_at: None,
            dedupe: String::new(),
            delivered: None,
            deliveries: None,
            experiment: Some("intents".into()),
            suite_name: suite.map(str::to_string),
        }
    }

    fn rule(m: RuleMatch, channels: &[&str], stop: bool) -> Rule {
        Rule {
            name: None,
            matches: m,
            channels: channels.iter().map(|c| c.to_string()).collect(),
            stop,
        }
    }

    #[test]
    fn rules_add_channels_in_order_and_stop_skips_the_default() {
        let channels = [
            channel("pager", false, true),
            channel("slack", true, true),
            channel("mail", false, true),
            channel("off", true, false),
        ];
        let rules = [
            rule(
                RuleMatch {
                    kinds: vec![AlertKind::ScheduleFailed],
                    ..Default::default()
                },
                &["mail"],
                false,
            ),
            rule(
                RuleMatch {
                    severities: vec![Severity::Critical],
                    suite: Some("intent-*".into()),
                    ..Default::default()
                },
                &["pager"],
                true,
            ),
        ];
        let sched = alert(AlertKind::ScheduleFailed, Severity::Critical, None);
        assert_eq!(route(&rules, &channels, &sched), ["slack", "mail"]);
        let crit = alert(
            AlertKind::BehaviourDrift,
            Severity::Critical,
            Some("intent-clf-v2"),
        );
        assert_eq!(route(&rules, &channels, &crit), ["pager"]);
        let warn = alert(
            AlertKind::BehaviourDrift,
            Severity::Warning,
            Some("intent-clf-v2"),
        );
        assert_eq!(route(&rules, &channels, &warn), ["slack"]);
        // The suite pattern matches its key too, and the experiment matches by glob.
        let by_key = rule(
            RuleMatch {
                suite: Some("916d*".into()),
                experiment: Some("int*".into()),
                ..Default::default()
            },
            &["mail"],
            false,
        );
        assert_eq!(route(&[by_key], &channels, &warn), ["slack", "mail"]);
        // No rules: the default channels that are enabled.
        assert_eq!(route(&[], &channels, &warn), ["slack"]);
    }
}
