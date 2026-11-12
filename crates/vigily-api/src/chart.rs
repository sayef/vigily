//! Charts: the x axis, series aligned on a shared x array, group bands, and the chart request and
//! answer.

use serde::{Deserialize, Serialize};

/// The quantity on a chart's x axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum XAxis {
    #[default]
    Step,
    Epoch,
    /// Seconds since the run was created.
    Relative,
    /// Seconds since the Unix epoch.
    Wall,
}

/// Series on one shared x array; `None` is a gap.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Aligned {
    pub x: Vec<f64>,
    pub ys: Vec<Vec<Option<f64>>>,
    /// True when no point was dropped.
    pub exact: bool,
}

/// The mean, minimum and maximum of a group of series on a shared x array, with the number of
/// series that contribute at each x.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Band {
    pub mean: Vec<Option<f64>>,
    pub min: Vec<Option<f64>>,
    pub max: Vec<Option<f64>>,
    pub n: Vec<u32>,
}

fn default_points() -> usize {
    800
}

/// `POST chart`: one metric for many runs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChartRequest {
    pub runs: Vec<String>,
    /// A metric key: `loss` or `loss[val]`.
    pub metric: String,
    #[serde(default)]
    pub x: XAxis,
    /// Zoom: the visible x range.
    #[serde(default)]
    pub range: Option<[f64; 2]>,
    /// The chart's width in pixels; the response has at most about twice as many x values.
    #[serde(default = "default_points")]
    pub points: usize,
    /// A column id; runs with the same value form a group.
    #[serde(default)]
    pub group_by: Option<String>,
    /// Draw each group as a mean with a min-max band instead of one line per run.
    #[serde(default)]
    pub aggregate: bool,
}

/// One run's line on a chart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartSeries {
    pub run_id: String,
    pub name: String,
    pub group: Option<String>,
    pub y: Vec<Option<f64>>,
    /// The last finite point in the visible range, `[x, y]`: the downsampled line may end on a
    /// bucket's minimum or maximum rather than on it.
    pub last: Option<[f64; 2]>,
}

/// One group's band on an aggregated chart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartGroup {
    pub label: String,
    pub runs: usize,
    #[serde(flatten)]
    pub band: Band,
}

/// The answer of `POST chart`: the series on one shared x array, or group bands.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartResponse {
    pub metric: String,
    pub x_axis: XAxis,
    pub x: Vec<f64>,
    pub exact: bool,
    /// The full x range of the data, to reset a zoom.
    pub domain: Option<[f64; 2]>,
    pub series: Vec<ChartSeries>,
    pub groups: Vec<ChartGroup>,
    /// Requested runs that do not log this metric.
    pub missing: Vec<String>,
}
