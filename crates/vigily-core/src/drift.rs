//! Drift: is an evaluation suite's newest run worse than its baseline (behaviour drift), and have
//! the inputs it saw moved (data drift)?
//!
//! A **suite** is the eval runs that share an experiment, a target reference and a dataset name:
//! the same thing measured again and again, usually by a schedule. Each run's aggregate scores are
//! metrics named `eval/<score>`; its per-item scores are records.
//!
//! **Behaviour drift** compares the per-item scores of one run with those of its baseline (the
//! previous `window` runs, or pinned runs):
//!
//! - effect: the difference of means, with a 95 % bootstrap interval;
//! - test: Mann–Whitney U on the item scores, two-sided;
//! - decision: drift when the run is worse, `p < alpha` (0.01) and the drop is at least
//!   `min_effect` (0.02 on a 0–1 score);
//! - regressions: items that passed in the previous run and fail now.
//!
//! **Data drift** compares one run's sampled input features with the baseline runs' samples:
//! numeric features by PSI (stable below 0.1, moderate to 0.2, drift above) with a
//! Kolmogorov–Smirnov test for reference; categorical features by Jensen–Shannon divergence
//! (stable below 0.05, moderate to 0.1, drift above) with a chi-squared test for reference. With
//! thousands of samples every test is significant, so the effect sizes decide.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use crate::catalog::{RunEntry, Snapshot};
use crate::error::Result;
use crate::evals::{self, Baseline, BaselineMode, EvalResult, FeatureSample};
use crate::model::RunStatus;
use crate::records::RecordStore;
use crate::stats;

pub use vigily_api::drift::{
    BehaviourDrift, Bin, DriftConfig, FeatureDrift, Flip, Level, Suite, SuiteRun, TestResult,
};

/// The reserved parameter under which the SDK records what an eval run evaluated.
pub const EVAL_META: &str = "__eval__";
/// Aggregate scores are metrics named `eval/<score>`.
pub const SCORE_PREFIX: &str = "eval/";

pub const PSI_MODERATE: f64 = 0.1;
pub const PSI_DRIFT: f64 = 0.2;
pub const JS_MODERATE: f64 = 0.05;
pub const JS_DRIFT: f64 = 0.1;

fn eval_meta(e: &RunEntry) -> &Value {
    e.params.get(EVAL_META).unwrap_or(&Value::Null)
}

fn schedule_of(e: &RunEntry) -> Option<String> {
    e.params
        .get(crate::model::RUN_META)
        .and_then(|m| m.get("schedule"))
        .and_then(|s| s.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The suite key of an eval run.
pub fn suite_key(experiment: Option<&str>, target: &Value, dataset: Option<&str>) -> String {
    let s = format!(
        "{}\u{0}{}\u{0}{}",
        experiment.unwrap_or(""),
        evals::canonical_json(target),
        dataset.unwrap_or("")
    );
    evals::hash_hex(s.as_bytes())[..12].to_string()
}

/// Groups the eval runs of a snapshot into suites, newest activity first.
pub fn suites(snap: &Snapshot) -> Vec<Suite> {
    let mut map: BTreeMap<String, Suite> = BTreeMap::new();
    for e in &snap.runs {
        if e.kind() != "eval" || e.run.archived {
            continue;
        }
        let meta = eval_meta(e);
        let dataset = meta
            .get("dataset")
            .and_then(|d| d.get("name"))
            .and_then(Value::as_str);
        let target = e.target();
        let key = suite_key(e.run.experiment.as_deref(), &target, dataset);
        let schedule = schedule_of(e);
        let s = map.entry(key.clone()).or_insert_with(|| Suite {
            key,
            experiment: e.run.experiment.clone(),
            target: target.clone(),
            dataset: dataset.map(str::to_string),
            schedule: None,
            runs: vec![],
            scores: vec![],
        });
        if schedule.is_some() {
            s.schedule = schedule.clone();
        }
        let scores: BTreeMap<String, f64> = e
            .series
            .iter()
            .filter_map(|x| {
                let name = x.series.name.strip_prefix(SCORE_PREFIX)?;
                Some((name.to_string(), x.summary.last?))
            })
            .collect();
        for k in scores.keys() {
            if !s.scores.contains(k) {
                s.scores.push(k.clone());
            }
        }
        s.runs.push(SuiteRun {
            run_id: e.run.id.clone(),
            name: e.run.display_name(),
            created_at: e.run.created_at,
            status: e.run.status,
            dataset_version: meta
                .get("dataset")
                .and_then(|d| d.get("version"))
                .and_then(Value::as_u64),
            schedule,
            scores,
        });
    }
    let mut out: Vec<Suite> = map.into_values().collect();
    for s in &mut out {
        s.runs.sort_by_key(|r| r.created_at);
        s.scores.sort();
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.runs.last().map_or(0, |r| r.created_at)));
    out
}

/// The runs a run is compared with: the pinned runs, or the last `window` finished runs before it
/// that are not in `drifted`.
pub fn baseline_runs<'a>(
    suite: &'a Suite,
    run_id: &str,
    b: &Baseline,
    drifted: &HashSet<String>,
) -> Vec<&'a SuiteRun> {
    let done = |r: &&SuiteRun| r.status == RunStatus::Finished && r.run_id != run_id;
    match b.mode {
        BaselineMode::Pinned => suite
            .runs
            .iter()
            .filter(|r| b.runs.contains(&r.run_id))
            .filter(done)
            .collect(),
        BaselineMode::Rolling => {
            let pos = suite
                .runs
                .iter()
                .position(|r| r.run_id == run_id)
                .unwrap_or(suite.runs.len());
            let mut v: Vec<&SuiteRun> = suite.runs[..pos]
                .iter()
                .filter(done)
                .filter(|r| !drifted.contains(&r.run_id))
                .collect();
            let skip = v.len().saturating_sub(b.window);
            v.drain(..skip);
            v
        }
    }
}

/// One item's score in one run: value, pass verdict and the item's content hash.
#[derive(Clone, Debug)]
struct ItemScore {
    value: f64,
    passed: Option<bool>,
    hash: String,
}

/// Per-item scores of one run, by item id.
type ItemScores = HashMap<String, ItemScore>;

/// Items matched across runs must have the same id and the same content.
const MIN_PAIRS: usize = 10;

fn item_scores(results: &[EvalResult], score: &str) -> ItemScores {
    results
        .iter()
        .filter_map(|r| {
            let s = r.scores.iter().find(|s| s.name == score)?;
            let v = s.value.or(s.passed.map(|p| if p { 1.0 } else { 0.0 }))?;
            v.is_finite().then(|| {
                (
                    r.item_id.clone(),
                    ItemScore {
                        value: v,
                        passed: s.passed,
                        hash: r.item_hash.clone(),
                    },
                )
            })
        })
        .collect()
}

fn passed(v: &ItemScore, higher: bool) -> bool {
    v.passed.unwrap_or(if higher {
        v.value >= 0.5
    } else {
        v.value < 0.5
    })
}

/// Each item's mean score over the baseline runs, by (id, content hash), in id order.
fn baseline_means<'a>(base: &'a [(&str, &ItemScores)]) -> BTreeMap<(&'a str, &'a str), f64> {
    let mut acc: BTreeMap<(&str, &str), (f64, usize)> = BTreeMap::new();
    for (_, m) in base {
        for (id, v) in m.iter() {
            let e = acc.entry((id.as_str(), v.hash.as_str())).or_default();
            e.0 += v.value;
            e.1 += 1;
        }
    }
    acc.into_iter()
        .map(|(k, (s, n))| (k, s / n as f64))
        .collect()
}

/// Compares one run's per-item scores with its baseline runs'.
///
/// Items present in both (same id and content) are paired: each item's score now against its own
/// mean over the baseline runs, tested with a Wilcoxon signed-rank test on those differences, with
/// a bootstrap interval that resamples items. Repeated baseline runs of the same items are not
/// independent samples, so they are averaged per item rather than pooled. With fewer than
/// [`MIN_PAIRS`] matched items (a changed dataset), the run's scores are compared with the
/// baseline's per-item means by a Mann–Whitney U test instead.
fn compare(
    score: &str,
    run_id: &str,
    current: &ItemScores,
    base: &[(&str, &ItemScores)],
    cfg: DriftConfig,
) -> Option<BehaviourDrift> {
    let higher = !lower_is_better(score);
    if current.is_empty() || base.iter().all(|(_, m)| m.is_empty()) {
        return None;
    }
    let means = baseline_means(base);
    let mut ids: Vec<&String> = current.keys().collect();
    ids.sort();
    let pairs: Vec<(f64, f64)> = ids
        .iter()
        .filter_map(|id| {
            let c = &current[*id];
            means
                .get(&(id.as_str(), c.hash.as_str()))
                .map(|b| (c.value, *b))
        })
        .collect();
    let paired = pairs.len() >= MIN_PAIRS;
    let (cm, bm, delta, test, ci, n_current, n_baseline) = if paired {
        let diffs: Vec<f64> = pairs.iter().map(|(c, b)| c - b).collect();
        let cur: Vec<f64> = pairs.iter().map(|p| p.0).collect();
        let bas: Vec<f64> = pairs.iter().map(|p| p.1).collect();
        let test = stats::wilcoxon_signed_rank(&diffs).map(|t| TestResult {
            name: "wilcoxon signed-rank".into(),
            statistic: t.statistic,
            z: t.z,
            p: t.p,
        });
        (
            stats::mean(&cur),
            stats::mean(&bas),
            stats::mean(&diffs),
            test,
            stats::bootstrap_mean(&diffs, 1000, 0x6b69_7465),
            pairs.len(),
            pairs.len(),
        )
    } else {
        let cur: Vec<f64> = ids.iter().map(|id| current[*id].value).collect();
        let bas: Vec<f64> = means.values().copied().collect();
        let test = stats::mann_whitney(&cur, &bas).map(|t| TestResult {
            name: "mann-whitney u".into(),
            statistic: t.u,
            z: t.z,
            p: t.p,
        });
        let (cm, bm) = (stats::mean(&cur), stats::mean(&bas));
        (
            cm,
            bm,
            cm - bm,
            test,
            stats::bootstrap_mean_diff(&cur, &bas, 1000, 0x6b69_7465),
            cur.len(),
            bas.len(),
        )
    };
    let worse = if higher { delta < 0.0 } else { delta > 0.0 };
    let drifted =
        worse && delta.abs() >= cfg.min_effect && test.as_ref().is_some_and(|t| t.p < cfg.alpha);
    let (mut regressions, mut fixed) = (Vec::new(), Vec::new());
    let previous = base.last();
    if let Some((_, prev)) = previous {
        for id in &ids {
            let a = &current[*id];
            let Some(b) = prev.get(*id).filter(|b| b.hash == a.hash) else {
                continue;
            };
            let flip = Flip {
                item_id: (*id).clone(),
                before: Some(b.value),
                after: Some(a.value),
            };
            match (passed(b, higher), passed(a, higher)) {
                (true, false) => regressions.push(flip),
                (false, true) => fixed.push(flip),
                _ => {}
            }
        }
    }
    let base_values: Vec<f64> = means.values().copied().collect();
    Some(BehaviourDrift {
        score: score.to_string(),
        run_id: run_id.to_string(),
        baseline_runs: base.iter().map(|(r, _)| r.to_string()).collect(),
        higher_is_better: higher,
        paired,
        n_current,
        n_baseline,
        current_mean: cm,
        baseline_mean: bm,
        baseline_std: stats::std_dev(&base_values),
        delta,
        ci,
        test,
        worse,
        drifted,
        previous_run: previous.map(|(r, _)| r.to_string()),
        regressions,
        fixed,
    })
}

/// Behaviour drift of `score` for every finished run of a suite, oldest first. A rolling baseline
/// leaves out runs that drifted themselves, so a lasting regression keeps being reported until a
/// new baseline is pinned instead of becoming the new normal after `window` runs.
pub async fn behaviour_history(
    rs: &dyn RecordStore,
    suite: &Suite,
    score: &str,
    baseline: &Baseline,
    cfg: DriftConfig,
) -> Result<Vec<(String, Option<BehaviourDrift>)>> {
    let finished: Vec<&SuiteRun> = suite
        .runs
        .iter()
        .filter(|r| r.status == RunStatus::Finished)
        .collect();
    let ids: Vec<&str> = finished.iter().map(|r| r.run_id.as_str()).collect();
    let mut all = evals::results_of_runs(rs, &ids).await?;
    let mut scores: HashMap<String, ItemScores> = HashMap::new();
    for r in &finished {
        let res = all.remove(&r.run_id).unwrap_or_default();
        scores.insert(r.run_id.clone(), item_scores(&res, score));
    }
    let mut drifted: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for r in &finished {
        let base: Vec<(&str, &ItemScores)> = baseline_runs(suite, &r.run_id, baseline, &drifted)
            .iter()
            .filter_map(|b| scores.get(&b.run_id).map(|m| (b.run_id.as_str(), m)))
            .collect();
        let d = compare(score, &r.run_id, &scores[&r.run_id], &base, cfg);
        if d.as_ref().is_some_and(|d| d.drifted) {
            drifted.insert(r.run_id.clone());
        }
        out.push((r.run_id.clone(), d));
    }
    Ok(out)
}

/// Behaviour drift of `score` for one run. `None` when either side has no items.
pub async fn behaviour_drift(
    rs: &dyn RecordStore,
    suite: &Suite,
    run_id: &str,
    score: &str,
    baseline: &Baseline,
    cfg: DriftConfig,
) -> Result<Option<BehaviourDrift>> {
    Ok(behaviour_history(rs, suite, score, baseline, cfg)
        .await?
        .into_iter()
        .find(|(r, _)| r == run_id)
        .and_then(|(_, d)| d))
}

/// Scores where lower is better: latencies, errors, costs, losses.
pub fn lower_is_better(score: &str) -> bool {
    let s = score.to_lowercase();
    [
        "latency",
        "error",
        "loss",
        "cost",
        "toxicity",
        "hallucination",
        "time",
    ]
    .iter()
    .any(|w| s.contains(w))
}

fn label_num(v: f64) -> String {
    let a = v.abs();
    if a != 0.0 && (a < 1e-3 || a >= 1e6) {
        format!("{v:.2e}")
    } else if (v - v.round()).abs() < 1e-9 {
        format!("{v:.0}")
    } else {
        format!("{v:.3}")
    }
}

/// Compares one run's sample of a feature with the pooled samples of its baseline runs.
pub fn feature_drift(baseline: &[&FeatureSample], current: &FeatureSample) -> FeatureDrift {
    let numeric = current.is_numeric() && baseline.iter().all(|b| b.is_numeric());
    if numeric {
        let b: Vec<f64> = baseline
            .iter()
            .flat_map(|s| s.numbers())
            .filter(|x| x.is_finite())
            .collect();
        let c: Vec<f64> = current
            .numbers()
            .into_iter()
            .filter(|x| x.is_finite())
            .collect();
        if b.is_empty() || c.is_empty() {
            return FeatureDrift {
                feature: current.feature.clone(),
                kind: "numeric".into(),
                n_baseline: b.len(),
                n_current: c.len(),
                psi: None,
                ks: None,
                js: None,
                chi_squared: None,
                // A feature that lost all its finite values has changed; a baseline without any
                // cannot be compared with.
                level: if c.is_empty() && !b.is_empty() {
                    Level::Drift
                } else {
                    Level::Stable
                },
                note: Some(
                    if c.is_empty() {
                        "no finite values in this run"
                    } else {
                        "no finite values in the baseline"
                    }
                    .into(),
                ),
                bins: vec![],
            };
        }
        let edges = stats::quantile_edges(&b, 10);
        let (bs, cs) = (stats::bin_shares(&b, &edges), stats::bin_shares(&c, &edges));
        let psi = (!b.is_empty() && !c.is_empty()).then(|| stats::psi_of_shares(&bs, &cs));
        let bins = (0..bs.len())
            .map(|i| Bin {
                label: match (i.checked_sub(1).map(|j| edges[j]), edges.get(i)) {
                    (None, Some(hi)) => format!("≤ {}", label_num(*hi)),
                    (Some(lo), Some(hi)) => format!("{}–{}", label_num(lo), label_num(*hi)),
                    (Some(lo), None) => format!("> {}", label_num(lo)),
                    (None, None) => "all".into(),
                },
                baseline: bs[i],
                current: cs[i],
            })
            .collect();
        let level = match psi {
            Some(p) if p >= PSI_DRIFT => Level::Drift,
            Some(p) if p >= PSI_MODERATE => Level::Moderate,
            _ => Level::Stable,
        };
        FeatureDrift {
            feature: current.feature.clone(),
            kind: "numeric".into(),
            n_baseline: b.len(),
            n_current: c.len(),
            psi,
            ks: stats::ks(&b, &c),
            js: None,
            chi_squared: None,
            level,
            note: None,
            bins,
        }
    } else {
        let b: Vec<String> = baseline.iter().flat_map(|s| s.strings()).collect();
        let c = current.strings();
        let (bc, cc) = (stats::counts(&b), stats::counts(&c));
        let js = stats::js_divergence(&bc, &cc);
        let (nb, nc) = (b.len().max(1) as f64, c.len().max(1) as f64);
        let mut cats: Vec<(&String, f64)> = bc
            .keys()
            .chain(cc.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|k| {
                (
                    k,
                    bc.get(k).unwrap_or(&0.0) / nb + cc.get(k).unwrap_or(&0.0) / nc,
                )
            })
            .collect();
        cats.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
        let mut bins: Vec<Bin> = cats
            .iter()
            .take(12)
            .map(|(k, _)| Bin {
                label: (*k).clone(),
                baseline: bc.get(*k).unwrap_or(&0.0) / nb,
                current: cc.get(*k).unwrap_or(&0.0) / nc,
            })
            .collect();
        if cats.len() > 12 {
            let (sb, sc) = bins
                .iter()
                .fold((0.0, 0.0), |a, x| (a.0 + x.baseline, a.1 + x.current));
            bins.push(Bin {
                label: "other".into(),
                baseline: (1.0 - sb).max(0.0),
                current: (1.0 - sc).max(0.0),
            });
        }
        let level = match js {
            Some(j) if j >= JS_DRIFT => Level::Drift,
            Some(j) if j >= JS_MODERATE => Level::Moderate,
            _ => Level::Stable,
        };
        FeatureDrift {
            feature: current.feature.clone(),
            kind: "categorical".into(),
            n_baseline: b.len(),
            n_current: c.len(),
            psi: None,
            ks: None,
            js,
            chi_squared: stats::chi_squared(&bc, &cc),
            level,
            note: None,
            bins,
        }
    }
}

/// Data drift of every feature for every finished run of a suite, oldest first, with the baseline
/// runs used. As for behaviour, a rolling baseline leaves out runs whose inputs drifted.
pub async fn data_history(
    rs: &dyn RecordStore,
    suite: &Suite,
    baseline: &Baseline,
) -> Result<Vec<(String, Vec<String>, Vec<FeatureDrift>)>> {
    let finished: Vec<&SuiteRun> = suite
        .runs
        .iter()
        .filter(|r| r.status == RunStatus::Finished)
        .collect();
    let ids: Vec<&str> = finished.iter().map(|r| r.run_id.as_str()).collect();
    let mut all = evals::features_of_runs(rs, &ids).await?;
    let mut samples: HashMap<String, Vec<FeatureSample>> = HashMap::new();
    for r in &finished {
        samples.insert(r.run_id.clone(), all.remove(&r.run_id).unwrap_or_default());
    }
    let mut drifted: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for r in &finished {
        let base_runs: Vec<String> = baseline_runs(suite, &r.run_id, baseline, &drifted)
            .iter()
            .map(|b| b.run_id.clone())
            .collect();
        let mut by_feature: HashMap<&str, Vec<&FeatureSample>> = HashMap::new();
        for b in &base_runs {
            for f in samples.get(b).into_iter().flatten() {
                by_feature.entry(f.feature.as_str()).or_default().push(f);
            }
        }
        let verdicts: Vec<FeatureDrift> = samples[&r.run_id]
            .iter()
            .filter_map(|c| Some(feature_drift(by_feature.get(c.feature.as_str())?, c)))
            .collect();
        if verdicts.iter().any(|f| f.level == Level::Drift) {
            drifted.insert(r.run_id.clone());
        }
        out.push((r.run_id.clone(), base_runs, verdicts));
    }
    Ok(out)
}

/// Data drift of every feature `run_id` logged, and the baseline runs it was compared with.
pub async fn data_drift(
    rs: &dyn RecordStore,
    suite: &Suite,
    run_id: &str,
    baseline: &Baseline,
) -> Result<(Vec<String>, Vec<FeatureDrift>)> {
    Ok(data_history(rs, suite, baseline)
        .await?
        .into_iter()
        .find(|(r, _, _)| r == run_id)
        .map(|(_, b, f)| (b, f))
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evals::{put_results, Score};
    use crate::records::MemoryRecords;
    use serde_json::json;

    fn run(id: &str, t: i64) -> SuiteRun {
        SuiteRun {
            run_id: id.into(),
            name: id.into(),
            created_at: t,
            status: RunStatus::Finished,
            dataset_version: Some(1),
            schedule: None,
            scores: BTreeMap::new(),
        }
    }

    async fn results_with(rs: &MemoryRecords, run: &str, pass_rate: f64, n: usize) {
        let fails = ((1.0 - pass_rate) * n as f64).round() as usize;
        let rows = (0..n)
            .map(|i| EvalResult {
                run_id: run.into(),
                item_id: format!("i{i:03}"),
                item_hash: "h".into(),
                dataset: "d".into(),
                dataset_version: 1,
                output: json!("x"),
                error: None,
                latency_ms: None,
                // The last `fails` items fail.
                scores: vec![Score {
                    name: "exact_match".into(),
                    value: None,
                    passed: Some(i < n - fails),
                    reason: None,
                    evaluator: "exact_match".into(),
                    evaluator_version: "1".into(),
                }],
                created_at: 0,
                trace_id: None,
            })
            .collect();
        put_results(rs, rows).await.unwrap();
    }

    #[tokio::test]
    async fn detects_a_drop_and_lists_regressions() {
        let rs = MemoryRecords::new();
        let mut runs = Vec::new();
        for d in 0..8 {
            let id = format!("r{d}");
            let rate = if d < 7 { 0.9 } else { 0.7 };
            results_with(&rs, &id, rate, 300).await;
            runs.push(run(&id, d));
        }
        let suite = Suite {
            key: "k".into(),
            experiment: None,
            target: Value::Null,
            dataset: Some("d".into()),
            schedule: None,
            runs,
            scores: vec!["exact_match".into()],
        };
        let b = Baseline::default();
        let d = behaviour_drift(&rs, &suite, "r7", "exact_match", &b, DriftConfig::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(d.baseline_runs.len(), 7);
        assert!((d.delta + 0.2).abs() < 1e-9);
        assert!(d.drifted && d.worse);
        assert_eq!(d.regressions.len(), 60);
        assert_eq!(d.previous_run.as_deref(), Some("r6"));
        // A stable run does not drift.
        let s = behaviour_drift(&rs, &suite, "r6", "exact_match", &b, DriftConfig::default())
            .await
            .unwrap()
            .unwrap();
        assert!(!s.drifted);
        // Pinned baseline.
        let pinned = Baseline {
            mode: BaselineMode::Pinned,
            runs: vec!["r0".into()],
            ..Default::default()
        };
        assert_eq!(
            baseline_runs(&suite, "r7", &pinned, &HashSet::new()).len(),
            1
        );
        // A lasting drop keeps drifting: the drifted run stays out of the rolling baseline.
        results_with(&rs, "r8", 0.7, 300).await;
        let mut suite = suite;
        suite.runs.push(run("r8", 8));
        let d = behaviour_drift(&rs, &suite, "r8", "exact_match", &b, DriftConfig::default())
            .await
            .unwrap()
            .unwrap();
        assert!(d.drifted);
        assert!(!d.baseline_runs.contains(&"r7".to_string()));
        // No baseline for the first run.
        assert!(
            behaviour_drift(&rs, &suite, "r0", "exact_match", &b, DriftConfig::default())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn pairs_items_and_falls_back_when_content_changed() {
        let rs = MemoryRecords::new();
        let mut runs = Vec::new();
        for d in 0..4 {
            let id = format!("r{d}");
            results_with(&rs, &id, if d < 3 { 0.9 } else { 0.7 }, 200).await;
            runs.push(run(&id, d));
        }
        let suite = Suite {
            key: "k".into(),
            experiment: None,
            target: Value::Null,
            dataset: Some("d".into()),
            schedule: None,
            runs,
            scores: vec!["exact_match".into()],
        };
        let d = behaviour_drift(
            &rs,
            &suite,
            "r3",
            "exact_match",
            &Baseline::default(),
            DriftConfig::default(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(d.paired && d.drifted);
        assert_eq!(d.test.unwrap().name, "wilcoxon signed-rank");
        assert_eq!(
            (d.n_current, d.n_baseline),
            (200, 200),
            "items, not items times runs"
        );
        // The same ids with new content do not pair: the unpaired fallback is used.
        let mut changed = evals::results(&rs, "r3").await.unwrap();
        for r in &mut changed {
            r.item_hash = "new".into();
        }
        put_results(&rs, changed).await.unwrap();
        let d = behaviour_drift(
            &rs,
            &suite,
            "r3",
            "exact_match",
            &Baseline::default(),
            DriftConfig::default(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!d.paired);
        assert_eq!(d.test.unwrap().name, "mann-whitney u");
        assert!(
            d.regressions.is_empty(),
            "changed items are not regressions"
        );
    }

    #[test]
    fn feature_drift_levels() {
        let mk = |vals: Vec<Value>| FeatureSample {
            run_id: "r".into(),
            feature: "f".into(),
            values: vals,
        };
        let base = mk((0..1000).map(|i| json!((i % 100) as f64)).collect());
        let same = mk((0..1000).map(|i| json!((i % 100) as f64)).collect());
        let moved = mk((0..1000).map(|i| json!((i % 100) as f64 + 40.0)).collect());
        assert_eq!(feature_drift(&[&base], &same).level, Level::Stable);
        let d = feature_drift(&[&base], &moved);
        assert_eq!((d.level, d.kind), (Level::Drift, "numeric".to_string()));
        assert_eq!(d.bins.len(), 10);
        let langs = |s: &str| {
            let one: Vec<Value> = s.chars().map(|c| json!(c.to_string())).collect();
            mk(one.iter().cycle().take(one.len() * 50).cloned().collect())
        };
        let d = feature_drift(&[&langs("eeeedf")], &langs("eddddf"));
        assert_eq!((d.level, d.kind), (Level::Drift, "categorical".to_string()));
        assert!(d.chi_squared.unwrap().p < 0.001);
        // NaN and infinities are stored as JSON null: a feature that lost every finite value.
        let all_nan = mk(vec![Value::Null; 3]);
        let d = feature_drift(&[&base], &all_nan);
        assert_eq!(
            (d.level, d.note.as_deref()),
            (Level::Drift, Some("no finite values in this run"))
        );
    }

    #[test]
    fn score_direction() {
        assert!(lower_is_better("latency_ms"));
        assert!(!lower_is_better("exact_match"));
    }
}
