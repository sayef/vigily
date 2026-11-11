# Drift methods

vigily answers two questions about each new eval run in a suite: did the target get worse on the
same items (**behaviour drift**), and do the inputs look different from before (**data drift**)?
Every number on the Drift page comes from one function in
[`stats.rs`](https://github.com/sayef/vigily/blob/main/crates/vigily-core/src/stats.rs), tested
against reference values.

## Suites and baselines

A **suite** is the eval runs that share an experiment, a target reference and a dataset name. Each run
is compared with a **baseline**:

- **Rolling (default):** the last 7 runs before it that did not drift themselves. Excluding drifted
  runs matters: otherwise a lasting regression becomes the baseline within a week and stops being
  reported. With the exclusion, it is reported until someone fixes the target or pins a new baseline.
- **Pinned:** runs you choose on the Drift page, for example the evaluation you shipped with.

## Behaviour drift

For each score (for example `exact_match`), vigily pairs items: an item in the run matches an item in
the baseline when both its id and its content hash match, so an edited item is never compared with its
old self. Each matched item's score is paired with the same item's mean over the baseline runs.

| Step | Method |
|---|---|
| Test | Wilcoxon signed-rank on the per-item differences, two-sided, zeros dropped, with tie and continuity corrections |
| Effect | Mean per-item difference with a 95 % bootstrap interval that resamples items |
| Fallback | Fewer than 10 matched items (the dataset changed): Mann–Whitney U of the run's scores against the baseline's per-item means |
| Verdict | Drift when the run is worse, the drop is at least `min_effect` (0.02) and `p < alpha` (0.01) |
| Evidence | The items that passed in the last baseline run and fail now, with their outputs |

Pairing is why vigily needs fewer items than a two-sample test to see a real drop: item difficulty
cancels out. Pooling repeated baseline runs as independent samples would overstate the baseline's
precision, so vigily does not.

## Data drift

`evaluate(features=...)` samples up to 5 000 values per feature and run. Each feature is compared with
the baseline runs' pooled samples:

| Feature | Measure | Stable | Moderate | Drift | Reference test |
|---|---|---|---|---|---|
| Numeric | PSI on baseline deciles | < 0.1 | 0.1–0.2 | ≥ 0.2 | Kolmogorov–Smirnov (asymptotic) |
| Categorical | Jensen–Shannon divergence, base 2 | < 0.05 | 0.05–0.1 | ≥ 0.1 | Chi-squared |

With thousands of samples every test is significant, so the effect sizes decide and the p-values are
shown for reference. Non-finite values are left out.

## Reading a verdict

The Drift page shows each suite's score per run against the baseline band, the verdict for the
selected run in a sentence ("exact_match fell by 0.07 on 412 matched items, p = 0.0003"), the
regressed items, and the feature table. A verdict is evidence for a person to review, not an
automatic decision about a model.

Tune `alpha` and `min_effect` in [`vigily.toml`](configuration.md). Lowering `min_effect` finds
smaller drops and raises more alerts on noise.
