//! Regression detection against a previously recorded baseline report.
//!
//! Enables `llmrace` to act as a CI quality gate: race the providers, then
//! compare the fresh results against a JSON report from an earlier run and
//! fail (non-zero exit) when any metric degrades beyond a threshold.

use crate::bench::LaneSummary;
use serde::Deserialize;

/// Threshold-aware comparison result for one provider.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderComparison {
    /// Provider id.
    pub name: String,
    /// Model recorded in the baseline.
    pub baseline_model: String,
    /// Model benchmarked now.
    pub current_model: String,
    /// Per-metric comparisons.
    pub metrics: Vec<MetricComparison>,
}

/// Comparison of a single metric between baseline and current run.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricComparison {
    /// Human-readable metric label.
    pub label: &'static str,
    /// Baseline value.
    pub baseline: f64,
    /// Current value.
    pub current: f64,
    /// Relative change in percent (positive = worse for latency metrics).
    pub delta_pct: f64,
    /// Whether this metric regressed beyond the threshold.
    pub regressed: bool,
    /// Welch p-value for the difference, when both sides carry per-run
    /// samples (`None` for reports saved without per-run data).
    pub p_value: Option<f64>,
}

/// Outcome of a full baseline comparison.
#[derive(Debug, Clone, Default)]
pub struct RegressionReport {
    /// Per-provider comparisons for providers present in both runs.
    pub comparisons: Vec<ProviderComparison>,
    /// Providers benchmarked now that were absent from the baseline.
    pub new_providers: Vec<String>,
    /// Providers present in the baseline that were not benchmarked now.
    pub skipped_providers: Vec<String>,
}

impl RegressionReport {
    /// `true` when at least one metric regressed beyond the threshold.
    #[must_use]
    pub fn any_regressed(&self) -> bool {
        self.comparisons
            .iter()
            .any(|c| c.metrics.iter().any(|m| m.regressed))
    }

    /// `true` when at least one metric regressed beyond the threshold
    /// **and** the difference is statistically significant (Welch
    /// p < 0.05, when per-run data allows the test). Threshold breaches
    /// without a significant p-value are treated as noise — the
    /// flake-resistant gate for CI.
    #[must_use]
    pub fn any_regressed_significant(&self) -> bool {
        self.comparisons.iter().any(|c| {
            c.metrics
                .iter()
                .any(|m| m.regressed && m.p_value.is_some_and(|p| p < 0.05))
        })
    }

    /// Renders the comparison as a terminal table.
    pub fn terminal_table(&self, max_regression_pct: f64) -> String {
        let mut out = format!(
            "regression check vs baseline (threshold {max_regression_pct:.0}%)\n{:<14} {:<16} {:>18} {:>18} {:>9}\n",
            "Provider", "Metric", "Baseline", "Current", "Δ"
        );
        out.push_str(&"-".repeat(out.len()));
        out.push('\n');
        for c in &self.comparisons {
            for m in &c.metrics {
                let mark = if m.regressed { " ✗" } else { "" };
                let p_note = match m.p_value {
                    Some(p) if p < 0.05 => format!(" p={p:.3}"),
                    Some(p) => format!(" p={p:.2} n.s."),
                    None => String::new(),
                };
                out.push_str(&format!(
                    "{:<14} {:<16} {:>16.1} \u{2192} {:<16.1} {:>+8.1}%{}{}\n",
                    c.name, m.label, m.baseline, m.current, m.delta_pct, p_note, mark
                ));
            }
        }
        for name in &self.new_providers {
            out.push_str(&format!("{name:<14} new provider (no baseline)\n"));
        }
        for name in &self.skipped_providers {
            out.push_str(&format!("{name:<14} in baseline but not benchmarked\n"));
        }
        if self.any_regressed() {
            out.push_str("\n✗ REGRESSION DETECTED: at least one metric exceeded the threshold\n");
        } else {
            out.push_str("\n✓ no regressions beyond threshold\n");
        }
        out
    }
}

/// The subset of a saved JSON report needed for regression checks.
#[derive(Debug, Clone, Deserialize)]
pub struct BaselineReport {
    /// Providers recorded in the baseline.
    pub providers: Vec<BaselineProvider>,
}

/// One per-run measurement of a baseline (the report's `runs` array).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BaselineRun {
    /// Round latency in milliseconds.
    #[serde(default)]
    pub total_ms: f64,
    /// Round time-to-first-byte in milliseconds.
    #[serde(default)]
    pub ttfb_ms: f64,
    /// Round throughput in tokens per second.
    #[serde(default)]
    pub tokens_per_sec: f64,
}

/// One provider entry of a baseline report.
#[derive(Debug, Clone, Deserialize)]
pub struct BaselineProvider {
    /// Provider id.
    pub provider: String,
    /// Prompt-variant label for sweep baselines; `None` for plain races.
    #[serde(default)]
    pub variant: Option<String>,
    /// Model that was benchmarked.
    #[serde(default)]
    pub model: String,
    /// Mean total latency in milliseconds.
    #[serde(default)]
    pub mean_total_ms: f64,
    /// p95 total latency in milliseconds.
    #[serde(default)]
    pub p95_total_ms: f64,
    /// Mean time-to-first-byte in milliseconds.
    #[serde(default)]
    pub mean_ttfb_ms: f64,
    /// Aggregate throughput in tokens per second.
    #[serde(default)]
    pub mean_tokens_per_sec: f64,
    /// Per-run measurements, when the report was saved with them.
    #[serde(default)]
    pub runs: Vec<BaselineRun>,
}

impl BaselineReport {
    /// Parses a baseline from the JSON written by `llmrace --json`.
    pub fn from_json_str(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }
}

/// Compares a fresh race against a baseline with a regression threshold in percent.
///
/// Latency metrics (mean, p95, TTFB) regress when they increase beyond the
/// threshold; throughput regresses when it decreases beyond the threshold.
/// Providers must have succeeded in the current run to be compared.
///
/// ```
/// use llmrace::regression::{evaluate, BaselineReport};
///
/// let baseline = BaselineReport::from_json_str(
///     r#"{"providers":[{"provider":"mock","model":"m","mean_total_ms":100.0,
///        "p95_total_ms":120.0,"mean_ttfb_ms":20.0,"mean_tokens_per_sec":80.0}]}"#,
/// ).unwrap();
///
/// let current = llmrace::bench::LaneSummary {
///     name: "mock".into(),
///     model: "m".into(),
///     runs: vec![llmrace::bench::RunMetrics {
///         total_ms: 105.0, ttfb_ms: 21.0, tokens: 6, chars: 24,
///         prompt_tokens: 0, tool_args: String::new(),
///         server_ms: None, request_id: None, ratelimit_remaining: None,
///         ..llmrace::bench::RunMetrics::default()
///     }],
///     failures: Vec::new(),
///     retry_attempts: 0,
///     error: None,
///     prompt: "p".into(),
///     concurrency: 1,
///     variant: None,
///     tool_task: None,
/// };
/// let report = evaluate(&[current], &baseline, 15.0);
/// assert!(!report.any_regressed());
/// ```
pub fn evaluate(
    current: &[LaneSummary],
    baseline: &BaselineReport,
    max_regression_pct: f64,
) -> RegressionReport {
    let current_report = baseline_from_lanes(current);
    compare_baselines(&current_report, baseline, max_regression_pct)
}

/// Converts live lane summaries into the baseline subset used by comparisons.
fn baseline_from_lanes(lanes: &[LaneSummary]) -> BaselineReport {
    BaselineReport {
        providers: lanes
            .iter()
            .filter(|s| s.successful())
            .map(|s| BaselineProvider {
                provider: s.name.clone(),
                variant: s.variant.clone(),
                model: s.model.clone(),
                mean_total_ms: s.mean_total_ms(),
                p95_total_ms: s.p95_total_ms(),
                mean_ttfb_ms: s.mean_ttfb_ms(),
                mean_tokens_per_sec: s.mean_tokens_per_sec(),
                runs: s
                    .runs
                    .iter()
                    .map(|r| BaselineRun {
                        total_ms: r.total_ms,
                        ttfb_ms: r.ttfb_ms,
                        tokens_per_sec: r.tokens_per_sec(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Finds the baseline entry for one lane: exact variant match first, then a
/// variant-less entry for that provider (plain baselines still gate sweep
/// runs, per lane).
fn find_base<'a>(
    baseline: &'a BaselineReport,
    provider: &str,
    variant: Option<&str>,
) -> Option<&'a BaselineProvider> {
    if let Some(v) = variant {
        if let Some(hit) = baseline
            .providers
            .iter()
            .find(|p| p.provider == provider && p.variant.as_deref() == Some(v))
        {
            return Some(hit);
        }
    }
    baseline
        .providers
        .iter()
        .find(|p| p.provider == provider && p.variant.is_none())
}

/// Compares two saved JSON reports (A/B mode): `b` is treated as the
/// "current" side and `a` as the baseline.
///
/// ```
/// use llmrace::regression::{compare_baselines, BaselineReport};
///
/// let a = BaselineReport::from_json_str(
///     r#"{"providers":[{"provider":"mock","mean_total_ms":100.0,"p95_total_ms":120.0,
///        "mean_ttfb_ms":20.0,"mean_tokens_per_sec":80.0}]}"#).unwrap();
/// let b = BaselineReport::from_json_str(
///     r#"{"providers":[{"provider":"mock","mean_total_ms":200.0,"p95_total_ms":240.0,
///        "mean_ttfb_ms":40.0,"mean_tokens_per_sec":60.0}]}"#).unwrap();
/// let report = compare_baselines(&b, &a, 15.0);
/// assert!(report.any_regressed());
/// ```
pub fn compare_baselines(
    b: &BaselineReport,
    a: &BaselineReport,
    max_regression_pct: f64,
) -> RegressionReport {
    let mut report = RegressionReport::default();
    let mut compared: Vec<(String, Option<String>)> = Vec::new();

    for current in &b.providers {
        let Some(base) = find_base(a, &current.provider, current.variant.as_deref()) else {
            report.new_providers.push(current.provider.clone());
            continue;
        };
        compared.push((current.provider.clone(), current.variant.clone()));
        let metrics = vec![
            latency_metric(
                "mean latency",
                base.mean_total_ms,
                current.mean_total_ms,
                max_regression_pct,
                metric_p_value(&base.runs, &current.runs, |r| r.total_ms),
            ),
            latency_metric(
                "p95 latency",
                base.p95_total_ms,
                current.p95_total_ms,
                max_regression_pct,
                None, // p95 is derived, not per-run
            ),
            latency_metric(
                "ttfb",
                base.mean_ttfb_ms,
                current.mean_ttfb_ms,
                max_regression_pct,
                metric_p_value(&base.runs, &current.runs, |r| r.ttfb_ms),
            ),
            throughput_metric(
                "tokens/s",
                base.mean_tokens_per_sec,
                current.mean_tokens_per_sec,
                max_regression_pct,
                metric_p_value(&base.runs, &current.runs, |r| r.tokens_per_sec),
            ),
        ];
        report.comparisons.push(ProviderComparison {
            name: current.provider.clone(),
            baseline_model: base.model.clone(),
            current_model: current.model.clone(),
            metrics,
        });
    }

    report.skipped_providers = a
        .providers
        .iter()
        .filter(|p| !compared.contains(&(p.provider.clone(), p.variant.clone())))
        .map(|p| match &p.variant {
            Some(v) => format!("{} ({v})", p.provider),
            None => p.provider.clone(),
        })
        .collect();
    report
}

fn latency_metric(
    label: &'static str,
    baseline: f64,
    current: f64,
    threshold: f64,
    p_value: Option<f64>,
) -> MetricComparison {
    let delta_pct = if baseline.abs() < f64::EPSILON {
        0.0
    } else {
        (current - baseline) / baseline * 100.0
    };
    MetricComparison {
        label,
        baseline,
        current,
        delta_pct,
        regressed: delta_pct > threshold,
        p_value,
    }
}

fn throughput_metric(
    label: &'static str,
    baseline: f64,
    current: f64,
    threshold: f64,
    p_value: Option<f64>,
) -> MetricComparison {
    let delta_pct = if baseline.abs() < f64::EPSILON {
        0.0
    } else {
        (current - baseline) / baseline * 100.0
    };
    MetricComparison {
        label,
        baseline,
        current,
        delta_pct,
        regressed: delta_pct < -threshold,
        p_value,
    }
}

/// Welch p-value for a metric across two per-run series, when both sides
/// carry at least two samples.
fn metric_p_value(
    base_runs: &[BaselineRun],
    current_runs: &[BaselineRun],
    pick: fn(&BaselineRun) -> f64,
) -> Option<f64> {
    if base_runs.len() < 2 || current_runs.len() < 2 {
        return None;
    }
    let a: Vec<f64> = base_runs.iter().map(pick).collect();
    let b: Vec<f64> = current_runs.iter().map(pick).collect();
    let (t, df) = crate::stats::welch_t(&a, &b)?;
    Some(crate::stats::t_two_sided_p(t, df))
}
