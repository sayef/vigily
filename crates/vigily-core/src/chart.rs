//! Chart data: x axes, downsampling to the pixels a chart has, and aggregation of run groups.
//!
//! Charts are drawn with every run on one shared x array (uPlot's aligned data), so the
//! downsampler aligns as it reduces:
//!
//! - **Exact** when the distinct x values of all series in the range fit the budget: the union of
//!   x values, with gaps where a series has no point.
//! - **Envelope** otherwise: the x range is cut into `buckets` equal buckets with two slots each;
//!   every series puts its minimum and maximum in the bucket into the slots, in the order they
//!   occurred. With buckets about one pixel wide this draws the same picture as the raw data,
//!   spikes and divergence included, which LTTB and averaging do not guarantee.

use crate::model::{Micros, Points};

pub use vigily_api::chart::{Aligned, Band, XAxis};

/// The x value of each point, or `None` when the point has none (an epoch was not logged).
pub fn x_values(p: &Points, axis: XAxis, run_created: Micros) -> Vec<Option<f64>> {
    match axis {
        XAxis::Step => p.steps.iter().map(|s| Some(*s as f64)).collect(),
        XAxis::Epoch => p.epochs.iter().map(|e| e.map(|e| e as f64)).collect(),
        XAxis::Relative => p
            .timestamps
            .iter()
            .map(|t| Some((t - run_created) as f64 / 1e6))
            .collect(),
        XAxis::Wall => p.timestamps.iter().map(|t| Some(*t as f64 / 1e6)).collect(),
    }
}

/// One series as (x, y) pairs sorted by x, finite x only.
#[derive(Clone, Debug, Default)]
pub struct Xy {
    pub x: Vec<f64>,
    pub y: Vec<f64>,
}

impl Xy {
    /// Pairs points with their x values, dropping points without x; sorts by x and keeps the last
    /// point of equal x values (for epochs, the end of the epoch).
    pub fn new(xs: Vec<Option<f64>>, ys: &[f64]) -> Xy {
        let mut pairs: Vec<(f64, f64)> = xs
            .into_iter()
            .zip(ys.iter().copied())
            .filter_map(|(x, y)| x.filter(|x| x.is_finite()).map(|x| (x, y)))
            .collect();
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut out = Xy::default();
        for (x, y) in pairs {
            if out.x.last() == Some(&x) {
                *out.y.last_mut().unwrap() = y;
            } else {
                out.x.push(x);
                out.y.push(y);
            }
        }
        out
    }

    fn bounds(&self) -> Option<(f64, f64)> {
        Some((*self.x.first()?, *self.x.last()?))
    }
}

fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

/// The x range covered by all series, narrowed to `range` when given.
pub fn domain(series: &[Xy], range: Option<(f64, f64)>) -> Option<(f64, f64)> {
    let (lo, hi) =
        series
            .iter()
            .filter_map(Xy::bounds)
            .fold(None::<(f64, f64)>, |acc, (a, b)| match acc {
                None => Some((a, b)),
                Some((x, y)) => Some((x.min(a), y.max(b))),
            })?;
    match range {
        Some((a, b)) if a <= b => {
            let (a, b) = (a.max(lo), b.min(hi));
            (a <= b).then_some((a, b))
        }
        _ => Some((lo, hi)),
    }
}

/// The index range of `xy` inside `[lo, hi]`, widened by one point on each side so lines run to
/// the chart's edges when zoomed.
fn window(xy: &Xy, lo: f64, hi: f64) -> std::ops::Range<usize> {
    let a = xy.x.partition_point(|x| *x < lo);
    let b = xy.x.partition_point(|x| *x <= hi);
    a.saturating_sub(1)..(b + 1).min(xy.x.len())
}

/// Aligns and downsamples `series` to at most about `2 * buckets` x values.
pub fn align(series: &[Xy], range: Option<(f64, f64)>, buckets: usize) -> Aligned {
    let buckets = buckets.max(1);
    let Some((lo, hi)) = domain(series, range) else {
        return Aligned {
            x: vec![],
            ys: vec![vec![]; series.len()],
            exact: true,
        };
    };
    let windows: Vec<_> = series.iter().map(|s| window(s, lo, hi)).collect();
    let total: usize = windows.iter().map(|w| w.len()).sum();

    let mut xs: Vec<f64> = Vec::new();
    let small = total <= buckets * 2 || hi <= lo;
    if small {
        for (s, w) in series.iter().zip(&windows) {
            xs.extend_from_slice(&s.x[w.clone()]);
        }
        xs.sort_by(f64::total_cmp);
        xs.dedup();
    }
    if small && xs.len() <= buckets * 2 {
        let ys = series
            .iter()
            .zip(&windows)
            .map(|(s, w)| {
                let mut col = vec![None; xs.len()];
                let mut j = 0;
                for i in w.clone() {
                    while xs[j] < s.x[i] {
                        j += 1;
                    }
                    col[j] = finite(s.y[i]);
                }
                col
            })
            .collect();
        return Aligned {
            x: xs,
            ys,
            exact: true,
        };
    }

    let width = (hi - lo) / buckets as f64;
    let x: Vec<f64> = (0..buckets)
        .flat_map(|b| {
            let base = lo + b as f64 * width;
            [base + 0.25 * width, base + 0.75 * width]
        })
        .collect();
    let bucket_of = |v: f64| (((v - lo) / width) as usize).min(buckets - 1);
    let ys = series
        .iter()
        .map(|s| {
            let mut col = vec![None; buckets * 2];
            // (min index, min, max index, max) per bucket
            let mut acc: Vec<Option<(usize, f64, usize, f64)>> = vec![None; buckets];
            let a = s.x.partition_point(|v| *v < lo);
            let b = s.x.partition_point(|v| *v <= hi);
            for i in a..b {
                let Some(v) = finite(s.y[i]) else { continue };
                let k = bucket_of(s.x[i]);
                acc[k] = Some(match acc[k] {
                    None => (i, v, i, v),
                    Some((mi, mn, xi, mx)) => {
                        let (mi, mn) = if v < mn { (i, v) } else { (mi, mn) };
                        let (xi, mx) = if v > mx { (i, v) } else { (xi, mx) };
                        (mi, mn, xi, mx)
                    }
                });
            }
            for (k, a) in acc.into_iter().enumerate() {
                let Some((mi, mn, xi, mx)) = a else { continue };
                if mi == xi {
                    col[2 * k] = Some(mn);
                } else if mi < xi {
                    col[2 * k] = Some(mn);
                    col[2 * k + 1] = Some(mx);
                } else {
                    col[2 * k] = Some(mx);
                    col[2 * k + 1] = Some(mn);
                }
            }
            col
        })
        .collect();
    Aligned {
        x,
        ys,
        exact: false,
    }
}

/// Linear interpolation over the empty slots between the first and last filled one.
fn fill_gaps(v: &[Option<f64>], x: &[f64]) -> Vec<Option<f64>> {
    let mut out = v.to_vec();
    let mut prev: Option<usize> = None;
    for k in 0..v.len() {
        if v[k].is_none() {
            continue;
        }
        if let Some(p) = prev {
            if k > p + 1 {
                let (a, b) = (v[p].unwrap(), v[k].unwrap());
                for (j, slot) in out.iter_mut().enumerate().take(k).skip(p + 1) {
                    let t = (x[j] - x[p]) / (x[k] - x[p]);
                    *slot = Some(a + (b - a) * t);
                }
            }
        }
        prev = Some(k);
    }
    out
}

/// Aggregates groups of series. Each series is first reduced to its mean per bucket (or per
/// distinct x when they fit), then the group's mean, minimum and maximum are taken across series.
pub fn aggregate(
    groups: &[Vec<Xy>],
    range: Option<(f64, f64)>,
    buckets: usize,
) -> (Vec<f64>, Vec<Band>) {
    let buckets = buckets.max(1);
    let all: Vec<Xy> = groups.iter().flatten().cloned().collect();
    let Some((lo, hi)) = domain(&all, range) else {
        return (vec![], vec![Band::default(); groups.len()]);
    };
    let mut distinct: Vec<f64> = all
        .iter()
        .flat_map(|s| s.x.iter().copied().filter(|x| *x >= lo && *x <= hi))
        .collect();
    distinct.sort_by(f64::total_cmp);
    distinct.dedup();
    let exact = distinct.len() <= buckets || hi <= lo;
    let (x, slots): (Vec<f64>, usize) = if exact {
        let n = distinct.len();
        (distinct.clone(), n)
    } else {
        let w = (hi - lo) / buckets as f64;
        (
            (0..buckets).map(|b| lo + (b as f64 + 0.5) * w).collect(),
            buckets,
        )
    };
    let slot_of = |v: f64| -> usize {
        if exact {
            distinct.partition_point(|d| *d < v)
        } else {
            (((v - lo) / ((hi - lo) / buckets as f64)) as usize).min(buckets - 1)
        }
    };
    let bands = groups
        .iter()
        .map(|g| {
            let mut sum = vec![0.0; slots];
            let mut mn = vec![f64::INFINITY; slots];
            let mut mx = vec![f64::NEG_INFINITY; slots];
            let mut n = vec![0u32; slots];
            for s in g {
                let mut acc = vec![(0.0f64, 0u32); slots];
                for (xv, yv) in s.x.iter().zip(&s.y) {
                    if *xv < lo || *xv > hi || !yv.is_finite() {
                        continue;
                    }
                    let k = slot_of(*xv);
                    acc[k].0 += yv;
                    acc[k].1 += 1;
                }
                // Slots the series spans but has no point in (it logs less often than the grid,
                // or at other steps than its group) are interpolated from the neighbouring
                // slots, so every series contributes across its whole range and the mean does
                // not jump between subsets of the group.
                let means: Vec<Option<f64>> = acc
                    .iter()
                    .map(|(t, c)| (*c > 0).then(|| t / *c as f64))
                    .collect();
                for (k, m) in fill_gaps(&means, &x).into_iter().enumerate() {
                    let Some(m) = m else { continue };
                    sum[k] += m;
                    mn[k] = mn[k].min(m);
                    mx[k] = mx[k].max(m);
                    n[k] += 1;
                }
            }
            let pick = |k: usize, v: f64| (n[k] > 0).then_some(v);
            Band {
                mean: (0..slots)
                    .map(|k| pick(k, sum[k] / n[k].max(1) as f64))
                    .collect(),
                min: (0..slots).map(|k| pick(k, mn[k])).collect(),
                max: (0..slots).map(|k| pick(k, mx[k])).collect(),
                n,
            }
        })
        .collect();
    (x, bands)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xy(x: &[f64], y: &[f64]) -> Xy {
        Xy::new(x.iter().map(|v| Some(*v)).collect(), y)
    }

    #[test]
    fn exact_alignment_unions_x_values() {
        let a = xy(&[0.0, 1.0, 2.0], &[1.0, 2.0, 3.0]);
        let b = xy(&[1.0, 3.0], &[5.0, f64::NAN]);
        let al = align(&[a, b], None, 100);
        assert!(al.exact);
        assert_eq!(al.x, vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(al.ys[0], vec![Some(1.0), Some(2.0), Some(3.0), None]);
        assert_eq!(al.ys[1], vec![None, Some(5.0), None, None]);
    }

    #[test]
    fn envelope_keeps_spikes_and_order() {
        let n = 100_000;
        let x: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let mut y: Vec<f64> = (0..n).map(|i| (i as f64 * 0.01).sin()).collect();
        y[54_321] = 50.0;
        let al = align(&[xy(&x, &y)], None, 500);
        assert!(!al.exact);
        assert_eq!(al.x.len(), 1000);
        let max = al.ys[0].iter().flatten().fold(f64::MIN, |a, b| a.max(*b));
        assert_eq!(max, 50.0);
        assert!(al.x.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn zoom_range_includes_neighbours() {
        let s = xy(&[0.0, 10.0, 20.0, 30.0], &[0.0, 1.0, 2.0, 3.0]);
        let al = align(&[s], Some((12.0, 18.0)), 100);
        assert_eq!(al.x, vec![10.0, 20.0]);
        assert_eq!(domain(&[xy(&[0.0], &[1.0])], Some((5.0, 6.0))), None);
    }

    #[test]
    fn epochs_keep_the_last_point() {
        let s = Xy::new(
            vec![Some(0.0), Some(0.0), None, Some(1.0)],
            &[1.0, 2.0, 9.0, 3.0],
        );
        assert_eq!(s.x, vec![0.0, 1.0]);
        assert_eq!(s.y, vec![2.0, 3.0]);
    }

    #[test]
    fn aggregate_mean_min_max() {
        let g = vec![
            xy(&[0.0, 1.0], &[1.0, 3.0]),
            xy(&[0.0, 1.0, 2.0], &[3.0, 5.0, 7.0]),
        ];
        let (x, bands) = aggregate(&[g], None, 100);
        assert_eq!(x, vec![0.0, 1.0, 2.0]);
        let b = &bands[0];
        assert_eq!(b.mean, vec![Some(2.0), Some(4.0), Some(7.0)]);
        assert_eq!(b.min, vec![Some(1.0), Some(3.0), Some(7.0)]);
        assert_eq!(b.max, vec![Some(3.0), Some(5.0), Some(7.0)]);
        assert_eq!(b.n, vec![2, 2, 1]);
        let many: Vec<Xy> = (0..3)
            .map(|k| {
                let x: Vec<f64> = (0..10_000).map(|i| i as f64).collect();
                let y = vec![k as f64; 10_000];
                xy(&x, &y)
            })
            .collect();
        let (x, bands) = aggregate(&[many], None, 200);
        assert_eq!(x.len(), 200);
        assert!(bands[0].mean.iter().all(|m| *m == Some(1.0)));
    }

    #[test]
    fn aggregate_interpolates_series_logged_at_other_steps() {
        // Two runs of one group log at interleaved steps; the mean must not saw-tooth.
        let a: Vec<f64> = (0..50).map(|i| (i * 20) as f64).collect();
        let b: Vec<f64> = (0..50).map(|i| (i * 20 + 10) as f64).collect();
        let g = vec![
            xy(&a, &a.iter().map(|x| 1000.0 - x).collect::<Vec<_>>()),
            xy(&b, &b.iter().map(|x| 1000.0 - x).collect::<Vec<_>>()),
        ];
        let (x, bands) = aggregate(&[g], None, 40);
        let m = &bands[0].mean;
        for (i, v) in m.iter().enumerate() {
            let v = v.unwrap();
            assert!(
                (v - (1000.0 - x[i])).abs() < 15.0,
                "slot {i}: {v} at x {}",
                x[i]
            );
        }
        assert!(bands[0].n.iter().skip(1).take(37).all(|n| *n == 2));
    }
}
