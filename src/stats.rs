//! Descriptive statistics for benchmark samples.
//!
//! Provides mean, median, percentiles with linear interpolation, sample
//! variance, a 95% confidence interval for the mean via the Student t
//! distribution, and 3-sigma outlier detection.

/// A collection of `f64` observations with descriptive-statistics methods.
#[derive(Debug, Clone, Default)]
pub struct Sample {
    values: Vec<f64>,
}

impl Sample {
    /// Creates an empty sample.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a sample from an existing vector.
    pub fn from_values(values: Vec<f64>) -> Self {
        Sample { values }
    }

    /// Appends an observation.
    pub fn push(&mut self, v: f64) {
        self.values.push(v);
    }

    /// Returns the number of observations.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns `true` when no observations have been recorded.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns a slice of the raw observations.
    pub fn values(&self) -> &[f64] {
        &self.values
    }

    /// Arithmetic mean. Returns `0.0` for an empty sample.
    #[must_use]
    pub fn mean(&self) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }
        self.values.iter().sum::<f64>() / self.values.len() as f64
    }

    /// Minimum observation, or `None` if empty.
    pub fn min(&self) -> Option<f64> {
        self.values.iter().copied().fold(None, |acc, v| match acc {
            None => Some(v),
            Some(m) => Some(m.min(v)),
        })
    }

    /// Maximum observation, or `None` if empty.
    pub fn max(&self) -> Option<f64> {
        self.values.iter().copied().fold(None, |acc, v| match acc {
            None => Some(v),
            Some(m) => Some(m.max(v)),
        })
    }

    /// Median (50th percentile). Returns `0.0` for an empty sample.
    #[must_use]
    pub fn median(&self) -> f64 {
        self.percentile(50.0)
    }

    /// Unbiased sample variance (denominator `n - 1`). Returns `0.0` for fewer than two observations.
    #[must_use]
    pub fn variance(&self) -> f64 {
        if self.values.len() < 2 {
            return 0.0;
        }
        let m = self.mean();
        let ss: f64 = self.values.iter().map(|v| (v - m) * (v - m)).sum();
        ss / (self.values.len() - 1) as f64
    }

    /// Sample standard deviation. Returns `0.0` for fewer than two observations.
    #[must_use]
    pub fn stddev(&self) -> f64 {
        self.variance().sqrt()
    }

    /// Percentile with linear interpolation between closest ranks.
    ///
    /// The `p` argument is a percentage in `0.0..=100.0`. For an empty sample
    /// this returns `0.0`.
    ///
    /// ```
    /// use llmrace::stats::Sample;
    /// let s = Sample::from_values(vec![10.0, 20.0, 30.0, 40.0, 100.0]);
    /// assert!((s.percentile(50.0) - 30.0).abs() < 1e-12);
    /// ```
    #[must_use]
    pub fn percentile(&self, p: f64) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }
        if self.values.len() == 1 {
            return self.values[0];
        }
        let sorted = self.sorted();
        interpolate(&sorted, p.clamp(0.0, 100.0))
    }

    /// Several percentiles from one sort — cheaper than repeated
    /// [`Sample::percentile`] calls, which sort per invocation.
    ///
    /// ```
    /// use llmrace::stats::Sample;
    ///
    /// let s = Sample::from_values(vec![10.0, 20.0, 30.0, 40.0, 100.0]);
    /// let got = s.percentiles(&[50.0, 95.0]);
    /// assert!((got[0] - 30.0).abs() < 1e-12);
    /// assert!(got[1] > 80.0);
    /// ```
    #[must_use]
    pub fn percentiles(&self, ps: &[f64]) -> Vec<f64> {
        if self.values.is_empty() {
            return vec![0.0; ps.len()];
        }
        if self.values.len() == 1 {
            return vec![self.values[0]; ps.len()];
        }
        let sorted = self.sorted();
        ps.iter()
            .map(|p| interpolate(&sorted, p.clamp(0.0, 100.0)))
            .collect()
    }

    fn sorted(&self) -> Vec<f64> {
        let mut sorted = self.values.clone();
        sorted.sort_by(f64::total_cmp);
        sorted
    }

    /// 95% confidence interval for the mean, `(low, high)`.
    ///
    /// Uses the Student t distribution for every sample size. Returns
    /// `None` for fewer than two observations.
    #[must_use]
    pub fn t_ci95(&self) -> Option<(f64, f64)> {
        let n = self.values.len();
        if n < 2 {
            return None;
        }
        let df = n - 1;
        let t = t_critical_95(df);
        let m = self.mean();
        let half = t * self.stddev() / (n as f64).sqrt();
        Some((m - half, m + half))
    }

    /// Indices of observations more than three standard deviations from the mean.
    #[must_use]
    pub fn outliers_3sigma(&self) -> Vec<usize> {
        if self.values.len() < 2 {
            return Vec::new();
        }
        let m = self.mean();
        let sd = self.stddev();
        if sd == 0.0 {
            return Vec::new();
        }
        self.values
            .iter()
            .enumerate()
            .filter(|(_, v)| ((**v - m).abs() / sd) > 3.0)
            .map(|(i, _)| i)
            .collect()
    }
}

/// Linear interpolation between closest ranks on a pre-sorted slice.
fn interpolate(sorted: &[f64], p: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let idx = p / 100.0 * (sorted.len() - 1) as f64;
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = idx - lo as f64;
        sorted[lo] + frac * (sorted[hi] - sorted[lo])
    }
}

/// Formats a millisecond value for display, collapsing sub-millisecond
/// values to `<1` so local-inference TTFB does not render as a broken `0`.
#[must_use]
pub fn fmt_ms(v: f64) -> String {
    if v > 0.0 && v < 1.0 {
        "<1".to_string()
    } else {
        format!("{v:.0}")
    }
}

fn t_critical_95(df: usize) -> f64 {
    let (mut low, mut high) = (0.0, 16.0);
    for _ in 0..48 {
        let middle = (low + high) / 2.0;
        if t_two_sided_p(middle, df as f64) > 0.05 {
            low = middle;
        } else {
            high = middle;
        }
    }
    (low + high) / 2.0
}

/// Welch's t statistic and degrees of freedom for two independent samples
/// with unequal variances.
///
/// Returns `(t, df)`; `None` when either sample has fewer than two
/// observations or a zero variance sum.
#[must_use]
pub fn welch_t(sample_a: &[f64], sample_b: &[f64]) -> Option<(f64, f64)> {
    if sample_a.len() < 2
        || sample_b.len() < 2
        || sample_a.iter().any(|v| !v.is_finite())
        || sample_b.iter().any(|v| !v.is_finite())
    {
        return None;
    }
    let a = Sample::from_values(sample_a.to_vec());
    let b = Sample::from_values(sample_b.to_vec());
    let (va, vb) = (a.variance(), b.variance());
    let (na, nb) = (a.len() as f64, b.len() as f64);
    let sea = va / na;
    let seb = vb / nb;
    let denom = sea + seb;
    if !denom.is_finite() || denom <= 0.0 {
        return None;
    }
    let t = (a.mean() - b.mean()) / denom.sqrt();
    let df_denom = sea * sea / (na - 1.0) + seb * seb / (nb - 1.0);
    if !df_denom.is_finite() || df_denom <= 0.0 {
        return None;
    }
    let df = denom * denom / df_denom;
    if !t.is_finite() || !df.is_finite() || df <= 0.0 {
        return None;
    }
    Some((t, df))
}

/// Two-sided p-value of Student's t under the null hypothesis, via the
/// regularized incomplete beta function `I_x(df/2, 1/2)`.
///
/// ```
/// use llmrace::stats::t_two_sided_p;
///
/// // textbook values: t=2.776 with df=4 → p ≈ 0.05
/// let p = t_two_sided_p(2.776, 4.0);
/// assert!((p - 0.05).abs() < 0.001, "{p}");
/// // t=12.706 with df=1 → p ≈ 0.05
/// let p1 = t_two_sided_p(12.706, 1.0);
/// assert!((p1 - 0.05).abs() < 0.001, "{p1}");
/// ```
#[must_use]
pub fn t_two_sided_p(t: f64, df: f64) -> f64 {
    if !t.is_finite() || !df.is_finite() || df <= 0.0 {
        return 1.0;
    }
    let x = df / (df + t * t);
    if !x.is_finite() {
        return 0.0;
    }
    let p = betai(df / 2.0, 0.5, x);
    if !p.is_finite() {
        return 1.0;
    }
    p.clamp(0.0, 1.0)
}

/// Regularized incomplete beta function `I_x(a, b)` via the continued
/// fraction expansion (Lentz's method) — the standard Numerical Recipes
/// formulation, accurate to roughly 1e-10 for the ranges used here.
fn betai(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let lbeta = ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln();
    let front = lbeta.exp();
    // the continued fraction converges quickly for x < (a+1)/(a+b+2);
    // use the symmetry I_x(a,b) = 1 - I_{1-x}(b,a) otherwise
    if x < (a + 1.0) / (a + b + 2.0) {
        front * beta_cf(a, b, x) / a
    } else {
        1.0 - front * beta_cf(b, a, 1.0 - x) / b
    }
}

/// Incomplete beta continued fraction (modified Lentz).
fn beta_cf(a: f64, b: f64, x: f64) -> f64 {
    const MAX_ITERS: usize = 200;
    const EPS: f64 = 1e-12;
    const FPMIN: f64 = 1e-300;
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < FPMIN {
        d = FPMIN;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=MAX_ITERS {
        let m = m as f64;
        let m2 = 2.0 * m;
        // even step
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        h *= d * c;
        // odd step
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            break;
        }
    }
    h
}

/// Minimum sample size per side required for [`bootstrap_percentile_p`].
pub const MIN_BOOTSTRAP_SAMPLES: usize = 20;

/// Maximum sample size per side accepted by [`bootstrap_percentile_p`].
pub const MAX_BOOTSTRAP_SAMPLES: usize = 10_000;

/// Exact linear-interpolated percentile in `O(n)` average time on a mutable
/// scratch slice via quickselect (`select_nth_unstable_by`).
fn quickselect_percentile(buf: &mut [f64], p: f64) -> f64 {
    let n = buf.len();
    if n == 0 {
        return 0.0;
    }
    if n == 1 {
        return buf[0];
    }
    let idx = (p / 100.0) * (n - 1) as f64;
    let lo = (idx.floor() as usize).min(n - 1);
    let hi = (idx.ceil() as usize).min(n - 1);
    let (_, lo_ref, right) = buf.select_nth_unstable_by(lo, f64::total_cmp);
    let lo_val = *lo_ref;
    if hi > lo {
        let hi_val = right
            .iter()
            .copied()
            .min_by(f64::total_cmp)
            .unwrap_or(lo_val);
        let frac = idx - lo as f64;
        (1.0 - frac) * lo_val + frac * hi_val
    } else {
        lo_val
    }
}

/// Deterministic SplitMix64 PRNG step.
fn splitmix64_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Deterministic two-sided percentile bootstrap p-value comparing the `p`-th
/// percentile of `sample_a` and `sample_b`.
///
/// Uses a fixed-seed SplitMix64 generator (`B = 2000` replicates) so results
/// are reproducible across runs and architectures. Returns `None` when either
/// sample is outside `MIN_BOOTSTRAP_SAMPLES..=MAX_BOOTSTRAP_SAMPLES`, when `p`
/// is outside `0.0..=100.0` (or non-finite), or when any observation is
/// non-finite. Returns `Some(1.0)` when the observed percentiles are equal.
#[must_use]
pub fn bootstrap_percentile_p(sample_a: &[f64], sample_b: &[f64], p: f64) -> Option<f64> {
    if sample_a.len() < MIN_BOOTSTRAP_SAMPLES
        || sample_b.len() < MIN_BOOTSTRAP_SAMPLES
        || sample_a.len() > MAX_BOOTSTRAP_SAMPLES
        || sample_b.len() > MAX_BOOTSTRAP_SAMPLES
        || !p.is_finite()
        || !(0.0..=100.0).contains(&p)
        || sample_a.iter().any(|v| !v.is_finite())
        || sample_b.iter().any(|v| !v.is_finite())
    {
        return None;
    }

    let mut buf_a = sample_a.to_vec();
    let mut buf_b = sample_b.to_vec();
    let obs_a = quickselect_percentile(&mut buf_a, p);
    let obs_b = quickselect_percentile(&mut buf_b, p);
    if obs_a == obs_b {
        return Some(1.0);
    }

    const B: usize = 2_000;
    let na = sample_a.len() as u64;
    let nb = sample_b.len() as u64;
    let mut state: u64 = 0x517C_C1B7_2722_0A95;
    let mut le_zero: usize = 0;
    let mut ge_zero: usize = 0;

    for _ in 0..B {
        for slot in &mut buf_a {
            let idx = (splitmix64_next(&mut state) % na) as usize;
            if let Some(&v) = sample_a.get(idx) {
                *slot = v;
            }
        }
        for slot in &mut buf_b {
            let idx = (splitmix64_next(&mut state) % nb) as usize;
            if let Some(&v) = sample_b.get(idx) {
                *slot = v;
            }
        }
        let pa = quickselect_percentile(&mut buf_a, p);
        let pb = quickselect_percentile(&mut buf_b, p);
        if pb <= pa {
            le_zero += 1;
        }
        if pb >= pa {
            ge_zero += 1;
        }
    }

    Some(((2.0 * (le_zero.min(ge_zero) as f64 + 1.0)) / (B as f64 + 1.0)).min(1.0))
}

/// Lanczos approximation of `ln Γ(x)`.
#[allow(clippy::excessive_precision)] // the g coefficients are canonical
fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.99999999999980993,
        676.5203681218851,
        -1259.1392167224028,
        771.32342877765313,
        -176.61502916214059,
        12.507343278686905,
        -0.13857109526572012,
        9.9843695780195716e-6,
        1.5056327351493116e-7,
    ];
    if x < 0.5 {
        // reflection formula
        std::f64::consts::PI.ln() - (std::f64::consts::PI * x).sin().ln() - ln_gamma(1.0 - x)
    } else {
        let x = x - 1.0;
        let mut ag = G[0];
        let t = x + 7.5;
        for (i, g) in G.iter().enumerate().skip(1) {
            ag += g / (x + i as f64);
        }
        0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + ag.ln()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_sorting_with_nan_does_not_panic() {
        let s = Sample::from_values(vec![3.0, f64::NAN, 1.0, 2.0, f64::NAN]);
        let _ = s.percentile(50.0);
        let _ = s.percentiles(&[10.0, 50.0, 95.0]);
    }

    #[test]
    fn welch_t_rejects_non_finite_and_handles_small_positive_variances() {
        assert!(welch_t(&[1.0, f64::NAN], &[1.0, 2.0]).is_none());
        assert!(welch_t(&[1.0, 2.0], &[1.0, f64::INFINITY]).is_none());
        assert!(welch_t(&[1.0, 1.0], &[2.0, 2.0]).is_none());

        // Small non-zero variances (smaller than f64::EPSILON) must still compute.
        let a = [0.000_000_001, 0.000_000_002, 0.000_000_001_5];
        let b = [0.000_000_010, 0.000_000_011, 0.000_000_010_5];
        let (t, df) = welch_t(&a, &b).expect("small variance Welch t");
        assert!(t < 0.0 && df > 0.0);
        assert!(t_two_sided_p(t, df) < 0.05);
    }

    #[test]
    fn t_two_sided_p_guards_non_finite_and_invalid_df() {
        assert_eq!(t_two_sided_p(f64::NAN, 4.0), 1.0);
        assert_eq!(t_two_sided_p(f64::INFINITY, 4.0), 1.0);
        assert_eq!(t_two_sided_p(2.0, f64::NAN), 1.0);
        assert_eq!(t_two_sided_p(2.0, f64::INFINITY), 1.0);
        assert_eq!(t_two_sided_p(2.0, 0.0), 1.0);
        assert_eq!(t_two_sided_p(2.0, -3.0), 1.0);
    }

    #[test]
    fn quickselect_percentile_matches_sample_percentile_exactly() {
        let cases: &[&[f64]] = &[
            &[42.0],
            &[10.0, 20.0],
            &[100.0, 10.0, 40.0, 20.0, 30.0],
            &[
                95.5, 12.1, 45.0, 88.8, 12.1, 3.0, 77.7, 61.2, 19.4, 50.0, 101.0, 2.5, 33.3, 66.6,
                44.4, 55.5, 22.2, 77.7, 11.1, 99.9,
            ],
            &[
                5.0, 1.0, 9.0, 3.0, 7.0, 2.0, 8.0, 4.0, 6.0, 10.0, 15.0, 11.0, 19.0, 13.0, 17.0,
                12.0, 18.0, 14.0, 16.0, 20.0, 21.0,
            ],
        ];
        for &data in cases {
            let sample = Sample::from_values(data.to_vec());
            for p in [0.0, 50.0, 95.0, 99.0, 100.0] {
                let mut scratch = data.to_vec();
                let got = quickselect_percentile(&mut scratch, p);
                let expected = sample.percentile(p);
                assert!(
                    (got - expected).abs() < 1e-12,
                    "p={p}, len={}: got {got}, expected {expected}",
                    data.len()
                );
            }
        }
    }

    #[test]
    fn bootstrap_percentile_p_detects_shift_and_is_deterministic() {
        let a: Vec<f64> = (0..MIN_BOOTSTRAP_SAMPLES)
            .map(|i| 100.0 + (i as f64) * 0.5)
            .collect();
        let b: Vec<f64> = (0..MIN_BOOTSTRAP_SAMPLES)
            .map(|i| 200.0 + (i as f64) * 0.5)
            .collect();
        let p1 = bootstrap_percentile_p(&a, &b, 95.0).expect("valid bootstrap p");
        let p2 = bootstrap_percentile_p(&a, &b, 95.0).expect("deterministic bootstrap p");
        assert_eq!(p1, p2);
        assert!(
            p1 < 0.05,
            "expected p < 0.05 on clearly shifted samples, got {p1}"
        );
    }

    #[test]
    fn bootstrap_percentile_p_is_non_significant_on_overlapping_samples() {
        let a: Vec<f64> = (0..MIN_BOOTSTRAP_SAMPLES)
            .map(|i| 100.0 + (i as f64) * 2.0)
            .collect();
        let b: Vec<f64> = (0..MIN_BOOTSTRAP_SAMPLES)
            .map(|i| 101.0 + (i as f64) * 2.0)
            .collect();
        let p = bootstrap_percentile_p(&a, &b, 95.0).expect("valid bootstrap p");
        assert!(
            p > 0.05,
            "expected p > 0.05 on overlapping samples, got {p}"
        );
    }

    #[test]
    fn bootstrap_percentile_p_returns_one_on_identical_constant_samples() {
        let a = vec![50.0; MIN_BOOTSTRAP_SAMPLES];
        let b = vec![50.0; MIN_BOOTSTRAP_SAMPLES];
        assert_eq!(bootstrap_percentile_p(&a, &b, 95.0), Some(1.0));
    }

    #[test]
    fn bootstrap_percentile_p_validates_bounds_and_non_finite_inputs() {
        let valid = vec![10.0; MIN_BOOTSTRAP_SAMPLES];
        let short = vec![10.0; MIN_BOOTSTRAP_SAMPLES - 1];
        assert!(bootstrap_percentile_p(&short, &valid, 95.0).is_none());
        assert!(bootstrap_percentile_p(&valid, &short, 95.0).is_none());

        let too_large = vec![10.0; MAX_BOOTSTRAP_SAMPLES + 1];
        let started = std::time::Instant::now();
        assert!(bootstrap_percentile_p(&too_large, &valid, 95.0).is_none());
        assert!(bootstrap_percentile_p(&valid, &too_large, 95.0).is_none());
        assert!(
            started.elapsed() < std::time::Duration::from_millis(10),
            "rejection of oversized samples must be upfront and fast"
        );

        assert!(bootstrap_percentile_p(&valid, &valid, f64::NAN).is_none());
        assert!(bootstrap_percentile_p(&valid, &valid, -1.0).is_none());
        assert!(bootstrap_percentile_p(&valid, &valid, 101.0).is_none());

        let mut with_nan = valid.clone();
        with_nan[5] = f64::NAN;
        assert!(bootstrap_percentile_p(&with_nan, &valid, 95.0).is_none());
        assert!(bootstrap_percentile_p(&valid, &with_nan, 95.0).is_none());

        let mut with_inf = valid.clone();
        with_inf[3] = f64::INFINITY;
        assert!(bootstrap_percentile_p(&with_inf, &valid, 95.0).is_none());
        assert!(bootstrap_percentile_p(&valid, &with_inf, 95.0).is_none());
    }
}
