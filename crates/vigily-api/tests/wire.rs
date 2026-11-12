//! The wire format is pinned: each answer type reads its golden file (captured from the engine's
//! answers) and writes it back unchanged, keys in the same order. A failing test here means the
//! JSON the web app and the SDK read would change.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use vigily_api::{chart, evals, runs, traces};

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Reads fixture `name` as `T` and writes it back: the same JSON, keys in the same order.
fn same<T: Serialize + DeserializeOwned>(name: &str) {
    let want = fixture(name);
    let typed: T = serde_json::from_value(want.clone())
        .unwrap_or_else(|e| panic!("{name}.json does not read as the type: {e}"));
    let got = serde_json::to_value(&typed).unwrap();
    assert_eq!(
        serde_json::to_string_pretty(&got).unwrap(),
        serde_json::to_string_pretty(&want).unwrap(),
        "{name}.json changed shape"
    );
}

#[test]
fn runs_keep_their_wire_shape() {
    same::<runs::Meta>("meta");
    same::<runs::RunsResponse>("runs");
    same::<runs::RunDetail>("run_detail");
}

#[test]
fn charts_keep_their_wire_shape() {
    same::<chart::ChartResponse>("chart");
    same::<chart::ChartResponse>("chart_aggregate");
}

#[test]
fn evaluations_keep_their_wire_shape() {
    same::<Vec<evals::DatasetInfo>>("datasets");
    same::<evals::DatasetResponse>("dataset");
    same::<evals::ItemsResponse>("run_items");
    same::<evals::CompareResponse>("compare");
    same::<Vec<evals::Alert>>("alerts");
}

#[test]
fn schedules_keep_their_wire_shape() {
    same::<evals::SchedulesResponse>("schedules");
    same::<evals::ScheduleRunsResponse>("schedule_runs");
    same::<evals::ScheduleRunDetail>("schedule_run");
}

#[test]
fn suites_keep_their_wire_shape() {
    same::<Vec<evals::SuiteSummary>>("suites");
    same::<evals::SuiteDetail>("suite");
    same::<evals::DataDriftResponse>("data_drift");
}

#[test]
fn traces_keep_their_wire_shape() {
    same::<traces::TracesResponse>("traces");
    same::<traces::TraceResponse>("trace");
}
