//! Drift monitoring and alerts.
//!
//! Every 30 seconds the server takes the newest finished run of each evaluation suite and checks it
//! against the suite's baseline: behaviour drift per score, data drift per logged feature. A
//! detected drift raises an alert once (alerts are de-duplicated by suite, run and score or
//! feature). Failed schedules raise alerts too (see [`crate::scheduler`]).
//!
//! Each new alert is routed to notification channels by the routing rules ([`crate::rules`]) and
//! delivered by [`crate::delivery`]. A configured `alerts.webhook` is one of those channels.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use vigily_core::drift::{self, DriftConfig, Level};
use vigily_core::evals::{self, Alert, AlertKind, Severity};
use vigily_core::RunStatus;

use crate::AppState;

pub const CHECK_EVERY: Duration = Duration::from_secs(30);

/// Stores an alert (once per dedupe key) and queues it for its channels. Returns true when the
/// alert is new.
pub async fn notify(state: &AppState, alert: Alert) -> bool {
    let stored = match evals::raise_alert(&*state.records, alert).await {
        Ok(Some(a)) => a,
        Ok(None) => return false,
        Err(e) => {
            tracing::warn!(error = %e, "could not store an alert");
            return false;
        }
    };
    tracing::warn!(title = %stored.title, "alert");
    // Queued for the channels the routing rules name; the delivery worker sends them.
    crate::rules::dispatch(state, &stored).await;
    true
}

/// The result count each suite's newest run had when it was last checked. A run is checked again
/// when its count changes (its results were still being written) and after a failed check.
#[derive(Default)]
pub struct Checked(Mutex<std::collections::HashMap<String, usize>>);

/// Starts the periodic drift check.
pub fn start(state: Arc<AppState>) {
    tokio::spawn(async move {
        let checked = Checked::default();
        loop {
            if let Err(e) = check(&state, &checked).await {
                tracing::warn!(error = %e, "drift check failed");
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

fn fmt(v: f64) -> String {
    format!("{v:.3}")
}

/// Checks the newest finished run of every suite whose results changed since its last check.
/// Returns the number of new alerts. An error in one suite is logged and leaves the others unaffected.
pub async fn check(state: &AppState, checked: &Checked) -> vigily_core::Result<usize> {
    state.records.refresh().await?;
    let snap = state.catalog.snapshot();
    let cfg = DriftConfig {
        alpha: state.config.drift.alpha,
        min_effect: state.config.drift.min_effect,
    };
    let mut raised = 0;
    for suite in drift::suites(&snap) {
        match check_suite(state, &suite, checked, cfg).await {
            Ok(n) => raised += n,
            Err(e) => {
                tracing::warn!(suite = %suite.key, error = %e, "drift check of a suite failed")
            }
        }
    }
    Ok(raised)
}

async fn check_suite(
    state: &AppState,
    suite: &drift::Suite,
    checked: &Checked,
    cfg: DriftConfig,
) -> vigily_core::Result<usize> {
    let Some(run) = suite
        .runs
        .iter()
        .rev()
        .find(|r| r.status == RunStatus::Finished)
    else {
        return Ok(0);
    };
    let key = format!("{}/{}", suite.key, run.run_id);
    let count = state
        .records
        .keys(&format!("res/{}/", run.run_id))
        .await?
        .len()
        + state
            .records
            .keys(&format!("feat/{}/", run.run_id))
            .await?
            .len();
    if checked.0.lock().get(&key) == Some(&count) {
        return Ok(0);
    }
    let mut raised = 0;
    let baseline = evals::baseline(&*state.records, &suite.key).await?;
    let label = suite_label(suite);
    for score in &suite.scores {
        let Some(d) =
            drift::behaviour_drift(&*state.records, suite, &run.run_id, score, &baseline, cfg)
                .await?
        else {
            continue;
        };
        if !d.drifted {
            continue;
        }
        let test = d.test.map_or_else(String::new, |t| {
            format!(", {} test p = {:.2e}", t.name, t.p)
        });
        raised += notify(
            state,
            Alert {
                id: String::new(),
                kind: AlertKind::BehaviourDrift,
                severity: if d.delta.abs() >= 2.0 * cfg.min_effect { Severity::Critical } else { Severity::Warning },
                title: format!("{score} dropped on {label}"),
                detail: format!(
                    "{score} is {} against {} on the same {} items over {} baseline runs (difference {}{test}). {} items that passed in the last baseline run fail now.",
                    fmt(d.current_mean),
                    fmt(d.baseline_mean),
                    d.n_current,
                    d.baseline_runs.len(),
                    fmt(d.delta),
                    d.regressions.len()
                ),
                suite: Some(suite.key.clone()),
                run_id: Some(run.run_id.clone()),
                schedule: suite.schedule.clone(),
                created_at: 0,
                acknowledged_at: None,
                dedupe: format!("behaviour/{}/{}/{score}", suite.key, run.run_id),
                delivered: None,
                deliveries: None,
                experiment: suite.experiment.clone(),
                suite_name: Some(label.clone()),
            },
        )
        .await as usize;
    }
    for f in drift::data_drift(&*state.records, suite, &run.run_id, &baseline)
        .await?
        .1
    {
        if f.level != Level::Drift {
            continue;
        }
        let measure = match (f.psi, f.js, &f.note) {
            (Some(p), _, _) => format!("PSI {p:.2} (drift above {})", drift::PSI_DRIFT),
            (_, Some(j), _) => format!(
                "Jensen–Shannon divergence {j:.2} (drift above {})",
                drift::JS_DRIFT
            ),
            (_, _, Some(n)) => n.clone(),
            _ => String::new(),
        };
        raised += notify(
            state,
            Alert {
                id: String::new(),
                kind: AlertKind::DataDrift,
                severity: Severity::Warning,
                title: format!("Input feature {} shifted on {label}", f.feature),
                detail: format!(
                    "The distribution of {} moved against the baseline runs: {measure}.",
                    f.feature
                ),
                suite: Some(suite.key.clone()),
                run_id: Some(run.run_id.clone()),
                schedule: suite.schedule.clone(),
                created_at: 0,
                acknowledged_at: None,
                dedupe: format!("data/{}/{}/{}", suite.key, run.run_id, f.feature),
                delivered: None,
                deliveries: None,
                experiment: suite.experiment.clone(),
                suite_name: Some(label.clone()),
            },
        )
        .await as usize;
    }
    checked.0.lock().insert(key, count);
    Ok(raised)
}

/// A suite's name for people: the target's model, name or reference, else all its values, else
/// the experiment.
pub fn suite_label(s: &drift::Suite) -> String {
    match &s.target {
        serde_json::Value::Object(m) if !m.is_empty() => ["model", "name", "ref"]
            .iter()
            .find_map(|k| m.get(*k).and_then(|v| v.as_str()).map(str::to_string))
            .unwrap_or_else(|| {
                m.values()
                    .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
        _ => s.experiment.clone().unwrap_or_else(|| s.key.clone()),
    }
}
