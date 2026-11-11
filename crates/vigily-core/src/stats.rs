//! Statistical tests for drift detection, implemented here so every number in the UI can be traced
//! to a formula in one file.
//!
//! | Function | Question it answers |
//! |---|---|
//! | [`wilcoxon_signed_rank`] | Did matched items change? (paired rank test on per-item differences) |
//! | [`bootstrap_mean`] | A 95 % interval for the mean of per-item differences, resampling items |
//! | [`mann_whitney`] | Do two samples of scores come from the same distribution? (rank test, no normality assumption) |
//! | [`bootstrap_mean_diff`] | How big is the difference of means, with a 95 % interval? |
//! | [`psi`] | How far has a numeric distribution moved? (population stability index) |
//! | [`ks`] | Largest gap between two empirical distributions, with a p-value (Kolmogorov–Smirnov) |
//! | [`chi_squared`] | Do category counts differ? (chi-squared test of homogeneity) |
//! | [`js_divergence`] | How far apart are two category distributions? (Jensen–Shannon, base 2, 0 to 1) |

use std::collections::BTreeMap;

/// The complementary error function, |relative error| < 1.2e-7 (Numerical Recipes `erfcc`).
pub fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let ans = t
        * (-z * z - 1.265_512_23
            + t * (1.000_023_68
                + t * (0.374_091_96
                    + t * (0.096_784_18
                        + t * (-0.186_288_06
                            + t * (0.278_868_07
                                + t * (-1.135_203_98
                                    + t * (1.488_515_87
                                        + t * (-0.822_152_23 + t * 0.170_872_77)))))))))
            .exp();
    if x >= 0.0 {
        ans
    } else {
        2.0 - ans
    }
}

/// The standard normal cumulative distribution function.
pub fn normal_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / std::f64::consts::SQRT_2)
}

pub fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        f64::NAN
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

pub fn std_dev(v: &[f64]) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let m = mean(v);
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
}

/// Ranks with ties averaged (1-based).
fn ranks(values: &[f64]) -> (Vec<f64>, f64) {
    let mut idx: Vec<usize> = (0..values.len()).collect();
    idx.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
    let mut out = vec![0.0; values.len()];
    let mut tie_term = 0.0;
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && values[idx[j + 1]] == values[idx[i]] {
            j += 1;
        }
        let r = (i + j) as f64 / 2.0 + 1.0;
        for k in &idx[i..=j] {
            out[*k] = r;
        }
        let t = (j - i + 1) as f64;
        tie_term += t * t * t - t;
        i = j + 1;
    }
    (out, tie_term)
}

/// The result of a Mann–Whitney U test.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct MannWhitney {
    /// U of the first sample.
    pub u: f64,
    pub z: f64,
    /// Two-sided p-value (normal approximation with tie and continuity correction).
    pub p: f64,
}

/// Mann–Whitney U test of `a` against `b`.
pub fn mann_whitney(a: &[f64], b: &[f64]) -> Option<MannWhitney> {
    let (n1, n2) = (a.len() as f64, b.len() as f64);
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let all: Vec<f64> = a.iter().chain(b).copied().collect();
    let (r, tie_term) = ranks(&all);
    let r1: f64 = r[..a.len()].iter().sum();
    let u = r1 - n1 * (n1 + 1.0) / 2.0;
    let mu = n1 * n2 / 2.0;
    let n = n1 + n2;
    let var = n1 * n2 / 12.0 * ((n + 1.0) - tie_term / (n * (n - 1.0)));
    if var <= 0.0 {
        return Some(MannWhitney { u, z: 0.0, p: 1.0 });
    }
    let diff = u - mu;
    let z = (diff.abs() - 0.5).max(0.0) / var.sqrt() * diff.signum() + 0.0;
    let p = (2.0 * (1.0 - normal_cdf(z.abs()))).clamp(0.0, 1.0);
    Some(MannWhitney { u: u + 0.0, z, p })
}

/// The result of a Wilcoxon signed-rank test.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct SignedRank {
    /// The smaller of the positive and negative rank sums (as scipy reports it).
    pub statistic: f64,
    /// Positive when the differences tend to be positive.
    pub z: f64,
    /// Two-sided p-value (normal approximation with tie and continuity correction).
    pub p: f64,
    /// Non-zero differences used.
    pub n: usize,
}

/// Wilcoxon signed-rank test of paired differences `d` against zero. Zero differences are dropped
/// (the "wilcox" method); ties get average ranks and the variance is corrected for them.
pub fn wilcoxon_signed_rank(d: &[f64]) -> Option<SignedRank> {
    let nz: Vec<f64> = d
        .iter()
        .copied()
        .filter(|x| x.is_finite() && *x != 0.0)
        .collect();
    let n = nz.len();
    if n == 0 {
        return None;
    }
    let abs: Vec<f64> = nz.iter().map(|x| x.abs()).collect();
    let (r, tie_term) = ranks(&abs);
    let w_plus: f64 = nz
        .iter()
        .zip(&r)
        .filter(|(x, _)| **x > 0.0)
        .map(|(_, r)| r)
        .sum();
    let nf = n as f64;
    let total = nf * (nf + 1.0) / 2.0;
    let mean = total / 2.0;
    let var = nf * (nf + 1.0) * (2.0 * nf + 1.0) / 24.0 - tie_term / 48.0;
    // `+ 0.0` turns a negative zero into zero, so it never serialises as -0.0.
    let statistic = w_plus.min(total - w_plus) + 0.0;
    if var <= 0.0 {
        return Some(SignedRank {
            statistic,
            z: 0.0,
            p: 1.0,
            n,
        });
    }
    let diff = w_plus - mean;
    let z = (diff.abs() - 0.5).max(0.0) / var.sqrt() * diff.signum() + 0.0;
    let p = (2.0 * (1.0 - normal_cdf(z.abs()))).clamp(0.0, 1.0);
    Some(SignedRank { statistic, z, p, n })
}

/// A small deterministic random generator (xorshift64*), so bootstrap intervals are reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// The 95 % percentile bootstrap interval of `mean(a) - mean(b)`.
pub fn bootstrap_mean_diff(
    a: &[f64],
    b: &[f64],
    iterations: usize,
    seed: u64,
) -> Option<(f64, f64)> {
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let mut rng = Rng(seed | 1);
    let mut diffs: Vec<f64> = (0..iterations.max(100))
        .map(|_| {
            let ma = (0..a.len()).map(|_| a[rng.below(a.len())]).sum::<f64>() / a.len() as f64;
            let mb = (0..b.len()).map(|_| b[rng.below(b.len())]).sum::<f64>() / b.len() as f64;
            ma - mb
        })
        .collect();
    diffs.sort_by(f64::total_cmp);
    let q = |p: f64| diffs[((diffs.len() - 1) as f64 * p).round() as usize];
    Some((q(0.025), q(0.975)))
}

/// The 95 % percentile bootstrap interval of the mean of `d`, resampling its elements (items).
pub fn bootstrap_mean(d: &[f64], iterations: usize, seed: u64) -> Option<(f64, f64)> {
    if d.is_empty() {
        return None;
    }
    let mut rng = Rng(seed | 1);
    let mut means: Vec<f64> = (0..iterations.max(100))
        .map(|_| (0..d.len()).map(|_| d[rng.below(d.len())]).sum::<f64>() / d.len() as f64)
        .collect();
    means.sort_by(f64::total_cmp);
    let q = |p: f64| means[((means.len() - 1) as f64 * p).round() as usize];
    Some((q(0.025), q(0.975)))
}

/// Bin edges at the baseline's deciles (fewer when values repeat).
pub fn quantile_edges(baseline: &[f64], bins: usize) -> Vec<f64> {
    let mut v: Vec<f64> = baseline.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return vec![];
    }
    v.sort_by(f64::total_cmp);
    let mut edges: Vec<f64> = (1..bins).map(|i| v[(v.len() - 1) * i / bins]).collect();
    edges.dedup();
    edges
}

/// The share of `values` in each bin defined by inner `edges` (`edges.len() + 1` bins).
pub fn bin_shares(values: &[f64], edges: &[f64]) -> Vec<f64> {
    let mut counts = vec![0.0; edges.len() + 1];
    let mut n = 0.0;
    for v in values.iter().filter(|x| x.is_finite()) {
        counts[edges.partition_point(|e| e < v)] += 1.0;
        n += 1.0;
    }
    if n > 0.0 {
        for c in &mut counts {
            *c /= n;
        }
    }
    counts
}

/// Population stability index of `current` against `baseline`, on baseline deciles.
/// Rule of thumb: below 0.1 stable, 0.1 to 0.2 moderate shift, above 0.2 significant shift.
pub fn psi(baseline: &[f64], current: &[f64]) -> Option<f64> {
    if baseline.is_empty() || current.is_empty() {
        return None;
    }
    let edges = quantile_edges(baseline, 10);
    let (b, c) = (bin_shares(baseline, &edges), bin_shares(current, &edges));
    Some(psi_of_shares(&b, &c))
}

/// PSI of two share vectors over the same bins; empty bins count as 0.0001.
pub fn psi_of_shares(b: &[f64], c: &[f64]) -> f64 {
    b.iter()
        .zip(c)
        .map(|(b, c)| {
            let (b, c) = (b.max(1e-4), c.max(1e-4));
            (c - b) * (c / b).ln()
        })
        .sum()
}

/// The result of a two-sample Kolmogorov–Smirnov test.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Ks {
    /// The largest gap between the two empirical distribution functions.
    pub d: f64,
    /// Asymptotic p-value.
    pub p: f64,
}

/// The Kolmogorov distribution's survival function Q(λ) (Numerical Recipes `probks`).
fn probks(lambda: f64) -> f64 {
    let a2 = -2.0 * lambda * lambda;
    let mut fac = 2.0;
    let mut sum = 0.0;
    let mut termbf = 0.0;
    for j in 1..=100 {
        let term = fac * (a2 * (j * j) as f64).exp();
        sum += term;
        if term.abs() <= 0.001 * termbf || term.abs() <= 1e-8 * sum {
            return sum.clamp(0.0, 1.0);
        }
        fac = -fac;
        termbf = term.abs();
    }
    1.0
}

/// Two-sample Kolmogorov–Smirnov test.
pub fn ks(a: &[f64], b: &[f64]) -> Option<Ks> {
    let mut a: Vec<f64> = a.iter().copied().filter(|x| x.is_finite()).collect();
    let mut b: Vec<f64> = b.iter().copied().filter(|x| x.is_finite()).collect();
    if a.is_empty() || b.is_empty() {
        return None;
    }
    a.sort_by(f64::total_cmp);
    b.sort_by(f64::total_cmp);
    let (n1, n2) = (a.len() as f64, b.len() as f64);
    let (mut i, mut j, mut d) = (0usize, 0usize, 0.0f64);
    while i < a.len() && j < b.len() {
        let x = a[i].min(b[j]);
        while i < a.len() && a[i] <= x {
            i += 1;
        }
        while j < b.len() && b[j] <= x {
            j += 1;
        }
        d = d.max((i as f64 / n1 - j as f64 / n2).abs());
    }
    let ne = (n1 * n2 / (n1 + n2)).sqrt();
    let p = probks((ne + 0.12 + 0.11 / ne) * d);
    Some(Ks { d, p })
}

/// ln Γ(x) (Lanczos, Numerical Recipes `gammln`).
fn ln_gamma(x: f64) -> f64 {
    const C: [f64; 6] = [
        76.180_091_729_471_46,
        -86.505_320_329_416_77,
        24.014_098_240_830_91,
        -1.231_739_572_450_155,
        0.120_865_097_386_617_9e-2,
        -0.539_523_938_495_3e-5,
    ];
    let mut y = x;
    let tmp = x + 5.5;
    let tmp = tmp - (x + 0.5) * tmp.ln();
    let mut ser = 1.000_000_000_190_015;
    for c in C {
        y += 1.0;
        ser += c / y;
    }
    -tmp + (2.506_628_274_631_000_5 * ser / x).ln()
}

/// The regularised upper incomplete gamma function Q(a, x).
pub fn gamma_q(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 1.0;
    }
    if x < a + 1.0 {
        // Series for P, then Q = 1 - P.
        let (mut ap, mut sum) = (a, 1.0 / a);
        let mut del = sum;
        for _ in 0..500 {
            ap += 1.0;
            del *= x / ap;
            sum += del;
            if del.abs() < sum.abs() * 3e-14 {
                break;
            }
        }
        1.0 - sum * (-x + a * x.ln() - ln_gamma(a)).exp()
    } else {
        // Continued fraction for Q (modified Lentz).
        let tiny = 1e-300;
        let mut b = x + 1.0 - a;
        let mut c = 1.0 / tiny;
        let mut d = 1.0 / b;
        let mut h = d;
        for i in 1..500 {
            let an = -(i as f64) * (i as f64 - a);
            b += 2.0;
            d = an * d + b;
            if d.abs() < tiny {
                d = tiny;
            }
            c = b + an / c;
            if c.abs() < tiny {
                c = tiny;
            }
            d = 1.0 / d;
            let del = d * c;
            h *= del;
            if (del - 1.0).abs() < 3e-14 {
                break;
            }
        }
        (-x + a * x.ln() - ln_gamma(a)).exp() * h
    }
}

/// The result of a chi-squared test.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct ChiSquared {
    pub statistic: f64,
    pub df: f64,
    pub p: f64,
}

/// Chi-squared test of homogeneity of two category count tables.
pub fn chi_squared(a: &BTreeMap<String, f64>, b: &BTreeMap<String, f64>) -> Option<ChiSquared> {
    let cats: Vec<&String> = a
        .keys()
        .chain(b.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let (na, nb): (f64, f64) = (a.values().sum(), b.values().sum());
    if cats.len() < 2 || na == 0.0 || nb == 0.0 {
        return None;
    }
    let n = na + nb;
    let mut stat = 0.0;
    for c in &cats {
        let (oa, ob) = (*a.get(*c).unwrap_or(&0.0), *b.get(*c).unwrap_or(&0.0));
        let total = oa + ob;
        let (ea, eb) = (total * na / n, total * nb / n);
        if ea > 0.0 {
            stat += (oa - ea).powi(2) / ea;
        }
        if eb > 0.0 {
            stat += (ob - eb).powi(2) / eb;
        }
    }
    let df = (cats.len() - 1) as f64;
    Some(ChiSquared {
        statistic: stat,
        df,
        p: gamma_q(df / 2.0, stat / 2.0).clamp(0.0, 1.0),
    })
}

/// Jensen–Shannon divergence (base 2, from 0 for identical to 1 for disjoint) of two count tables.
pub fn js_divergence(a: &BTreeMap<String, f64>, b: &BTreeMap<String, f64>) -> Option<f64> {
    let (na, nb): (f64, f64) = (a.values().sum(), b.values().sum());
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    let cats: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let kl = |p: f64, m: f64| if p > 0.0 { p * (p / m).log2() } else { 0.0 };
    let mut js = 0.0;
    for c in cats {
        let p = a.get(c).unwrap_or(&0.0) / na;
        let q = b.get(c).unwrap_or(&0.0) / nb;
        let m = (p + q) / 2.0;
        js += 0.5 * kl(p, m) + 0.5 * kl(q, m);
    }
    Some(js.clamp(0.0, 1.0))
}

/// Category counts of a sample.
pub fn counts(values: &[String]) -> BTreeMap<String, f64> {
    let mut m = BTreeMap::new();
    for v in values {
        *m.entry(v.clone()).or_insert(0.0) += 1.0;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn normal_and_gamma_reference_values() {
        assert!(close(normal_cdf(1.96), 0.975, 1e-4));
        assert!(close(normal_cdf(0.0), 0.5, 1e-7));
        // chi-squared with 1 degree of freedom at 3.841 has p = 0.05; with 5 at 11.07, 0.05.
        assert!(close(gamma_q(0.5, 3.841 / 2.0), 0.05, 1e-3));
        assert!(close(gamma_q(2.5, 11.07 / 2.0), 0.05, 1e-3));
        assert!(close(gamma_q(1.0, 2.0), (-2.0f64).exp(), 1e-9));
    }

    #[test]
    fn mann_whitney_matches_a_textbook_example() {
        // Two groups with U = 3 for the first (classic small example); z with continuity correction.
        let a = [1.1, 2.2, 3.3, 4.4];
        let b = [3.0, 5.5, 6.6, 7.7, 8.8];
        let r = mann_whitney(&a, &b).unwrap();
        assert_eq!(r.u, 2.0);
        assert!(r.z < 0.0);
        assert!(r.p > 0.03 && r.p < 0.1, "{}", r.p);
        let same = mann_whitney(&[1.0, 1.0, 1.0], &[1.0, 1.0]).unwrap();
        assert_eq!(same.p, 1.0);
        // Pass rates 0.9 against 0.6 on 200 items each: clearly different.
        let x: Vec<f64> = (0..200)
            .map(|i| if i % 10 == 0 { 0.0 } else { 1.0 })
            .collect();
        let y: Vec<f64> = (0..200)
            .map(|i| if i % 10 < 4 { 0.0 } else { 1.0 })
            .collect();
        assert!(mann_whitney(&x, &y).unwrap().p < 1e-6);
    }

    #[test]
    fn wilcoxon_matches_scipy() {
        // scipy.stats.wilcoxon(d, zero_method="wilcox", correction=True, method="approx")
        let small = [
            1.83, 0.50, 1.62, 2.48, 1.68, 1.88, 1.55, 3.06, 1.30, -0.3, 0.0, 0.5,
        ];
        let r = wilcoxon_signed_rank(&small).unwrap();
        assert_eq!(r.statistic, 1.0);
        assert!(close(r.p, 0.005077485059608319, 1e-6), "{}", r.p);
        let ties = [
            0.5, -0.5, 1.0, 1.0, -1.0, 0.5, 0.5, 2.0, 0.0, 0.0, 1.5, -0.25, 0.75, 1.0,
        ];
        let r = wilcoxon_signed_rank(&ties).unwrap();
        assert_eq!(r.statistic, 13.0);
        assert!(close(r.p, 0.04380499441476658, 1e-6), "{}", r.p);
        let mut binary = vec![-1.0 / 7.0; 30];
        binary.extend([6.0 / 7.0; 5]);
        binary.extend([-6.0 / 7.0; 40]);
        binary.extend([0.0; 100]);
        let r = wilcoxon_signed_rank(&binary).unwrap();
        assert_eq!((r.statistic, r.n), (265.0, 75));
        assert!(close(r.p, 2.2357704456882971e-10, 1e-14), "{}", r.p);
        assert!(r.z < 0.0);
        assert!(wilcoxon_signed_rank(&[0.0, 0.0]).is_none());
        let balanced = wilcoxon_signed_rank(&[1.0, -1.0]).unwrap();
        assert!(balanced.z.is_sign_positive() && balanced.statistic.is_sign_positive());
        let (lo, hi) = bootstrap_mean(&[1.0, 2.0, 3.0, 4.0], 1000, 3).unwrap();
        assert!(lo >= 1.0 && hi <= 4.0 && lo < 2.5 && hi > 2.5);
    }

    #[test]
    fn bootstrap_interval_covers_the_difference() {
        let a: Vec<f64> = (0..100).map(|i| (i % 10) as f64).collect();
        let b: Vec<f64> = (0..100).map(|i| (i % 10) as f64 + 1.0).collect();
        let (lo, hi) = bootstrap_mean_diff(&a, &b, 1000, 7).unwrap();
        assert!(lo < -1.0 + 0.6 && hi > -1.0 - 0.6 && lo < hi);
        assert_eq!(
            bootstrap_mean_diff(&a, &b, 1000, 7),
            Some((lo, hi)),
            "deterministic"
        );
    }

    #[test]
    fn psi_and_ks_detect_shifts() {
        let base: Vec<f64> = (0..1000).map(|i| (i % 100) as f64).collect();
        let same = base.clone();
        let shifted: Vec<f64> = base.iter().map(|x| x + 30.0).collect();
        assert!(psi(&base, &same).unwrap() < 0.01);
        assert!(psi(&base, &shifted).unwrap() > 0.2);
        let k = ks(&base, &same).unwrap();
        assert_eq!(k.d, 0.0);
        assert!(k.p > 0.99);
        let k = ks(&base, &shifted).unwrap();
        assert!((k.d - 0.3).abs() < 0.01 && k.p < 1e-10);
    }

    #[test]
    fn categorical_tests() {
        let rep = |v: [&str; 5]| -> Vec<String> {
            v.iter().cycle().take(200).map(|s| s.to_string()).collect()
        };
        let a = counts(&rep(["en", "en", "de", "en", "fr"]));
        let b = counts(&rep(["en", "de", "de", "de", "fr"]));
        let c = chi_squared(&a, &b).unwrap();
        assert_eq!(c.df, 2.0);
        assert!(c.p < 1e-6);
        assert!(chi_squared(&a, &a).unwrap().p > 0.99);
        assert_eq!(js_divergence(&a, &a), Some(0.0));
        let only_x = counts(&["x".to_string()]);
        let only_y = counts(&["y".to_string()]);
        assert!(close(js_divergence(&only_x, &only_y).unwrap(), 1.0, 1e-12));
        assert!(js_divergence(&a, &b).unwrap() > 0.05);
    }
}
